//! Small helpers that differ between operating systems.
use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Read a non-empty environment variable.
pub fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Resolve a directory from `override_env` (if non-empty) else `~/<default_sub>`.
pub fn env_dir(override_env: &str, default_sub: &str) -> Option<PathBuf> {
    if let Some(v) = env_non_empty(override_env) {
        return Some(PathBuf::from(v));
    }
    home_dir().map(|h| h.join(default_sub))
}

/// Resolve a config directory: `override_env` -> `$XDG_CONFIG_HOME/<subdir>` -> `~/.config/<subdir>`.
pub fn xdg_config_dir(override_env: Option<&str>, subdir: &str) -> Option<PathBuf> {
    if let Some(key) = override_env {
        if let Some(v) = env_non_empty(key) {
            return Some(PathBuf::from(v));
        }
    }
    if let Some(v) = env_non_empty("XDG_CONFIG_HOME").filter(|v| Path::new(v).is_absolute()) {
        return Some(PathBuf::from(v).join(subdir));
    }
    home_dir().map(|h| h.join(".config").join(subdir))
}

/// The home folder: `USERPROFILE` first on Windows (avoids Git Bash POSIX `HOME`),
/// `HOME` first on Unix. Never empty.
pub fn home_dir() -> Option<PathBuf> {
    let vars: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME", "USERPROFILE"]
    };
    vars.iter()
        .filter_map(|k| env_non_empty(k))
        .map(PathBuf::from)
        .next()
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

/// True for `/`, `C:\`, `C:/`, `C:`, and UNC share roots `\\srv\share`.
pub fn is_root(s: &str) -> bool {
    if s == "/" {
        return true;
    }
    let b = s.as_bytes();
    if b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || (b.len() == 3 && (b[2] == b'\\' || b[2] == b'/')))
    {
        return true;
    }
    // UNC share root: \\server\share or //server/share (with optional single trailing separator).
    let norm = s.replace('/', "\\");
    if let Some(rest) = norm.strip_prefix(r"\\") {
        let trimmed = rest.trim_end_matches('\\');
        if let Some((srv, share)) = trimmed.split_once('\\') {
            return !srv.is_empty() && !share.is_empty() && !share.contains('\\');
        }
    }
    false
}

/// Trim trailing `/` and `\`, but keep roots.
pub fn trim_separators(s: &str) -> String {
    if s.starts_with(r"\\") && is_root(s) {
        return s.replace('/', "\\").trim_end_matches('\\').to_string();
    }
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
/// Relative fallback paths are anchored against `current_dir` when possible.
pub fn normalize(p: &Path) -> String {
    let resolved = match std::fs::canonicalize(p) {
        Ok(c) => c,
        Err(_) => {
            if p.is_relative() {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p))
                    .unwrap_or_else(|_| p.to_path_buf())
            } else {
                p.to_path_buf()
            }
        }
    };
    clean_path_str(&resolved.to_string_lossy())
}

/// Compare two paths. Windows paths ignore ASCII case and treat `/` and `\` alike.
pub fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        let a = a.replace('/', "\\");
        let b = b.replace('/', "\\");
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

/// True if `s` contains CR, LF, or NUL — unsafe for line-delimited shell protocols or configs.
pub fn has_line_break_or_nul(s: &str) -> bool {
    s.contains(['\n', '\r', '\0'])
}

/// True if `c` is an ASCII/Unicode control char or BiDi override/isolate control.
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control() || is_bidi_control(c)
}

/// Sanitize arbitrary strings for safe terminal / UI display:
/// replaces tabs with 4 spaces, and replaces ASCII/Unicode control characters and
/// Unicode BiDi override/isolate controls with '?'.
pub fn sanitize_display(s: &str) -> Cow<'_, str> {
    if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Cow::Borrowed(s);
    }
    let mut dirty = false;
    for c in s.chars() {
        if c == '\t' || is_unsafe_char(c) {
            dirty = true;
            break;
        }
    }
    if !dirty {
        return Cow::Borrowed(s);
    }
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => o.push_str("    "),
            c if is_unsafe_char(c) => o.push('?'),
            c => o.push(c),
        }
    }
    Cow::Owned(o)
}

/// Shorten a displayed path with `~` only at an actual path separator boundary.
/// Handles `/` vs `\` and ASCII case-insensitivity on Windows.
pub fn display_path(path: &str, home: Option<&str>) -> String {
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        let h = trim_separators(h);
        if same_path(path, &h) {
            return "~".into();
        }
        if let (Some(pre), Some(rest)) = (path.get(..h.len()), path.get(h.len()..)) {
            let sep = rest.starts_with('/') || (cfg!(windows) && rest.starts_with('\\'));
            if sep && same_path(pre, &h) {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

/// Clean, sanitize, and shorten `p` for terminal diagnostics/logging.
pub fn short_path(p: &Path, home: Option<&str>) -> String {
    let raw = clean_path_str(&p.to_string_lossy());
    let sanitized = sanitize_display(&raw);
    display_path(&sanitized, home)
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// Quote a string for POSIX sh / zsh / bash.
/// Safe unquoted characters omit `\` and `%` so paths are unambiguous across platforms.
pub fn quote_posix(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-:+@=".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Quote a program/argument for the host command runner (`cmd.exe`/Win32 on Windows, POSIX on Unix).
pub fn quote_cmd(s: &str) -> String {
    if cfg!(windows) {
        let plain = !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || r"\/_.-:+@=".contains(c));
        if plain {
            s.to_string()
        } else {
            format!("\"{}\"", s.replace('"', "\\\""))
        }
    } else {
        quote_posix(s)
    }
}

/// Quote a string for PowerShell (`Invoke-Expression` / `-Command`):
/// doubles ASCII `'` AND all four Unicode single-quote delimiters PowerShell recognizes.
pub fn quote_powershell(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => {
                out.push(c);
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Read a UTF-8 text file up to `max_bytes`, failing cleanly if larger or unreadable.
pub fn read_to_string_capped(path: &Path, max_bytes: usize) -> std::io::Result<String> {
    let f = std::fs::File::open(path)?;
    let cap = f
        .metadata()
        .ok()
        .map_or(0, |m| (m.len() as usize).min(max_bytes));
    let mut buf = Vec::with_capacity(cap.saturating_add(1));
    f.take((max_bytes + 1) as u64).read_to_end(&mut buf)?;
    if buf.len() > max_bytes {
        buf.truncate(max_bytes);
    }
    Ok(String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// Path of the running grace program, for configs that call it later
/// (CLI hooks, the hub tmux.conf).
///
/// A package manager can run grace from a folder that the next upgrade
/// deletes. Homebrew runs `/opt/homebrew/Cellar/grace/<version>/bin/grace`.
/// When a `grace` on PATH is the same file, this returns that stable path
/// (`/opt/homebrew/bin/grace`). Otherwise it returns the real path.
pub fn stable_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(stable_exe_in(&exe, std::env::var_os("PATH").as_deref()))
}

fn stable_exe_in(exe: &Path, path: Option<&std::ffi::OsStr>) -> PathBuf {
    let Ok(real) = std::fs::canonicalize(exe) else {
        return exe.to_path_buf();
    };
    let (Some(name), Some(path)) = (exe.file_name(), path) else {
        return real;
    };
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|cand| std::fs::canonicalize(cand).is_ok_and(|c| c == real))
        .unwrap_or(real)
}

/// "s" for a plural count, "" for exactly one: `format!("{n} run{}", plural(n))`.
pub fn plural(n: impl TryInto<i64>) -> &'static str {
    if n.try_into().ok() == Some(1) {
        ""
    } else {
        "s"
    }
}

/// Other `grace` programs on PATH: absolute paths, in PATH order, each a
/// different file from `exe` and from each other.
pub fn other_graces_on_path(exe: &str) -> Vec<String> {
    other_graces_in(exe, std::env::var_os("PATH").as_deref())
}

fn other_graces_in(exe: &str, path: Option<&std::ffi::OsStr>) -> Vec<String> {
    let name = if cfg!(windows) { "grace.exe" } else { "grace" };
    let mut seen: Vec<PathBuf> = std::fs::canonicalize(exe).into_iter().collect();
    let mut out = Vec::new();
    for dir in path.map(std::env::split_paths).into_iter().flatten() {
        let cand = dir.join(name);
        if !dir.is_absolute() || !cand.is_file() {
            continue;
        }
        let Ok(real) = std::fs::canonicalize(&cand) else {
            continue;
        };
        if !seen.contains(&real) {
            seen.push(real);
            out.push(clean_path_str(&cand.to_string_lossy()));
        }
    }
    out
}

/// Pre-resolved ignore matcher (`home` and canonicalized prefixes resolved once).
#[derive(Debug, Clone, Default)]
pub struct IgnoreMatcher {
    home_raw: Option<String>,
    home_canon: Option<String>,
    patterns: Vec<String>,
}

impl IgnoreMatcher {
    pub fn new(patterns: &[String]) -> Self {
        let home_raw = home_dir().map(|h| clean_path_str(&h.to_string_lossy()));
        let home_canon = home_raw
            .as_ref()
            .and_then(|h| std::fs::canonicalize(h).ok())
            .map(|c| clean_path_str(&c.to_string_lossy()))
            .filter(|ch| home_raw.as_deref() != Some(ch.as_str()));

        let mut expanded = Vec::with_capacity(patterns.len() * 2);
        for pat in patterns {
            let is_tilde = pat == "~" || pat.starts_with("~/") || pat.starts_with("~\\");
            if is_tilde {
                if let Some(hr) = home_raw.as_deref() {
                    let p = crate::config::expand_tilde(pat, Some(hr));
                    Self::push_with_canon_prefix(&mut expanded, p);
                } else {
                    Self::push_with_canon_prefix(&mut expanded, pat.clone());
                }
                if let Some(hc) = home_canon.as_deref() {
                    let p = crate::config::expand_tilde(pat, Some(hc));
                    Self::push_with_canon_prefix(&mut expanded, p);
                }
            } else {
                Self::push_with_canon_prefix(&mut expanded, pat.clone());
            }
        }
        Self {
            home_raw,
            home_canon,
            patterns: expanded,
        }
    }

    fn push_with_canon_prefix(out: &mut Vec<String>, pat: String) {
        let first_wild = pat.find(['*', '?']).unwrap_or(pat.len());
        let static_part = &pat[..first_wild];
        let prefix = if first_wild == pat.len() {
            static_part.trim_end_matches(['/', '\\'])
        } else {
            static_part
                .rfind(['/', '\\'])
                .map_or("", |idx| &static_part[..idx])
        };
        if !prefix.is_empty() {
            if let Ok(canon) = std::fs::canonicalize(prefix) {
                let canon_s = clean_path_str(&canon.to_string_lossy());
                if !same_path(&canon_s, prefix) {
                    let rest = &pat[prefix.len()..];
                    let alt = format!("{canon_s}{rest}");
                    if !out.contains(&alt) {
                        out.push(alt);
                    }
                }
            }
        }
        if !out.contains(&pat) {
            out.push(pat);
        }
    }

    pub fn is_ignored(&self, path: &str) -> bool {
        if path.is_empty() || has_line_break_or_nul(path) || is_root(path) {
            return true;
        }
        if let Some(hr) = &self.home_raw {
            if same_path(path, hr) {
                return true;
            }
        }
        if let Some(hc) = &self.home_canon {
            if same_path(path, hc) {
                return true;
            }
        }
        self.patterns
            .iter()
            .any(|pat| crate::config::glob_match(pat, path, None))
    }
}

/// Folders we never track: paths with control newlines/NUL, filesystem roots (`/`, `C:\`),
/// the home folder (canonical or raw), and user-configured ignore globs.
#[cfg(test)]
pub fn ignored_with_patterns(path: &str, patterns: &[String]) -> bool {
    IgnoreMatcher::new(patterns).is_ignored(path)
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

/// Suppress flashing a console window when spawning helper commands on Windows.
pub fn apply_no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// True when running inside Windows Subsystem for Linux.
pub fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WSL_DISTRO_NAME").is_some_and(|v| !v.is_empty())
            || std::env::var_os("WSL_INTEROP").is_some_and(|v| !v.is_empty()))
}

/// Open a file or folder with the system opener, in the background. False if it fails to start.
pub fn open_path(p: &Path) -> bool {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    #[cfg(windows)]
    {
        open_path_windows(&abs)
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg("--")
            .arg(&abs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if is_wsl() {
            if let Ok(mut child) = Command::new("wslview")
                .arg("--")
                .arg(&abs)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                let _ = child.try_wait();
                return true;
            }
            if let Ok(out) = Command::new("wslpath").arg("-w").arg(&abs).output() {
                if out.status.success() {
                    let wp = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if Command::new("explorer.exe")
                        .arg(wp)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .is_ok()
                    {
                        return true;
                    }
                }
            }
            return false;
        }
        let has_display = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        if !has_display {
            return false;
        }
        Command::new("xdg-open")
            .arg(&abs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = abs;
        false
    }
}

#[cfg(windows)]
fn open_path_windows(p: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let clean = clean_path_str(&p.to_string_lossy()).replace('/', "\\");
    if clean.is_empty() {
        return false;
    }
    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = std::ffi::OsStr::new(&clean)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: ShellExecuteW takes null-terminated UTF-16 strings and returns an HINSTANCE (> 32 on success).
    let res = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    (res as isize) > 32
}

/// Copy `text` to the system clipboard (native tool first, else OSC 52 on stderr/tty).
pub fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    let try_pipe = |prog: &str, args: &[&str]| -> bool {
        let mut cmd = Command::new(prog);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        apply_no_window(&mut cmd);
        let Ok(mut child) = cmd.spawn() else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        child.wait().is_ok_and(|s| s.success())
    };
    let ok = if cfg!(target_os = "macos") {
        try_pipe("pbcopy", &[])
    } else if cfg!(windows) {
        try_pipe("clip.exe", &[])
    } else if is_wsl() {
        try_pipe("clip.exe", &[])
            || try_pipe("wl-copy", &[])
            || try_pipe("xclip", &["-selection", "clipboard"])
    } else {
        try_pipe("wl-copy", &[])
            || try_pipe("xclip", &["-selection", "clipboard"])
            || try_pipe("xsel", &["--clipboard", "--input"])
    };
    if ok {
        return true;
    }
    // OSC 52 fallback (works over SSH and inside tmux with set-clipboard on).
    let b64 = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{b64}\x07");
    #[cfg(unix)]
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        if tty.write_all(seq.as_bytes()).is_ok() {
            let _ = tty.flush();
            return true;
        }
    }
    let mut err = std::io::stderr();
    err.write_all(seq.as_bytes()).is_ok() && err.flush().is_ok()
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Create a directory with mode `0700` on Unix.
pub fn create_dir_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !path.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Best-effort restrict file permissions to `0600` on Unix.
pub fn restrict_file_0600(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Identity and state of one process, read from the OS.
///
/// `start` is an opaque start-time token. It only compares against other
/// tokens from the same OS: microseconds since the epoch on macOS, clock
/// ticks since boot on Linux, and FILETIME units (100 ns) on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub ppid: i64,
    pub start: i64,
    /// The process exited, but its parent did not reap it yet.
    pub zombie: bool,
    /// The process belongs to our effective user. Always true on Windows
    /// when the process opens for query.
    pub mine: bool,
    pub name: String,
}

/// Ancestor names that stop the agent walk: we left the agent's process tree.
const PIN_DENY: &[&str] = &["launchd", "init", "systemd", "tmux", "sshd", "login"];

/// Ancestor names that the agent walk steps over: shells and small wrappers
/// between the agent and the hook.
const PIN_SHIMS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "ksh",
    "env",
    "nohup",
    "timeout",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "bash.exe",
    "sh.exe",
];

/// Most ancestor hops `agent_pin` follows.
const PIN_MAX_HOPS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinClass {
    Deny,
    Shim,
    Agent,
}

/// Basename (split on `/` and `\`), lowercase, without a leading `-` (login shells).
fn normalize_proc_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.trim_start_matches('-').to_ascii_lowercase()
}

/// Classify a name from `normalize_proc_name`.
fn pin_class(norm: &str) -> PinClass {
    if PIN_DENY.contains(&norm) {
        PinClass::Deny
    } else if PIN_SHIMS.contains(&norm) {
        PinClass::Shim
    } else {
        PinClass::Agent
    }
}

/// Walk up from `pid` (the parent of a process that started at `child_start`)
/// and return `(pid, start)` of the first ancestor that is not a shim.
/// `lookup` reads one process. Pure, so tests can feed a fake process table.
fn pin_walk(
    mut pid: i64,
    mut child_start: i64,
    lookup: impl Fn(i64) -> Option<ProcInfo>,
) -> Option<(i64, i64)> {
    for _ in 0..PIN_MAX_HOPS {
        if pid <= 1 {
            return None;
        }
        let info = lookup(pid)?;
        // A parent that started after its child is a reused PID.
        if !info.mine || info.zombie || info.start > child_start {
            return None;
        }
        match pin_class(&normalize_proc_name(&info.name)) {
            PinClass::Deny => return None,
            PinClass::Agent => return Some((pid, info.start)),
            PinClass::Shim => {
                child_start = info.start;
                pid = info.ppid;
            }
        }
    }
    None
}

/// Parse the text of `/proc/<pid>/stat`. The name sits between the first `(`
/// and the last `)`, so names with spaces or `)` parse correctly.
#[cfg(any(target_os = "linux", test))]
fn parse_linux_proc_stat(stat: &str, mine: bool) -> Option<ProcInfo> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    if close < open {
        return None;
    }
    let name = stat[open + 1..close].to_string();
    let fields: Vec<&str> = stat[close + 1..].split_ascii_whitespace().collect();
    let state = *fields.first()?;
    let ppid = fields.get(1)?.parse::<i64>().ok()?;
    let start = fields.get(19)?.parse::<i64>().ok()?;
    Some(ProcInfo {
        ppid,
        start,
        zombie: matches!(state, "Z" | "X"),
        mine,
        name,
    })
}

/// Bytes of a C name buffer up to the first NUL.
#[cfg(target_os = "macos")]
fn c_name(buf: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Read identity and state of a process. `None` when it does not exist or
/// the OS does not let us read it.
#[cfg(target_os = "macos")]
pub fn proc_info(pid: i64) -> Option<ProcInfo> {
    if pid <= 0 || pid > libc::c_int::MAX as i64 {
        return None;
    }
    // SAFETY: `proc_bsdinfo` is a plain C struct of integers and byte arrays,
    // so all-zero bytes are a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer points to a live `proc_bsdinfo` of exactly `size` bytes,
    // and the kernel writes at most `size` bytes into it.
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if ret != size {
        return None;
    }
    let start = (info.pbi_start_tvsec as i64)
        .saturating_mul(1_000_000)
        .saturating_add(info.pbi_start_tvusec as i64);
    let mut name = c_name(&info.pbi_name);
    if name.is_empty() {
        name = c_name(&info.pbi_comm);
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    Some(ProcInfo {
        ppid: info.pbi_ppid as i64,
        start,
        // SZOMB in <sys/proc.h>.
        zombie: info.pbi_status == 5,
        mine: info.pbi_uid == euid,
        name,
    })
}

/// Read identity and state of a process. `None` when it does not exist or
/// the OS does not let us read it.
#[cfg(target_os = "linux")]
pub fn proc_info(pid: i64) -> Option<ProcInfo> {
    use std::os::unix::fs::MetadataExt;
    if pid <= 0 || pid > i32::MAX as i64 {
        return None;
    }
    let stat = read_to_string_capped(Path::new(&format!("/proc/{pid}/stat")), 1024).ok()?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let mine = std::fs::metadata(format!("/proc/{pid}"))
        .ok()
        .is_some_and(|m| m.uid() == euid);
    parse_linux_proc_stat(&stat, mine)
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const STILL_ACTIVE: u32 = 259;

    /// A process handle that closes on drop.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful `OpenProcess` and is closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// `(running, creation_time)` of a process. `None` when it does not open.
    pub(super) fn query(pid: i64) -> Option<(bool, Option<i64>)> {
        if pid <= 0 || pid > u32::MAX as i64 {
            return None;
        }
        // SAFETY: plain Win32 call. A null result means failure.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32) };
        if h.is_null() {
            return None;
        }
        let h = Handle(h);
        let mut code: u32 = 0;
        // SAFETY: `h` is a valid process handle and `code` is a valid out pointer.
        let running = unsafe { GetExitCodeProcess(h.0, &mut code) } != 0 && code == STILL_ACTIVE;
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: `h` is a valid process handle and all four out pointers are valid.
        let ok = unsafe { GetProcessTimes(h.0, &mut created, &mut exited, &mut kernel, &mut user) };
        let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        let start = (ok != 0).then_some(ticks as i64);
        Some((running, start))
    }

    /// `pid -> (ppid, exe name)` for every process, from one ToolHelp snapshot.
    pub(super) fn process_table() -> std::collections::HashMap<i64, (i64, String)> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        let mut out = std::collections::HashMap::new();
        // SAFETY: plain Win32 call. `INVALID_HANDLE_VALUE` means failure.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap == INVALID_HANDLE_VALUE || snap.is_null() {
            return out;
        }
        let snap = Handle(snap);
        // SAFETY: `PROCESSENTRY32W` is a plain C struct, so all-zero bytes are valid.
        let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: `snap` is a valid snapshot handle and `e` has `dwSize` set.
        let mut more = unsafe { Process32FirstW(snap.0, &mut e) } != 0;
        while more {
            let len = e
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]);
            out.insert(e.th32ProcessID as i64, (e.th32ParentProcessID as i64, name));
            // SAFETY: same handle and buffer as above.
            more = unsafe { Process32NextW(snap.0, &mut e) } != 0;
        }
        out
    }
}

/// Read identity and state of a process. `None` when it does not open.
/// Takes a ToolHelp snapshot for `ppid` and `name`, so it costs a few ms.
#[cfg(windows)]
pub fn proc_info(pid: i64) -> Option<ProcInfo> {
    let (running, start) = win::query(pid)?;
    let start = start?;
    let (ppid, name) = win::process_table()
        .remove(&pid)
        .unwrap_or((0, String::new()));
    Some(ProcInfo {
        ppid,
        start,
        zombie: !running,
        mine: true,
        name,
    })
}

/// Read identity and state of a process. Not supported on this OS.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn proc_info(_pid: i64) -> Option<ProcInfo> {
    None
}

/// Start-time token of a process, as `ProcInfo::start`. On Windows this
/// skips the ToolHelp snapshot that `proc_info` takes.
pub fn proc_start(pid: i64) -> Option<i64> {
    #[cfg(windows)]
    {
        match win::query(pid)? {
            (true, start) => start,
            (false, _) => None,
        }
    }
    #[cfg(not(windows))]
    {
        proc_info(pid).filter(|i| !i.zombie).map(|i| i.start)
    }
}

/// True if the process with this PID exists, belongs to us and is not a zombie.
/// With `expected_start`, the process must also have that start time, so a
/// reused PID counts as dead.
pub fn pid_alive_at(pid: i64, expected_start: Option<i64>) -> bool {
    if pid <= 0 {
        return false;
    }
    match expected_start {
        Some(exp) => alive_with_start(pid, exp),
        None => pid_exists(pid),
    }
}

/// True if a process with this PID exists and belongs to us (not a zombie).
/// Same as `pid_alive_at(pid, None)`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn pid_alive(pid: i64) -> bool {
    pid_alive_at(pid, None)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn alive_with_start(pid: i64, exp: i64) -> bool {
    proc_info(pid).is_some_and(|i| !i.zombie && i.mine && i.start == exp)
}

#[cfg(windows)]
fn alive_with_start(pid: i64, exp: i64) -> bool {
    matches!(win::query(pid), Some((true, Some(start))) if start == exp)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn alive_with_start(pid: i64, _exp: i64) -> bool {
    pid_exists(pid)
}

#[cfg(unix)]
fn pid_exists(pid: i64) -> bool {
    if pid <= 0 || pid > i32::MAX as i64 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists and is signalable by our UID.
    // EPERM means another user recycled this PID (not our shell), so treat as dead.
    let exists = unsafe { libc::kill(pid as i32, 0) == 0 };
    if !exists {
        return false;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if proc_info(pid).is_some_and(|i| i.zombie) {
        return false;
    }
    true
}

#[cfg(windows)]
fn pid_exists(pid: i64) -> bool {
    matches!(win::query(pid), Some((true, _)))
}

/// Unknown platform: assume the process exists.
#[cfg(not(any(unix, windows)))]
fn pid_exists(_pid: i64) -> bool {
    true
}

/// PID and start time of the agent CLI that runs this process: the first
/// ancestor that is not a shell or wrapper. Hooks call this to pin a session
/// to its agent. `None` when the walk reaches a deny-listed process
/// (launchd, tmux, sshd...), a process of another user, or 6 hops.
#[cfg(not(windows))]
pub fn agent_pin() -> Option<(i64, i64)> {
    let me = proc_info(std::process::id() as i64)?;
    pin_walk(me.ppid, me.start, proc_info)
}

/// PID and start time of the agent CLI that runs this process. One ToolHelp
/// snapshot gives the tree; each hop checks that the parent started before
/// its child, so a reused parent PID stops the walk.
#[cfg(windows)]
pub fn agent_pin() -> Option<(i64, i64)> {
    let table = win::process_table();
    let me = std::process::id() as i64;
    let my_start = win::query(me)?.1?;
    let my_ppid = table.get(&me)?.0;
    pin_walk(my_ppid, my_start, |pid| {
        let (ppid, name) = table.get(&pid)?.clone();
        let (running, start) = win::query(pid)?;
        Some(ProcInfo {
            ppid,
            start: start?,
            zombie: !running,
            mine: true,
            name,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists other grace files on PATH once each, skipping the running one,
    /// links to it, relative dirs and missing dirs.
    #[cfg(unix)]
    #[test]
    fn other_graces_skip_self_and_duplicates() {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap();
        let (dev, brew, link, cargo) = (
            root.join("dev"),
            root.join("brew"),
            root.join("link"),
            root.join("cargo"),
        );
        for d in [&dev, &brew, &link, &cargo] {
            std::fs::create_dir_all(d).unwrap();
        }
        let me = dev.join("grace");
        std::fs::write(&me, "").unwrap();
        std::fs::write(brew.join("grace"), "").unwrap();
        std::os::unix::fs::symlink(&me, link.join("grace")).unwrap();
        std::os::unix::fs::symlink(brew.join("grace"), cargo.join("grace")).unwrap();
        let path = std::env::join_paths([
            PathBuf::from("rel"),
            link.clone(),
            dev.clone(),
            root.join("missing"),
            brew.clone(),
            cargo.clone(),
        ])
        .unwrap();
        let got = other_graces_in(&me.to_string_lossy(), Some(&path));
        assert_eq!(got, vec![brew.join("grace").to_string_lossy().into_owned()]);
        assert!(other_graces_in(&me.to_string_lossy(), None).is_empty());
    }

    /// Homebrew layout: the program lives in a versioned Cellar folder and
    /// bin/grace links to it. The link on PATH wins over the Cellar path.
    #[cfg(unix)]
    #[test]
    fn stable_exe_prefers_link_on_path() {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap();
        let cellar = root.join("Cellar/grace/2.0.1/bin");
        let bin = root.join("bin");
        let other = root.join("other");
        for d in [&cellar, &bin, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let real = cellar.join("grace");
        std::fs::write(&real, "").unwrap();
        std::os::unix::fs::symlink("../Cellar/grace/2.0.1/bin/grace", bin.join("grace")).unwrap();
        // A different file with the same name earlier on PATH is skipped.
        std::fs::write(other.join("grace"), "").unwrap();
        let path = std::env::join_paths([
            "relative/bin".into(),
            other.clone(),
            root.join("missing"),
            bin.clone(),
        ])
        .unwrap();
        assert_eq!(stable_exe_in(&real, Some(&path)), bin.join("grace"));
        // Not on PATH: the real path, as before.
        let only_other = std::env::join_paths([other]).unwrap();
        assert_eq!(stable_exe_in(&real, Some(&only_other)), real);
        assert_eq!(stable_exe_in(&real, None), real);
        // Started through the link: still the link, not the Cellar path.
        assert_eq!(
            stable_exe_in(&bin.join("grace"), Some(&path)),
            bin.join("grace")
        );
    }

    #[test]
    fn pid_liveness() {
        assert!(pid_alive(std::process::id() as i64));
        assert!(!pid_alive(999_999_999));
        assert!(!pid_alive(0));
        assert!(!pid_alive(-5));
        assert!(!pid_alive_at(0, Some(1)));
        assert!(!pid_alive_at(-5, None));
        assert!(!pid_alive_at(999_999_999, Some(1)));
    }

    /// The running test reads its own identity, and only its own start time
    /// counts as alive.
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn self_proc_info_and_start_check() {
        let me = std::process::id() as i64;
        let info = proc_info(me).expect("proc_info(self)");
        assert!(!info.zombie);
        assert!(info.mine);
        assert!(info.start > 0);
        assert!(!info.name.is_empty());
        #[cfg(unix)]
        assert_eq!(info.ppid, std::os::unix::process::parent_id() as i64);
        assert_eq!(proc_start(me), Some(info.start));
        assert!(pid_alive_at(me, Some(info.start)));
        assert!(!pid_alive_at(me, Some(info.start + 1)));
        assert!(pid_alive_at(me, None));
        // The parent started before us.
        let parent = proc_info(info.ppid).expect("proc_info(parent)");
        assert!(parent.start <= info.start);
        assert!(proc_info(999_999_999).is_none());
    }

    #[test]
    fn proc_name_normalization_and_classes() {
        assert_eq!(normalize_proc_name("-zsh"), "zsh");
        assert_eq!(normalize_proc_name("/bin/Bash"), "bash");
        assert_eq!(
            normalize_proc_name(r"C:\Windows\System32\cmd.exe"),
            "cmd.exe"
        );
        assert_eq!(normalize_proc_name("PwSh.EXE"), "pwsh.exe");
        assert_eq!(normalize_proc_name("node"), "node");
        for n in ["launchd", "init", "systemd", "tmux", "sshd", "login"] {
            assert_eq!(pin_class(n), PinClass::Deny, "{n}");
        }
        for n in [
            "sh", "bash", "zsh", "fish", "env", "nohup", "timeout", "cmd.exe", "pwsh.exe",
        ] {
            assert_eq!(pin_class(n), PinClass::Shim, "{n}");
        }
        for n in [
            "node", "bun", "python", "python3", "claude", "codex", "node.exe",
        ] {
            assert_eq!(pin_class(n), PinClass::Agent, "{n}");
        }
    }

    fn fake(ppid: i64, start: i64, name: &str) -> ProcInfo {
        ProcInfo {
            ppid,
            start,
            zombie: false,
            mine: true,
            name: name.into(),
        }
    }

    #[test]
    fn pin_walk_skips_shims_and_stops_at_deny() {
        use std::collections::HashMap;
        let table: HashMap<i64, ProcInfo> = [
            (1, fake(0, 1, "launchd")),
            (10, fake(1, 10, "login")),
            (20, fake(10, 20, "-zsh")),
            (30, fake(20, 30, "node")),
            (40, fake(30, 40, "/bin/sh")),
            (50, fake(40, 50, "env")),
        ]
        .into_iter()
        .collect();
        let look = |pid: i64| table.get(&pid).cloned();
        // hook (start 60) -> env -> sh -> node: pins node.
        assert_eq!(pin_walk(50, 60, look), Some((30, 30)));
        // From zsh: login is on the deny list.
        assert_eq!(pin_walk(20, 25, look), None);
        // PID 1 and below stop the walk.
        assert_eq!(pin_walk(1, 5, look), None);
        assert_eq!(pin_walk(0, 5, look), None);
        // Unknown parent.
        assert_eq!(pin_walk(99, 100, look), None);
        // A parent that started after its child is a reused PID.
        assert_eq!(pin_walk(30, 29, look), None);
    }

    #[test]
    fn pin_walk_rejects_foreign_zombie_and_deep_chains() {
        let foreign = |_: i64| {
            Some(ProcInfo {
                mine: false,
                ..fake(1, 1, "node")
            })
        };
        assert_eq!(pin_walk(5, 10, foreign), None);
        let zombie = |_: i64| {
            Some(ProcInfo {
                zombie: true,
                ..fake(1, 1, "node")
            })
        };
        assert_eq!(pin_walk(5, 10, zombie), None);
        // Shells all the way up: the walk stops after 6 hops.
        let shells = |pid: i64| Some(fake(pid - 1, pid, "bash"));
        assert_eq!(pin_walk(100, 1000, shells), None);
        // Five shells, then an agent: within 6 hops.
        let near = |pid: i64| Some(fake(pid - 1, pid, if pid == 95 { "claude" } else { "sh" }));
        assert_eq!(pin_walk(100, 1000, near), Some((95, 95)));
    }

    #[test]
    fn linux_proc_stat_parser() {
        let tail = |state: &str| {
            let mut f = vec![state.to_string(), "4242".into()];
            f.extend((2..19).map(|i| i.to_string()));
            f.push("987654".into());
            f.extend(["0".into(), "1".into()]);
            f.join(" ")
        };
        let s = format!("123 (my prog) x) {}\n", tail("S"));
        let p = parse_linux_proc_stat(&s, true).unwrap();
        assert_eq!(
            p,
            ProcInfo {
                ppid: 4242,
                start: 987654,
                zombie: false,
                mine: true,
                name: "my prog) x".into(),
            }
        );
        let z = parse_linux_proc_stat(&format!("7 (sh) {}", tail("Z")), false).unwrap();
        assert!(z.zombie && !z.mine);
        assert!(
            parse_linux_proc_stat(&format!("7 (sh) {}", tail("X")), true)
                .unwrap()
                .zombie
        );
        assert!(parse_linux_proc_stat("7 (sh) S 1 2", true).is_none());
        assert!(parse_linux_proc_stat("garbage", true).is_none());
        assert!(parse_linux_proc_stat(") 7 (", true).is_none());
    }

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
        assert!(is_root(r"\\srv\share"));
        assert!(is_root(r"\\srv\share\"));
        assert!(!is_root(r"\\srv\share\sub"));
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

    #[test]
    fn shell_quoting_safety() {
        assert_eq!(quote_posix("/a/b"), "/a/b");
        assert_eq!(quote_posix(r"C:\a\b"), r"'C:\a\b'");
        assert_eq!(quote_posix("/a b'c"), r"'/a b'\''c'");
        assert_eq!(quote_powershell("a'b\u{2019}c"), "'a''b\u{2019}\u{2019}c'");
    }

    #[test]
    fn display_sanitizes_controls_and_bidi() {
        assert_eq!(sanitize_display("a\x1b[31mR\rC\x07\tx"), "a?[31mR?C?    x");
        assert_eq!(sanitize_display("foo\u{202E}bar\u{2066}baz"), "foo?bar?baz");
        assert!(has_line_break_or_nul("/tmp/a\nb"));
        assert!(ignored_with_patterns("/tmp/a\nb", &[]));
        assert!(ignored_with_patterns("/tmp/foo", &["/tmp/*".into()]));
    }

    #[cfg(unix)]
    #[test]
    fn ignore_matcher_handles_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        let link = d.path().join("link");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let pat = format!("{}/*", link.to_string_lossy());
        let matcher = IgnoreMatcher::new(&[pat]);
        let target_canon = format!("{}/proj", normalize(&real));
        assert!(matcher.is_ignored(&target_canon));
    }
}
