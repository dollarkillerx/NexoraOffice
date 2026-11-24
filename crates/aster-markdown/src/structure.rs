//! The structure of a Markdown document: its headings, for the outline the
//! assistant's tools report and for jumping to a section.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::syntax;

/// A heading: its level (1 to 6), its text, and the 1-based line it starts
/// on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub line: usize,
    /// Byte offset where the heading starts in the source.
    pub offset: usize,
}

/// The headings of `source`, in order. Headings inside code blocks are not
/// headings; ones in quotes and list items are. Front matter has none, even
/// where its lines look like Markdown headings.
pub fn headings(source: &str) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut current: Option<Heading> = None;
    let start = syntax::front_matter(source).map_or(0, |range| range.end);
    let mut line = 1;
    let mut counted = 0;
    let events = Parser::new_ext(&source[start..], syntax::options())
        .into_offset_iter()
        .map(|(event, range)| (event, start + range.start..start + range.end));
    for (event, range) in events {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                line += source[counted..range.start].matches('\n').count();
                counted = range.start;
                current = Some(Heading {
                    level: heading_level(level),
                    text: String::new(),
                    line,
                    offset: range.start,
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(mut heading) = current.take() {
                    heading.text = heading.text.trim().to_owned();
                    out.push(heading);
                }
            }
            Event::Text(text) | Event::Code(text) | Event::InlineMath(text) => {
                if let Some(heading) = &mut current {
                    heading.text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(heading) = &mut current {
                    heading.text.push(' ');
                }
            }
            _ => {}
        }
    }
    out
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The anchor a heading gets, as GitHub makes it: lower case, spaces and
/// hyphens to `-`, other punctuation dropped; letters and digits of every
/// script kept (`## 第二章 Notes!` is `第二章-notes`).
pub fn slug(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '_' {
                Some(c.to_lowercase().collect::<String>())
            } else if c == ' ' || c == '-' {
                Some("-".to_owned())
            } else {
                None
            }
        })
        .collect()
}

/// Gives each heading its anchor, numbering repeats as GitHub does: the
/// second `Notes` is `notes-1`.
#[derive(Debug, Default)]
pub struct Slugs {
    seen: std::collections::HashMap<String, usize>,
}

impl Slugs {
    pub fn next(&mut self, text: &str) -> String {
        let base = slug(text);
        let count = self.seen.entry(base.clone()).or_insert(0);
        let anchor = if *count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        anchor
    }
}

/// A table of contents for `source`: a nested list linking to each heading,
/// indented by level relative to the highest level present. `None` when
/// there are no headings.
pub fn table_of_contents(source: &str) -> Option<String> {
    let headings = headings(source);
    let top = headings.iter().map(|h| h.level).min()?;
    let mut slugs = Slugs::default();
    let mut out = String::new();
    for heading in &headings {
        let indent = "  ".repeat(usize::from(heading.level - top));
        let label = heading.text.replace('[', "\\[").replace(']', "\\]");
        out.push_str(&format!(
            "{indent}- [{label}](#{})\n",
            slugs.next(&heading.text)
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_made_as_github_makes_them() {
        assert_eq!(slug("Hello World"), "hello-world");
        assert_eq!(slug("第二章 Notes!"), "第二章-notes");
        assert_eq!(slug("C++ & Rust: 2026"), "c--rust-2026");
        assert_eq!(slug("a_b-c"), "a_b-c");
        let mut slugs = Slugs::default();
        assert_eq!(slugs.next("Notes"), "notes");
        assert_eq!(slugs.next("Notes"), "notes-1");
        assert_eq!(slugs.next("notes"), "notes-2");
    }

    #[test]
    fn a_table_of_contents_links_every_heading() {
        let source = "## 概述\n\n文字\n\n### 细节 [1]\n\n## 概述\n\n```\n# 不是标题\n```\n";
        assert_eq!(
            table_of_contents(source).unwrap(),
            "- [概述](#概述)\n  - [细节 \\[1\\]](#细节-1)\n- [概述](#概述-1)\n"
        );
        assert_eq!(table_of_contents("没有标题\n"), None);
    }

    #[test]
    fn headings_with_levels_and_lines() {
        let source = "# 标题 `代码`\n\n正文\n\n## 第二节\n\n```\n# 不是标题\n```\n\n设置\n===\n\n> ### 引用里的\n";
        let found = headings(source);
        let summary: Vec<(u8, &str, usize)> = found
            .iter()
            .map(|h| (h.level, h.text.as_str(), h.line))
            .collect();
        assert_eq!(
            summary,
            [
                (1, "标题 代码", 1),
                (2, "第二节", 5),
                (1, "设置", 11),
                (3, "引用里的", 14)
            ]
        );
        assert_eq!(&source[found[1].offset..found[1].offset + 3], "## ");
        assert!(headings("没有标题").is_empty());
    }

    #[test]
    fn front_matter_has_no_headings() {
        // Without front matter `title: 标题` over `---` would be a heading,
        // and so would the comment.
        let source = "---\ntitle: 标题\n# 注释\n---\n\n# 正文\n\n设置\n---\n";
        let found = headings(source);
        let summary: Vec<(u8, &str, usize)> = found
            .iter()
            .map(|h| (h.level, h.text.as_str(), h.line))
            .collect();
        assert_eq!(summary, [(1, "正文", 6), (2, "设置", 8)]);
        assert_eq!(&source[found[0].offset..found[0].offset + 2], "# ");
        // Not at the start: no front matter, so the heading is one.
        let source = "前言\n\n---\ntitle: 标题\n---\n";
        assert_eq!(headings(source)[0].text, "title: 标题");
    }
}
