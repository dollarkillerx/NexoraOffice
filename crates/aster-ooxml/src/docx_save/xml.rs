//! Reading XML fragments as flat tokens, for the bytes between elements and
//! for checking that a slice of a part is an element or a run of whole
//! elements.

/// One piece of a fragment of XML.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok<'a> {
    /// `<w:hyperlink ...>`: the qualified name and the bytes.
    Open(&'a str, &'a str),
    /// `</w:hyperlink>`.
    Close(&'a str, &'a str),
    /// `<w:bookmarkStart .../>`, a comment, a declaration, or text.
    Other(&'a str),
    /// White space between elements.
    Space(&'a str),
}

impl<'a> Tok<'a> {
    pub fn bytes(&self) -> &'a str {
        match self {
            Tok::Open(_, b) | Tok::Close(_, b) | Tok::Other(b) | Tok::Space(b) => b,
        }
    }
}

/// The local part of a qualified name.
pub fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

fn name_at(tag: &str, skip: usize) -> &str {
    let rest = &tag[skip..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The tokens of `xml`; `None` if a tag does not end.
pub fn tokens(xml: &str) -> Option<Vec<Tok<'_>>> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'<' {
            let end = xml[at..].find('<').map_or(xml.len(), |i| at + i);
            let text = &xml[at..end];
            if text.trim().is_empty() {
                out.push(Tok::Space(text));
            } else {
                out.push(Tok::Other(text));
            }
            at = end;
            continue;
        }
        let rest = &xml[at..];
        let end = if rest.starts_with("<!--") {
            at + rest.find("-->")? + 3
        } else if rest.starts_with("<![CDATA[") {
            at + rest.find("]]>")? + 3
        } else if rest.starts_with("<?") {
            at + rest.find("?>")? + 2
        } else {
            // A tag: the first `>` outside quotes.
            let mut quote = 0u8;
            let mut i = at + 1;
            loop {
                let b = *bytes.get(i)?;
                if quote != 0 {
                    if b == quote {
                        quote = 0;
                    }
                } else if b == b'"' || b == b'\'' {
                    quote = b;
                } else if b == b'>' {
                    break i + 1;
                }
                i += 1;
            }
        };
        let tag = &xml[at..end];
        if tag.starts_with("</") {
            out.push(Tok::Close(name_at(tag, 2), tag));
        } else if tag.starts_with("<?") || tag.starts_with("<!") || tag.ends_with("/>") {
            out.push(Tok::Other(tag));
        } else {
            out.push(Tok::Open(name_at(tag, 1), tag));
        }
        at = end;
    }
    Some(out)
}

/// Whole elements and nothing else (white space allowed): every tag closes.
pub fn is_balanced(xml: &str) -> bool {
    let Some(tokens) = tokens(xml) else {
        return false;
    };
    let mut stack: Vec<&str> = Vec::new();
    for token in tokens {
        match token {
            Tok::Open(name, _) => stack.push(name),
            Tok::Close(name, _) => {
                if stack.pop() != Some(name) {
                    return false;
                }
            }
            _ => {}
        }
    }
    stack.is_empty()
}

/// `xml` is exactly one element, named `local_name`.
pub fn is_element(xml: &str, local_name: &str) -> bool {
    let Some(tokens) = tokens(xml) else {
        return false;
    };
    let mut depth = 0usize;
    let mut stack: Vec<&str> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Tok::Open(name, _) => {
                if depth == 0 && (i != 0 || local(name) != local_name) {
                    return false;
                }
                depth += 1;
                stack.push(name);
            }
            Tok::Close(name, _) => {
                if stack.pop() != Some(*name) {
                    return false;
                }
                depth -= 1;
                if depth == 0 && i + 1 != tokens.len() {
                    return false;
                }
            }
            Tok::Other(text) => {
                if depth == 0 && (i != 0 || tokens.len() != 1 || !text.starts_with('<')) {
                    return false;
                }
                if depth == 0 {
                    let name = name_at(text, 1);
                    return local(name) == local_name;
                }
            }
            Tok::Space(_) => {
                if depth == 0 {
                    return false;
                }
            }
        }
    }
    depth == 0 && !tokens.is_empty()
}

/// The length of the closing tags (and white space between them) a
/// fragment starts with.
pub fn leading_closes(xml: &str) -> usize {
    let Some(tokens) = tokens(xml) else {
        return 0;
    };
    let mut len = 0;
    let mut closes = 0;
    for token in tokens {
        match token {
            Tok::Close(..) => {
                len += token.bytes().len();
                closes = len;
            }
            Tok::Space(_) => len += token.bytes().len(),
            _ => break,
        }
    }
    closes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_tags_text_and_space() {
        let t = tokens(r#"<a x=">"> <b/>text</a>"#).unwrap();
        assert_eq!(t.len(), 5);
        assert!(matches!(t[0], Tok::Open("a", _)));
        assert!(matches!(t[1], Tok::Space(" ")));
        assert!(matches!(t[2], Tok::Other("<b/>")));
        assert!(matches!(t[3], Tok::Other("text")));
        assert!(matches!(t[4], Tok::Close("a", _)));
        assert!(tokens("<a").is_none());
    }

    #[test]
    fn elements_and_balance() {
        assert!(is_element("<w:p><w:r/></w:p>", "p"));
        assert!(is_element("<w:bookmarkEnd w:id=\"1\"/>", "bookmarkEnd"));
        assert!(!is_element("<w:p/><w:p/>", "p"));
        assert!(!is_element("<w:p></w:p>x", "p"));
        assert!(!is_element("<w:p><w:r></w:p>", "p"));
        assert!(!is_element("<w:p/>", "r"));
        assert!(is_balanced("<a/> <b></b>"));
        assert!(!is_balanced("<a></b>"));
        assert!(!is_balanced("</a>"));
    }

    #[test]
    fn leading_closing_tags() {
        assert_eq!(leading_closes("</a> </b><c>"), 9);
        assert_eq!(leading_closes("<c></a>"), 0);
        assert_eq!(leading_closes(""), 0);
    }
}
