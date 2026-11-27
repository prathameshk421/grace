//! Live editor state: which folders have an editor window open right now, and a cheap
//! background refresh of editor history so folders opened from File > Open show up.
use crate::db::Db;
use crate::import_editors::{config_root, uri_to_path, workspace_dir_from_cfg};
use crate::platform::{self, IgnoreMatcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MAX_STORAGE_JSON_BYTES: usize = 8 << 20;

/// One folder with an editor window open right now.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenEditor {
    /// Absolute folder path (normalized like other grace paths).
    pub path: String,
    /// grace tag of the editor, e.g. "vscode", "antigravity", "cursor".
    pub tag: String,
    /// Executable program name or path to launch/focus this editor directly.
    pub prog: String,
    /// Direct arguments to pass to `prog` (never shell-evaluated).
    pub args: Vec<String>,
}

/// One editor of the VS Code family.
pub(crate) struct App {
    pub(crate) source: &'static str,
    /// Folder name in the app data root, also the last part of `--user-data-dir`.
    pub(crate) dir: &'static str,
    pub(crate) tag: &'static str,
    /// CLI command that opens a folder.
    cli: &'static str,
    /// macOS bundle name without `.app`.
    bundle: &'static str,
    /// Linux binary name.
    bin: &'static str,
    /// Windows executable name.
    exe: &'static str,
}

pub(crate) const APPS: &[App] = &[
    App {
        source: "vscode",
        dir: "Code",
        tag: "vscode",
        cli: "code",
        bundle: "Visual Studio Code",
        bin: "code",
        exe: "Code.exe",
    },
    App {
        source: "vscode",
        dir: "Code - Insiders",
        tag: "vscode",
        cli: "code-insiders",
        bundle: "Visual Studio Code - Insiders",
        bin: "code-insiders",
        exe: "Code - Insiders.exe",
    },
    App {
        source: "vscode",
        dir: "VSCodium",
        tag: "vscodium",
        cli: "codium",
        bundle: "VSCodium",
        bin: "codium",
        exe: "VSCodium.exe",
    },
    App {
        source: "cursor",
        dir: "Cursor",
        tag: "cursor",
        cli: "cursor",
        bundle: "Cursor",
        bin: "cursor",
        exe: "Cursor.exe",
    },
    App {
        source: "windsurf",
        dir: "Windsurf",
        tag: "windsurf",
        cli: "windsurf",
        bundle: "Windsurf",
        bin: "windsurf",
        exe: "Windsurf.exe",
    },
    App {
        source: "antigravity",
        dir: "Antigravity",
        tag: "antigravity",
        cli: "antigravity",
        bundle: "Antigravity",
        bin: "antigravity",
        exe: "Antigravity.exe",
    },
    App {
        source: "antigravity",
        dir: "Antigravity IDE",
        tag: "antigravity",
        cli: "antigravity",
        bundle: "Antigravity IDE",
        bin: "antigravity-ide",
        exe: "Antigravity.exe",
    },
];

/// Meta key with the Unix time of the last background editor import.
const IMPORTED_AT: &str = "editors_imported_at";
/// Seconds before editor history counts as stale.
const STALE_SECS: i64 = 600;

fn app(dir: &str) -> Option<&'static App> {
    APPS.iter().find(|a| a.dir == dir)
}

// ---------- pure helpers ----------

/// Folder paths of open windows in a `storage.json` text, in order, without duplicates.
/// Workspace windows give the folder of the `.code-workspace` file.
/// Empty windows, remote windows and untitled workspaces are skipped.
pub fn parse_open_folders(json: &str, windows: bool) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(state) = v.get("windowsState") else {
        return Vec::new();
    };
    let mut wins: Vec<&serde_json::Value> = Vec::new();
    if let Some(w) = state.get("lastActiveWindow") {
        wins.push(w);
    }
    if let Some(a) = state.get("openedWindows").and_then(|a| a.as_array()) {
        wins.extend(a);
    }
    let mut out: Vec<String> = Vec::new();
    for w in wins {
        let Some(p) = window_folder(w, windows) else {
            continue;
        };
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn window_folder(w: &serde_json::Value, windows: bool) -> Option<String> {
    let s = |k: &str| w.get(k).and_then(|x| x.as_str());
    if let Some(u) = s("folder").or_else(|| s("folderUri")) {
        return uri_to_path(u, windows);
    }
    let ws = w.get("workspaceIdentifier")?;
    let cfg = ws
        .get("configURIPath")
        .or_else(|| ws.get("configPath"))?
        .as_str()?;
    workspace_dir_from_cfg(cfg, windows)
}

/// Last path segment of a `/` or `\` separated string.
fn base_name(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// The outer bundle (`/x/Name.app`) from `argv[0]` only (never arbitrary flag arguments).
fn bundle_of(cmd: &str) -> Option<&str> {
    let i = cmd.find(".app/")?;
    let head = &cmd[..i + 4];
    if head.contains(" --") {
        return None;
    }
    Some(head)
}

/// Map process lines to running apps: `app data folder name -> bundle path (macOS)`.
/// Unix lines are `pgrep` / `/proc` output (`PID command args`).
/// Windows lines are executable names or `tasklist /fo csv /nh` output.
pub fn parse_running(out: &str, windows: bool) -> HashMap<&'static str, Option<String>> {
    let mut found: HashMap<&'static str, Option<String>> = HashMap::new();
    let mut udd_bundles: HashSet<String> = HashSet::new();
    let mut main: Vec<(&'static App, Option<String>)> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if windows {
            let exe = line.trim_start_matches('"').split('"').next().unwrap_or("");
            for a in APPS.iter().filter(|a| a.exe.eq_ignore_ascii_case(exe)) {
                found.entry(a.dir).or_insert(None);
            }
            continue;
        }
        let cmd = match line.split_once(' ') {
            Some((pid, rest)) if pid.chars().all(|c| c.is_ascii_digit()) => rest.trim_start(),
            _ => line,
        };
        let bundle = bundle_of(cmd).map(String::from);
        if let Some(i) = cmd.find("--user-data-dir=") {
            let v = &cmd[i + "--user-data-dir=".len()..];
            let v = v.split(" --").next().unwrap_or(v).trim().trim_matches('"');
            if let Some(a) = app(base_name(v.trim_end_matches(['/', '\\']))) {
                if let Some(b) = &bundle {
                    udd_bundles.insert(b.clone());
                }
                let e = found.entry(a.dir).or_insert(None);
                if e.is_none() {
                    *e = bundle;
                }
            }
            continue;
        }
        let hit = match &bundle {
            Some(b) => {
                let name = base_name(b).trim_end_matches(".app");
                APPS.iter().find(|a| a.bundle == name)
            }
            None => {
                let exe = base_name(cmd.split(' ').next().unwrap_or(cmd));
                APPS.iter().find(|a| a.bin == exe)
            }
        };
        if let Some(a) = hit {
            main.push((a, bundle));
        }
    }
    for (a, bundle) in main {
        if bundle.as_ref().is_some_and(|b| udd_bundles.contains(b)) {
            continue;
        }
        let e = found.entry(a.dir).or_insert(None);
        if e.is_none() {
            *e = bundle;
        }
    }
    found
}

/// True when the last import time is missing, bad, 10 minutes old or more, or in the future.
pub fn is_stale(last: Option<&str>, now: i64) -> bool {
    match last.and_then(|s| s.trim().parse::<i64>().ok()) {
        Some(t) => now.saturating_sub(t) >= STALE_SECS || t > now,
        None => true,
    }
}

// ---------- system ----------

#[cfg(all(unix, not(target_os = "linux")))]
fn pgrep_pattern() -> String {
    let dirs: Vec<&str> = APPS.iter().map(|a| a.dir).collect();
    let mut pat = format!("user-data-dir=.*/({})( --|$)", dirs.join("|"));
    if cfg!(target_os = "macos") {
        let bundles: Vec<&str> = APPS.iter().map(|a| a.bundle).collect();
        pat.push_str(&format!("|/({})\\.app/Contents/MacOS/", bundles.join("|")));
    } else {
        let bins: Vec<&str> = APPS.iter().map(|a| a.bin).collect();
        pat.push_str(&format!("|^(.*/)?({})( |$)", bins.join("|")));
    }
    pat
}

/// Fast OS-native process listing filtered to the current user where applicable.
fn process_listing() -> String {
    #[cfg(target_os = "linux")]
    {
        linux_proc_listing()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::process::{Command, Stdio};
        // SAFETY: geteuid is always safe and returns the effective user ID.
        let uid = unsafe { libc::geteuid() }.to_string();
        match Command::new("pgrep")
            .args(["-u", &uid, "-lf"])
            .arg(pgrep_pattern())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            Ok(o) => String::from_utf8(o.stdout)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
            Err(_) => String::new(),
        }
    }
    #[cfg(windows)]
    {
        windows_process_listing()
    }
    #[cfg(not(any(unix, windows)))]
    {
        String::new()
    }
}

#[cfg(target_os = "linux")]
fn linux_proc_listing() -> String {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid is always safe.
    let euid = unsafe { libc::geteuid() };
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return String::new();
    };
    let mut out = String::new();
    for entry in rd.flatten() {
        let fname = entry.file_name();
        let pid_str = fname.to_string_lossy();
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let p = entry.path();
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        if meta.uid() != euid {
            continue;
        }
        let Ok(bytes) = std::fs::read(p.join("cmdline")) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        out.push_str(&pid_str);
        for part in bytes.split(|&b| b == 0).filter(|s| !s.is_empty()) {
            out.push(' ');
            out.push_str(&String::from_utf8_lossy(part));
        }
        out.push('\n');
    }
    out
}

#[cfg(windows)]
fn windows_process_listing() -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out = String::new();
    // SAFETY: Standard ToolHelp32 process snapshot; handle is closed before return.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if !name.is_empty() {
                    out.push_str(&name);
                    out.push('\n');
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Validate that a discovered macOS `.app` bundle is safe to execute/open.
#[cfg(target_os = "macos")]
pub(crate) fn trusted_bundle(bundle: &str) -> bool {
    trusted_bundle_with_home(bundle, platform::home_dir().as_deref())
}

#[cfg(target_os = "macos")]
fn trusted_bundle_with_home(bundle: &str, home: Option<&Path>) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let p = Path::new(bundle);
    if !p.is_absolute() {
        return false;
    }
    let Ok(lmeta) = std::fs::symlink_metadata(p) else {
        return false;
    };
    if lmeta.file_type().is_symlink() || !lmeta.is_dir() {
        return false;
    }
    let home_apps = home.map(|h| h.join("Applications"));
    let in_trusted_root = |cand: &Path| -> bool {
        cand.starts_with("/Applications/")
            || cand.starts_with("/System/Applications/")
            || home_apps.as_ref().is_some_and(|ha| {
                let ha_canon = std::fs::canonicalize(ha).unwrap_or_else(|_| ha.clone());
                cand.starts_with(ha) || cand.starts_with(&ha_canon)
            })
    };
    if !in_trusted_root(p) {
        return false;
    }
    // SAFETY: geteuid is always safe.
    let euid = unsafe { libc::geteuid() };
    let safe_meta = |m: &std::fs::Metadata| {
        let owner_ok = m.uid() == 0 || m.uid() == euid;
        let not_world_writable = m.permissions().mode() & 0o002 == 0;
        owner_ok && not_world_writable
    };
    if !safe_meta(&lmeta) {
        return false;
    }
    if let Ok(canon) = std::fs::canonicalize(p) {
        if !canon.is_dir() || !in_trusted_root(&canon) {
            return false;
        }
        let Ok(cmeta) = std::fs::metadata(&canon) else {
            return false;
        };
        if !safe_meta(&cmeta) {
            return false;
        }
    }
    true
}

enum LaunchMode {
    Cli(&'static str),
    #[cfg(target_os = "macos")]
    MacBin(String),
    #[cfg(target_os = "macos")]
    MacOpen(String),
}

fn resolve_launch(app: &App, bundle: Option<&str>, cli_on_path: bool) -> LaunchMode {
    if cli_on_path || !cfg!(target_os = "macos") {
        return LaunchMode::Cli(app.cli);
    }
    #[cfg(target_os = "macos")]
    {
        let bundle_path = bundle
            .filter(|b| trusted_bundle(b))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/Applications/{}.app", app.bundle)));
        let cli = bundle_path.join("Contents/Resources/app/bin").join(app.cli);
        if cli.is_file() {
            return LaunchMode::MacBin(cli.to_string_lossy().into_owned());
        }
        let target = if bundle_path.is_dir() {
            bundle_path.to_string_lossy().into_owned()
        } else {
            app.bundle.to_string()
        };
        LaunchMode::MacOpen(target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle;
        LaunchMode::Cli(app.cli)
    }
}

fn build_open_target(mode: &LaunchMode, path: &str) -> (String, Vec<String>) {
    match mode {
        LaunchMode::Cli(cli) => ((*cli).to_string(), vec![path.to_string()]),
        #[cfg(target_os = "macos")]
        LaunchMode::MacBin(prog) => (prog.clone(), vec![path.to_string()]),
        #[cfg(target_os = "macos")]
        LaunchMode::MacOpen(target) => (
            "open".to_string(),
            vec!["-a".into(), target.clone(), path.to_string()],
        ),
    }
}

fn storage_files_for(root: &Path, a: &App) -> Vec<PathBuf> {
    #[allow(unused_mut)]
    let mut files = vec![root.join(a.dir).join("User/globalStorage/storage.json")];
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(home) = platform::home_dir() {
        for &(src, app_id, dir_name, tag) in crate::import_editors::FLATPAK_APPS {
            if src == a.source && tag == a.tag {
                let f = home
                    .join(".var/app")
                    .join(app_id)
                    .join("config")
                    .join(dir_name)
                    .join("User/globalStorage/storage.json");
                if !files.contains(&f) {
                    files.push(f);
                }
            }
        }
    }
    files
}

/// Folders with an editor window open now.
pub fn open_editors(ignore: &IgnoreMatcher) -> Vec<OpenEditor> {
    let running = parse_running(&process_listing(), cfg!(windows));
    if running.is_empty() {
        return Vec::new();
    }
    let Some(root) = config_root() else {
        return Vec::new();
    };
    let mut out: Vec<OpenEditor> = Vec::new();
    let mut cli_cache: HashMap<&str, bool> = HashMap::new();
    for a in APPS {
        let Some(bundle) = running.get(a.dir) else {
            continue;
        };
        let mut texts = Vec::new();
        for file in storage_files_for(&root, a) {
            if let Ok(text) = platform::read_to_string_capped(&file, MAX_STORAGE_JSON_BYTES) {
                texts.push(text);
            }
        }
        if texts.is_empty() {
            continue;
        }
        let cli = *cli_cache
            .entry(a.cli)
            .or_insert_with(|| crate::tools::on_path(a.cli));
        let launch_mode = resolve_launch(a, bundle.as_deref(), cli);
        for text in texts {
            for p in parse_open_folders(&text, cfg!(windows)) {
                if platform::has_line_break_or_nul(&p) {
                    continue;
                }
                let dir = Path::new(&p);
                if !dir.is_dir() {
                    continue;
                }
                let path = platform::normalize(dir);
                if ignore.is_ignored(&path) || out.iter().any(|e| e.tag == a.tag && e.path == path)
                {
                    continue;
                }
                let (prog, args) = build_open_target(&launch_mode, &path);
                out.push(OpenEditor {
                    path,
                    tag: a.tag.to_string(),
                    prog,
                    args,
                });
            }
        }
    }
    out
}

/// Re-import editor history using pre-collected open editors when available.
/// Returns `true` if any new folder entries were added to the database.
pub fn refresh_if_stale_with(
    db: &Db,
    already_open: Option<&[OpenEditor]>,
    ignore: &IgnoreMatcher,
) -> bool {
    let now = crate::model::now();
    let owned;
    let open = match already_open {
        Some(list) => list,
        None => {
            owned = open_editors(ignore);
            &owned
        }
    };
    let pairs: Vec<(&str, &str)> = open
        .iter()
        .map(|e| (e.path.as_str(), e.tag.as_str()))
        .collect();
    let mut added = db.import_open_editors_batch(&pairs, now).unwrap_or(0);
    let last = db.get_meta(IMPORTED_AT).ok().flatten();
    if !is_stale(last.as_deref(), now) {
        return added > 0;
    }
    if db.set_meta(IMPORTED_AT, &now.to_string()).is_err() {
        return added > 0;
    }
    added += crate::import_editors::import_editors_quiet(db, ignore);
    added > 0
}

/// Record that the initial import ran so we do not immediately re-scan on the next command.
pub fn mark_imported_now(db: &Db) {
    let _ = db.set_meta(IMPORTED_AT, &crate::model::now().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_storage_json() {
        let j = r#"{"theme":"x","windowsState":{
            "lastActiveWindow":{"folder":"file:///a/last","backupPath":"/b/1"},
            "openedWindows":[
                {"folder":"file:///a/one%20two","backupPath":"/b/2"},
                {"folder":"file:///a/last"},
                {"workspaceIdentifier":{"id":"w","configURIPath":"file:///ws/proj/x.code-workspace"}},
                {"workspaceIdentifier":{"id":"u","configURIPath":"file:///data/Code/Workspaces/1/workspace.json"}},
                {"backupPath":"/b/empty"},
                {"folder":"vscode-remote://ssh-remote%2Bhost/home/x"}
            ]}}"#;
        assert_eq!(
            parse_open_folders(j, false),
            vec!["/a/last", "/a/one two", "/ws/proj"]
        );
        assert!(parse_open_folders("{}", false).is_empty());
        assert!(parse_open_folders("garbage", false).is_empty());
        let empty =
            r#"{"windowsState":{"lastActiveWindow":{"backupPath":"/b"},"openedWindows":[]}}"#;
        assert!(parse_open_folders(empty, false).is_empty());
    }

    #[test]
    fn maps_mac_processes() {
        let out = "\
12128 /Applications/Visual Studio Code.app/Contents/MacOS/Code
12132 /Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper --type=gpu-process --user-data-dir=/Users/me/Library/Application Support/Code --gpu-preferences=x
500 /Applications/Antigravity.app/Contents/MacOS/Electron
501 /Applications/Antigravity.app/Contents/Frameworks/X Helper.app/Contents/MacOS/X Helper --user-data-dir=/Users/me/Library/Application Support/Antigravity IDE
600 /Users/me/Applications/Cursor.app/Contents/MacOS/Cursor
700 /Applications/Other.app/Contents/MacOS/Other --user-data-dir=/x/Brave
";
        let r = parse_running(out, false);
        let mut names: Vec<&str> = r.keys().copied().collect();
        names.sort();
        assert_eq!(names, vec!["Antigravity IDE", "Code", "Cursor"]);
        assert_eq!(
            r["Code"].as_deref(),
            Some("/Applications/Visual Studio Code.app")
        );
        assert_eq!(
            r["Cursor"].as_deref(),
            Some("/Users/me/Applications/Cursor.app")
        );
        assert!(parse_running("", false).is_empty());
        assert_eq!(bundle_of("foo --arg=/tmp/Fake.app/Contents/MacOS/x"), None);
    }

    #[test]
    fn maps_linux_and_windows_processes() {
        let out = "10 /usr/share/code/code\n\
                   11 /usr/share/code/code --type=zygote\n\
                   12 /opt/cursor/cursor --type=renderer --user-data-dir=/home/me/.config/Cursor --x\n";
        let r = parse_running(out, false);
        let mut names: Vec<&str> = r.keys().copied().collect();
        names.sort();
        assert_eq!(names, vec!["Code", "Cursor"]);
        let win = "\"Code.exe\",\"1\",\"Console\",\"1\",\"1 K\"\n\"explorer.exe\",\"2\",\"Console\",\"1\",\"1 K\"\n";
        let r = parse_running(win, true);
        assert_eq!(r.len(), 1);
        assert!(r.contains_key("Code"));
    }

    #[test]
    fn staleness_rule() {
        assert!(is_stale(None, 1000));
        assert!(is_stale(Some("bad"), 1000));
        assert!(!is_stale(Some("1000"), 1000));
        assert!(!is_stale(Some("500"), 1099));
        assert!(is_stale(Some("400"), 1000));
        assert!(is_stale(Some("2000"), 1000));
    }

    #[test]
    fn builds_open_command() {
        let a = app("Code").unwrap();
        let mode = resolve_launch(a, None, true);
        let (prog, args) = build_open_target(&mode, "/a b");
        assert_eq!(prog, "code");
        assert_eq!(args, vec!["/a b"]);
    }

    #[test]
    fn refresh_claims_the_run() {
        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("g.db")).unwrap();
        db.set_meta(IMPORTED_AT, "0").unwrap();
        refresh_if_stale_with(&db, Some(&[]), &IgnoreMatcher::default());
        let t: i64 = db.get_meta(IMPORTED_AT).unwrap().unwrap().parse().unwrap();
        assert!(crate::model::now() - t < 60);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn trusted_bundle_rejects_untrusted_and_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("Outside.app");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(!trusted_bundle_with_home(
            outside.to_str().unwrap(),
            Some(d.path())
        ));

        let apps = d.path().join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        let valid = apps.join("Valid.app");
        std::fs::create_dir_all(&valid).unwrap();
        assert!(trusted_bundle_with_home(
            valid.to_str().unwrap(),
            Some(d.path())
        ));

        let linked = apps.join("Linked.app");
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        assert!(!trusted_bundle_with_home(
            linked.to_str().unwrap(),
            Some(d.path())
        ));
    }
}
