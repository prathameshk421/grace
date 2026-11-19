//! AI tool detection.

/// (command name the user types, canonical tag)
pub const TOOLS: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("agy", "antigravity"),
    ("antigravity", "antigravity"),
    ("codex", "codex"),
    ("gemini", "gemini"),
    ("cursor", "cursor"),
    ("cursor-agent", "cursor"),
    ("opencode", "opencode"),
    ("aider", "aider"),
    ("copilot", "copilot"),
    ("amp", "amp"),
    ("goose", "goose"),
    ("crush", "crush"),
    ("qwen", "qwen"),
];

/// Map a command name or tag to the canonical tag. Unknown names pass through in lowercase.
pub fn normalize_tag(name: &str) -> String {
    let n = name.trim().to_lowercase();
    TOOLS
        .iter()
        .find(|(cmd, tag)| *cmd == n || *tag == n)
        .map(|(_, tag)| tag.to_string())
        .unwrap_or(n)
}

/// The command that starts the tool with this canonical tag.
pub fn launch_command(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "claude" => "claude",
        "antigravity" => "agy",
        "codex" => "codex",
        "gemini" => "gemini",
        "cursor" => "cursor-agent",
        "opencode" => "opencode",
        "aider" => "aider",
        "copilot" => "copilot",
        "amp" => "amp",
        "goose" => "goose",
        "crush" => "crush",
        "qwen" => "qwen",
        _ => return None,
    })
}

/// All command names, for the shell script.
pub fn command_names() -> Vec<&'static str> {
    TOOLS.iter().map(|(cmd, _)| *cmd).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        assert_eq!(normalize_tag("AGY"), "antigravity");
        assert_eq!(normalize_tag(" cursor-agent "), "cursor");
        assert_eq!(normalize_tag("Foo"), "foo");
    }

    #[test]
    fn launch() {
        assert_eq!(launch_command("antigravity"), Some("agy"));
        assert_eq!(launch_command("cursor"), Some("cursor-agent"));
        assert_eq!(launch_command("nope"), None);
        assert!(command_names().contains(&"agy"));
    }
}
