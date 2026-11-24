//! Markdown to a standalone HTML page, for File > Export as HTML.
//!
//! The page carries its own small style sheet and no scripts. Addresses are
//! written as they are in the source, so pictures next to the Markdown file
//! show when the page is saved in the same folder. Raw HTML in the source
//! is passed through, as every Markdown renderer does; the page is the
//! user's own document.

use pulldown_cmark::{Parser, html};

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

/// The HTML of the document's body: what the Markdown renders to.
pub fn body(source: &str) -> String {
    let mut out = String::with_capacity(source.len() * 3 / 2);
    html::push_html(&mut out, Parser::new_ext(source, syntax::options()));
    out
}

/// A whole HTML page for `source`, with `title` in its head.
pub fn page(source: &str, title: &str) -> String {
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
}
