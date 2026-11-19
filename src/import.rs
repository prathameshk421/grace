//! Import from Claude Code history.
use crate::db::Db;
use crate::model;
use anyhow::Result;
use std::path::Path;

/// Return the folder paths (keys of the top level "projects" object).
pub fn claude_project_paths(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    match v.get("projects").and_then(|p| p.as_object()) {
        Some(map) => map
            .keys()
            .map(|k| crate::platform::clean_path_str(k))
            .filter(|k| !k.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

pub fn import_claude(db: &Db) -> Result<usize> {
    let Some(home) = crate::platform::home_dir() else {
        return Ok(0);
    };
    let home = crate::platform::clean_path_str(&home.to_string_lossy());
    let Ok(text) = std::fs::read_to_string(Path::new(&home).join(".claude.json")) else {
        return Ok(0);
    };
    let mut count = 0;
    for path in claude_project_paths(&text) {
        if crate::platform::is_root(&path) || crate::platform::same_path(&path, &home) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let ts = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(model::now);
        if db.import_dir(&path, ts, Some("claude"))? {
            count += 1;
        }
    }
    Ok(count)
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
