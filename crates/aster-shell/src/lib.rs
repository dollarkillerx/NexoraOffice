//! The Aster Office application shell.
//!
//! This library half holds everything about the shell that is not drawing:
//! which tabs are open, what the toolbar offers, the design tokens, the
//! interface text in each language, the counts in the status bar. None of it refers to the GUI toolkit, so it can be tested
//! without a window and survives a change of toolkit. The binary in `main.rs`
//! binds it to the Slint UI under `ui/`.

pub mod assistant;
pub mod cli;
pub mod content;
pub mod files;
pub mod formula_hints;
pub mod i18n;
pub mod icons;
pub mod office;
pub mod pdf;
pub mod recovery;
pub mod sheet_tools;
pub mod sidebar;
pub mod stats;
pub mod tabs;
pub mod theme;
pub mod toolbar;

#[cfg(test)]
mod tests {
    /// The modules above must stay free of the GUI toolkit. The crate as a whole
    /// links Slint for the binary, so the compiler cannot enforce this.
    #[test]
    fn shell_logic_does_not_mention_the_toolkit() {
        let sources = [
            ("cli.rs", include_str!("cli.rs")),
            ("files.rs", include_str!("files.rs")),
            ("i18n.rs", include_str!("i18n.rs")),
            ("icons.rs", include_str!("icons.rs")),
            ("recovery.rs", include_str!("recovery.rs")),
            ("sidebar.rs", include_str!("sidebar.rs")),
            ("stats.rs", include_str!("stats.rs")),
            ("tabs.rs", include_str!("tabs.rs")),
            ("theme.rs", include_str!("theme.rs")),
            ("toolbar.rs", include_str!("toolbar.rs")),
        ];
        for (name, source) in sources {
            for (number, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or_default();
                for toolkit in ["slint", "winit", "aster_surface"] {
                    assert!(
                        !code.contains(toolkit),
                        "{name}:{} refers to {toolkit}",
                        number + 1
                    );
                }
            }
        }
    }
}
