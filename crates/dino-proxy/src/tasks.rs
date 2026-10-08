//! What an agent keeps track of besides its conversation, from its hooks: its task list and the
//! work it runs in the background. Claude's task tools change one task per call, so the list is
//! kept here and updated call by call; `Stop` lists what's still running in the background.
//! Its own scheduled prompts (Claude's CronCreate, `/loop`) too: only the agent holds them, in
//! memory, so they're followed the same way, and `Stop` lists the ones it still has.

use crate::{Stats, Subagent};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Most background commands and subagents kept per session; the oldest finished ones go first.
const KEEP: usize = 100;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Todo {
    pub id: String,
    pub subject: String,
    /// "pending", "in_progress" or "completed".
    pub status: String,
    /// What to show while it's in progress ("Running the tests").
    pub active: Option<String>,
}

/// A shell command or monitor the agent left running while it carries on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Background {
    pub id: String,
    /// "shell" or "monitor".
    pub kind: String,
    pub description: Option<String>,
    pub command: Option<String>,
    pub started: u64,
    pub finished: Option<u64>,
    pub running: bool,
}

/// A prompt the agent scheduled for itself (Claude's CronCreate): it lives in the agent, which runs
/// it while it's open and drops it when it's deleted, has run (once), expires or the agent exits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cron {
    /// The agent's id for it (Claude: 8 hex digits, which CronDelete takes).
    pub id: String,
    /// Its 5-field cron expression, local time, as the agent was given it.
    pub schedule: String,
    pub recurring: bool,
    pub prompt: String,
    /// How the agent itself put the schedule ("Every hour at :23"), when dino saw it made.
    pub human: Option<String>,
    /// Unix seconds dino saw it made; 0 when it was already there (listed at a turn's end).
    pub created: u64,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub(crate) fn record(stats: &Stats, session: &str, event: &str, v: &Value) {
    // A subagent's own task list and commands are its business; its parent's list is what the
    // user follows.
    if v["agent_id"].is_string() {
        return;
    }
    let text = |x: &Value| x.as_str().filter(|s| !s.is_empty()).map(String::from);
    let input = &v["tool_input"];
    let response = &v["tool_response"];
    match (event, v["tool_name"].as_str().unwrap_or_default()) {
        ("PostToolUse", "TodoWrite") => {
            let Some(todos) = input["todos"].as_array() else { return };
            let todos = todos
                .iter()
                .enumerate()
                .map(|(i, t)| Todo {
                    id: text(&t["id"]).unwrap_or_else(|| (i + 1).to_string()),
                    subject: text(&t["content"]).unwrap_or_default(),
                    status: text(&t["status"]).unwrap_or_else(|| "pending".into()),
                    active: text(&t["activeForm"]),
                })
                .collect();
            stats.update(session, |s| s.todos = todos);
        }
        ("PostToolUse", "TaskCreate") => {
            let Some(id) = text(&response["task"]["id"]).or_else(|| response["task"]["id"].as_u64().map(|n| n.to_string())) else {
                return;
            };
            let todo = Todo { id, subject: text(&input["subject"]).or_else(|| text(&response["task"]["subject"])).unwrap_or_default(), status: "pending".into(), active: text(&input["activeForm"]) };
            stats.update(session, |s| match s.todos.iter_mut().find(|t| t.id == todo.id) {
                Some(t) => *t = todo,
                None => s.todos.push(todo),
            });
        }
        ("PostToolUse", "TaskUpdate") => {
            let Some(id) = text(&input["taskId"]).or_else(|| input["taskId"].as_u64().map(|n| n.to_string())) else { return };
            let status = text(&input["status"]);
            stats.update(session, |s| {
                if status.as_deref() == Some("deleted") {
                    s.todos.retain(|t| t.id != id);
                    return;
                }
                let Some(t) = s.todos.iter_mut().find(|t| t.id == id) else { return };
                if let Some(status) = status {
                    t.status = status;
                }
                if let Some(subject) = text(&input["subject"]) {
                    t.subject = subject;
                }
                if let Some(active) = text(&input["activeForm"]) {
                    t.active = Some(active);
                }
            });
        }
        ("PostToolUse", "Bash") if input["run_in_background"].as_bool() == Some(true) => {
            let Some(id) = text(&response["backgroundTaskId"]) else { return };
            started(stats, session, Background { id, kind: "shell".into(), description: text(&input["description"]), command: text(&input["command"]), ..Default::default() });
        }
        ("PostToolUse", "Monitor") => {
            let Some(id) = text(&response["taskId"]) else { return };
            started(stats, session, Background { id, kind: "monitor".into(), description: text(&input["description"]), command: text(&input["command"]), ..Default::default() });
        }
        // Stopping one: the shell tool's old name and the general one, which stops agents too.
        ("PostToolUse", "KillShell" | "KillBash" | "TaskStop") => {
            let Some(id) = text(&input["shell_id"]).or_else(|| text(&input["task_id"])) else { return };
            stats.update(session, |s| {
                let t = now();
                for b in s.background.iter_mut().filter(|b| b.id == id && b.running) {
                    b.running = false;
                    b.finished = Some(t);
                }
                for a in s.subagents.iter_mut().filter(|a| a.id == id && a.running) {
                    a.running = false;
                    a.finished = Some(t);
                }
            });
        }
        ("PostToolUse", "CronCreate") => {
            let Some(id) = text(&response["id"]) else { return };
            let cron = Cron {
                id,
                schedule: text(&input["cron"]).unwrap_or_default(),
                // Recurring unless it says otherwise, as Claude takes it.
                recurring: response["recurring"].as_bool().or_else(|| input["recurring"].as_bool()).unwrap_or(true),
                prompt: text(&input["prompt"]).unwrap_or_default(),
                human: text(&response["humanSchedule"]),
                created: now(),
            };
            stats.update(session, |s| match s.crons.iter_mut().find(|c| c.id == cron.id) {
                Some(c) => *c = cron,
                None => s.crons.push(cron),
            });
        }
        ("PostToolUse", "CronDelete") => {
            let Some(id) = text(&input["id"]).or_else(|| text(&response["id"])) else { return };
            stats.update(session, |s| s.crons.retain(|c| c.id != id));
        }
        // `/clear` starts a new conversation with a new task list.
        ("SessionStart", _) if v["source"].as_str() == Some("clear") => stats.update(session, |s| s.todos.clear()),
        // A new agent process: what the last one left running ended with it. Its scheduled
        // prompts too; one resumed brings back its own, which its first `Stop` lists.
        ("SessionStart", _) if matches!(v["source"].as_str(), Some("startup" | "resume")) => stats.update(session, |s| {
            s.waiting_on.clear();
            s.crons.clear();
        }),
        ("Stop", _) => {
            if let Some(listed) = v["session_crons"].as_array() {
                stats.update(session, |s| s.crons = listed_crons(&s.crons, listed));
            }
            let Some(listed) = v["background_tasks"].as_array() else { return };
            stats.update(session, |s| reconcile(s, listed));
        }
        _ => {}
    }
}

/// The scheduled prompts the agent still has at the end of a turn (Claude's `session_crons`): the
/// whole list, so ones that ran once, expired or went otherwise are gone, and ones made before dino
/// was watching (a resumed conversation's) are there. What dino saw of the ones it knew stays.
fn listed_crons(known: &[Cron], listed: &[Value]) -> Vec<Cron> {
    listed
        .iter()
        .filter_map(|l| {
            let id = l["id"].as_str().filter(|id| !id.is_empty())?;
            let seen = known.iter().find(|c| c.id == id);
            Some(Cron {
                id: id.into(),
                schedule: l["schedule"].as_str().or_else(|| l["cron"].as_str()).map(String::from).or_else(|| seen.map(|c| c.schedule.clone())).unwrap_or_default(),
                recurring: l["recurring"].as_bool().or_else(|| seen.map(|c| c.recurring)).unwrap_or(true),
                prompt: l["prompt"].as_str().map(String::from).or_else(|| seen.map(|c| c.prompt.clone())).unwrap_or_default(),
                human: seen.and_then(|c| c.human.clone()),
                created: seen.map_or(0, |c| c.created),
            })
        })
        .collect()
}

fn started(stats: &Stats, session: &str, mut b: Background) {
    b.running = true;
    b.started = now();
    stats.update(session, |s| {
        s.background.retain(|o| o.id != b.id);
        s.background.push(b);
        trim(&mut s.background, |b| b.running);
    });
}

/// At the end of a turn the agent lists what still runs: everything else has finished, whether
/// or not a hook said so (an interrupted subagent sends no `SubagentStop`).
fn reconcile(s: &mut crate::SessionStats, listed: &[Value]) {
    let running = |id: &str| listed.iter().any(|t| t["id"].as_str() == Some(id) && t["status"].as_str().is_none_or(|st| st == "running"));
    s.waiting_on = listed.iter().filter_map(|l| l["id"].as_str()).filter(|id| running(id)).map(String::from).collect();
    let t = now();
    for b in s.background.iter_mut().filter(|b| b.running && !running(&b.id)) {
        b.running = false;
        b.finished = Some(t);
    }
    for a in s.subagents.iter_mut().filter(|a| a.running && !running(&a.id)) {
        a.running = false;
        a.finished = Some(t);
    }
    // Ones started before dino was watching (it restarted, or the conversation was resumed).
    for l in listed.iter().filter(|l| l["status"].as_str().is_none_or(|st| st == "running")) {
        let Some(id) = l["id"].as_str().filter(|id| !id.is_empty()) else { continue };
        let description = l["description"].as_str().map(String::from);
        if l["type"].as_str() == Some("subagent") {
            if !s.subagents.iter().any(|a| a.id == id) {
                s.subagents.push(Subagent { id: id.into(), agent_type: l["agent_type"].as_str().map(String::from), description, running: true, ..Default::default() });
            }
        } else if !s.background.iter().any(|b| b.id == id) {
            let kind = if l["type"].as_str().is_some_and(|t| t.contains("monitor")) { "monitor" } else { "shell" };
            let command = l["command"].as_str().map(String::from);
            s.background.push(Background { id: id.into(), kind: kind.into(), description, command, running: true, ..Default::default() });
        }
    }
    trim(&mut s.subagents, |a| a.running);
    trim(&mut s.background, |b| b.running);
}

/// Keep at most `KEEP`, dropping the oldest finished ones.
fn trim<T>(list: &mut Vec<T>, running: impl Fn(&T) -> bool) {
    while list.len() > KEEP {
        match list.iter().position(|x| !running(x)) {
            Some(i) => list.remove(i),
            None => list.remove(0),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Payloads as Claude Code 2.1 sends them.
    #[test]
    fn task_list_from_task_tools() {
        let stats = Stats::default();
        let feed = |event: &str, v: Value| record(&stats, "s", event, &v);
        for (id, subject) in [("1", "Task a"), ("2", "Task b"), ("3", "Task c")] {
            feed(
                "PostToolUse",
                json!({"tool_name": "TaskCreate", "tool_input": {"subject": subject, "description": "x"},
                "tool_response": {"task": {"id": id, "subject": subject}}}),
            );
        }
        feed("PostToolUse", json!({"tool_name": "TaskUpdate", "tool_input": {"taskId": "1", "status": "in_progress", "activeForm": "Doing a"}}));
        feed("PostToolUse", json!({"tool_name": "TaskUpdate", "tool_input": {"taskId": "2", "status": "completed"}}));
        feed("PostToolUse", json!({"tool_name": "TaskUpdate", "tool_input": {"taskId": "3", "status": "deleted"}}));
        // A subagent's own list stays out of it.
        feed(
            "PostToolUse",
            json!({"tool_name": "TaskCreate", "agent_id": "a1", "tool_input": {"subject": "mine"},
            "tool_response": {"task": {"id": "9", "subject": "mine"}}}),
        );
        let got: Vec<_> = stats.session("s").todos.iter().map(|t| (t.id.clone(), t.status.clone(), t.active.clone())).collect();
        assert_eq!(got, vec![("1".into(), "in_progress".into(), Some("Doing a".into())), ("2".into(), "completed".into(), None)]);

        feed("SessionStart", json!({"source": "resume"}));
        assert_eq!(stats.session("s").todos.len(), 2);
        feed("SessionStart", json!({"source": "clear"}));
        assert!(stats.session("s").todos.is_empty());
    }

    #[test]
    fn task_list_from_todo_write() {
        let stats = Stats::default();
        record(
            &stats,
            "s",
            "PostToolUse",
            &json!({"tool_name": "TodoWrite", "tool_input": {"todos": [
                {"content": "Write it", "status": "completed", "activeForm": "Writing it"},
                {"content": "Test it", "status": "in_progress", "activeForm": "Testing it"},
            ]}}),
        );
        let todos = stats.session("s").todos;
        assert_eq!(todos.iter().map(|t| (t.id.as_str(), t.subject.as_str())).collect::<Vec<_>>(), vec![("1", "Write it"), ("2", "Test it")]);
    }

    #[test]
    fn background_work_ends_when_stop_no_longer_lists_it() {
        let stats = Stats::default();
        let feed = |event: &str, v: Value| record(&stats, "s", event, &v);
        feed(
            "PostToolUse",
            json!({"tool_name": "Bash", "tool_input": {"command": "sleep 4", "description": "Wait", "run_in_background": true},
            "tool_response": {"backgroundTaskId": "b1"}}),
        );
        feed("PostToolUse", json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_response": {"stdout": ""}}));
        feed(
            "PostToolUse",
            json!({"tool_name": "Monitor", "tool_input": {"command": "tail -f x", "description": "Errors"},
            "tool_response": {"taskId": "m1"}}),
        );
        stats.update("s", |s| s.subagents.push(Subagent { id: "a1".into(), running: true, ..Default::default() }));
        let s = stats.session("s");
        assert_eq!(s.background.iter().map(|b| (b.id.as_str(), b.kind.as_str(), b.running)).collect::<Vec<_>>(), vec![("b1", "shell", true), ("m1", "monitor", true)]);

        feed("PostToolUse", json!({"tool_name": "TaskStop", "tool_input": {"task_id": "m1"}}));
        feed(
            "Stop",
            json!({"background_tasks": [
                {"id": "b1", "type": "shell", "status": "running", "description": "Wait"},
                {"id": "a2", "type": "subagent", "status": "running", "description": "Earlier", "agent_type": "Explore"},
            ]}),
        );
        let s = stats.session("s");
        assert_eq!(s.background.iter().map(|b| (b.id.as_str(), b.running)).collect::<Vec<_>>(), vec![("b1", true), ("m1", false)]);
        assert!(s.background[1].finished.is_some());
        assert_eq!(s.subagents.iter().map(|a| (a.id.as_str(), a.running)).collect::<Vec<_>>(), vec![("a1", false), ("a2", true)]);

        feed("Stop", json!({"background_tasks": []}));
        assert!(stats.session("s").background.iter().all(|b| !b.running));
    }

    #[test]
    fn a_turn_that_ends_on_background_work_waits_for_it() {
        let stats = Stats::default();
        let feed = |event: &str, v: Value| record(&stats, "s", event, &v);
        feed(
            "PostToolUse",
            json!({"tool_name": "Bash", "tool_input": {"command": "sleep 25", "run_in_background": true},
            "tool_response": {"backgroundTaskId": "b1"}}),
        );
        feed("PostToolUse", json!({"tool_name": "Monitor", "tool_input": {"command": "tail -f x"}, "tool_response": {"taskId": "m1"}}));
        // A foreground subagent cut short by Esc: no SubagentStop, and no Stop listed it.
        stats.update("s", |s| s.subagents.push(Subagent { id: "a0".into(), running: true, ..Default::default() }));
        assert_eq!(stats.session("s").waiting(), (0, 0));

        // A subagent's own background command is listed as the session's.
        feed(
            "Stop",
            json!({"background_tasks": [
                {"id": "b1", "type": "shell", "status": "running", "command": "sleep 25"},
                {"id": "m1", "type": "monitor", "status": "running"},
                {"id": "a1", "type": "subagent", "status": "running", "description": "nap", "agent_type": "general-purpose"},
                {"id": "b2", "type": "shell", "status": "running", "description": "Its own sleep", "command": "sleep 9"},
            ]}),
        );
        let s = stats.session("s");
        assert_eq!(s.waiting(), (1, 2));
        assert_eq!(s.background.iter().find(|b| b.id == "b2").and_then(|b| b.command.as_deref()), Some("sleep 9"));

        stats.update("s", |s| s.subagents.iter_mut().for_each(|a| a.running = false));
        assert_eq!(stats.session("s").waiting(), (0, 2));
        feed("Stop", json!({"background_tasks": [{"id": "m1", "type": "monitor", "status": "running"}]}));
        assert_eq!(stats.session("s").waiting(), (0, 0));

        feed("Stop", json!({"background_tasks": [{"id": "b3", "type": "shell", "status": "running"}]}));
        assert_eq!(stats.session("s").waiting(), (0, 1));
        feed("SessionStart", json!({"source": "resume"}));
        assert_eq!(stats.session("s").waiting(), (0, 0));
    }

    /// Payloads as Claude Code 2.1.293 sends them.
    #[test]
    fn scheduled_prompts_from_cron_tools_and_stop() {
        let stats = Stats::default();
        let feed = |event: &str, v: Value| record(&stats, "s", event, &v);
        let ids = |stats: &Stats| stats.session("s").crons.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        feed(
            "PostToolUse",
            json!({"tool_name": "CronCreate",
            "tool_input": {"cron": "23 * * * *", "prompt": "Post-merge production watch: say hi", "recurring": true},
            "tool_response": {"id": "efc5ae94", "humanSchedule": "Every hour at :23", "recurring": true, "durable": false}}),
        );
        feed(
            "PostToolUse",
            json!({"tool_name": "CronCreate",
            "tool_input": {"cron": "59 23 31 12 *", "prompt": "new year", "recurring": false},
            "tool_response": {"id": "08ec38c8", "humanSchedule": "59 23 31 12 *", "recurring": false, "durable": false}}),
        );
        // A subagent's own stays out of it, as its other tools do.
        feed(
            "PostToolUse",
            json!({"tool_name": "CronCreate", "agent_id": "a1",
            "tool_input": {"cron": "* * * * *", "prompt": "x"}, "tool_response": {"id": "aaaaaaaa"}}),
        );
        let s = stats.session("s");
        assert_eq!(ids(&stats), vec!["efc5ae94", "08ec38c8"]);
        assert_eq!((s.crons[0].schedule.as_str(), s.crons[0].recurring, s.crons[0].human.as_deref()), ("23 * * * *", true, Some("Every hour at :23")));
        assert!(!s.crons[1].recurring && s.crons[1].created > 0);

        feed("PostToolUse", json!({"tool_name": "CronDelete", "tool_input": {"id": "08ec38c8"}, "tool_response": {"id": "08ec38c8"}}));
        assert_eq!(ids(&stats), vec!["efc5ae94"]);

        // The turn's end lists what it has: a `/loop` wakeup it made, and not one gone since.
        feed(
            "Stop",
            json!({"background_tasks": [], "session_crons": [
            {"id": "efc5ae94", "schedule": "23 * * * *", "recurring": true, "prompt": "Post-merge production watch: say hi"},
            {"id": "f5672ede", "schedule": "46 11 * * *", "recurring": false, "prompt": "ping"}]}),
        );
        let s = stats.session("s");
        assert_eq!(ids(&stats), vec!["efc5ae94", "f5672ede"]);
        assert_eq!(s.crons[0].human.as_deref(), Some("Every hour at :23"), "what dino saw stays");
        assert_eq!(s.crons[1].created, 0);
        // The one-shot ran: the next turn's end no longer lists it.
        feed(
            "Stop",
            json!({"background_tasks": [], "session_crons": [
            {"id": "efc5ae94", "schedule": "23 * * * *", "recurring": true, "prompt": "Post-merge production watch: say hi"}]}),
        );
        assert_eq!(ids(&stats), vec!["efc5ae94"]);
        // An agent that says nothing of them leaves them as they are.
        feed("Stop", json!({"background_tasks": []}));
        assert_eq!(ids(&stats), vec!["efc5ae94"]);

        // A new process starts with none (dino starting it again, or its SessionStart); a resumed
        // one lists its own at its first turn's end.
        stats.restarted("s");
        assert!(stats.session("s").crons.is_empty());
        stats.update("s", |s| s.crons.push(Cron { id: "x".into(), ..Default::default() }));
        feed("SessionStart", json!({"source": "resume"}));
        assert!(stats.session("s").crons.is_empty());
        feed("Stop", json!({"session_crons": [{"id": "efc5ae94", "schedule": "23 * * * *", "recurring": true, "prompt": "p"}]}));
        assert_eq!(ids(&stats), vec!["efc5ae94"]);
        // The agent exits: they go with it.
        stats.update("s", |s| s.hooked = true);
        assert!(stats.agent_left("s"));
        assert!(stats.session("s").crons.is_empty());
    }
}
