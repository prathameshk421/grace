//! Small git helpers for `grace diff` and `grace tree`. They run the git CLI.
use std::path::Path;
use std::process::{Command, Stdio};

/// Max lines of one diff. Longer diffs are cut.
pub const MAX_DIFF_LINES: usize = 5000;
/// Max bytes read from git or from an untracked file.
const MAX_BYTES: usize = 2 << 20;

/// One changed file from `git status --porcelain=v1 -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Index status, for example 'M', 'A', 'D', 'R', '?'.
    pub x: char,
    /// Worktree status.
    pub y: char,
    /// Path relative to the repository root, with '/' separators.
    pub path: String,
    /// The old path of a rename or copy.
    pub orig: Option<String>,
}

/// How a change shows: the letter and its color group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Modified,
    Added,
    Deleted,
    Renamed,
    Conflict,
}

impl Change {
    pub fn untracked(&self) -> bool {
        self.x == '?'
    }

    pub fn mark(&self) -> Mark {
        let (x, y) = (self.x, self.y);
        if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
            Mark::Conflict
        } else if x == '?' || x == 'A' {
            Mark::Added
        } else if x == 'D' || y == 'D' {
            Mark::Deleted
        } else if x == 'R' || x == 'C' {
            Mark::Renamed
        } else {
            Mark::Modified
        }
    }

    /// Two-letter code for the list, for example "M ", "??", "R ".
    pub fn code(&self) -> String {
        if self.untracked() {
            return "??".into();
        }
        format!("{}{}", self.x, self.y)
    }
}

/// Branch line of `git status --branch`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

/// Parse `## main...origin/main [ahead 1, behind 2]` (without the "## ").
pub fn parse_branch(s: &str) -> Branch {
    let (head, info) = match s.find(" [") {
        Some(i) => (&s[..i], s[i + 2..].trim_end_matches(']')),
        None => (s, ""),
    };
    let mut b = Branch::default();
    if let Some(rest) = head.strip_prefix("No commits yet on ") {
        b.name = rest.to_string();
    } else if let Some((name, up)) = head.split_once("...") {
        b.name = name.to_string();
        b.upstream = Some(up.to_string());
    } else {
        b.name = head.to_string();
    }
    for part in info.split(", ") {
        if let Some(n) = part.strip_prefix("ahead ") {
            b.ahead = n.parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            b.behind = n.parse().unwrap_or(0);
        }
    }
    b
}

/// Parse `git status --porcelain=v1 -z --branch` output.
pub fn parse_porcelain(out: &[u8]) -> (Option<Branch>, Vec<Change>) {
    let text = String::from_utf8_lossy(out);
    let mut fields = text.split('\0');
    let mut branch = None;
    let mut changes = Vec::new();
    while let Some(f) = fields.next() {
        if let Some(b) = f.strip_prefix("## ") {
            branch = Some(parse_branch(b));
            continue;
        }
        let chars: Vec<char> = f.chars().collect();
        if chars.len() < 4 || chars[2] != ' ' {
            continue;
        }
        let (x, y) = (chars[0], chars[1]);
        let path: String = chars[3..].iter().collect();
        // A rename or copy is followed by the old path.
        let orig = if x == 'R' || x == 'C' { fields.next().map(str::to_string) } else { None };
        changes.push(Change { x, y, path, orig });
    }
    (branch, changes)
}

/// What a diff line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// `diff --git`, `index`, `---`, `+++`, mode lines.
    Header,
    /// `@@ -1,2 +1,3 @@`
    Hunk,
    Add,
    Del,
    Context,
    /// Notes such as "Binary files differ" or "\ No newline at end of file".
    Meta,
}

/// Classify one line of a unified diff. `in_hunk` is true after the first "@@".
pub fn classify(line: &str, in_hunk: bool) -> LineKind {
    if line.starts_with("@@") {
        return LineKind::Hunk;
    }
    if !in_hunk {
        if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            return LineKind::Meta;
        }
        return LineKind::Header;
    }
    match line.chars().next() {
        Some('+') => LineKind::Add,
        Some('-') => LineKind::Del,
        Some('\\') => LineKind::Meta,
        Some(' ') | None => LineKind::Context,
        // A new file section starts.
        _ if line.starts_with("diff ") => LineKind::Header,
        _ => LineKind::Context,
    }
}

/// Classify all lines of a diff.
pub fn classify_all(text: &str) -> Vec<(LineKind, String)> {
    let mut in_hunk = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with("diff ") {
            in_hunk = false;
        }
        let k = classify(line, in_hunk);
        if k == LineKind::Hunk {
            in_hunk = true;
        }
        out.push((k, printable(line)));
    }
    out
}

/// A line that is safe to draw: tabs become spaces, other control bytes become "?".
/// Raw ESC or CR bytes from a file would move the cursor or change colors.
pub fn printable(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => o.push_str("    "),
            c if c.is_control() => o.push('?'),
            c => o.push(c),
        }
    }
    o
}

/// Cut a diff to `max` lines. Adds a note when it cuts.
pub fn cap_lines(mut lines: Vec<(LineKind, String)>, max: usize) -> Vec<(LineKind, String)> {
    if lines.len() > max {
        let n = lines.len();
        lines.truncate(max);
        lines.push((LineKind::Meta, format!("… {} more lines not shown", n - max)));
    }
    lines
}

/// True if the bytes look binary: a NUL byte in the first 8 KB.
pub fn looks_binary(b: &[u8]) -> bool {
    b.iter().take(8000).any(|&c| c == 0)
}

/// An untracked file as an "added" diff.
pub fn untracked_diff(path: &str, content: &[u8]) -> Vec<(LineKind, String)> {
    let mut out = vec![(LineKind::Header, format!("new file {path} (untracked)"))];
    if looks_binary(content) {
        out.push((LineKind::Meta, "Binary file".into()));
        return out;
    }
    let text = String::from_utf8_lossy(content);
    let n = text.lines().count();
    out.push((LineKind::Hunk, format!("@@ -0,0 +1,{n} @@")));
    for l in text.lines() {
        out.push((LineKind::Add, format!("+{}", printable(l))));
    }
    out
}

// ---------------------------------------------------------------------------
// Running git

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut b = out.stdout;
    b.truncate(MAX_BYTES);
    Some(b)
}

fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).map(|b| String::from_utf8_lossy(&b).trim_end_matches(['\n', '\r']).to_string())
}

/// A repository as `grace diff` and `grace tree` see it.
#[derive(Debug, Clone, Default)]
pub struct Repo {
    /// The repository root.
    pub root: String,
    /// The folder below the root that we look at, like "src/", or "".
    pub prefix: String,
    pub branch: Branch,
    pub changes: Vec<Change>,
}

/// Status of the repository that holds `dir`. None: not a git repository (or no git).
pub fn status(dir: &Path) -> Option<Repo> {
    let root = git_line(dir, &["rev-parse", "--show-toplevel"])?;
    let prefix = git_line(dir, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let out = git(Path::new(&root), &["status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all"])?;
    let (branch, changes) = parse_porcelain(&out);
    Some(Repo { root, prefix, branch: branch.unwrap_or_default(), changes })
}

/// Unified diff of one changed file against HEAD (staged and unstaged together).
pub fn file_diff(repo: &Repo, c: &Change) -> Vec<(LineKind, String)> {
    let root = Path::new(&repo.root);
    if c.untracked() {
        let full = root.join(&c.path);
        // A symlink: show where it points, like git does. Do not follow it: it may
        // point at a folder, at nothing, or at itself.
        if let Ok(target) = std::fs::read_link(&full) {
            return vec![
                (LineKind::Header, format!("new symlink {} (untracked)", printable(&c.path))),
                (LineKind::Meta, format!("points to {}", printable(&target.to_string_lossy()))),
            ];
        }
        if full.is_dir() {
            return vec![(LineKind::Meta, "untracked folder".into())];
        }
        return match read_capped(&full) {
            Some(b) => cap_lines(untracked_diff(&c.path, &b), MAX_DIFF_LINES),
            None => vec![(LineKind::Meta, "cannot read the file".into())],
        };
    }
    // Without commits, compare with the empty tree.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let base = if git(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_some() { "HEAD" } else { EMPTY_TREE };
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-M", base, "--"];
    if let Some(o) = &c.orig {
        args.push(o);
    }
    args.push(&c.path);
    match git(root, &args) {
        Some(b) if !b.is_empty() => cap_lines(classify_all(&String::from_utf8_lossy(&b)), MAX_DIFF_LINES),
        Some(_) => vec![(LineKind::Meta, "no changes against HEAD".into())],
        None => vec![(LineKind::Meta, "git diff failed".into())],
    }
}

fn read_capped(p: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(p).ok()?;
    let mut b = Vec::new();
    f.take(MAX_BYTES as u64).read_to_end(&mut b).ok()?;
    Some(b)
}

/// Files below `dir` that git does not ignore, relative to `dir`, with '/' separators.
/// None: not a repository.
pub fn ls_files(dir: &Path) -> Option<Vec<String>> {
    let out = git(dir, &["ls-files", "--cached", "--others", "--exclude-standard", "-z"])?;
    let text = String::from_utf8_lossy(&out);
    let mut v: Vec<String> = text.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
    v.dedup();
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parsing() {
        let out = b"## main...origin/main [ahead 2, behind 1]\0 M src/a.rs\0A  new.rs\0R  b.rs\0old b.rs\0?? notes/x.md\0 D gone.txt\0UU both.rs\0";
        let (b, c) = parse_porcelain(out);
        let b = b.unwrap();
        assert_eq!((b.name.as_str(), b.ahead, b.behind), ("main", 2, 1));
        assert_eq!(b.upstream.as_deref(), Some("origin/main"));
        assert_eq!(c.len(), 6);
        assert_eq!(c[0], Change { x: ' ', y: 'M', path: "src/a.rs".into(), orig: None });
        assert_eq!(c[0].mark(), Mark::Modified);
        assert_eq!(c[1].mark(), Mark::Added);
        assert_eq!(c[2].orig.as_deref(), Some("old b.rs"));
        assert_eq!(c[2].mark(), Mark::Renamed);
        assert!(c[3].untracked());
        assert_eq!((c[3].code().as_str(), c[3].mark()), ("??", Mark::Added));
        assert_eq!(c[4].mark(), Mark::Deleted);
        assert_eq!(c[5].mark(), Mark::Conflict);
    }

    #[test]
    fn branch_lines() {
        assert_eq!(parse_branch("main").name, "main");
        assert_eq!(parse_branch("No commits yet on dev").name, "dev");
        let b = parse_branch("feat...up/feat [behind 3]");
        assert_eq!((b.ahead, b.behind), (0, 3));
        assert_eq!(parse_branch("HEAD (no branch)").name, "HEAD (no branch)");
    }

    #[test]
    fn diff_lines() {
        let d = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n\\ No newline at end of file\ndiff --git a/y b/y\nBinary files a/y and b/y differ\n";
        let kinds: Vec<LineKind> = classify_all(d).into_iter().map(|(k, _)| k).collect();
        use LineKind::*;
        assert_eq!(kinds, vec![Header, Header, Header, Header, Hunk, Context, Del, Add, Meta, Header, Meta]);
        // "---" in a hunk is a removed line, not a header.
        assert_eq!(classify("--- x", true), Del);
        assert_eq!(classify("--- x", false), Header);
    }

    #[test]
    fn caps_and_untracked() {
        let lines: Vec<_> = (0..10).map(|i| (LineKind::Context, i.to_string())).collect();
        let c = cap_lines(lines, 4);
        assert_eq!(c.len(), 5);
        assert_eq!(c[4].1, "… 6 more lines not shown");
        let u = untracked_diff("a.txt", b"one\ntwo\n");
        assert_eq!(u[1], (LineKind::Hunk, "@@ -0,0 +1,2 @@".into()));
        assert_eq!(u[3], (LineKind::Add, "+two".into()));
        let bin = untracked_diff("a.bin", b"\x00\x01");
        assert_eq!(bin[1], (LineKind::Meta, "Binary file".into()));
    }

    #[test]
    fn real_repo() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let run = |args: &[&str]| {
            Command::new("git").arg("-C").arg(p).args(args).output().unwrap();
        };
        run(&["init", "-q"]);
        std::fs::write(p.join("a.txt"), "x\n").unwrap();
        let r = status(p).unwrap();
        assert_eq!(r.changes.len(), 1);
        assert!(r.changes[0].untracked());
        let diff = file_diff(&r, &r.changes[0]);
        assert!(diff.iter().any(|(k, l)| *k == LineKind::Add && l == "+x"));
        assert_eq!(ls_files(p).unwrap(), vec!["a.txt".to_string()]);
    }
}
