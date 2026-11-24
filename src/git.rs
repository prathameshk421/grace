//! Small git helpers for `grace diff`, `grace tree`, and picker previews. They run the git CLI.
use crate::platform;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Max lines of one diff. Longer diffs are cut.
pub const MAX_DIFF_LINES: usize = 5000;
/// Max bytes read from git or from an untracked file.
const MAX_BYTES: usize = 2 << 20;
const MAX_UNTRACKED_STATS: usize = 64;
const MAX_UNTRACKED_STAT_BYTES: usize = 512 * 1024;

/// Diff scope filter in `grace diff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffScope {
    #[default]
    All,
    Unstaged,
    Staged,
}

impl DiffScope {
    pub fn next(self) -> DiffScope {
        match self {
            DiffScope::All => DiffScope::Unstaged,
            DiffScope::Unstaged => DiffScope::Staged,
            DiffScope::Staged => DiffScope::All,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            DiffScope::All => "all",
            DiffScope::Unstaged => "unstaged",
            DiffScope::Staged => "staged",
        }
    }
}

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
    /// Line additions (from numstat / untracked).
    pub added: Option<u32>,
    /// Line deletions (from numstat).
    pub deleted: Option<u32>,
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
    pub ahead: u32,
    pub behind: u32,
    pub has_commits: bool,
}

/// Parse `## main...origin/main [ahead 1, behind 2]` (without the "## ").
pub fn parse_branch(s: &str) -> Branch {
    let (head, info) = match s.find(" [") {
        Some(i) => (&s[..i], s[i + 2..].trim_end_matches(']')),
        None => (s, ""),
    };
    let mut b = Branch {
        has_commits: true,
        ..Branch::default()
    };
    if let Some(rest) = head.strip_prefix("No commits yet on ") {
        b.name = rest.to_string();
        b.has_commits = false;
    } else if let Some(rest) = head.strip_prefix("Initial commit on ") {
        b.name = rest.to_string();
        b.has_commits = false;
    } else {
        b.name = head.split("...").next().unwrap_or(head).to_string();
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
        let orig = if x == 'R' || x == 'C' {
            fields.next().map(str::to_string)
        } else {
            None
        };
        changes.push(Change {
            x,
            y,
            path,
            orig,
            added: None,
            deleted: None,
        });
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

/// A parsed line of a unified diff, with optional old/new line numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

impl DiffLine {
    pub fn meta(text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Meta,
            old_no: None,
            new_no: None,
            text: text.into(),
        }
    }
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

/// Parse `@@ -old[,count] +new[,count] @@` (and combined `@@@ -old ... +new @@@`) into `(old_start, new_start)`.
fn parse_hunk_start(line: &str) -> Option<(u32, u32)> {
    let rest = line.trim_start_matches('@').trim_start();
    let (header, _) = rest.split_once(" @@")?;
    let old_part = header.strip_prefix('-')?.split([' ', ',']).next()?;
    let plus_pos = header.rfind(" +")?;
    let new_part = header[plus_pos + 2..].split([',', ' ']).next()?;
    let old = old_part.parse::<u32>().ok()?;
    let new = new_part.parse::<u32>().ok()?;
    Some((old, new))
}

/// Classify all lines of a diff and track old/new line numbers.
#[cfg(test)]
pub fn classify_lines(text: &str) -> Vec<DiffLine> {
    classify_lines_capped(text, usize::MAX)
}

/// Classify up to `max` diff lines, appending a `Meta` summary when truncated.
pub fn classify_lines_capped(text: &str, max: usize) -> Vec<DiffLine> {
    let mut in_hunk = false;
    let mut old_ln = 0u32;
    let mut new_ln = 0u32;
    let mut out = Vec::new();
    let mut lines = text.lines();
    for line in lines.by_ref() {
        if out.len() == max {
            let remaining = 1 + lines.count();
            out.push(DiffLine::meta(format!(
                "… {remaining} more lines not shown"
            )));
            return out;
        }
        if line.starts_with("diff ") {
            in_hunk = false;
        }
        let kind = classify(line, in_hunk);
        let (old_no, new_no) = match kind {
            LineKind::Hunk => {
                in_hunk = true;
                if let Some((o, n)) = parse_hunk_start(line) {
                    old_ln = o;
                    new_ln = n;
                }
                (None, None)
            }
            LineKind::Add => {
                let n = Some(new_ln);
                new_ln = new_ln.saturating_add(1);
                (None, n)
            }
            LineKind::Del => {
                let o = Some(old_ln);
                old_ln = old_ln.saturating_add(1);
                (o, None)
            }
            LineKind::Context => {
                let res = (Some(old_ln), Some(new_ln));
                old_ln = old_ln.saturating_add(1);
                new_ln = new_ln.saturating_add(1);
                res
            }
            LineKind::Header | LineKind::Meta => (None, None),
        };
        out.push(DiffLine {
            kind,
            old_no,
            new_no,
            text: platform::sanitize_display(line).into_owned(),
        });
    }
    out
}

/// Cut a diff to `max` lines. Adds a note when it cuts.
#[cfg(test)]
pub fn cap_diff_lines(mut lines: Vec<DiffLine>, max: usize) -> Vec<DiffLine> {
    if lines.len() > max {
        let n = lines.len();
        lines.truncate(max);
        lines.push(DiffLine::meta(format!(
            "… {} more lines not shown",
            n - max
        )));
    }
    lines
}

/// True if the bytes look binary: a NUL byte in the first 8 KB.
pub fn looks_binary(b: &[u8]) -> bool {
    b.iter().take(8000).any(|&c| c == 0)
}

/// An untracked file as an "added" diff, bounded to `MAX_DIFF_LINES`.
pub fn untracked_diff(path: &str, content: &[u8]) -> Vec<DiffLine> {
    untracked_diff_capped(path, content, MAX_DIFF_LINES)
}

fn untracked_diff_capped(path: &str, content: &[u8], max: usize) -> Vec<DiffLine> {
    let mut out = vec![DiffLine {
        kind: LineKind::Header,
        old_no: None,
        new_no: None,
        text: format!("new file {} (untracked)", platform::sanitize_display(path)),
    }];
    if looks_binary(content) {
        out.push(DiffLine::meta("Binary file"));
        return out;
    }
    let text = String::from_utf8_lossy(content);
    let n = text.lines().count();
    out.push(DiffLine {
        kind: LineKind::Hunk,
        old_no: None,
        new_no: None,
        text: format!("@@ -0,0 +1,{n} @@"),
    });
    for (idx, l) in text.lines().enumerate() {
        if out.len() >= max {
            out.push(DiffLine::meta(format!(
                "… {} more lines not shown",
                n - idx
            )));
            break;
        }
        out.push(DiffLine {
            kind: LineKind::Add,
            old_no: None,
            new_no: Some(idx as u32 + 1),
            text: format!("+{}", platform::sanitize_display(l)),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Running git

/// Run git safely with `LC_ALL=C`, disabled fsmonitor/pager, and bounded streaming stdout read.
/// Returns `(bytes, truncated)`.
fn git_raw(dir: &Path, args: &[&str]) -> Option<(Vec<u8>, bool)> {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.fsmonitor=false", "-c", "core.pager=cat", "-C"])
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    platform::apply_no_window(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let mut buf = Vec::new();
    let read_ok = stdout
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .is_ok();
    let truncated = buf.len() > MAX_BYTES;
    if truncated {
        buf.truncate(MAX_BYTES);
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    if !read_ok || (!truncated && !status.success()) {
        return None;
    }
    Some((buf, truncated))
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    git_raw(dir, args).map(|(b, _)| b)
}

/// Drop the final partial NUL-delimited record when stdout hit `MAX_BYTES`.
fn trim_last_nul_record(mut b: Vec<u8>, truncated: bool) -> Vec<u8> {
    if truncated {
        if let Some(pos) = b.iter().rposition(|&c| c == 0) {
            b.truncate(pos + 1);
        } else {
            b.clear();
        }
    }
    b
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

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Status of the repository that holds `dir`, including per-file `+N -M` numstat.
pub fn status(dir: &Path) -> Option<Repo> {
    status_opts(dir, true)
}

/// Status of the repository that holds `dir`, optionally skipping per-file `+N -M` line counts.
pub fn status_opts(dir: &Path, with_numstat: bool) -> Option<Repo> {
    let rp = git(dir, &["rev-parse", "--show-toplevel", "--show-prefix"])?;
    let rp_str = String::from_utf8_lossy(&rp);
    let mut rp_lines = rp_str.lines();
    let root = rp_lines.next()?.trim_end_matches('\r').to_string();
    let prefix = rp_lines
        .next()
        .unwrap_or("")
        .trim_end_matches('\r')
        .to_string();

    let (raw, trunc) = git_raw(
        Path::new(&root),
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--branch",
            "--untracked-files=all",
        ],
    )?;
    let out = trim_last_nul_record(raw, trunc);
    let (branch, mut changes) = parse_porcelain(&out);
    let branch = branch.unwrap_or_default();

    if with_numstat {
        let stats = if changes.iter().any(|c| !c.untracked()) {
            let base = if branch.has_commits {
                "HEAD"
            } else {
                EMPTY_TREE
            };
            numstat(Path::new(&root), base)
        } else {
            HashMap::new()
        };
        let mut untracked_seen = 0usize;
        for c in &mut changes {
            if c.untracked() {
                if untracked_seen < MAX_UNTRACKED_STATS {
                    untracked_seen += 1;
                    let full = Path::new(&root).join(&c.path);
                    if let Some(lines) = count_untracked_lines(&full) {
                        c.added = Some(lines);
                        c.deleted = Some(0);
                    }
                }
            } else if let Some(&(a, d)) = stats.get(&c.path) {
                c.added = Some(a);
                c.deleted = Some(d);
            }
        }
    }
    Some(Repo {
        root,
        prefix,
        branch,
        changes,
    })
}

fn count_untracked_lines(full: &Path) -> Option<u32> {
    let meta = std::fs::symlink_metadata(full).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_UNTRACKED_STAT_BYTES as u64 {
        return None;
    }
    let mut f = std::fs::File::open(full).ok()?;
    let mut buf = [0u8; 16 * 1024];
    let mut total = 0usize;
    let mut lines = 0u32;
    let mut last_byte = b'\n';
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        if total == 0 && looks_binary(&buf[..n]) {
            return None;
        }
        total += n;
        if total > MAX_UNTRACKED_STAT_BYTES {
            return None;
        }
        let chunk = &buf[..n];
        lines = lines.saturating_add(chunk.iter().filter(|&&b| b == b'\n').count() as u32);
        last_byte = chunk[n - 1];
    }
    if total > 0 && last_byte != b'\n' {
        lines = lines.saturating_add(1);
    }
    Some(lines)
}

fn numstat(root: &Path, base: &str) -> HashMap<String, (u32, u32)> {
    let mut map = HashMap::new();
    let Some(out) = git(
        root,
        &["diff", "--numstat", "-z", "--no-ext-diff", base, "--"],
    ) else {
        return map;
    };
    let text = String::from_utf8_lossy(&out);
    let mut fields = text.split('\0');
    while let Some(f) = fields.next() {
        if f.is_empty() {
            continue;
        }
        let mut parts = f.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if p.is_empty() {
            // `-z` rename format: `<add>\t<del>\t\0<orig>\0<new>\0`
            let _orig = fields.next();
            match fields.next() {
                Some(newp) if !newp.is_empty() => newp,
                _ => continue,
            }
        } else {
            p
        };
        if let (Ok(an), Ok(dn)) = (a.parse::<u32>(), d.parse::<u32>()) {
            map.insert(path.to_string(), (an, dn));
        }
    }
    map
}

/// Unified diff of one changed file according to `scope`.
pub fn file_diff(repo: &Repo, c: &Change, scope: DiffScope) -> Vec<DiffLine> {
    let root = Path::new(&repo.root);
    if c.untracked() {
        if scope == DiffScope::Staged {
            return vec![DiffLine::meta("untracked file (not staged)")];
        }
        let full = root.join(&c.path);
        // Never follow symlinks or open FIFOs/sockets!
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            return vec![DiffLine::meta("cannot inspect file")];
        };
        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&full) {
                return vec![
                    DiffLine {
                        kind: LineKind::Header,
                        old_no: None,
                        new_no: None,
                        text: format!(
                            "new symlink {} (untracked)",
                            platform::sanitize_display(&c.path)
                        ),
                    },
                    DiffLine::meta(format!(
                        "points to {}",
                        platform::sanitize_display(&target.to_string_lossy())
                    )),
                ];
            }
        }
        if meta.file_type().is_dir() {
            return vec![DiffLine::meta("untracked folder")];
        }
        if !meta.file_type().is_file() {
            return vec![DiffLine::meta("special file (skipped)")];
        }
        return match read_capped(&full) {
            Some(b) => untracked_diff(&c.path, &b),
            None => vec![DiffLine::meta("cannot read the file")],
        };
    }
    let base = if repo.branch.has_commits {
        "HEAD"
    } else {
        EMPTY_TREE
    };
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M"];
    match scope {
        DiffScope::All => args.push(base),
        DiffScope::Staged => {
            args.push("--cached");
            args.push(base);
        }
        DiffScope::Unstaged => {}
    }
    args.push("--");
    if let Some(o) = &c.orig {
        args.push(o);
    }
    args.push(&c.path);
    match git(root, &args) {
        Some(b) if !b.is_empty() => {
            classify_lines_capped(&String::from_utf8_lossy(&b), MAX_DIFF_LINES)
        }
        Some(_) => vec![DiffLine::meta(format!("no {} changes", scope.label()))],
        None => vec![DiffLine::meta("git diff failed")],
    }
}

/// Stage or unstage one file relative to `repo.root`.
pub fn stage_file(repo: &Repo, c: &Change, stage: bool) -> bool {
    let root = Path::new(&repo.root);
    let mut args: Vec<&str> = if stage {
        vec!["add", "--"]
    } else if repo.branch.has_commits {
        vec!["restore", "--staged", "--"]
    } else {
        vec!["rm", "--cached", "-f", "--"]
    };
    if !stage {
        if let Some(o) = &c.orig {
            args.push(o);
        }
    }
    args.push(&c.path);
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.fsmonitor=false", "-C"])
        .arg(root)
        .args(args);
    platform::apply_no_window(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn read_capped(p: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(p).ok()?;
    let mut b = Vec::new();
    f.take(MAX_BYTES as u64).read_to_end(&mut b).ok()?;
    Some(b)
}

/// Files below `dir` that git does not ignore, relative to `dir`, with '/' separators.
/// None: not a repository.
pub fn ls_files(dir: &Path) -> Option<Vec<String>> {
    let (raw, trunc) = git_raw(
        dir,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let out = trim_last_nul_record(raw, trunc);
    let text = String::from_utf8_lossy(&out);
    let mut v: Vec<String> = text
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    v.dedup();
    Some(v)
}

/// Locate `.git` directory or gitfile up to 32 ancestors above `dir`.
/// Returns `(git_dir, is_worktree)`.
pub fn resolve_git_dir(dir: &Path) -> Option<(PathBuf, bool)> {
    for anc in dir.ancestors().take(32) {
        let dot_git = anc.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.file_type().is_dir() {
            return Some((dot_git, false));
        }
        if meta.file_type().is_file() {
            let content = platform::read_to_string_capped(&dot_git, 4096).ok()?;
            let rest = content.trim().strip_prefix("gitdir:")?.trim();
            let target = Path::new(rest);
            let gd = if target.is_absolute() {
                target.to_path_buf()
            } else {
                anc.join(target)
            };
            let wt = rest.contains("/worktrees/") || rest.contains("\\worktrees\\");
            return Some((gd, wt));
        }
        return None;
    }
    None
}

fn has_stash_ref(git_dir: &Path, is_worktree: bool) -> bool {
    let common = if is_worktree {
        match platform::read_to_string_capped(&git_dir.join("commondir"), 1024) {
            Ok(s) => {
                let rel = s.trim();
                let p = Path::new(rel);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    git_dir.join(p)
                }
            }
            Err(_) => return true,
        }
    } else {
        git_dir.to_path_buf()
    };
    common.join("refs/stash").exists() || common.join("logs/refs/stash").exists()
}

/// Zero-subprocess fast branch + worktree check from `.git/HEAD` (or `.git` file).
/// Returns `(branch_or_short_oid, is_worktree)`. Safe & < 0.1 ms.
pub fn fast_branch(dir: &Path) -> Option<(String, bool)> {
    let (git_dir, is_worktree) = resolve_git_dir(dir)?;
    let head = platform::read_to_string_capped(&git_dir.join("HEAD"), 512).ok()?;
    let h = head.trim();
    let label = if let Some(r) = h.strip_prefix("ref: refs/heads/") {
        r.to_string()
    } else if let Some(r) = h.strip_prefix("ref: ") {
        r.to_string()
    } else if h.chars().count() >= 7 {
        h.chars().take(7).collect()
    } else {
        return None;
    };
    Some((platform::sanitize_display(&label).into_owned(), is_worktree))
}

/// Compact summary for the picker Inspector drawer.
#[derive(Debug, Clone, Default)]
pub struct RepoSummary {
    pub branch: Branch,
    pub changed: usize,
    pub stashes: usize,
    pub commits: Vec<String>,
    pub worktree: bool,
}

pub fn summary(dir: &Path) -> Option<RepoSummary> {
    let repo = status_opts(dir, false)?;
    let root = Path::new(&repo.root);
    let commits = git(root, &["log", "--oneline", "-n", "3", "--no-decorate"])
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .map(|l| platform::sanitize_display(l).into_owned())
                .collect()
        })
        .unwrap_or_default();
    let (git_dir_opt, worktree) = match resolve_git_dir(root) {
        Some((gd, wt)) => (Some(gd), wt),
        None => (None, false),
    };
    let may_have_stash = git_dir_opt
        .as_deref()
        .map(|gd| has_stash_ref(gd, worktree))
        .unwrap_or(true);
    let stashes = if may_have_stash {
        git(root, &["stash", "list"])
            .map(|b| String::from_utf8_lossy(&b).lines().count())
            .unwrap_or(0)
    } else {
        0
    };
    Some(RepoSummary {
        branch: repo.branch,
        changed: repo.changes.len(),
        stashes,
        commits,
        worktree,
    })
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
        assert_eq!(c.len(), 6);
        assert_eq!(c[0].path, "src/a.rs");
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
        assert!(!parse_branch("No commits yet on dev").has_commits);
        let b = parse_branch("feat...up/feat [behind 3]");
        assert_eq!((b.ahead, b.behind), (0, 3));
        assert_eq!(parse_branch("HEAD (no branch)").name, "HEAD (no branch)");
    }

    #[test]
    fn diff_lines_and_numbers() {
        let d = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -10,2 +10,2 @@\n ctx\n-old\n+new\n\\ No newline at end of file\ndiff --git a/y b/y\nBinary files a/y and b/y differ\n";
        let lines = classify_lines(d);
        let kinds: Vec<LineKind> = lines.iter().map(|l| l.kind).collect();
        use LineKind::*;
        assert_eq!(
            kinds,
            vec![Header, Header, Header, Header, Hunk, Context, Del, Add, Meta, Header, Meta]
        );
        assert_eq!((lines[5].old_no, lines[5].new_no), (Some(10), Some(10)));
        assert_eq!((lines[6].old_no, lines[6].new_no), (Some(11), None));
        assert_eq!((lines[7].old_no, lines[7].new_no), (None, Some(11)));
        // "---" in a hunk is a removed line, not a header.
        assert_eq!(classify("--- x", true), Del);
        assert_eq!(classify("--- x", false), Header);
        // Combined conflict hunk header.
        assert_eq!(
            parse_hunk_start("@@@ -10,2 -15,3 +12,4 @@@ fn x + y"),
            Some((10, 12))
        );
    }

    #[test]
    fn caps_and_untracked() {
        let lines: Vec<_> = (0..10)
            .map(|i| DiffLine {
                kind: LineKind::Context,
                old_no: None,
                new_no: None,
                text: i.to_string(),
            })
            .collect();
        let c = cap_diff_lines(lines, 4);
        assert_eq!(c.len(), 5);
        assert_eq!(c[4].text, "… 6 more lines not shown");
        let u = untracked_diff("a.txt", b"one\ntwo\n");
        assert_eq!(u[1].text, "@@ -0,0 +1,2 @@");
        assert_eq!(u[3].text, "+two");
        assert_eq!(u[3].new_no, Some(2));
        let bin = untracked_diff("a.bin", b"\x00\x01");
        assert_eq!(bin[1].text, "Binary file");
    }

    #[test]
    fn real_repo() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(p)
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a.txt"), "x\n").unwrap();
        let r = status(p).unwrap();
        assert_eq!(r.changes.len(), 1);
        assert!(r.changes[0].untracked());
        let diff = file_diff(&r, &r.changes[0], DiffScope::All);
        assert!(diff
            .iter()
            .any(|l| l.kind == LineKind::Add && l.text == "+x"));
        assert_eq!(ls_files(p).unwrap(), vec!["a.txt".to_string()]);
        assert_eq!(fast_branch(p), Some(("main".into(), false)));
        let sub = p.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(fast_branch(&sub), Some(("main".into(), false)));
        let sum = summary(p).unwrap();
        assert_eq!(sum.changed, 1);
        assert_eq!(sum.stashes, 0);
        assert!(!sum.worktree);
    }
}
