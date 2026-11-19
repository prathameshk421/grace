//! Live editor state: which folders have an editor window open right now, and a cheap
//! background refresh of editor history so folders opened from File > Open show up.
//! OWNER: editors agent. The picker agent only calls these two functions.
use crate::db::Db;

/// One folder with an editor window open right now.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenEditor {
    /// Absolute folder path (normalized like other grace paths).
    pub path: String,
    /// grace tag of the editor, e.g. "vscode", "antigravity", "cursor".
    pub tag: String,
    /// Command that opens or focuses this folder in that editor, e.g. `code "/a b"`.
    pub open_cmd: String,
}

/// Folders with an editor window open now. Must be fast (< 30 ms) and never fail.
pub fn open_editors() -> Vec<OpenEditor> {
    Vec::new()
}

/// Re-import editor history if the last import is older than 10 minutes. Never fails loudly.
pub fn refresh_if_stale(db: &Db) {
    let _ = db;
}
