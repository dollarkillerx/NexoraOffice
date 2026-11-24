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

#[cfg(test)]
mod tests {
    use super::*;

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
