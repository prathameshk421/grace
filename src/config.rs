//! Optional user configuration (`~/.config/grace/config.toml`).
//!
//! Strictly zero-config by default: if missing or invalid, defaults apply silently.
use std::path::{Path, PathBuf};
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
}

/// Resolve the optional config path.
pub fn config_path() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("GRACE_CONFIG") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    if cfg!(windows) {
        if let Ok(v) = std::env::var("APPDATA") {
            if !v.is_empty() {
                return Some(PathBuf::from(v).join("grace").join("config.toml"));
            }
        }
    } else if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        if !v.is_empty() && Path::new(&v).is_absolute() {
            return Some(PathBuf::from(v).join("grace").join("config.toml"));
        }
    }
    crate::platform::home_dir().map(|h| h.join(".config").join("grace").join("config.toml"))
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
    Config {
        theme: str_opt("theme"),
        editor: str_opt("editor"),
        ignore,
        ascii,
        hub_layout,
    }
}

/// Match simple glob pattern (`*` matches any sequence, `?` matches one char; leading `~/` expands to home).
pub fn glob_match(pattern: &str, path: &str, home: Option<&str>) -> bool {
    let pat = if let Some(h) = home {
        let h = crate::platform::trim_separators(h);
        if pattern == "~" {
            h
        } else if let Some(rest) = pattern
            .strip_prefix("~/")
            .or_else(|| pattern.strip_prefix("~\\"))
        {
            let sep = if cfg!(windows) { '\\' } else { '/' };
            format!("{h}{sep}{rest}")
        } else {
            pattern.to_string()
        }
    } else {
        pattern.to_string()
    };
    let (p, s) = if cfg!(windows) {
        (
            pat.replace('/', "\\").to_ascii_lowercase(),
            path.replace('/', "\\").to_ascii_lowercase(),
        )
    } else {
        (pat, path.to_string())
    };
    wildcard_match(p.as_bytes(), s.as_bytes())
}

fn wildcard_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut match_si) = (None, 0);
    while si < s.len() {
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
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
    while pi < pat.len() && pat[pi] == b'*' {
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
ignore = ["/tmp/*", "~/Downloads/*"]
[hub]
layout = "code+diff"
"#,
        );
        assert_eq!(c.theme.as_deref(), Some("light"));
        assert_eq!(c.editor.as_deref(), Some("zed"));
        assert_eq!(c.ascii, Some(false));
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
    }
}
