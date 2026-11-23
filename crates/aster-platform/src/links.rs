//! Opening links from documents in the system's browser or mail program.
//!
//! Only web pages and new e-mail are opened. A link in a document is written by
//! whoever wrote the document, so everything else is refused: local files and
//! programs (`file:`, a relative path), scripts (`javascript:`), and the
//! schemes other applications register for themselves, which can start those
//! applications with arguments chosen by the link.

use std::io;
use std::process::Command;

/// Why a link was not opened.
#[derive(Debug)]
pub enum OpenError {
    /// Not `http`, `https` or `mailto`.
    Refused,
    /// The system could not start the program that opens links.
    Failed(io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Refused => f.write_str("only web and e-mail links are opened"),
            OpenError::Failed(err) => write!(f, "cannot open the link: {err}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Whether a link may be opened: an absolute `http` or `https` address, or
/// `mailto`.
pub fn is_openable(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    // Control characters, white space at the ends and option-like starts
    // never reach the system.
    if url.chars().any(char::is_control) || url.trim() != url || url.starts_with('-') {
        return false;
    }
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.len() > 2 && rest.starts_with("//") && !rest[2..].starts_with('/'),
        "mailto" => !rest.is_empty(),
        _ => false,
    }
}

/// Open a link with the system's default program, without asking. Returns
/// once the program has been started.
pub fn open(url: &str) -> Result<(), OpenError> {
    if !is_openable(url) {
        return Err(OpenError::Refused);
    }
    let mut command = opener(url);
    command.spawn().map(drop).map_err(OpenError::Failed)
}

/// The command that hands a URL to the default program. The URL is passed as
/// one argument, never through a shell.
fn opener(url: &str) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("/usr/bin/open");
        command.arg(url);
        command
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("rundll32.exe");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_and_mail_links_are_opened() {
        for url in [
            "https://example.com",
            "http://example.com/a?b=c#d",
            "HTTPS://EXAMPLE.COM/",
            "https://例子.测试/路径",
            "mailto:someone@example.com",
            "mailto:someone@example.com?subject=hi",
        ] {
            assert!(is_openable(url), "{url}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for url in [
            "",
            "example.com",
            "notes.md",
            "/etc/hosts",
            "./a.html",
            "file:///etc/hosts",
            "javascript:alert(1)",
            "data:text/html,<b>x</b>",
            "ftp://example.com",
            "ssh://host",
            "vscode://file/x",
            "smb://server/share",
            "http:",
            "https://",
            "https:///path",
            "http:example.com",
            "mailto:",
            " https://example.com",
            "https://example.com\n",
            "https://exa\u{7}mple.com",
            "-https://example.com",
        ] {
            assert!(!is_openable(url), "{url:?}");
            assert!(matches!(open(url), Err(OpenError::Refused)), "{url:?}");
        }
    }
}
