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
}

impl Policy {
    /// The level of a file or folder by the folder rules.
    pub fn level_of_path(&self, path: &Path) -> Level {
        self.folders
            .iter()
            .filter(|(folder, _)| path.starts_with(folder))
            .max_by_key(|(folder, _)| folder.components().count())
            .map_or(Level::Off, |(_, level)| *level)
    }

    /// The level of an open document: its session grant, else its file's
    /// folder rule, else the level for documents without a file.
    pub fn level_of(&self, id: &str, path: Option<&Path>) -> Level {
        if let Some(level) = self.grants.get(id) {
            return *level;
        }
        match path {
            Some(path) => self.level_of_path(path),
            None => self.untitled,
        }
    }

    /// Set or clear (with Off) the rule for a folder.
    pub fn set_folder(&mut self, folder: PathBuf, level: Level) {
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
}
