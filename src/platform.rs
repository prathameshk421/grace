//! Small helpers that differ between operating systems.
use std::path::{Path, PathBuf};

/// The home folder: `HOME`, else `USERPROFILE` (Windows). Never empty.
pub fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Remove the Windows verbatim prefix: `\\?\C:\x` becomes `C:\x`,
/// and `\\?\UNC\srv\share` becomes `\\srv\share`.
pub fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// True for `/`, `C:\`, `C:/` and `C:`.
pub fn is_root(s: &str) -> bool {
    if s == "/" {
        return true;
    }
    let b = s.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || (b.len() == 3 && (b[2] == b'\\' || b[2] == b'/')))
}

/// Trim trailing `/` and `\`, but keep roots.
pub fn trim_separators(s: &str) -> String {
    let t = s.trim_end_matches(['/', '\\']);
    if t.is_empty() {
        // Only separators: keep one when the input was a root.
        return s.chars().next().map(String::from).unwrap_or_default();
    }
    if t.len() == 2 && t.as_bytes()[1] == b':' {
        return format!("{t}\\");
    }
    t.to_string()
}

/// Clean up a path string: strip the verbatim prefix and trailing separators.
pub fn clean_path_str(s: &str) -> String {
    trim_separators(&strip_verbatim(s))
}

/// Canonicalize when possible and return a native, clean string.
pub fn normalize(p: &Path) -> String {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    clean_path_str(&p.to_string_lossy())
}

/// Compare two paths. Windows paths ignore case.
pub fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Folders we never track: filesystem roots (`/`, `C:\`) and the home folder.
pub fn ignored(path: &str) -> bool {
    if path.is_empty() || is_root(path) {
        return true;
    }
    match home_dir() {
        Some(h) => same_path(path, &clean_path_str(&h.to_string_lossy())),
        None => false,
    }
}

/// Pick the folder to tag. `cwd` is where the command ran. `target` is the first
/// non-flag argument. A leading `~` means home. A relative target is relative to `cwd`.
/// Existing folder: that folder. Existing file or anything else: `cwd`.
pub fn tag_target(cwd: &str, target: Option<&str>) -> String {
    let Some(t) = target.map(str::trim).filter(|t| !t.is_empty()) else {
        return cwd.to_string();
    };
    let mut p = PathBuf::from(t);
    if t == "~" || t.starts_with("~/") || t.starts_with("~\\") {
        match home_dir() {
            Some(h) => p = h.join(t[1..].trim_start_matches(['/', '\\'])),
            None => return cwd.to_string(),
        }
    }
    let full = Path::new(cwd).join(p);
    if full.is_dir() {
        normalize(&full)
    } else {
        cwd.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_verbatim_prefix() {
        assert_eq!(clean_path_str(r"\\?\C:\Users\me\"), r"C:\Users\me");
        assert_eq!(clean_path_str(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
        assert_eq!(clean_path_str("/a/b/"), "/a/b");
        assert_eq!(clean_path_str("/"), "/");
        assert_eq!(clean_path_str(r"C:\"), r"C:\");
        assert_eq!(clean_path_str(r"\\?\C:"), r"C:\");
    }

    #[test]
    fn roots() {
        assert!(is_root("/"));
        assert!(is_root(r"C:\"));
        assert!(is_root("d:"));
        assert!(!is_root(r"C:\x"));
        assert!(!is_root("/x"));
    }

    #[test]
    fn tag_target_resolution() {
        let d = tempfile::tempdir().unwrap();
        let cwd = normalize(d.path());
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("f.txt"), "x").unwrap();
        let sub = normalize(&d.path().join("sub"));
        assert_eq!(tag_target(&cwd, None), cwd);
        assert_eq!(tag_target(&cwd, Some("")), cwd);
        assert_eq!(tag_target(&cwd, Some("sub")), sub);
        assert_eq!(tag_target(&cwd, Some("./sub/")), sub);
        assert_eq!(tag_target(&cwd, Some(&sub)), sub);
        assert_eq!(tag_target(&cwd, Some("f.txt")), cwd);
        assert_eq!(tag_target(&cwd, Some("missing")), cwd);
    }
}
