//! A session's status in the app's words (Working, Needs you, Done, Idle), and how it ended, for
//! `dino ls` and `dino status`. Worked out from the same fields, in the same order, as the app's
//! sidebar (`status(of:)` in app/Sources/Dino/Model.swift), so the two never disagree. The app also
//! remembers what you've looked at, which a command line can't: there a turn that ended is Done
//! until you look, here it's Done until the next one starts.

use serde::Serialize;

use crate::ipc::SessionInfo;

/// In the order `dino ls` lists them: what wants you first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    NeedsYou,
    Working,
    Done,
    Idle,
    /// Its program exited cleanly; `dino resume` starts it again.
    Ended,
    /// Its program failed.
    Exited,
}

impl Status {
    pub fn of(s: &SessionInfo) -> Status {
        if s.exited {
            return if s.exit_code.unwrap_or(0) == 0 { Status::Ended } else { Status::Exited };
        }
        if needs(s).is_some() {
            return Status::NeedsYou;
        }
        // An agent run by hand in a shell says whether it's busy, unless its hooks report to dino
        // (typed into a dino shell): those say more, as for dino's sessions. As the app has it.
        if let Some(st) = s.inside.as_ref().and_then(|f| f.status.as_deref()).filter(|_| s.activity.is_none()) {
            return match st {
                "needs" => Status::NeedsYou,
                "busy" => Status::Working,
                _ => Status::Idle,
            };
        }
        // A turn that ended on subagents or background commands isn't over until they are.
        match s.activity.as_deref() {
            Some(a) if a.starts_with("waiting:") => Status::Working,
            Some(a) if a != "working" => Status::Done,
            _ if s.in_flight > 0 || s.activity.as_deref() == Some("working") => Status::Working,
            _ if s.output_ms_ago.is_some_and(|ms| ms < 1500) => Status::Working,
            _ => Status::Idle,
        }
    }

    /// The word the app shows, except that a session that ended says so.
    pub fn label(self) -> &'static str {
        match self {
            Status::NeedsYou => "Needs you",
            Status::Working => "Working",
            Status::Done => "Done",
            Status::Idle => "Idle",
            Status::Ended => "Ended",
            Status::Exited => "Exited",
        }
    }

    /// The same, for scripts: `needs_you`, `working`, `done`, `idle`, `ended`, `exited`.
    pub fn key(self) -> &'static str {
        match self {
            Status::NeedsYou => "needs_you",
            Status::Working => "working",
            Status::Done => "done",
            Status::Idle => "idle",
            Status::Ended => "ended",
            Status::Exited => "exited",
        }
    }
}

/// What the agent is asking for ("Run: touch hello.txt?"), when it is.
pub fn needs(s: &SessionInfo) -> Option<&str> {
    s.activity.as_deref().and_then(|a| a.strip_prefix("needs:"))
}

/// An agent rather than a plain shell: one dino started, or one run by hand in a dino shell.
pub fn is_agent(s: &SessionInfo) -> bool {
    s.agent_id != "shell" || s.inside.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(activity: Option<&str>) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": "1", "name": "claude", "agent_id": "claude", "title": null, "exited": false,
            "output_ms_ago": 60_000, "bells": 0, "requests": 0, "in_flight": 0, "input_tokens": 0,
            "output_tokens": 0, "last_model": null, "tier": null, "activity": activity,
        }))
        .unwrap()
    }

    #[test]
    fn status_is_the_apps() {
        assert_eq!(Status::of(&session(None)), Status::Idle);
        assert_eq!(Status::of(&session(Some("working"))), Status::Working);
        assert_eq!(Status::of(&session(Some("needs:Run: ls?"))), Status::NeedsYou);
        assert_eq!(needs(&session(Some("needs:Run: ls?"))), Some("Run: ls?"));
        assert_eq!(Status::of(&session(Some("waiting:1 agent"))), Status::Working);
        assert_eq!(Status::of(&session(Some("done"))), Status::Done);
        assert_eq!(Status::of(&session(Some("server:3000"))), Status::Done);
        // Between turns, a redraw isn't work; with no turn yet, recent output is.
        assert_eq!(Status::of(&SessionInfo { output_ms_ago: Some(100), ..session(Some("done")) }), Status::Done);
        assert_eq!(Status::of(&SessionInfo { output_ms_ago: Some(100), ..session(None) }), Status::Working);
        assert_eq!(Status::of(&SessionInfo { in_flight: 1, ..session(None) }), Status::Working);
        assert_eq!(Status::of(&SessionInfo { exited: true, ..session(Some("needs:x")) }), Status::Ended);
        assert_eq!(Status::of(&SessionInfo { exited: true, exit_code: Some(1), ..session(None) }), Status::Exited);
        assert!(Status::NeedsYou < Status::Working && Status::Done < Status::Idle);
    }

    #[test]
    fn an_agent_typed_into_a_shell() {
        let inside = |status: &str| {
            serde_json::from_value(serde_json::json!({
                "source": "running", "agent": "claude", "session_id": "", "title": "", "cwd": null, "updated_at": 0,
                "pid": 7, "status": status, "terminal": null, "args": [], "url": null,
            }))
            .ok()
        };
        let shell = |activity: Option<&str>, status: &str| SessionInfo { agent_id: "shell".into(), inside: inside(status), ..session(activity) };
        // Without hooks, what the agent says; with them, what they say, as in the app.
        assert_eq!(Status::of(&shell(None, "busy")), Status::Working);
        assert_eq!(Status::of(&shell(Some("done"), "busy")), Status::Done);
        assert_eq!(Status::of(&shell(Some("working"), "idle")), Status::Working);
    }
}
