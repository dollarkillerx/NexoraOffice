//! Markdown to a standalone HTML page, for File > Export as HTML.
//!
//! The page carries its own small style sheet and no scripts. Addresses are
//! written as they are in the source, so pictures next to the Markdown file
//! show when the page is saved in the same folder.
//!
//! Raw HTML in the source is written out as text, not as markup: a document
//! may hold text an assistant, an MCP client or someone else put there, and
//! the page should not run its scripts, event handlers or load what its
//! tags point at when it is opened. Links with `javascript:`, `vbscript:`
//! or `data:` addresses lose them.
//!
//! Pictures on the web are not put on the page as pictures: opening the
//! page would fetch them and tell their server it was opened. Each becomes
//! a link to the picture, marked 🖼 and named by its description, which the
//! reader can follow. Local pictures and `data:image/…` stay pictures.

use pulldown_cmark::{CowStr, Event, Parser, Tag, TagEnd, html};

use crate::syntax;

const STYLE: &str = "\
body{margin:0;background:#fff;color:#1b1b1b;\
font:16px/1.65 -apple-system,BlinkMacSystemFont,\"Segoe UI\",\"PingFang SC\",\"Hiragino Sans\",\"Microsoft YaHei\",sans-serif}\
main{max-width:46rem;margin:0 auto;padding:3rem 1.5rem}\
h1,h2{border-bottom:1px solid #e3e3e1;padding-bottom:.3em}\
a{color:#6a48d6}\
code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.9em;background:#f1f1f0;border-radius:4px;padding:.1em .3em}\
pre{background:#f1f1f0;border-radius:6px;padding:1em;overflow:auto}\
pre code{background:none;padding:0}\
blockquote{margin:0;padding:0 1em;color:#5a5a58;border-left:3px solid #dededd}\
table{border-collapse:collapse}th,td{border:1px solid #dededd;padding:.35em .7em}\
img{max-width:100%}hr{border:0;border-top:1px solid #dededd}\
@media (prefers-color-scheme:dark){body{background:#1b1b1d;color:#e8e8e6}\
code,pre{background:#2a2a2d}a{color:#a891f0}th,td,hr,blockquote{border-color:#3a3a3d}}";

/// The HTML of the document's body: what the Markdown renders to. Front
/// matter is data about the document, not part of it, and is left out.
pub fn body(source: &str) -> String {
    let source = match syntax::front_matter(source) {
        Some(front) => &source[front.end..],
        None => source,
    };
    let mut out = String::with_capacity(source.len() * 3 / 2);
    // Whether each picture being written is one from the web, turned into a
    // link: its end must end the link.
    let mut pictures: Vec<bool> = Vec::new();
    let events = Parser::new_ext(source, syntax::options()).flat_map(move |event| {
        let one = |event| vec![event];
        match event {
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) if is_web(&dest_url) => {
                pictures.push(true);
                vec![
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }),
                    Event::Text(CowStr::Borrowed("🖼 ")),
                ]
            }
            Event::Start(Tag::Image { .. }) => {
                pictures.push(false);
                one(sanitize(event))
            }
            Event::End(TagEnd::Image) => {
                if pictures.pop() == Some(true) {
                    one(Event::End(TagEnd::Link))
                } else {
                    one(event)
                }
            }
            other => one(sanitize(other)),
        }
    });
    html::push_html(&mut out, events);
    out
}

/// Whether an address is on the web.
fn is_web(address: &str) -> bool {
    let address = address.trim_start().to_ascii_lowercase();
    address.starts_with("http://") || address.starts_with("https://") || address.starts_with("//")
}

/// Raw HTML as text, and addresses that could run code taken away.
fn sanitize(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_address(dest_url, false),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_address(dest_url, true),
            title,
            id,
        }),
        other => other,
    }
}

/// An address that cannot run code: `javascript:` and `vbscript:` go, and so
/// does `data:` except for a picture's own data.
fn safe_address(address: CowStr<'_>, picture: bool) -> CowStr<'_> {
    let scheme: String = address
        .trim_start()
        .chars()
        .take_while(|c| *c != ':')
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    let unsafe_scheme = address.contains(':')
        && (scheme == "javascript"
            || scheme == "vbscript"
            || (scheme == "data"
                && !(picture
                    && address
                        .split_once(':')
                        .is_some_and(|(_, rest)| rest.trim_start().starts_with("image/")))));
    if unsafe_scheme {
        CowStr::Borrowed("#")
    } else {
        address
    }
}

/// A whole HTML page for `source`, with `title` in its head unless its
/// front matter has a `title` of its own.
pub fn page(source: &str, title: &str) -> String {
    let own = syntax::front_matter(source).and_then(|front| {
        crate::preview::read_front_matter(&source[front])
            .into_iter()
            .find_map(|line| match line {
                crate::preview::FrontMatterLine::Field { key, value }
                    if key.eq_ignore_ascii_case("title") && !value.is_empty() =>
                {
                    Some(value)
                }
                _ => None,
            })
    });
    let title = own.as_deref().unwrap_or(title);
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"generator\" content=\"Aster Office\">\n\
         <title>{}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n<main>\n{}</main>\n</body>\n</html>\n",
        escape(title),
        body(source)
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_is_left_out_and_its_title_used() {
        let source = "---\ntitle: \"周报\"\ntags: [a, b]\n---\n# 正文\n";
        let page = page(source, "file.md");
        assert!(page.contains("<title>周报</title>"), "{page}");
        assert!(!page.contains("tags"));
        assert!(!page.contains("<hr"));
        assert!(page.contains("<h1>正文</h1>"));
        // Without front matter the given title stays.
        assert!(super::page("# x\n", "file.md").contains("<title>file.md</title>"));
    }

    #[test]
    fn a_page_has_the_rendered_body_and_an_escaped_title() {
        let page = page(
            "# 标题\n\n**粗体** 和 [链接](a.md)\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] 完成\n\n![图](pic.png)",
            "笔记 <草稿>",
        );
        assert!(page.starts_with("<!DOCTYPE html>"));
        assert!(page.contains("<title>笔记 &lt;草稿&gt;</title>"));
        assert!(page.contains("<h1>标题</h1>"));
        assert!(page.contains("<strong>粗体</strong>"));
        assert!(page.contains("<a href=\"a.md\">链接</a>"));
        assert!(page.contains("<table>"));
        assert!(page.contains("type=\"checkbox\""));
        assert!(page.contains("<img src=\"pic.png\" alt=\"图\""));
        assert!(!page.contains("<script"));
    }

    #[test]
    fn raw_html_and_script_addresses_do_not_survive() {
        let page = body(
            "<script>alert(1)</script>\n\n文字 <img src=\"https://track.example/p.gif\" onerror=\"x()\"> 后\n\n\
             [点我](javascript:alert(1)) [也不行]( JaVaScRiPt:x) [数据](data:text/html,<b>x</b>)\n\n\
             ![图](data:image/png;base64,AAAA) ![坏图](javascript:x) [正常](https://example.com)",
        );
        assert!(!page.contains("<script"), "{page}");
        assert!(
            page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{page}"
        );
        assert!(!page.contains("<img src=\"https://track.example"), "{page}");
        assert!(!page.contains("javascript:"), "{page}");
        assert!(!page.contains("JaVaScRiPt:"), "{page}");
        assert!(!page.contains("data:text/html"), "{page}");
        assert!(
            page.contains("<img src=\"data:image/png;base64,AAAA\""),
            "{page}"
        );
        assert!(
            page.contains("<a href=\"https://example.com\">正常</a>"),
            "{page}"
        );
    }

    #[test]
    fn pictures_on_the_web_become_links_and_local_ones_stay() {
        let page = body(
            "![白板](https://example.com/b.png) 与 ![本地](pic.png) 与 ![小](//cdn.example/x.png)",
        );
        assert!(!page.contains("<img src=\"https://"), "{page}");
        assert!(!page.contains("<img src=\"//"), "{page}");
        assert!(
            page.contains("<a href=\"https://example.com/b.png\">🖼 白板</a>"),
            "{page}"
        );
        assert!(page.contains("<img src=\"pic.png\" alt=\"本地\""), "{page}");
        // A picture inside a link: the link still closes properly.
        let linked = body("[![图](https://example.com/p.png)](https://example.com)");
        assert_eq!(
            linked.matches("<a ").count(),
            linked.matches("</a>").count(),
            "{linked}"
        );
    }
}
