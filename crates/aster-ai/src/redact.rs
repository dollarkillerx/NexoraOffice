//! Masking likely secrets before text goes to a model provider: the user's
//! messages and every tool result. It catches the usual shapes: PEM private
//! key blocks, `password=` / `secret:`-style assignments, `Authorization`
//! headers, passwords in URLs (`scheme://user:pass@host`) and well-known
//! token formats (`sk-…`, `ghp_…`, `AKIA…`, `xoxb-…` and a few more).
//!
//! It is a safety net, not a guarantee: a secret in an unusual shape goes
//! through, and ordinary text that looks like an assignment (`Password: see
//! below`) is masked too. The scanning is written by hand, without regular
//! expressions. Every pattern is ASCII, so the scanning works on bytes and
//! only ever cuts the text next to an ASCII character, which keeps any
//! other text (CJK included) intact.

/// What a secret is replaced with.
pub const MASK: &str = "[REDACTED]";
const KEY_MASK: &str = "[REDACTED PRIVATE KEY]";

/// The text with its secrets masked, and how many were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub count: usize,
}

/// Mask the likely secrets in `text`. Running it again on its own output
/// changes nothing.
pub fn redact(text: &str) -> Redacted {
    let mut count = 0;
    let text = private_keys(text, &mut count);
    let text = url_credentials(&text, &mut count);
    let text = assignments(&text, &mut count);
    let text = tokens(&text, &mut count);
    Redacted { text, count }
}

/// `-----BEGIN … PRIVATE KEY-----` up to its `-----END …-----` line, or to
/// the end of the text when the end line is missing.
fn private_keys(text: &str, count: &mut usize) -> String {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let block = &rest[start..];
        let header_end = block[BEGIN.len()..]
            .find("-----")
            .map(|at| BEGIN.len() + at + 5);
        let is_private = header_end.is_some_and(|end| {
            !block[..end].contains('\n') && block[..end].contains("PRIVATE KEY")
        });
        let Some(header_end) = header_end.filter(|_| is_private) else {
            out.push_str(&rest[..start + BEGIN.len()]);
            rest = &rest[start + BEGIN.len()..];
            continue;
        };
        let end = block[header_end..]
            .find(END)
            .map(|at| header_end + at + END.len())
            .map(|after| {
                block[after..]
                    .find("-----")
                    .map_or(block.len(), |at| after + at + 5)
            })
            .unwrap_or(block.len());
        out.push_str(&rest[..start]);
        out.push_str(KEY_MASK);
        *count += 1;
        rest = &block[end..];
    }
    out.push_str(rest);
    out
}

/// The password in `scheme://user:password@host`.
fn url_credentials(text: &str, count: &mut usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = text[pos..].find("://") {
        let separator = pos + found;
        let authority_start = separator + 3;
        let authority_len = text[authority_start..]
            .find(|c: char| {
                matches!(c, '/' | '?' | '#' | '"' | '\'' | '<' | '>' | '`' | ')')
                    || c.is_whitespace()
            })
            .unwrap_or(text.len() - authority_start);
        let authority_end = authority_start + authority_len;
        let authority = &text[authority_start..authority_end];
        let has_scheme = text[..separator]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        let password = authority.rfind('@').and_then(|at| {
            let colon = authority[..at].find(':')?;
            let password = &authority[colon + 1..at];
            (!password.is_empty() && password != MASK).then_some((colon, at))
        });
        match password {
            Some((colon, at)) if has_scheme => {
                out.push_str(&text[pos..authority_start + colon + 1]);
                out.push_str(MASK);
                out.push_str(&text[authority_start + at..authority_end]);
                *count += 1;
            }
            _ => out.push_str(&text[pos..authority_end]),
        }
        pos = authority_end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Names whose value is a secret, matched at the end of an identifier so
/// `DB_PASSWORD`, `client_secret` and `x-api-key` count but `secretary`
/// does not. `pwd` is left out: `PWD` is the working directory.
const SECRET_NAMES: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "access_key",
    "private_key",
    "token",
    "authorization",
];

fn is_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

/// `name = value`, `name: value`, `"name": "value"` for the names above.
/// An `Authorization` value runs to the end of its line (`Bearer …`, `Basic
/// …`).
fn assignments(text: &str, count: &mut usize) -> String {
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut copied = 0;
    let mut i = 0;
    while i < len {
        if !is_identifier(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < len && is_identifier(bytes[i]) {
            i += 1;
        }
        let name = &lower[start..i];
        let Some(&key) = SECRET_NAMES.iter().find(|key| name.ends_with(*key)) else {
            continue;
        };
        let mut j = i;
        if j < len && matches!(bytes[j], b'"' | b'\'') {
            j += 1;
        }
        while j < len && matches!(bytes[j], b' ' | b'\t') {
            j += 1;
        }
        if j >= len || !matches!(bytes[j], b'=' | b':') {
            continue;
        }
        j += 1;
        if j < len && bytes[j] == b'=' {
            j += 1;
        }
        while j < len && matches!(bytes[j], b' ' | b'\t') {
            j += 1;
        }
        let (value_start, value_end) = if j < len && matches!(bytes[j], b'"' | b'\'') {
            let quote = bytes[j] as char;
            let start = j + 1;
            let end = text[start..]
                .find([quote, '\n'])
                .map_or(len, |at| start + at);
            (start, end)
        } else if key == "authorization" {
            // To the end of the line, or of the quoted string the header
            // is in (`curl -H "Authorization: …"`).
            let end = text[j..]
                .find(['\n', '\r', '"', '\'', '`'])
                .map_or(len, |at| j + at);
            (j, text[j..end].trim_end().len() + j)
        } else {
            let end = text[j..]
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            ',' | ';' | '&' | '"' | '\'' | '<' | '>' | ')' | ']' | '}'
                        )
                })
                .map_or(len, |at| j + at);
            (j, end)
        };
        let value = &text[value_start..value_end];
        if value.is_empty() || value.starts_with("[REDACTED") || value.starts_with(['{', '[']) {
            i = i.max(value_end);
            continue;
        }
        out.push_str(&text[copied..value_start]);
        out.push_str(MASK);
        copied = value_end;
        *count += 1;
        i = value_end;
    }
    out.push_str(&text[copied..]);
    out
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Well-known token formats, as whole words.
fn tokens(text: &str, count: &mut usize) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !is_word(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word(bytes[i]) {
            i += 1;
        }
        if is_token(&text[start..i]) {
            out.push_str(&text[copied..start]);
            out.push_str(MASK);
            copied = i;
            *count += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

fn is_token(word: &str) -> bool {
    // A random part: long enough, with letters and digits in it, so words
    // like `sk-learn-related-packages` are not taken for keys.
    let random = |prefix: &str, min: usize| {
        word.starts_with(prefix) && {
            let rest = &word[prefix.len()..];
            rest.len() >= min
                && rest.bytes().any(|b| b.is_ascii_digit())
                && rest.bytes().any(|b| b.is_ascii_alphabetic())
        }
    };
    random("sk-", 20)
        || ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
            .iter()
            .any(|prefix| random(prefix, 30))
        || random("github_pat_", 30)
        || random("glpat-", 20)
        || ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"]
            .iter()
            .any(|prefix| random(prefix, 10))
        || ((word.starts_with("AKIA") || word.starts_with("ASIA"))
            && word.len() == 20
            && word[4..]
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
        || (word.starts_with("AIza") && word.len() == 39)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(text: &str) -> (String, usize) {
        let redacted = redact(text);
        // Masking is stable: a second pass finds nothing new.
        assert_eq!(redact(&redacted.text).text, redacted.text);
        assert_eq!(redact(&redacted.text).count, 0);
        (redacted.text, redacted.count)
    }

    #[test]
    fn private_key_blocks_are_masked_whole() {
        let text = "key:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAA\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nafter";
        assert_eq!(
            masked(text),
            ("key:\n[REDACTED PRIVATE KEY]\nafter".into(), 1)
        );
        let cut = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow";
        assert_eq!(masked(cut), ("[REDACTED PRIVATE KEY]".into(), 1));
        let certificate = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(masked(certificate), (certificate.into(), 0));
    }

    #[test]
    fn assignments_lose_their_values() {
        assert_eq!(
            masked("password=hunter2 user=bob"),
            ("password=[REDACTED] user=bob".into(), 1)
        );
        assert_eq!(masked("passwd: abc123"), ("passwd: [REDACTED]".into(), 1));
        assert_eq!(
            masked("export AWS_SECRET=xyz; DB_PASSWORD = 's3 cret'"),
            (
                "export AWS_SECRET=[REDACTED]; DB_PASSWORD = '[REDACTED]'".into(),
                2
            )
        );
        assert_eq!(
            masked(r#"{"api_key": "abc", "name": "x"}"#),
            (r#"{"api_key": "[REDACTED]", "name": "x"}"#.into(), 1)
        );
        assert_eq!(
            masked("https://x.test/cb?access_token=abc&state=1"),
            (
                "https://x.test/cb?access_token=[REDACTED]&state=1".into(),
                1
            )
        );
        // Not assignments, or not secret names.
        for text in [
            "The secretary said: hello",
            "max_tokens=64000",
            "password reset link",
            "PWD=/home/me",
            "secret = {",
        ] {
            assert_eq!(masked(text), (text.into(), 0), "{text}");
        }
    }

    #[test]
    fn authorization_headers_are_masked_to_the_end_of_the_line() {
        assert_eq!(
            masked("GET / HTTP/1.1\r\nAuthorization: Bearer abc.def\r\nHost: x"),
            (
                "GET / HTTP/1.1\r\nAuthorization: [REDACTED]\r\nHost: x".into(),
                1
            )
        );
        assert_eq!(
            masked(r#"curl -H "Authorization: Basic dXNlcjpwYXNz" https://x.test"#),
            (
                r#"curl -H "Authorization: [REDACTED]" https://x.test"#.into(),
                1
            )
        );
    }

    #[test]
    fn credentials_in_urls_are_masked() {
        assert_eq!(
            masked("postgres://admin:pa55w0rd@db.local:5432/app and https://example.com/a:b@c"),
            (
                "postgres://admin:[REDACTED]@db.local:5432/app and https://example.com/a:b@c"
                    .into(),
                1
            )
        );
        assert_eq!(
            masked("git clone https://user@github.com/x"),
            ("git clone https://user@github.com/x".into(), 0)
        );
    }

    #[test]
    fn token_shapes_are_masked() {
        let cases = [
            "sk-ant-api03-AbCdEf0123456789abcdefXYZ",
            "sk-proj-1234567890abcdefghijKLMN",
            "ghp_0123456789abcdefghijABCDEFGHIJ012345",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
            "xoxb-123456789012-abcdefABCDEF",
            "glpat-abcdefghij0123456789",
        ];
        for token in cases {
            let (text, count) = masked(&format!("key {token}, done"));
            assert_eq!(
                (text.as_str(), count),
                ("key [REDACTED], done", 1),
                "{token}"
            );
        }
        for word in [
            "sk-learn-related-packages",
            "AKIA",
            "risk-assessment-2024-draft-v2",
        ] {
            assert_eq!(masked(word), (word.into(), 0), "{word}");
        }
    }

    #[test]
    fn other_text_is_untouched() {
        let text = "日本語の文章です。パスワードの話をしますが、値はありません。\n한국어 텍스트\n中文：密钥管理\n\
                    Ordinary English with key: value pairs and a URL https://example.com/path?q=1";
        assert_eq!(masked(text), (text.into(), 0));
        // Secrets next to CJK text are still found, and the CJK text kept.
        assert_eq!(
            masked("鍵は sk-abcdefghij0123456789XYZ です。password=秘密の値 終わり"),
            ("鍵は [REDACTED] です。password=[REDACTED] 終わり".into(), 2)
        );
    }
}
