//! Live sessions: parse agent hook input and order sessions for display.
use crate::model::{Session, Status};
use serde_json::Value;

/// Map a Claude Code CLI hook event to a status. None means ignore the event.
pub fn claude_status(event: &str) -> Option<Status> {
    match event {
        "SessionStart" | "Stop" => Some(Status::Ready),
        "Notification" => Some(Status::Waiting),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(Status::Working),
        "SessionEnd" => Some(Status::Done),
        _ => None,
    }
}

fn non_empty_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
}

/// Parse the Claude Code hook JSON. Returns (status, cwd). None: bad JSON or ignored event.
pub fn parse_claude_hook(json: &str) -> Option<(Status, Option<String>)> {
    let v: Value = serde_json::from_str(json).ok()?;
    let status = claude_status(&non_empty_str(&v, "hook_event_name")?)?;
    Some((status, non_empty_str(&v, "cwd")))
}

/// Split the args of `session hook codex`. The last arg is the Codex JSON.
/// Items between `--then` and the JSON are the chained program and its args.
pub fn split_codex_args(args: &[String]) -> (Option<&str>, Option<&[String]>) {
    let Some((json, rest)) = args.split_last() else {
        return (None, None);
    };
    let chained = rest
        .iter()
        .position(|a| a == "--then")
        .map(|i| &rest[i + 1..])
        .filter(|c| !c.is_empty());
    (Some(json.as_str()), chained)
}

/// Parse the Codex CLI `notify` JSON. Returns the cwd (None inside: use the current dir)
/// when the event is `agent-turn-complete`. Outer None: bad JSON or ignored event.
pub fn parse_codex_notify(json: &str) -> Option<Option<String>> {
    let v: Value = serde_json::from_str(json).ok()?;
    if non_empty_str(&v, "type")? != "agent-turn-complete" {
        return None;
    }
    Some(non_empty_str(&v, "cwd"))
}

/// Sort key: waiting first, then ready, then working, then done. Newest first inside each group.
pub fn sort_sessions(list: &mut [Session]) {
    list.sort_by(|a, b| {
        crate::picker::urgency(b.status)
            .cmp(&crate::picker::urgency(a.status))
            .then(b.updated_at.cmp(&a.updated_at))
            .then(b.id.cmp(&a.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_events() {
        assert_eq!(claude_status("SessionStart"), Some(Status::Ready));
        assert_eq!(claude_status("UserPromptSubmit"), Some(Status::Working));
        assert_eq!(claude_status("PreToolUse"), Some(Status::Working));
        assert_eq!(claude_status("PostToolUse"), Some(Status::Working));
        assert_eq!(claude_status("Notification"), Some(Status::Waiting));
        assert_eq!(claude_status("Stop"), Some(Status::Ready));
        assert_eq!(claude_status("SubagentStop"), None);
        assert_eq!(claude_status("SessionEnd"), Some(Status::Done));
        assert_eq!(claude_status("Other"), None);
    }

    #[test]
    fn claude_json() {
        let j = r#"{"hook_event_name":"Notification","cwd":"/tmp/x","session_id":"1"}"#;
        assert_eq!(parse_claude_hook(j), Some((Status::Waiting, Some("/tmp/x".into()))));
        assert_eq!(
            parse_claude_hook(r#"{"hook_event_name":"Stop"}"#),
            Some((Status::Ready, None))
        );
        assert_eq!(parse_claude_hook(r#"{"hook_event_name":"SubagentStop"}"#), None);
        assert_eq!(parse_claude_hook("not json"), None);
        assert_eq!(parse_claude_hook(""), None);
    }

    #[test]
    fn split_args() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(split_codex_args(&[]), (None, None));
        let a = v(&["{}"]);
        assert_eq!(split_codex_args(&a), (Some("{}"), None));
        let a = v(&["--then", "prog", "turn-ended", "{\"x\":1}"]);
        let (j, c) = split_codex_args(&a);
        assert_eq!(j, Some("{\"x\":1}"));
        assert_eq!(c.unwrap(), &v(&["prog", "turn-ended"])[..]);
        let a = v(&["--then", "{}"]);
        assert_eq!(split_codex_args(&a), (Some("{}"), None));
    }

    #[test]
    fn codex_json() {
        let j = r#"{"type":"agent-turn-complete","cwd":"/p","turn-id":"1"}"#;
        assert_eq!(parse_codex_notify(j), Some(Some("/p".into())));
        assert_eq!(parse_codex_notify(r#"{"type":"agent-turn-complete"}"#), Some(None));
        assert_eq!(parse_codex_notify(r#"{"type":"other","cwd":"/p"}"#), None);
        assert_eq!(parse_codex_notify("{"), None);
    }

    #[test]
    fn ordering() {
        let s = |id, status, updated_at| Session {
            id,
            path: format!("/{id}"),
            tool: "claude".into(),
            status,
            shell_pid: None,
            started_at: 0,
            updated_at,
        };
        let mut v = vec![
            s(1, Status::Done, 90),
            s(2, Status::Working, 50),
            s(3, Status::Waiting, 10),
            s(4, Status::Waiting, 20),
            s(5, Status::Working, 60),
            s(6, Status::Ready, 5),
            s(7, Status::Ready, 15),
        ];
        sort_sessions(&mut v);
        let ids: Vec<_> = v.iter().map(|x| x.id).collect();
        assert_eq!(ids, vec![4, 3, 7, 6, 5, 2, 1]);
    }
}
