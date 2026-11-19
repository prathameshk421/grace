//! Shared data types. All modules use these.

/// One AI tool that the user ran in a folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    /// Canonical tag name, for example "claude" or "antigravity".
    pub tool: String,
    /// How many times the tool ran in this folder.
    pub count: i64,
    /// Unix time in seconds of the last run.
    pub last_used: i64,
}

/// One tracked folder (a "grace").
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Absolute path, no trailing slash.
    pub path: String,
    pub visits: i64,
    /// Unix time in seconds of the last visit.
    pub last_visit: i64,
    pub pinned: bool,
    /// Sorted by last_used, newest first.
    pub tags: Vec<Tag>,
}

impl Entry {
    /// The tool used most recently in this folder, if any.
    pub fn last_tool(&self) -> Option<&str> {
        self.tags.first().map(|t| t.tool.as_str())
    }

    pub fn has_tag(&self, tool: &str) -> bool {
        self.tags.iter().any(|t| t.tool == tool)
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
