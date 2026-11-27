//! Optional user configuration (`~/.config/grace/config.toml`).
//!
//! Strictly zero-config by default: if missing or invalid, defaults apply silently.
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// "auto" | "dark" | "light"
    pub theme: Option<String>,
    /// Fallback editor CLI for ctrl-e, diff, and tree (e.g. "zed", "code", "nvim").
    pub editor: Option<String>,
    /// Folder glob patterns to ignore (e.g. "/tmp/*", "~/Downloads/*").
    pub ignore: Vec<String>,
    /// Force ASCII-only glyphs instead of Unicode emblems.
    pub ascii: Option<bool>,
    /// Default hub layout preset ("full", "code+diff", "sidebar+main", "zen").
    pub hub_layout: Option<String>,
    /// Default Enter action in picker: "travel" (default) | "launch".
    pub default_action: Option<String>,
    /// Whether CLI agents launched from the picker open in `grace hub` (default true).
    pub cli_in_hub: Option<bool>,
}

/// Resolve the optional config path.
pub fn config_path() -> Option<PathBuf> {
    if let Some(v) = crate::platform::env_non_empty("GRACE_CONFIG") {
        return Some(PathBuf::from(v));
    }
    if cfg!(windows) {
        if let Some(v) = crate::platform::env_non_empty("APPDATA") {
            return Some(PathBuf::from(v).join("grace").join("config.toml"));
        }
    }
    crate::platform::xdg_config_dir(None, "grace").map(|d| d.join("config.toml"))
}

/// Load config from disk; missing or malformed files fall back cleanly to defaults.
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let Ok(text) = crate::platform::read_to_string_capped(&path, 256 * 1024) else {
        return Config::default();
    };
    parse(&text)
}

/// Parse TOML config text safely.
pub fn parse(text: &str) -> Config {
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return Config::default();
    };
    let str_opt = |k: &str| -> Option<String> {
        doc.get(k)
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let mut ignore = Vec::new();
    if let Some(arr) = doc.get("ignore").and_then(Item::as_array) {
        for v in arr.iter() {
            if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                ignore.push(s.to_string());
            }
        }
    }
    let ascii = doc.get("ascii").and_then(Item::as_bool);
    let hub_layout = str_opt("hub_layout").or_else(|| {
        doc.get("hub")
            .and_then(|h| h.get("layout"))
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    });
    let default_action = str_opt("default_action");
    let cli_in_hub = doc.get("cli_in_hub").and_then(Item::as_bool);
    Config {
        theme: str_opt("theme"),
        editor: str_opt("editor"),
        ignore,
        ascii,
        hub_layout,
        default_action,
        cli_in_hub,
    }
}

/// Expand a leading `~` or `~/` (or `~\`) in `pattern` using `home`.
pub(crate) fn expand_tilde(pattern: &str, home: Option<&str>) -> String {
    let Some(h) = home else {
        return pattern.to_string();
    };
    let h = crate::platform::trim_separators(h);
    if pattern == "~" {
        return h;
    }
    if let Some(rest) = pattern
        .strip_prefix("~/")
        .or_else(|| pattern.strip_prefix("~\\"))
    {
        let sep = if cfg!(windows) { '\\' } else { '/' };
        let rest = rest.trim_start_matches(['/', '\\']);
        if h.ends_with(['/', '\\']) {
            format!("{h}{rest}")
        } else {
            format!("{h}{sep}{rest}")
        }
    } else {
        pattern.to_string()
    }
}

/// Match simple glob pattern (`*` matches any sequence, `?` matches one Unicode char;
/// leading `~/` expands to home; trailing `/` or `/**` matches directory and descendants).
pub fn glob_match(pattern: &str, path: &str, home: Option<&str>) -> bool {
    let pat_raw = expand_tilde(pattern, home);
    let (mut pat, s) = if cfg!(windows) {
        (
            pat_raw.replace('/', "\\").to_ascii_lowercase(),
            path.replace('/', "\\").to_ascii_lowercase(),
        )
    } else {
        (pat_raw, path.to_string())
    };
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let dir_prefix = if pat.ends_with("/**") || pat.ends_with("\\**") {
        let trimmed = pat[..pat.len() - 3].trim_end_matches(['/', '\\']);
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    } else if !crate::platform::is_root(&pat) && (pat.ends_with('/') || pat.ends_with('\\')) {
        pat = pat.trim_end_matches(['/', '\\']).to_string();
        (!pat.is_empty()).then(|| pat.clone())
    } else {
        None
    };
    if let Some(base) = dir_prefix {
        if s == base {
            return true;
        }
        let sub_pat = format!("{base}{sep}*");
        let p_chars: Vec<char> = sub_pat.chars().collect();
        let s_chars: Vec<char> = s.chars().collect();
        return wildcard_match(&p_chars, &s_chars);
    }
    let p_chars: Vec<char> = pat.chars().collect();
    let s_chars: Vec<char> = s.chars().collect();
    wildcard_match(&p_chars, &s_chars)
}

fn wildcard_match(pat: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut match_si) = (None, 0);
    while si < s.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            pi += 1;
            match_si = si;
        } else if let Some(st) = star {
            pi = st + 1;
            match_si += 1;
            si = match_si;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_and_invalid_toml() {
        let c = parse(
            r#"
theme = "light"
editor = "zed"
ascii = false
default_action = "launch"
cli_in_hub = false
ignore = ["/tmp/*", "~/Downloads/*"]
[hub]
layout = "code+diff"
"#,
        );
        assert_eq!(c.theme.as_deref(), Some("light"));
        assert_eq!(c.editor.as_deref(), Some("zed"));
        assert_eq!(c.ascii, Some(false));
        assert_eq!(c.default_action.as_deref(), Some("launch"));
        assert_eq!(c.cli_in_hub, Some(false));
        assert_eq!(c.ignore, vec!["/tmp/*", "~/Downloads/*"]);
        assert_eq!(c.hub_layout.as_deref(), Some("code+diff"));
        assert_eq!(parse("not = [valid"), Config::default());
    }

    #[test]
    fn glob_matches_patterns() {
        assert!(glob_match("/tmp/*", "/tmp/foo/bar", Some("/Users/me")));
        assert!(glob_match(
            "~/Downloads/*",
            "/Users/me/Downloads/x",
            Some("/Users/me")
        ));
        assert!(!glob_match(
            "~/Downloads/*",
            "/Users/me/code/x",
            Some("/Users/me")
        ));
        assert!(glob_match("*/node_modules", "/a/b/node_modules", None));
        assert!(glob_match("/tmp/caf?", "/tmp/caf\u{00e9}", None));
        assert!(glob_match("/tmp/", "/tmp", None));
        assert!(glob_match("/tmp/", "/tmp/sub/x", None));
        assert!(glob_match("/tmp/**", "/tmp", None));
        assert!(glob_match("/tmp/**", "/tmp/sub/x", None));
        assert_eq!(
            expand_tilde("~/Downloads/*", Some("C:\\")),
            "C:\\Downloads/*"
        );
    }
}
