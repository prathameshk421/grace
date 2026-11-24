//! Import from Claude Code history.
use crate::db::Db;
use crate::model;
use crate::platform;
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

const MAX_CLAUDE_JSON_BYTES: usize = 16 << 20;

/// Return the folder paths (keys of the top level "projects" object).
pub fn claude_project_paths(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    match v.get("projects").and_then(|p| p.as_object()) {
        Some(map) => map
            .keys()
            .map(|k| platform::clean_path_str(k))
            .filter(|k| !k.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// Import folders from `~/.claude.json`. `None` means the file does not exist.
pub fn import_claude(db: &Db) -> Result<Option<usize>> {
    let Some(home) = platform::home_dir() else {
        return Ok(None);
    };
    let claude_json = home.join(".claude.json");
    let Ok(text) = platform::read_to_string_capped(&claude_json, MAX_CLAUDE_JSON_BYTES) else {
        return Ok(None);
    };
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for raw in claude_project_paths(&text) {
        if platform::has_line_break_or_nul(&raw) {
            continue;
        }
        let p = Path::new(&raw);
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let path = platform::normalize(p);
        if platform::ignored(&path) || !seen.insert(path.clone()) {
            continue;
        }
        let ts = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(model::now);
        items.push((path, ts));
    }
    let count = db.import_dirs_batch(&items, Some("claude"))?;
    Ok(Some(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_projects() {
        let j = r#"{"projects": {"/a/b/": {}, "/c": {"x": 1}}, "other": 1}"#;
        let mut p = claude_project_paths(j);
        p.sort();
        assert_eq!(p, vec!["/a/b", "/c"]);
    }

    #[test]
    fn bad_json_is_empty() {
        assert!(claude_project_paths("not json").is_empty());
        assert!(claude_project_paths("{}").is_empty());
    }
}
