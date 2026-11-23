//! The folder shown in the sidebar, read one folder at a time.
//!
//! [`read_folder`] lists one folder; it touches the disk, so the shell runs it
//! on a worker thread. [`FileTree`] holds what has been read, which folders are
//! expanded, and turns that into rows. A folder is read when it is first
//! shown expanded, and again when the shell asks for a refresh (the window
//! came back to the front, or the refresh button). The folder is not watched.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, DirEntry};
use std::io;
use std::path::{Path, PathBuf};

use crate::names::{Filter, natural_cmp};

/// At most this many entries of one folder are listed; the rest are counted.
pub const LIST_LIMIT: usize = 2000;
/// A folder with more entries than this is not read to the end.
pub const SCAN_LIMIT: usize = 100_000;
/// Folders that hold what tools make, not documents. Names starting with a
/// dot (`.git`, `.venv`) are hidden anyway.
const SKIPPED_FOLDERS: &[&str] = &["node_modules", "target", "__pycache__"];

/// The kinds of file the tree shows. Everything else is left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Markdown,
    Text,
    Word,
    Spreadsheet,
    Presentation,
    Pdf,
}

impl FileKind {
    /// By extension, ignoring case.
    pub fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "md" | "markdown" => Self::Markdown,
            "txt" => Self::Text,
            "docx" => Self::Word,
            "xlsx" => Self::Spreadsheet,
            "pptx" => Self::Presentation,
            "pdf" => Self::Pdf,
            _ => return None,
        })
    }

    /// Whether this version can open it. The others are shown, marked as not
    /// supported yet.
    pub fn opens(self) -> bool {
        matches!(self, Self::Markdown | Self::Text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Folder,
    /// A link to a folder that contains it: shown, but it does not expand.
    Loop,
    File(FileKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// For display. A name that is not valid Unicode has replacement marks.
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
}

/// What one folder holds, in display order: folders first, then files, each
/// by name with numbers in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// Entries beyond [`LIST_LIMIT`] that are not listed.
    pub omitted: usize,
    /// The folder had more than [`SCAN_LIMIT`] entries and was not read to
    /// the end, so `omitted` is a lower bound.
    pub incomplete: bool,
}

/// Why a folder could not be listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    Missing,
    Denied,
    NotFolder,
    Other(String),
}

impl From<io::Error> for ReadError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::NotFound => Self::Missing,
            io::ErrorKind::PermissionDenied => Self::Denied,
            io::ErrorKind::NotADirectory => Self::NotFolder,
            _ => Self::Other(err.to_string()),
        }
    }
}

/// List one folder. Hidden entries, the folders in [`SKIPPED_FOLDERS`] and
/// files of kinds the tree does not show are left out. Links are followed;
/// broken links are left out.
pub fn read_folder(dir: &Path) -> Result<Listing, ReadError> {
    if !fs::metadata(dir)?.is_dir() {
        return Err(ReadError::NotFolder);
    }
    let real = fs::canonicalize(dir).ok();
    let mut entries = Vec::new();
    let mut incomplete = false;
    for (scanned, item) in fs::read_dir(dir)?.enumerate() {
        if scanned == SCAN_LIMIT {
            incomplete = true;
            break;
        }
        let Ok(item) = item else {
            continue;
        };
        if let Some(entry) = entry(&item, real.as_deref()) {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| {
        let folder = |entry: &Entry| !matches!(entry.kind, EntryKind::File(_));
        folder(b)
            .cmp(&folder(a))
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    let omitted = entries.len().saturating_sub(LIST_LIMIT);
    entries.truncate(LIST_LIMIT);
    Ok(Listing {
        entries,
        omitted,
        incomplete,
    })
}

fn entry(item: &DirEntry, real_dir: Option<&Path>) -> Option<Entry> {
    let name = item.file_name().to_string_lossy().into_owned();
    if name.starts_with('.') || is_hidden(item) {
        return None;
    }
    let path = item.path();
    let file_type = item.file_type().ok()?;
    let (is_dir, is_file) = if file_type.is_symlink() {
        let target = fs::metadata(&path).ok()?;
        (target.is_dir(), target.is_file())
    } else {
        (file_type.is_dir(), file_type.is_file())
    };
    let kind = if is_dir {
        if SKIPPED_FOLDERS.contains(&name.as_str()) {
            return None;
        }
        if file_type.is_symlink() && leads_back(&path, real_dir) {
            EntryKind::Loop
        } else {
            EntryKind::Folder
        }
    } else if is_file {
        EntryKind::File(FileKind::of(&path)?)
    } else {
        return None;
    };
    Some(Entry { name, path, kind })
}

/// Whether the link at `path` points at the folder it is in, or above it.
fn leads_back(path: &Path, real_dir: Option<&Path>) -> bool {
    match (fs::canonicalize(path), real_dir) {
        (Ok(target), Some(dir)) => dir.starts_with(target),
        // Cannot tell: do not offer to go in.
        _ => true,
    }
}

#[cfg(windows)]
fn is_hidden(item: &DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    item.metadata()
        .is_ok_and(|meta| meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
}

#[cfg(not(windows))]
fn is_hidden(_item: &DirEntry) -> bool {
    false
}

/// A line under a folder that is not an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// The folder has not been read yet.
    Reading,
    /// Nothing in it that the tree shows.
    Empty,
    /// More entries than listed.
    Omitted {
        count: usize,
        at_least: bool,
    },
    Failed(ReadError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    Folder { expanded: bool },
    Loop,
    File(FileKind),
    Note(Note),
}

/// One line of the tree on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// 0 for what is directly in the folder.
    pub depth: usize,
    /// Empty for a note.
    pub name: String,
    /// The entry; for a note, the folder it is about.
    pub path: PathBuf,
    pub kind: RowKind,
}

#[derive(Debug, Default)]
struct Folder {
    read: Option<Result<Listing, ReadError>>,
    reading: bool,
}

/// A folder and what has been read of it.
#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    folders: HashMap<PathBuf, Folder>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            expanded: BTreeSet::new(),
            folders: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Why the folder itself could not be listed, once that is known.
    pub fn root_error(&self) -> Option<&ReadError> {
        self.folders.get(&self.root)?.read.as_ref()?.as_ref().err()
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        path == self.root || self.expanded.contains(path)
    }

    pub fn expand(&mut self, path: &Path) {
        if path.starts_with(&self.root) && path != self.root {
            self.expanded.insert(path.to_owned());
        }
    }

    pub fn collapse(&mut self, path: &Path) {
        self.expanded.remove(path);
    }

    pub fn toggle(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.collapse(path);
        } else {
            self.expand(path);
        }
    }

    /// Expand folders remembered from an earlier run. Those outside the
    /// folder are ignored; those that no longer exist do no harm.
    pub fn restore_expanded(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            self.expand(&path);
        }
    }

    /// The expanded folders, to remember.
    pub fn expanded(&self) -> Vec<PathBuf> {
        self.expanded.iter().cloned().collect()
    }

    /// The folders on screen: the folder itself and every expanded folder
    /// whose parents are all expanded and listed.
    fn shown_folders(&self) -> Vec<PathBuf> {
        let mut shown = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            if let Some(Folder {
                read: Some(Ok(listing)),
                ..
            }) = self.folders.get(&dir)
            {
                for entry in &listing.entries {
                    if entry.kind == EntryKind::Folder && self.expanded.contains(&entry.path) {
                        stack.push(entry.path.clone());
                    }
                }
            }
            shown.push(dir);
        }
        shown
    }

    /// The folders on screen that have never been read and are not being
    /// read: read them with [`read_folder`] and hand the results to
    /// [`Self::finished`]. They are marked as being read.
    pub fn wanted(&mut self) -> Vec<PathBuf> {
        let mut wanted = Vec::new();
        for dir in self.shown_folders() {
            let folder = self.folders.entry(dir.clone()).or_default();
            if folder.read.is_none() && !folder.reading {
                folder.reading = true;
                wanted.push(dir);
            }
        }
        wanted
    }

    /// Every folder on screen, to read again. What was read before stays on
    /// screen until the new listing arrives.
    pub fn reread(&mut self) -> Vec<PathBuf> {
        let mut wanted = Vec::new();
        for dir in self.shown_folders() {
            let folder = self.folders.entry(dir.clone()).or_default();
            if !folder.reading {
                folder.reading = true;
                wanted.push(dir);
            }
        }
        wanted
    }

    /// A listing asked for by [`Self::wanted`] or [`Self::reread`] arrived.
    /// What was read of folders that are gone from it is forgotten.
    pub fn finished(&mut self, dir: PathBuf, result: Result<Listing, ReadError>) {
        if !dir.starts_with(&self.root) {
            return;
        }
        // Its parent was read again in the meantime and no longer has it.
        if let Some(parent) = dir.parent().filter(|_| dir != self.root)
            && let Some(Folder {
                read: Some(Ok(listing)),
                ..
            }) = self.folders.get(parent)
            && !listing
                .entries
                .iter()
                .any(|entry| entry.kind == EntryKind::Folder && entry.path == dir)
        {
            self.folders.remove(&dir);
            return;
        }
        if let Ok(listing) = &result {
            let kept: BTreeSet<&Path> = listing
                .entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::Folder)
                .map(|entry| entry.path.as_path())
                .collect();
            let gone: Vec<PathBuf> = self
                .folders
                .keys()
                .filter(|path| path.parent() == Some(&dir) && !kept.contains(path.as_path()))
                .cloned()
                .collect();
            self.folders
                .retain(|path, _| !gone.iter().any(|gone| path.starts_with(gone)));
        }
        let folder = self.folders.entry(dir).or_default();
        folder.read = Some(result);
        folder.reading = false;
    }

    /// The tree as lines on screen. With a filter, only entries whose names
    /// match are shown, with the folders that lead to them; folders that have
    /// not been read are not searched.
    pub fn rows(&self, filter: Option<&Filter>) -> Vec<Row> {
        let mut rows = Vec::new();
        match filter {
            None => self.walk(&self.root, 0, &mut rows),
            Some(filter) => {
                self.walk_filtered(&self.root, 0, filter, &mut rows);
            }
        }
        rows
    }

    fn walk(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let note = |note| Row {
            depth,
            name: String::new(),
            path: dir.to_owned(),
            kind: RowKind::Note(note),
        };
        let listing = match self
            .folders
            .get(dir)
            .and_then(|folder| folder.read.as_ref())
        {
            None => return rows.push(note(Note::Reading)),
            Some(Err(err)) => return rows.push(note(Note::Failed(err.clone()))),
            Some(Ok(listing)) => listing,
        };
        if listing.entries.is_empty() && listing.omitted == 0 && !listing.incomplete {
            return rows.push(note(Note::Empty));
        }
        for entry in &listing.entries {
            let expanded = entry.kind == EntryKind::Folder && self.expanded.contains(&entry.path);
            rows.push(Row {
                depth,
                name: entry.name.clone(),
                path: entry.path.clone(),
                kind: match entry.kind {
                    EntryKind::Folder => RowKind::Folder { expanded },
                    EntryKind::Loop => RowKind::Loop,
                    EntryKind::File(kind) => RowKind::File(kind),
                },
            });
            if expanded {
                self.walk(&entry.path, depth + 1, rows);
            }
        }
        if listing.omitted > 0 || listing.incomplete {
            rows.push(note(Note::Omitted {
                count: listing.omitted,
                at_least: listing.incomplete,
            }));
        }
    }

    /// Returns whether anything was added.
    fn walk_filtered(
        &self,
        dir: &Path,
        depth: usize,
        filter: &Filter,
        rows: &mut Vec<Row>,
    ) -> bool {
        let Some(Folder {
            read: Some(Ok(listing)),
            ..
        }) = self.folders.get(dir)
        else {
            return false;
        };
        let before = rows.len();
        for entry in &listing.entries {
            match entry.kind {
                EntryKind::Folder => {
                    let at = rows.len();
                    let inside = self.walk_filtered(&entry.path, depth + 1, filter, rows);
                    if inside || filter.matches(&entry.name) {
                        rows.insert(
                            at,
                            Row {
                                depth,
                                name: entry.name.clone(),
                                path: entry.path.clone(),
                                kind: RowKind::Folder { expanded: inside },
                            },
                        );
                    }
                }
                EntryKind::Loop | EntryKind::File(_) if filter.matches(&entry.name) => {
                    rows.push(Row {
                        depth,
                        name: entry.name.clone(),
                        path: entry.path.clone(),
                        kind: match entry.kind {
                            EntryKind::File(kind) => RowKind::File(kind),
                            _ => RowKind::Loop,
                        },
                    });
                }
                _ => {}
            }
        }
        rows.len() > before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Read every folder the tree wants until it wants no more.
    fn settle(tree: &mut FileTree) {
        loop {
            let wanted = tree.wanted();
            if wanted.is_empty() {
                return;
            }
            for dir in wanted {
                let result = read_folder(&dir);
                tree.finished(dir, result);
            }
        }
    }

    fn shown(tree: &FileTree, filter: Option<&Filter>) -> Vec<String> {
        tree.rows(filter)
            .iter()
            .map(|row| {
                let label = match &row.kind {
                    RowKind::Folder { expanded: true } => format!("{}/ -", row.name),
                    RowKind::Folder { expanded: false } => format!("{}/ +", row.name),
                    RowKind::Loop => format!("{}/ loop", row.name),
                    RowKind::File(_) => row.name.clone(),
                    RowKind::Note(note) => format!("({note:?})"),
                };
                format!("{}{label}", "  ".repeat(row.depth))
            })
            .collect()
    }

    #[test]
    fn a_folder_lists_documents_and_folders_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in [
            "会议纪要.md",
            "README.MARKDOWN",
            "メモ.txt",
            "报告.docx",
            "预算.xlsx",
            "発表.pptx",
            "合同.pdf",
            "photo.png",
            "script.rs",
            "no-extension",
            ".hidden.md",
            ".git/config",
            "node_modules/x/index.md",
            "target/debug/notes.md",
            "第10章/a.md",
            "第2章/a.md",
            ".obsidian/app.md",
        ] {
            touch(&root.join(name));
        }
        let listing = read_folder(root).unwrap();
        // Folders first, in number order; then files by name.
        assert_eq!(
            names(&listing),
            [
                "第2章",
                "第10章",
                "README.MARKDOWN",
                "メモ.txt",
                "会议纪要.md",
                "合同.pdf",
                "报告.docx",
                "発表.pptx",
                "预算.xlsx",
            ]
        );
        let kind = |name: &str| {
            listing
                .entries
                .iter()
                .find(|e| e.name == name)
                .unwrap()
                .kind
        };
        assert_eq!(kind("第2章"), EntryKind::Folder);
        assert_eq!(kind("会议纪要.md"), EntryKind::File(FileKind::Markdown));
        assert_eq!(kind("README.MARKDOWN"), EntryKind::File(FileKind::Markdown));
        assert_eq!(kind("メモ.txt"), EntryKind::File(FileKind::Text));
        assert_eq!(kind("报告.docx"), EntryKind::File(FileKind::Word));
        assert_eq!(kind("合同.pdf"), EntryKind::File(FileKind::Pdf));
        assert!(FileKind::Markdown.opens() && FileKind::Text.opens());
        assert!(!FileKind::Word.opens() && !FileKind::Pdf.opens());
        assert_eq!(listing.omitted, 0);
        assert!(!listing.incomplete);
    }

    #[test]
    fn what_cannot_be_listed_says_why() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_folder(&dir.path().join("不存在")),
            Err(ReadError::Missing)
        );
        let file = dir.path().join("a.md");
        touch(&file);
        assert_eq!(read_folder(&file), Err(ReadError::NotFolder));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_without_permission_is_reported_not_skipped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("私密");
        touch(&locked.join("secret.md"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let result = read_folder(&locked);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if running_as_superuser() {
            // The superuser reads anything; nothing to check.
            return;
        }
        assert_eq!(result, Err(ReadError::Denied));

        // In the tree, the folder is listed and says why it is empty.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let mut tree = FileTree::new(dir.path().to_owned());
        tree.expand(&locked);
        settle(&mut tree);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(shown(&tree, None), ["私密/ -", "  (Failed(Denied))"]);
    }

    /// Whether the tests run as the superuser, judged by whether a folder
    /// with no permissions at all can be listed.
    #[cfg(unix)]
    fn running_as_superuser() -> bool {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        fs::create_dir(&probe).unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&probe).is_ok();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
        readable
    }

    #[cfg(unix)]
    #[test]
    fn a_link_back_up_the_tree_does_not_expand() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("文档/a.md"));
        // Links to itself, to its parent, to a file, to nothing, and to a
        // folder elsewhere that is fine to enter.
        symlink(root.join("文档"), root.join("文档/自己")).unwrap();
        symlink(root, root.join("文档/上面")).unwrap();
        symlink(root.join("文档/a.md"), root.join("文档/b.md")).unwrap();
        symlink(root.join("nowhere"), root.join("文档/broken.md")).unwrap();
        touch(&root.join("其他/c.md"));
        symlink(root.join("其他"), root.join("文档/旁边")).unwrap();

        let listing = read_folder(&root.join("文档")).unwrap();
        let kind = |name: &str| {
            listing
                .entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.kind)
        };
        assert_eq!(kind("自己"), Some(EntryKind::Loop));
        assert_eq!(kind("上面"), Some(EntryKind::Loop));
        assert_eq!(kind("旁边"), Some(EntryKind::Folder));
        assert_eq!(kind("b.md"), Some(EntryKind::File(FileKind::Markdown)));
        assert_eq!(kind("broken.md"), None);

        // Expanding everything that offers itself ends.
        let mut tree = FileTree::new(root.to_owned());
        for _ in 0..10 {
            settle(&mut tree);
            for row in tree.rows(None) {
                if matches!(row.kind, RowKind::Folder { expanded: false }) {
                    tree.expand(&row.path);
                }
            }
        }
        settle(&mut tree);
        assert_eq!(
            shown(&tree, None),
            [
                "其他/ -",
                "  c.md",
                "文档/ -",
                "  上面/ loop",
                "  旁边/ -",
                "    c.md",
                "  自己/ loop",
                "  a.md",
                "  b.md",
            ]
        );
    }

    #[test]
    fn a_huge_folder_is_listed_in_part() {
        let dir = tempfile::tempdir().unwrap();
        let extra = 300;
        for i in 0..LIST_LIMIT + extra {
            fs::write(dir.path().join(format!("{i:05}.md")), b"").unwrap();
        }
        // Not shown, so not counted.
        for i in 0..50 {
            fs::write(dir.path().join(format!("{i}.png")), b"").unwrap();
        }
        let listing = read_folder(dir.path()).unwrap();
        assert_eq!(listing.entries.len(), LIST_LIMIT);
        assert_eq!(listing.omitted, extra);
        assert_eq!(listing.entries[0].name, "00000.md");

        let mut tree = FileTree::new(dir.path().to_owned());
        settle(&mut tree);
        let rows = tree.rows(None);
        assert_eq!(rows.len(), LIST_LIMIT + 1);
        assert_eq!(
            rows.last().unwrap().kind,
            RowKind::Note(Note::Omitted {
                count: extra,
                at_least: false
            })
        );
    }

    #[test]
    fn folders_are_read_when_shown_and_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        touch(&root.join("2026/经营/报告.md"));
        touch(&root.join("2026/财务/预算.xlsx"));
        touch(&root.join("空/photo.png"));
        touch(&root.join("readme.md"));

        let mut tree = FileTree::new(root.clone());
        assert_eq!(shown(&tree, None), ["(Reading)"]);
        assert_eq!(tree.wanted(), std::slice::from_ref(&root));
        assert!(tree.wanted().is_empty(), "already being read");
        let listing = read_folder(&root);
        tree.finished(root.clone(), listing);
        assert_eq!(shown(&tree, None), ["2026/ +", "空/ +", "readme.md"]);
        assert!(tree.wanted().is_empty(), "nothing expanded");

        tree.toggle(&root.join("2026"));
        assert_eq!(
            shown(&tree, None),
            ["2026/ -", "  (Reading)", "空/ +", "readme.md"]
        );
        settle(&mut tree);
        tree.expand(&root.join("2026/经营"));
        tree.expand(&root.join("空"));
        settle(&mut tree);
        assert_eq!(
            shown(&tree, None),
            [
                "2026/ -",
                "  经营/ -",
                "    报告.md",
                "  财务/ +",
                "空/ -",
                "  (Empty)",
                "readme.md",
            ]
        );
        assert_eq!(
            tree.expanded(),
            [root.join("2026"), root.join("2026/经营"), root.join("空")]
        );

        // Collapsing a parent hides the expanded child; it is still
        // remembered as expanded.
        tree.toggle(&root.join("2026"));
        assert_eq!(
            shown(&tree, None),
            ["2026/ +", "空/ -", "  (Empty)", "readme.md"]
        );
        assert!(tree.is_expanded(&root.join("2026/经营")));
    }

    #[test]
    fn filtering_shows_matches_with_the_folders_that_lead_to_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        touch(&root.join("2026/经营/第三季度报告.md"));
        touch(&root.join("2026/经营/数据.xlsx"));
        touch(&root.join("报告模板/空.txt"));
        touch(&root.join("归档/旧报告.md"));
        touch(&root.join("notes.md"));
        let mut tree = FileTree::new(root.clone());
        tree.restore_expanded([
            root.join("2026"),
            root.join("2026/经营"),
            root.join("报告模板"),
        ]);
        settle(&mut tree);

        let filter = Filter::new("报告").unwrap();
        // 归档 was never expanded, so it was never read and is not searched.
        assert_eq!(
            shown(&tree, Some(&filter)),
            ["2026/ -", "  经营/ -", "    第三季度报告.md", "报告模板/ +"]
        );
        assert!(tree.rows(Some(&Filter::new("zzz").unwrap())).is_empty());
    }

    #[test]
    fn rereading_keeps_the_old_listing_until_the_new_one_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        touch(&root.join("a/one.md"));
        touch(&root.join("b.md"));
        let mut tree = FileTree::new(root.clone());
        tree.expand(&root.join("a"));
        settle(&mut tree);
        assert_eq!(shown(&tree, None), ["a/ -", "  one.md", "b.md"]);

        // Files change while the window is in the background.
        fs::remove_dir_all(root.join("a")).unwrap();
        touch(&root.join("c.md"));
        let again = tree.reread();
        assert_eq!(again.len(), 2);
        assert_eq!(
            shown(&tree, None),
            ["a/ -", "  one.md", "b.md"],
            "unchanged so far"
        );
        for dir in again {
            let result = read_folder(&dir);
            tree.finished(dir, result);
        }
        assert_eq!(shown(&tree, None), ["b.md", "c.md"]);

        // The folder comes back: it is read afresh, not from memory.
        touch(&root.join("a/two.md"));
        for dir in tree.reread() {
            let result = read_folder(&dir);
            tree.finished(dir, result);
        }
        settle(&mut tree);
        assert_eq!(shown(&tree, None), ["a/ -", "  two.md", "b.md", "c.md"]);
    }

    #[test]
    fn a_folder_that_is_gone_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("已删除");
        let mut tree = FileTree::new(root.clone());
        tree.restore_expanded([root.join("x"), PathBuf::from("/elsewhere"), root.clone()]);
        assert_eq!(tree.expanded(), [root.join("x")]);
        settle(&mut tree);
        assert_eq!(tree.root_error(), Some(&ReadError::Missing));
        assert_eq!(shown(&tree, None), ["(Failed(Missing))"]);
        // Results for another folder are ignored.
        tree.finished(PathBuf::from("/elsewhere"), Ok(Listing::default()));
        assert_eq!(tree.root_error(), Some(&ReadError::Missing));
    }
}
