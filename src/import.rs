//! Import from Claude Code history.
use crate::db::Db;
use crate::model;
use crate::platform::{self, IgnoreMatcher};
use anyhow::Result;
use std::collections::HashSet;

const MAX_CLAUDE_JSON_BYTES: usize = 16 << 20;

struct JsonScan<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

impl<'a> JsonScan<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s,
            b: s.as_bytes(),
            i: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn expect(&mut self, ch: u8) -> bool {
        self.skip_ws();
        if self.b.get(self.i).copied() == Some(ch) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn next_string_raw(&mut self) -> Option<&'a str> {
        self.skip_ws();
        if self.b.get(self.i).copied() != Some(b'"') {
            return None;
        }
        let start = self.i;
        self.i += 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'"' => {
                    self.i += 1;
                    return Some(&self.s[start..self.i]);
                }
                b'\\' => {
                    self.i += 1;
                    let ch = self.s[self.i..].chars().next()?;
                    self.i += ch.len_utf8();
                }
                b if b >= 0x80 => {
                    let ch = self.s[self.i..].chars().next()?;
                    self.i += ch.len_utf8();
                }
                _ => self.i += 1,
            }
        }
        None
    }

    fn next_string(&mut self) -> Option<String> {
        let raw = self.next_string_raw()?;
        if !raw.as_bytes().contains(&b'\\') {
            Some(raw[1..raw.len() - 1].to_string())
        } else {
            serde_json::from_str::<String>(raw).ok()
        }
    }

    fn skip_value(&mut self) -> bool {
        self.skip_ws();
        let Some(first) = self.b.get(self.i).copied() else {
            return false;
        };
        match first {
            b'"' => self.next_string_raw().is_some(),
            b'{' | b'[' => {
                let mut depth = 0usize;
                while self.i < self.b.len() {
                    match self.b[self.i] {
                        b'"' => {
                            if self.next_string_raw().is_none() {
                                return false;
                            }
                        }
                        b'{' | b'[' => {
                            depth += 1;
                            self.i += 1;
                        }
                        b'}' | b']' => {
                            if depth == 0 {
                                return false;
                            }
                            depth -= 1;
                            self.i += 1;
                            if depth == 0 {
                                return true;
                            }
                        }
                        b if b >= 0x80 => {
                            let Some(ch) = self.s[self.i..].chars().next() else {
                                return false;
                            };
                            self.i += ch.len_utf8();
                        }
                        _ => self.i += 1,
                    }
                }
                false
            }
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                while self.i < self.b.len()
                    && !matches!(
                        self.b[self.i],
                        b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']'
                    )
                {
                    self.i += 1;
                }
                true
            }
            _ => false,
        }
    }
}

/// Return the folder paths (keys of the top-level `"projects"` object) using a streaming scanner
/// so unrelated project histories and nested objects are skipped without allocating a DOM.
pub fn claude_project_paths(json: &str) -> Vec<String> {
    let mut sc = JsonScan::new(json);
    if !sc.expect(b'{') {
        return Vec::new();
    }
    let mut out = Vec::new();
    loop {
        sc.skip_ws();
        if sc.b.get(sc.i).copied() == Some(b'}') {
            break;
        }
        let Some(key) = sc.next_string() else {
            break;
        };
        if !sc.expect(b':') {
            break;
        }
        if key == "projects" {
            sc.skip_ws();
            if sc.b.get(sc.i).copied() != Some(b'{') {
                return Vec::new();
            }
            sc.i += 1;
            loop {
                sc.skip_ws();
                if sc.b.get(sc.i).copied() == Some(b'}') {
                    break;
                }
                let Some(proj_key) = sc.next_string() else {
                    return out;
                };
                if !sc.expect(b':') || !sc.skip_value() {
                    return out;
                }
                let clean = platform::clean_path_str(&proj_key);
                if !clean.is_empty() {
                    out.push(clean);
                }
                sc.skip_ws();
                match sc.b.get(sc.i).copied() {
                    Some(b',') => sc.i += 1,
                    Some(b'}') => break,
                    _ => return out,
                }
            }
            return out;
        }
        if !sc.skip_value() {
            break;
        }
        sc.skip_ws();
        match sc.b.get(sc.i).copied() {
            Some(b',') => sc.i += 1,
            _ => break,
        }
    }
    out
}

/// Import folders from `~/.claude.json`. `None` means the file does not exist.
pub fn import_claude(db: &Db, ignore: &IgnoreMatcher) -> Result<Option<usize>> {
    let Some(home) = platform::home_dir() else {
        return Ok(None);
    };
    let claude_json = home.join(".claude.json");
    let Ok(text) = platform::read_to_string_capped(&claude_json, MAX_CLAUDE_JSON_BYTES) else {
        return Ok(None);
    };
    let now = model::now();
    let found: Vec<(String, i64)> = claude_project_paths(&text)
        .into_iter()
        .map(|p| (p, now))
        .collect();
    let mut seen = HashSet::new();
    Ok(Some(crate::import_editors::store_with_mtime(
        db, found, "claude", &mut seen, ignore, true,
    )))
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
        assert!(claude_project_paths(r#"{"projects": 123}"#).is_empty());
        assert!(claude_project_paths(r#"{"projects": {"a": "\\\u{e9}"#).is_empty());
        assert!(claude_project_paths("{\"projects\":{\"a\":\"\\é").is_empty());
    }

    #[test]
    fn skips_nested_projects_and_handles_escapes_and_braces() {
        let j = r#"{
            "meta": {"projects": {"/wrong": {}}, "text": "a}b\"c{d"},
            "projects": {
                "/real/one": {"history": ["}not_end", "{\"projects\": 1}"]},
                "C:\\Users\\me\\proj\\": {"nested": [1, 2, {"k": "v}]}"]}
            }
        }"#;
        let p = claude_project_paths(j);
        assert_eq!(p, vec!["/real/one", r"C:\Users\me\proj"]);
    }
}
