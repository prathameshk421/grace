//! Live sessions: parse agent hook input and order sessions for display.
use crate::model::{Session, Status};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    pub status: Status,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub detail: Option<String>,
}

/// Map a Claude Code CLI hook event (plus optional `notification_type`) to a status.
pub fn claude_status_for(event: &str, ntype: &str) -> Option<Status> {
    match event {
        "SessionStart" | "Stop" => Some(Status::Ready),
        "Notification" => match ntype {
            "idle_prompt" => Some(Status::Ready),
            "auth_success" => None,
            _ => Some(Status::Waiting),
        },
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(Status::Working),
        "SessionEnd" => Some(Status::Done),
        _ => None,
    }
}

pub(crate) fn non_empty_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Parse the Claude Code hook JSON with `session_id` and concise `detail`.
pub fn parse_claude_event(json: &str) -> Option<HookEvent> {
    let v: Value = serde_json::from_str(json).ok()?;
    let ev = non_empty_str(&v, "hook_event_name")?;
    let ntype = non_empty_str(&v, "notification_type").unwrap_or_default();
    let status = claude_status_for(&ev, &ntype)?;
    let detail = match ev.as_str() {
        "Notification" => match ntype.as_str() {
            "permission_prompt" => Some("permission".into()),
            "elicitation_dialog" => Some("question".into()),
            "" => None,
            other => Some(crate::platform::sanitize_display(other).into_owned()),
        },
        "PreToolUse" | "PostToolUse" => non_empty_str(&v, "tool_name")
            .map(|t| crate::platform::sanitize_display(&t).into_owned()),
        _ => None,
    };
    Some(HookEvent {
        status,
        cwd: non_empty_str(&v, "cwd"),
        session_id: non_empty_str(&v, "session_id"),
        detail,
    })
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

/// Parse the Codex CLI `notify` JSON. Returns `(cwd, session_id)` for `agent-turn-complete`.
pub fn parse_codex_event(json: &str) -> Option<(Option<String>, Option<String>)> {
    let v: Value = serde_json::from_str(json).ok()?;
    if non_empty_str(&v, "type")? != "agent-turn-complete" {
        return None;
    }
    Some((non_empty_str(&v, "cwd"), non_empty_str(&v, "thread-id")))
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
        assert_eq!(claude_status_for("SessionStart", ""), Some(Status::Ready));
        assert_eq!(
            claude_status_for("UserPromptSubmit", ""),
            Some(Status::Working)
        );
        assert_eq!(claude_status_for("PreToolUse", ""), Some(Status::Working));
        assert_eq!(claude_status_for("PostToolUse", ""), Some(Status::Working));
        assert_eq!(claude_status_for("Notification", ""), Some(Status::Waiting));
        assert_eq!(
            claude_status_for("Notification", "idle_prompt"),
            Some(Status::Ready)
        );
        assert_eq!(claude_status_for("Notification", "auth_success"), None);
        assert_eq!(
            claude_status_for("Notification", "permission_prompt"),
            Some(Status::Waiting)
        );
        assert_eq!(claude_status_for("Stop", ""), Some(Status::Ready));
        assert_eq!(claude_status_for("SubagentStop", ""), None);
        assert_eq!(claude_status_for("SessionEnd", ""), Some(Status::Done));
        assert_eq!(claude_status_for("Other", ""), None);
    }

    #[test]
    fn claude_json() {
        let j = r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","cwd":"/tmp/x","session_id":"s1"}"#;
        let ev = parse_claude_event(j).unwrap();
        assert_eq!(ev.status, Status::Waiting);
        assert_eq!(ev.cwd.as_deref(), Some("/tmp/x"));
        assert_eq!(ev.session_id.as_deref(), Some("s1"));
        assert_eq!(ev.detail.as_deref(), Some("permission"));
        let ev2 = parse_claude_event(
            r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","cwd":"/tmp/x"}"#,
        )
        .unwrap();
        assert_eq!(
            (ev2.status, ev2.cwd.as_deref()),
            (Status::Ready, Some("/tmp/x"))
        );
        let stop = parse_claude_event(r#"{"hook_event_name":"Stop"}"#).unwrap();
        assert_eq!((stop.status, stop.cwd), (Status::Ready, None));
        assert_eq!(
            parse_claude_event(r#"{"hook_event_name":"SubagentStop"}"#),
            None
        );
        assert_eq!(parse_claude_event("not json"), None);
        assert_eq!(parse_claude_event(""), None);
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
        let j = r#"{"type":"agent-turn-complete","cwd":"/p","turn-id":"1","thread-id":"t1"}"#;
        assert_eq!(
            parse_codex_event(j),
            Some((Some("/p".into()), Some("t1".into())))
        );
        assert_eq!(
            parse_codex_event(r#"{"type":"agent-turn-complete"}"#),
            Some((None, None))
        );
        assert_eq!(parse_codex_event(r#"{"type":"other","cwd":"/p"}"#), None);
        assert_eq!(parse_codex_event("{"), None);
    }

    #[test]
    fn ordering() {
        let s = |id, status, updated_at| Session {
            id,
            path: format!("/{id}"),
            tool: "claude".into(),
            status,
            shell_pid: None,
            shell_start: None,
            agent_pid: None,
            agent_start: None,
            started_at: 0,
            updated_at,
            session_id: None,
            detail: None,
            tmux_pane: None,
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
