//! Stand-in data for parts of the shell whose real source does not exist yet.
//!
//! Nothing here is read from disk. The lists are fixed so the layout can be
//! judged with realistic content; the UI labels them as samples. File and
//! folder names are content, not interface text, so they are not translated.

use crate::tabs::DocKind;

/// One row of the recent-files list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecentFile {
    pub title: &'static str,
    pub kind: DocKind,
    /// Folder the file would be in.
    pub location: &'static str,
    /// When it was last opened, already formatted.
    pub opened: &'static str,
}

pub const RECENT_FILES: [RecentFile; 7] = [
    RecentFile {
        title: "第三季度经营分析报告",
        kind: DocKind::Writer,
        location: "文稿 / 2026 / 经营",
        opened: "今天 14:20",
    },
    RecentFile {
        title: "2026 年度预算",
        kind: DocKind::Sheets,
        location: "文稿 / 2026 / 财务",
        opened: "今天 11:05",
    },
    RecentFile {
        title: "新製品発表会",
        kind: DocKind::Slides,
        location: "文稿 / 发布",
        opened: "昨天",
    },
    RecentFile {
        title: "会议纪要 11-20",
        kind: DocKind::Markdown,
        location: "笔记 / 周会",
        opened: "昨天",
    },
    RecentFile {
        title: "供应商合同（签署版）",
        kind: DocKind::Pdf,
        location: "下载",
        opened: "11 月 19 日",
    },
    RecentFile {
        title: "产品路线图",
        kind: DocKind::Slides,
        location: "文稿 / 规划",
        opened: "11 月 18 日",
    },
    RecentFile {
        title: "客户访谈记录",
        kind: DocKind::Writer,
        location: "文稿 / 调研",
        opened: "11 月 15 日",
    },
];

/// One row of the workspace tree, already flattened for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub name: &'static str,
    /// Nesting level, 0 for the top.
    pub depth: u8,
    /// `None` for a folder.
    pub kind: Option<DocKind>,
    /// Folders only: whether its children are listed.
    pub expanded: bool,
}

const fn folder(name: &'static str, depth: u8, expanded: bool) -> TreeRow {
    TreeRow {
        name,
        depth,
        kind: None,
        expanded,
    }
}

const fn file(name: &'static str, depth: u8, kind: DocKind) -> TreeRow {
    TreeRow {
        name,
        depth,
        kind: Some(kind),
        expanded: false,
    }
}

pub const WORKSPACE_TREE: [TreeRow; 9] = [
    folder("2026", 0, true),
    folder("经营", 1, true),
    file("第三季度经营分析报告", 2, DocKind::Writer),
    file("经营数据", 2, DocKind::Sheets),
    folder("财务", 1, false),
    folder("发布", 0, true),
    file("新製品発表会", 1, DocKind::Slides),
    folder("笔记", 0, false),
    folder("归档", 0, false),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_files_cover_every_kind() {
        for kind in DocKind::ALL {
            assert!(
                RECENT_FILES.iter().any(|file| file.kind == kind),
                "no recent {kind:?} file"
            );
        }
    }

    #[test]
    fn the_tree_is_a_valid_preorder_listing() {
        let mut previous: Option<TreeRow> = None;
        for row in WORKSPACE_TREE {
            match previous {
                None => assert_eq!(row.depth, 0),
                Some(parent) => {
                    assert!(row.depth <= parent.depth + 1, "{} skips a level", row.name);
                    if row.depth > parent.depth {
                        assert!(
                            parent.kind.is_none() && parent.expanded,
                            "{} is listed under something that is not an open folder",
                            row.name
                        );
                    }
                }
            }
            previous = Some(row);
        }
    }
}
