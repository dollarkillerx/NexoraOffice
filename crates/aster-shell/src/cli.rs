//! Command-line options of the shell binary.
//!
//! They exist so a given state of the window can be reached without clicking:
//! for screenshots, and for checking a theme or a language.

use crate::i18n::Language;
use crate::tabs::DocKind;
use crate::theme::ThemeMode;

pub const USAGE: &str = "usage: aster-shell [--theme light|dark] [--assistant] [--open writer|sheets|slides|markdown|pdf]...";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub theme: ThemeMode,
    /// `None` leaves the choice to the shell.
    pub language: Option<Language>,
    /// Start with the assistant panel open.
    pub assistant: bool,
    /// Documents to open at start-up, in order.
    pub open: Vec<DocKind>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Light,
            language: None,
            assistant: false,
            open: Vec::new(),
        }
    }
}

fn parse_kind(name: &str) -> Option<DocKind> {
    DocKind::ALL
        .into_iter()
        .find(|kind| kind.name().eq_ignore_ascii_case(name))
}

/// Parse the arguments after the program name.
pub fn parse<I, S>(args: I) -> Result<Options, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        let mut value = || {
            args.next()
                .map(|value| value.as_ref().to_owned())
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--theme" => {
                options.theme = match value()?.as_str() {
                    "light" => ThemeMode::Light,
                    "dark" => ThemeMode::Dark,
                    other => return Err(format!("unknown theme {other:?}")),
                };
            }
            "--assistant" => options.assistant = true,
            "--lang" => {
                let tag = value()?;
                let language =
                    Language::from_tag(&tag).ok_or_else(|| format!("unknown language {tag:?}"))?;
                options.language = Some(language);
            }
            "--open" => {
                let name = value()?;
                let kind =
                    parse_kind(&name).ok_or_else(|| format!("unknown document kind {name:?}"))?;
                options.open.push(kind);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_means_light_theme_and_the_start_page() {
        assert_eq!(parse(Vec::<String>::new()), Ok(Options::default()));
    }

    #[test]
    fn theme_and_documents_are_read_in_order() {
        let options = parse([
            "--open",
            "writer",
            "--theme",
            "dark",
            "--assistant",
            "--lang",
            "ja",
            "--open",
            "PDF",
        ]);
        assert_eq!(
            options,
            Ok(Options {
                theme: ThemeMode::Dark,
                language: Some(Language::Ja),
                assistant: true,
                open: vec![DocKind::Writer, DocKind::Pdf],
            })
        );
    }

    #[test]
    fn every_kind_can_be_opened_by_name() {
        for kind in DocKind::ALL {
            let options = parse(["--open", &kind.name().to_lowercase()]).expect("valid");
            assert_eq!(options.open, [kind]);
        }
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(parse(["--theme"]).is_err());
        assert!(parse(["--theme", "sepia"]).is_err());
        assert!(parse(["--open", "visio"]).is_err());
        assert!(parse(["--lang", "tlh"]).is_err());
        assert!(parse(["--verbose"]).is_err());
    }
}
