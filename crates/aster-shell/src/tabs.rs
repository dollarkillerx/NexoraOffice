//! Which documents are open, in what order, and which one is in front.

/// The kinds of document the suite handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocKind {
    Writer,
    Sheets,
    Slides,
    Markdown,
    Pdf,
}

impl DocKind {
    /// In the order the start page shows them.
    pub const ALL: [DocKind; 5] = [
        DocKind::Writer,
        DocKind::Sheets,
        DocKind::Slides,
        DocKind::Markdown,
        DocKind::Pdf,
    ];

    /// Stable index, used to hand the kind to the UI layer.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|kind| *kind == self)
            .expect("ALL lists every kind")
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// Product name of the editor.
    pub fn name(self) -> &'static str {
        match self {
            DocKind::Writer => "Writer",
            DocKind::Sheets => "Sheets",
            DocKind::Slides => "Slides",
            DocKind::Markdown => "Markdown",
            DocKind::Pdf => "PDF",
        }
    }

    /// The phase of the roadmap that delivers the editor.
    pub fn planned_phase(self) -> Option<u8> {
        match self {
            DocKind::Markdown => Some(1),
            DocKind::Sheets => Some(2),
            DocKind::Writer => Some(3),
            DocKind::Slides => Some(4),
            DocKind::Pdf => None,
        }
    }
}

/// Identifies a tab for as long as the window lives. Never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    pub kind: DocKind,
    pub title: String,
    /// The document has changes that are not saved yet.
    pub modified: bool,
}

/// The tab strip's state. `active` is `None` while the start page is showing,
/// which is always the case when no tab is open and can also be asked for while
/// tabs are open.
#[derive(Clone, Debug, Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u32,
    untitled: [u32; DocKind::ALL.len()],
}

impl Tabs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Position of the active tab in the strip.
    pub fn active_index(&self) -> Option<usize> {
        self.active.and_then(|id| self.index_of(id))
    }

    pub fn showing_start_page(&self) -> bool {
        self.active.is_none()
    }

    /// Open a tab at the end of the strip and bring it to the front.
    pub fn open(&mut self, kind: DocKind, title: impl Into<String>) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            kind,
            title: title.into(),
            modified: false,
        });
        self.active = Some(id);
        id
    }

    /// Open a new, empty document. `stem` is the title without its number, in
    /// the interface language. Titles are numbered per kind and a number is not
    /// handed out twice, even after its tab is closed.
    pub fn open_untitled(&mut self, kind: DocKind, stem: &str) -> TabId {
        let counter = &mut self.untitled[kind.index()];
        *counter += 1;
        let title = format!("{stem} {}", *counter);
        self.open(kind, title)
    }

    /// Bring the tab with this title and kind to the front if it is open.
    pub fn activate_existing(&mut self, kind: DocKind, title: &str) -> Option<TabId> {
        let id = self
            .tabs
            .iter()
            .find(|tab| tab.kind == kind && tab.title == title)?
            .id;
        self.active = Some(id);
        Some(id)
    }

    /// Give a tab a new title, for a document that was saved under a new name.
    pub fn rename(&mut self, id: TabId, title: impl Into<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.title = title.into();
        }
    }

    /// Set whether a tab's document has unsaved changes. Returns whether that
    /// was news, so the caller knows to redraw the strip.
    pub fn set_modified(&mut self, id: TabId, modified: bool) -> bool {
        match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) if tab.modified != modified => {
                tab.modified = modified;
                true
            }
            _ => false,
        }
    }

    /// How many open documents have unsaved changes.
    pub fn modified_count(&self) -> usize {
        self.tabs.iter().filter(|tab| tab.modified).count()
    }

    /// Bring a tab to the front. Returns false if there is no such tab.
    pub fn activate(&mut self, id: TabId) -> bool {
        let found = self.get(id).is_some();
        if found {
            self.active = Some(id);
        }
        found
    }

    /// Show the start page without closing anything.
    pub fn show_start_page(&mut self) {
        self.active = None;
    }

    /// Close a tab. If it was in front, the tab that takes its place in the
    /// strip comes to the front, or the one before it when it was the last;
    /// closing the only tab shows the start page. Returns the closed tab.
    pub fn close(&mut self, id: TabId) -> Option<Tab> {
        let index = self.index_of(id)?;
        let closed = self.tabs.remove(index);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
        }
        Some(closed)
    }

    /// Move to the next (or previous) tab, wrapping around. From the start page
    /// this goes to the first (or last) tab.
    pub fn cycle(&mut self, forward: bool) {
        let count = self.tabs.len();
        if count == 0 {
            return;
        }
        let index = match (self.active_index(), forward) {
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
            (None, true) => 0,
            (None, false) => count - 1,
        };
        self.active = Some(self.tabs[index].id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(tabs: &Tabs) -> Vec<&str> {
        tabs.tabs().iter().map(|tab| tab.title.as_str()).collect()
    }

    #[test]
    fn starts_on_the_start_page() {
        let tabs = Tabs::new();
        assert!(tabs.is_empty());
        assert!(tabs.showing_start_page());
        assert_eq!(tabs.active(), None);
    }

    #[test]
    fn opening_appends_and_activates() {
        let mut tabs = Tabs::new();
        let a = tabs.open(DocKind::Writer, "a");
        let b = tabs.open(DocKind::Sheets, "b");
        assert_eq!(titles(&tabs), ["a", "b"]);
        assert_eq!(tabs.active_id(), Some(b));
        assert_eq!(tabs.active_index(), Some(1));
        assert!(tabs.activate(a));
        assert_eq!(tabs.active().map(|tab| tab.kind), Some(DocKind::Writer));
        assert!(!tabs.showing_start_page());
    }

    #[test]
    fn ids_are_never_reused() {
        let mut tabs = Tabs::new();
        let a = tabs.open(DocKind::Writer, "a");
        tabs.close(a);
        let b = tabs.open(DocKind::Writer, "b");
        assert_ne!(a, b);
        assert!(!tabs.activate(a));
        assert_eq!(tabs.active_id(), Some(b));
    }

    #[test]
    fn untitled_documents_are_numbered_per_kind() {
        let mut tabs = Tabs::new();
        let first = tabs.open_untitled(DocKind::Writer, "文档");
        tabs.open_untitled(DocKind::Sheets, "表格");
        tabs.open_untitled(DocKind::Writer, "文档");
        assert_eq!(titles(&tabs), ["文档 1", "表格 1", "文档 2"]);
        // Closing one does not free its number, and the count belongs to the
        // kind, not to the wording.
        tabs.close(first);
        tabs.open_untitled(DocKind::Writer, "Document");
        assert_eq!(titles(&tabs), ["表格 1", "文档 2", "Document 3"]);
    }

    #[test]
    fn closing_the_active_tab_activates_its_right_neighbour() {
        let mut tabs = Tabs::new();
        let a = tabs.open(DocKind::Writer, "a");
        let b = tabs.open(DocKind::Writer, "b");
        let c = tabs.open(DocKind::Writer, "c");
        tabs.activate(b);
        assert_eq!(tabs.close(b).map(|tab| tab.title), Some("b".to_owned()));
        assert_eq!(tabs.active_id(), Some(c));
        // The last tab has no right neighbour: fall back to the left one.
        tabs.close(c);
        assert_eq!(tabs.active_id(), Some(a));
        tabs.close(a);
        assert!(tabs.is_empty());
        assert!(tabs.showing_start_page());
    }

    #[test]
    fn closing_a_background_tab_keeps_the_active_one() {
        let mut tabs = Tabs::new();
        let a = tabs.open(DocKind::Writer, "a");
        let b = tabs.open(DocKind::Writer, "b");
        tabs.close(a);
        assert_eq!(tabs.active_id(), Some(b));
        assert_eq!(tabs.active_index(), Some(0));
        assert_eq!(tabs.close(a), None);
    }

    #[test]
    fn the_start_page_can_be_shown_over_open_tabs() {
        let mut tabs = Tabs::new();
        let a = tabs.open(DocKind::Writer, "a");
        let b = tabs.open(DocKind::Writer, "b");
        tabs.show_start_page();
        assert!(tabs.showing_start_page());
        assert_eq!(tabs.len(), 2);
        // Closing a tab from the start page stays on the start page.
        tabs.close(b);
        assert!(tabs.showing_start_page());
        assert!(tabs.activate(a));
    }

    #[test]
    fn reopening_a_document_brings_its_tab_forward() {
        let mut tabs = Tabs::new();
        let a = tabs.open(DocKind::Writer, "报告");
        tabs.open(DocKind::Sheets, "报告");
        assert_eq!(tabs.activate_existing(DocKind::Writer, "报告"), Some(a));
        assert_eq!(tabs.active_id(), Some(a));
        assert_eq!(tabs.activate_existing(DocKind::Slides, "报告"), None);
        assert_eq!(tabs.len(), 2);
    }

    #[test]
    fn tabs_can_be_renamed_and_marked_modified() {
        let mut tabs = Tabs::new();
        let a = tabs.open(DocKind::Markdown, "未命名笔记 1");
        let b = tabs.open(DocKind::Markdown, "b.md");
        assert_eq!(tabs.modified_count(), 0);
        assert!(tabs.set_modified(a, true));
        // Saying it again is not news.
        assert!(!tabs.set_modified(a, true));
        assert_eq!(tabs.modified_count(), 1);
        tabs.rename(a, "notes.md");
        assert_eq!(titles(&tabs), ["notes.md", "b.md"]);
        assert!(tabs.get(a).is_some_and(|tab| tab.modified));
        assert!(tabs.set_modified(a, false));
        assert!(!tabs.set_modified(b, false));
        // A tab that is gone is ignored.
        tabs.close(a);
        tabs.rename(a, "x");
        assert!(!tabs.set_modified(a, true));
        assert_eq!(titles(&tabs), ["b.md"]);
    }

    #[test]
    fn cycling_wraps_around() {
        let mut tabs = Tabs::new();
        tabs.cycle(true);
        assert!(tabs.showing_start_page());
        let a = tabs.open(DocKind::Writer, "a");
        let b = tabs.open(DocKind::Writer, "b");
        tabs.cycle(true);
        assert_eq!(tabs.active_id(), Some(a));
        tabs.cycle(false);
        assert_eq!(tabs.active_id(), Some(b));
        tabs.show_start_page();
        tabs.cycle(false);
        assert_eq!(tabs.active_id(), Some(b));
    }

    #[test]
    fn kind_indices_round_trip() {
        for (index, kind) in DocKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.index(), index);
            assert_eq!(DocKind::from_index(index), Some(kind));
            assert!(!kind.name().is_empty());
        }
        assert_eq!(DocKind::from_index(DocKind::ALL.len()), None);
    }
}
