//! Who may do what. Access is set per folder (a rule covers the folder and
//! everything under it, the longest matching folder wins) and can be
//! granted per document for the current session, which is how a document
//! that was never saved gets any access at all. Everything starts Off.
//!
//! One policy serves the built-in assistant and MCP clients alike.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::catalogue::Capability;

/// How much the assistant and MCP clients may do with a document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// The document does not exist for them.
    #[default]
    Off,
    /// Reading only.
    ReadOnly,
    /// Reading; every change and every save is asked about.
    Ask,
    /// Reading and changes; saving is asked about.
    Trusted,
    /// Everything, without asking.
    FullAccess,
}

impl Level {
    pub const ALL: [Level; 5] = [
        Level::Off,
        Level::ReadOnly,
        Level::Ask,
        Level::Trusted,
        Level::FullAccess,
    ];

    /// A stable machine name, for settings files and the audit log.
    pub fn key(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::ReadOnly => "read_only",
            Level::Ask => "ask",
            Level::Trusted => "trusted",
            Level::FullAccess => "full_access",
        }
    }

    pub fn from_key(key: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|level| level.key() == key)
    }
}

/// Why a call is asked about or refused, for the interface to translate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The level is read-only.
    ReadOnly,
    /// A change to a document's text.
    Edit,
    /// A new document.
    Create,
    /// Writing a file to disk.
    Save,
}

impl Reason {
    pub fn key(self) -> &'static str {
        match self {
            Reason::ReadOnly => "read_only",
            Reason::Edit => "edit",
            Reason::Create => "create",
            Reason::Save => "save",
        }
    }
}

/// What the policy says about one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask(Reason),
    Deny(Reason),
    /// The target does not exist for the caller: answer as if it were
    /// unknown, not refused.
    Hidden,
}

/// What a level allows a capability to do.
pub fn judge(level: Level, capability: Capability) -> Verdict {
    use Capability::*;
    match (level, capability) {
        (Level::Off, _) => Verdict::Hidden,
        (_, List | Read) => Verdict::Allow,
        (Level::ReadOnly, Edit) => Verdict::Deny(Reason::ReadOnly),
        (Level::ReadOnly, Create) => Verdict::Deny(Reason::ReadOnly),
        (Level::ReadOnly, Save) => Verdict::Deny(Reason::ReadOnly),
        (Level::Ask, Edit) => Verdict::Ask(Reason::Edit),
        (Level::Ask, Create) => Verdict::Ask(Reason::Create),
        (Level::Ask | Level::Trusted, Save) => Verdict::Ask(Reason::Save),
        (Level::Trusted, Edit | Create) => Verdict::Allow,
        (Level::FullAccess, _) => Verdict::Allow,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Folder rules. A folder's rule covers everything under it; the
    /// deepest matching folder wins.
    pub folders: Vec<(PathBuf, Level)>,
    /// What new documents (no file yet) start with. Off by default, so the
    /// assistant sees only what the user lets it see.
    pub untitled: Level,
    /// Session grants, by document id: they win over the folder rules and
    /// end when the program does.
    pub grants: HashMap<String, Level>,
    /// Documents a caller created, by document id: the conversation or
    /// connection that created it, and its level for that caller only. It
    /// holds while the document has no file; once saved, the folder's rule
    /// applies like for any other document.
    pub creations: HashMap<String, (String, Level)>,
}

impl Policy {
    /// The level of a file or folder by the folder rules. Both sides are
    /// compared normalised, so `/notes/../private` is not under `/notes`
    /// and a link into a folder counts as that folder.
    pub fn level_of_path(&self, path: &Path) -> Level {
        let path = normalize(path);
        self.folders
            .iter()
            .filter(|(folder, _)| path.starts_with(folder))
            .max_by_key(|(folder, _)| folder.components().count())
            .map_or(Level::Off, |(_, level)| *level)
    }

    /// The level of an open document: its session grant, else its file's
    /// folder rule, else the level for documents without a file.
    pub fn level_of(&self, id: &str, path: Option<&Path>) -> Level {
        self.level_for("", id, path)
    }

    /// The level of an open document for one caller (`session`): as
    /// [`Self::level_of`], and a document the caller created itself, still
    /// without a file, at the level it was given then.
    pub fn level_for(&self, session: &str, id: &str, path: Option<&Path>) -> Level {
        if let Some(level) = self.grants.get(id) {
            return *level;
        }
        match path {
            Some(path) => self.level_of_path(path),
            None => match self.creations.get(id) {
                Some((creator, level)) if !session.is_empty() && creator == session => *level,
                _ => self.untitled,
            },
        }
    }

    /// The level that decides whether a caller may create a document: that
    /// of new documents, where Off means asking (the new document would
    /// otherwise never be visible to anyone) and Read only means no.
    pub fn create_level(&self) -> Level {
        match self.untitled {
            Level::Off => Level::Ask,
            level => level,
        }
    }

    /// The caller in `session` created document `id`: give it `level` for
    /// that caller, while the document has no file.
    pub fn grant_creator(&mut self, session: &str, id: &str, level: Level) {
        self.creations
            .insert(id.to_owned(), (session.to_owned(), level));
    }

    /// Set or clear (with Off) the rule for a folder.
    pub fn set_folder(&mut self, folder: PathBuf, level: Level) {
        let folder = normalize(&folder);
        self.folders.retain(|(existing, _)| *existing != folder);
        if level != Level::Off {
            self.folders.push((folder, level));
        }
    }

    /// Grant a document a level for this session, or take the grant back
    /// with `None`.
    pub fn grant(&mut self, id: &str, level: Option<Level>) {
        match level {
            Some(level) => {
                self.grants.insert(id.to_owned(), level);
            }
            None => {
                self.grants.remove(id);
            }
        }
    }
}

/// A path with `.` and `..` worked out and links resolved: the deepest
/// part that exists is resolved on disk, the rest is appended as written.
/// So a file that does not exist yet still lands in the folder its parent
/// really is.
pub fn normalize(path: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut existing = lexical.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return lexical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_allow_more_and_more() {
        use Capability::*;
        assert_eq!(judge(Level::Off, List), Verdict::Hidden);
        assert_eq!(judge(Level::ReadOnly, Read), Verdict::Allow);
        assert_eq!(
            judge(Level::ReadOnly, Edit),
            Verdict::Deny(Reason::ReadOnly)
        );
        assert_eq!(judge(Level::Ask, Read), Verdict::Allow);
        assert_eq!(judge(Level::Ask, Edit), Verdict::Ask(Reason::Edit));
        assert_eq!(judge(Level::Trusted, Edit), Verdict::Allow);
        assert_eq!(judge(Level::Trusted, Save), Verdict::Ask(Reason::Save));
        assert_eq!(judge(Level::FullAccess, Save), Verdict::Allow);
        for level in Level::ALL {
            assert_eq!(Level::from_key(level.key()), Some(level));
        }
    }

    #[test]
    fn the_deepest_folder_rule_and_session_grants_win() {
        let mut policy = Policy::default();
        assert_eq!(policy.level_of("1", Some(Path::new("/a/b.md"))), Level::Off);
        policy.set_folder("/notes".into(), Level::ReadOnly);
        policy.set_folder("/notes/drafts".into(), Level::Trusted);
        assert_eq!(
            policy.level_of_path(Path::new("/notes/x.md")),
            Level::ReadOnly
        );
        assert_eq!(
            policy.level_of_path(Path::new("/notes/drafts/y.md")),
            Level::Trusted
        );
        // A folder whose name only starts the same is not inside.
        assert_eq!(policy.level_of_path(Path::new("/notes2/z.md")), Level::Off);
        // Documents without a file follow `untitled` unless granted.
        assert_eq!(policy.level_of("7", None), Level::Off);
        policy.grant("7", Some(Level::Ask));
        assert_eq!(policy.level_of("7", None), Level::Ask);
        policy.grant("7", None);
        assert_eq!(policy.level_of("7", None), Level::Off);
        // A grant wins over the folder.
        policy.grant("8", Some(Level::Off));
        assert_eq!(
            policy.level_of("8", Some(Path::new("/notes/drafts/y.md"))),
            Level::Off
        );
        // Off removes a rule.
        policy.set_folder("/notes".into(), Level::Off);
        assert_eq!(policy.folders.len(), 1);
    }

    #[test]
    fn creations_belong_to_their_creator_until_saved() {
        let mut policy = Policy::default();
        policy.grant_creator("chat-1", "9", Level::Trusted);
        assert_eq!(policy.level_for("chat-1", "9", None), Level::Trusted);
        // Another conversation or client does not inherit it.
        assert_eq!(policy.level_for("connection-3", "9", None), Level::Off);
        assert_eq!(policy.level_of("9", None), Level::Off);
        // Saved into a folder that is Off: the folder decides.
        assert_eq!(
            policy.level_for("chat-1", "9", Some(Path::new("/closed/新.md"))),
            Level::Off
        );
    }

    #[test]
    fn creating_follows_the_level_of_new_documents() {
        let mut policy = Policy::default();
        assert_eq!(policy.create_level(), Level::Ask);
        policy.untitled = Level::ReadOnly;
        assert_eq!(
            judge(policy.create_level(), Capability::Create),
            Verdict::Deny(Reason::ReadOnly)
        );
        policy.untitled = Level::Trusted;
        assert_eq!(
            judge(policy.create_level(), Capability::Create),
            Verdict::Allow
        );
    }

    #[test]
    fn paths_are_compared_normalised() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        std::fs::create_dir(&notes).unwrap();
        std::fs::create_dir(dir.path().join("private")).unwrap();
        let mut policy = Policy::default();
        // A rule given through `..` is the folder it names.
        policy.set_folder(dir.path().join("private/../notes"), Level::Ask);
        assert_eq!(policy.level_of_path(&notes.join("a.md")), Level::Ask);
        assert_eq!(
            policy.level_of_path(&notes.join("../private/b.md")),
            Level::Off
        );
        // A path that does not exist yet is worked out from its text.
        assert_eq!(policy.level_of_path(&notes.join("x/../new.md")), Level::Ask);
    }
}
