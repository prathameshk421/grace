//! AI tool detection, built-in and custom.

use anyhow::{bail, Result};

/// What a built-in tool is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Ai,
    Editor,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ai => "ai",
            Kind::Editor => "editor",
        }
    }
}

use Kind::{Ai, Editor};

/// (command name the user types, canonical tag, launch command, kind)
pub const TOOLS: &[(&str, &str, &str, Kind)] = &[
    ("claude", "claude", "claude", Ai),
    ("agy", "antigravity", "agy", Ai),
    ("antigravity", "antigravity", "agy", Ai),
    ("codex", "codex", "codex", Ai),
    ("gemini", "gemini", "gemini", Ai),
    ("cursor", "cursor", "cursor-agent", Ai),
    ("cursor-agent", "cursor", "cursor-agent", Ai),
    ("opencode", "opencode", "opencode", Ai),
    ("aider", "aider", "aider", Ai),
    ("copilot", "copilot", "copilot", Ai),
    ("amp", "amp", "amp", Ai),
    ("goose", "goose", "goose", Ai),
    ("crush", "crush", "crush", Ai),
    ("qwen", "qwen", "qwen", Ai),
    ("t3", "t3code", "t3", Ai),
    ("code", "vscode", "code .", Editor),
    ("code-insiders", "vscode", "code-insiders .", Editor),
    ("codium", "vscodium", "codium .", Editor),
    ("windsurf", "windsurf", "windsurf .", Editor),
    ("zed", "zed", "zed .", Editor),
    ("nvim", "neovim", "nvim", Editor),
    ("vim", "vim", "vim", Editor),
    ("subl", "sublime", "subl .", Editor),
    ("idea", "jetbrains", "idea .", Editor),
    ("pycharm", "jetbrains", "pycharm .", Editor),
    ("webstorm", "jetbrains", "webstorm .", Editor),
];

/// A command or tag name: `[A-Za-z0-9._-]`, starts with a letter or digit, max 64 chars.
/// The command name goes into a zsh `case` pattern, so this check is a security check.
pub fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A launch command: not empty, no control characters (this includes newlines).
pub fn valid_launch(s: &str) -> bool {
    !s.trim().is_empty() && !s.chars().any(|c| c.is_control())
}

/// Check the three fields of a custom tool. Error text says which field is wrong.
pub fn validate(command: &str, tag: &str, launch: &str) -> Result<()> {
    let rule = "use only letters, digits, '.', '_' and '-', start with a letter or digit, max 64 characters";
    if !valid_name(command) {
        bail!("invalid command name {command:?}: {rule}");
    }
    if !valid_name(tag) {
        bail!("invalid tag {tag:?}: {rule}");
    }
    if !valid_launch(launch) {
        bail!("invalid launch command: it must not be empty or contain newlines or control characters");
    }
    Ok(())
}

/// One known tool.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub command: String,
    pub tag: String,
    pub launch: String,
    /// None for custom tools.
    pub kind: Option<Kind>,
}

impl Tool {
    pub fn kind_label(&self) -> &'static str {
        self.kind.map_or("custom", Kind::label)
    }
}

/// Built-in tools plus custom tools from the database.
pub struct Registry {
    tools: Vec<Tool>,
}

impl Registry {
    /// Build from custom rows `(command, tag, launch)`. Invalid rows are skipped.
    /// A custom tool replaces a built-in tool with the same command.
    pub fn new(custom: &[(String, String, String)]) -> Registry {
        let custom: Vec<Tool> = custom
            .iter()
            .filter(|(c, t, l)| validate(c, t, l).is_ok())
            .map(|(c, t, l)| Tool {
                command: c.clone(),
                tag: t.to_lowercase(),
                launch: l.clone(),
                kind: None,
            })
            .collect();
        let mut tools: Vec<Tool> = TOOLS
            .iter()
            .filter(|(cmd, ..)| !custom.iter().any(|c| c.command.eq_ignore_ascii_case(cmd)))
            .map(|(cmd, tag, launch, kind)| Tool {
                command: cmd.to_string(),
                tag: tag.to_string(),
                launch: launch.to_string(),
                kind: Some(*kind),
            })
            .collect();
        tools.extend(custom);
        Registry { tools }
    }

    pub fn from_db(db: &crate::db::Db) -> Result<Registry> {
        Ok(Registry::new(&db.custom_tools()?))
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Map a command name or tag to the canonical tag. Unknown names pass through in lowercase.
    pub fn normalize_tag(&self, name: &str) -> String {
        let n = name.trim().to_lowercase();
        self.tools
            .iter()
            .find(|t| t.command.to_lowercase() == n)
            .or_else(|| self.tools.iter().find(|t| t.tag == n))
            .map(|t| t.tag.clone())
            .unwrap_or(n)
    }

    /// The command that starts the tool with this canonical tag. Custom tools come first.
    pub fn launch_command(&self, tag: &str) -> Option<String> {
        self.tools
            .iter()
            .rev()
            .find(|t| t.tag == tag && t.kind.is_none())
            .or_else(|| self.tools.iter().find(|t| t.tag == tag))
            .map(|t| t.launch.clone())
    }

    /// All command names, for the shell script.
    pub fn command_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.command.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(c: &str, t: &str, l: &str) -> (String, String, String) {
        (c.into(), t.into(), l.into())
    }

    #[test]
    fn normalize() {
        let r = Registry::new(&[]);
        assert_eq!(r.normalize_tag("AGY"), "antigravity");
        assert_eq!(r.normalize_tag(" cursor-agent "), "cursor");
        assert_eq!(r.normalize_tag("Foo"), "foo");
        assert_eq!(r.normalize_tag("t3"), "t3code");
    }

    #[test]
    fn editors() {
        let r = Registry::new(&[]);
        assert_eq!(r.normalize_tag("code"), "vscode");
        assert_eq!(r.normalize_tag("pycharm"), "jetbrains");
        assert_eq!(r.launch_command("vscode").as_deref(), Some("code ."));
        assert_eq!(r.launch_command("neovim").as_deref(), Some("nvim"));
        assert_eq!(r.launch_command("jetbrains").as_deref(), Some("idea ."));
        let t = r.tools().iter().find(|t| t.command == "zed").unwrap();
        assert_eq!(t.kind_label(), "editor");
    }

    #[test]
    fn launch() {
        let r = Registry::new(&[]);
        assert_eq!(r.launch_command("antigravity").as_deref(), Some("agy"));
        assert_eq!(r.launch_command("cursor").as_deref(), Some("cursor-agent"));
        assert_eq!(r.launch_command("nope"), None);
        assert!(r.command_names().contains(&"agy".to_string()));
    }

    #[test]
    fn validation() {
        for ok in ["kiro", "cursor-agent", "q", "a.b_c", "9lives"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["a;rm -rf", "$(x)", "foo bar", "", "-x", ".x", "a|b", "a\nb", &"a".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_launch("q chat"));
        assert!(!valid_launch("q\nchat"));
        assert!(!valid_launch(""));
        assert!(validate("kiro", "kiro", "kiro").is_ok());
        assert!(validate("a;b", "kiro", "kiro").is_err());
        assert!(validate("kiro", "$x", "kiro").is_err());
        assert!(validate("kiro", "kiro", "a\tb").is_err());
    }

    #[test]
    fn custom_tools() {
        let r = Registry::new(&[
            custom("q", "amazon-q", "q chat"),
            custom("claude", "mine", "my-claude"),
            custom("bad;cmd", "x", "x"),
        ]);
        assert_eq!(r.normalize_tag("q"), "amazon-q");
        assert_eq!(r.launch_command("amazon-q").as_deref(), Some("q chat"));
        assert_eq!(r.normalize_tag("claude"), "mine");
        assert_eq!(r.launch_command("mine").as_deref(), Some("my-claude"));
        assert_eq!(r.tools().iter().filter(|t| t.command == "claude").count(), 1);
        assert!(!r.command_names().contains(&"bad;cmd".to_string()));
        assert!(r.command_names().contains(&"codex".to_string()));
    }
}
