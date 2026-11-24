//! What the settings page shows for connecting a client: the command that
//! adds Aster Office to Claude Code, and the configuration block that
//! Claude Desktop and clients like it read. Both start the stdio bridge,
//! `aster-mcp`, which ships next to the application's own executable.

use std::path::{Path, PathBuf};

use serde_json::json;

/// The name clients list the server under.
pub const SERVER_NAME: &str = "aster-office";

/// The bridge's file name on this system.
pub const BRIDGE_NAME: &str = if cfg!(windows) {
    "aster-mcp.exe"
} else {
    "aster-mcp"
};

/// Ready-to-copy client setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSetup {
    /// `claude mcp add aster-office -- <bridge>`, quoted for the shell.
    pub claude_code: String,
    /// `{"mcpServers": {"aster-office": {"command": "<bridge>"}}}`, pretty
    /// printed.
    pub json_config: String,
}

/// The setup snippets for a bridge at `bridge`.
pub fn client_setup(bridge: &Path) -> ClientSetup {
    let path = bridge.display().to_string();
    let config = json!({ "mcpServers": { SERVER_NAME: { "command": path } } });
    ClientSetup {
        claude_code: format!("claude mcp add {SERVER_NAME} -- {}", shell_quote(&path)),
        json_config: serde_json::to_string_pretty(&config).unwrap_or_default(),
    }
}

/// Where the bridge is: next to the running executable. `None` if the
/// executable's own path is unknown.
pub fn bridge_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(BRIDGE_NAME))
}

/// Quote a path for the user's shell when it needs it. On macOS the bridge
/// usually sits inside `Aster Office.app`, so the space alone makes this
/// necessary.
fn shell_quote(text: &str) -> String {
    let plain = text.chars().all(|c| {
        c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c) || (cfg!(windows) && c == '\\')
    });
    if plain && !text.is_empty() {
        return text.to_owned();
    }
    if cfg!(windows) {
        // cmd and PowerShell both take double quotes; Windows paths cannot
        // contain them.
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn the_snippets_quote_the_path() {
        let setup = client_setup(Path::new(
            "/Applications/Aster Office.app/Contents/MacOS/aster-mcp",
        ));
        assert_eq!(
            setup.claude_code,
            "claude mcp add aster-office -- '/Applications/Aster Office.app/Contents/MacOS/aster-mcp'"
        );
        let config: serde_json::Value = serde_json::from_str(&setup.json_config).unwrap();
        assert_eq!(
            config["mcpServers"]["aster-office"]["command"],
            "/Applications/Aster Office.app/Contents/MacOS/aster-mcp"
        );
        let plain = client_setup(Path::new("/usr/bin/aster-mcp"));
        assert_eq!(
            plain.claude_code,
            "claude mcp add aster-office -- /usr/bin/aster-mcp"
        );
        assert_eq!(shell_quote("/it's"), "'/it'\\''s'");
    }

    #[test]
    fn the_bridge_is_next_to_the_executable() {
        let path = bridge_path().unwrap();
        assert!(path.ends_with(BRIDGE_NAME));
    }
}
