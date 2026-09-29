//! Agent controls: permission mode, model and effort, in dino's own words, and how each agent
//! takes them on its command line. An agent without a knob simply doesn't offer it.

use serde::{Deserialize, Serialize};

/// What a session was asked to run with; `None` leaves it to the agent's own default.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Controls {
    /// One of `MODES`' ids.
    pub mode: Option<String>,
    /// An alias the agent knows ("opus") or a full model name.
    pub model: Option<String>,
    /// One of the agent's `efforts`.
    pub effort: Option<String>,
}

impl Controls {
    pub fn is_empty(&self) -> bool {
        self.mode.is_none() && self.model.is_none() && self.effort.is_none()
    }

    /// `self`, with what it leaves open filled in from `defaults`.
    pub fn or(&self, defaults: &Controls) -> Controls {
        Controls {
            mode: self.mode.clone().or_else(|| defaults.mode.clone()),
            model: self.model.clone().or_else(|| defaults.model.clone()),
            effort: self.effort.clone().or_else(|| defaults.effort.clone()),
        }
    }
}

/// Permission modes, the same across agents: (id, label, what it means).
pub const MODES: &[(&str, &str, &str)] = &[
    ("ask", "Ask", "Asks before editing files or running commands"),
    ("edits", "Accept edits", "Edits files freely, asks before running commands"),
    ("plan", "Plan", "Reads and plans, changes nothing"),
    ("auto", "Auto", "Decides for itself what needs asking"),
    ("bypass", "Bypass", "Never asks. Only for sandboxes and throwaway machines"),
];

/// Which controls an agent offers, in dino's words.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Knobs {
    /// Mode ids from `MODES`, in that order.
    pub modes: Vec<String>,
    /// Whether the model can be chosen.
    pub model: bool,
    /// Short names the agent takes for its models, best known first.
    pub models: Vec<String>,
    /// Effort levels, lowest first; empty when there's no such knob.
    pub efforts: Vec<String>,
    /// Changing a control restarts the agent (resuming its conversation) rather than applying live.
    pub restart: bool,
}

fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| s.to_string()).collect()
}

/// The controls `agent_id` offers. `bypass` is left out unless the policies allow it.
pub fn knobs(agent_id: &str, allow_bypass: bool) -> Knobs {
    let mut k = match agent_id {
        "claude" => Knobs {
            modes: strings(&["ask", "edits", "plan", "auto", "bypass"]),
            model: true,
            models: strings(&["fable", "opus", "sonnet", "haiku"]),
            efforts: strings(&["low", "medium", "high", "xhigh", "max"]),
            restart: true,
        },
        // The free tier picks the model for each turn.
        "claude-free" => Knobs { model: false, models: vec![], ..knobs("claude", true) },
        "codex" => Knobs {
            modes: strings(&["ask", "edits", "auto", "bypass"]),
            model: true,
            models: vec![],
            efforts: strings(&["minimal", "low", "medium", "high", "xhigh"]),
            restart: true,
        },
        "gemini" => Knobs { modes: strings(&["ask", "edits", "plan", "bypass"]), model: true, models: vec![], efforts: vec![], restart: true },
        _ => Knobs::default(),
    };
    if !allow_bypass {
        k.modes.retain(|m| m != "bypass");
    }
    k
}

/// Command-line arguments that apply `c` to `agent_id`. Values the agent doesn't offer are
/// dropped rather than passed on to fail.
pub fn args(agent_id: &str, c: &Controls) -> Vec<String> {
    let k = knobs(agent_id, true);
    let mode = c.mode.as_deref().filter(|m| k.modes.iter().any(|x| x == m));
    let model = c.model.as_deref().map(str::trim).filter(|m| k.model && !m.is_empty());
    let effort = c.effort.as_deref().filter(|e| k.efforts.iter().any(|x| x == e));
    let mut out: Vec<String> = vec![];
    let mut push = |a: &[&str]| out.extend(a.iter().map(|s| s.to_string()));
    match agent_id {
        "claude" | "claude-free" => {
            if let Some(m) = mode {
                let m = match m {
                    "ask" => "manual",
                    "edits" => "acceptEdits",
                    "plan" => "plan",
                    "auto" => "auto",
                    _ => "bypassPermissions",
                };
                push(&["--permission-mode", m]);
            }
            if let Some(m) = model {
                push(&["--model", m]);
            }
            if let Some(e) = effort {
                push(&["--effort", e]);
            }
        }
        "codex" => {
            match mode {
                Some("ask") => push(&["-s", "read-only", "-a", "on-request"]),
                Some("edits") => push(&["-s", "workspace-write", "-a", "on-request"]),
                Some("auto") => push(&["--approve-for-me"]),
                Some(_) => push(&["--dangerously-bypass-approvals-and-sandbox"]),
                None => {}
            }
            if let Some(m) = model {
                push(&["-m", m]);
            }
            if let Some(e) = effort {
                push(&["-c", &format!("model_reasoning_effort=\"{e}\"")]);
            }
        }
        "gemini" => {
            if let Some(m) = mode {
                let m = match m {
                    "ask" => "default",
                    "edits" => "auto_edit",
                    "plan" => "plan",
                    _ => "yolo",
                };
                push(&["--approval-mode", m]);
            }
            if let Some(m) = model {
                push(&["-m", m]);
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(mode: Option<&str>, model: Option<&str>, effort: Option<&str>) -> Controls {
        Controls { mode: mode.map(Into::into), model: model.map(Into::into), effort: effort.map(Into::into) }
    }

    #[test]
    fn claude_flags() {
        assert_eq!(args("claude", &Controls::default()), Vec::<String>::new());
        assert_eq!(
            args("claude", &c(Some("ask"), Some("haiku"), Some("high"))),
            ["--permission-mode", "manual", "--model", "haiku", "--effort", "high"]
        );
        assert_eq!(args("claude", &c(Some("bypass"), None, None)), ["--permission-mode", "bypassPermissions"]);
        // The free tier routes models itself.
        assert_eq!(args("claude-free", &c(Some("plan"), Some("opus"), None)), ["--permission-mode", "plan"]);
        // Unknown values never reach the agent.
        assert_eq!(args("claude", &c(Some("yolo"), Some("  "), Some("huge"))), Vec::<String>::new());
    }

    #[test]
    fn codex_and_gemini_flags() {
        assert_eq!(args("codex", &c(Some("edits"), Some("gpt-5.5"), Some("low"))), ["-s", "workspace-write", "-a", "on-request", "-m", "gpt-5.5", "-c", "model_reasoning_effort=\"low\""]);
        assert_eq!(args("codex", &c(Some("plan"), None, None)), Vec::<String>::new(), "codex has no plan mode");
        assert_eq!(args("codex", &c(Some("bypass"), None, None)), ["--dangerously-bypass-approvals-and-sandbox"]);
        assert_eq!(args("gemini", &c(Some("edits"), Some("gemini-2.5-pro"), Some("high"))), ["--approval-mode", "auto_edit", "-m", "gemini-2.5-pro"]);
        assert_eq!(args("aider", &c(Some("ask"), Some("x"), Some("high"))), Vec::<String>::new());
    }

    #[test]
    fn knobs_follow_policy() {
        assert!(!knobs("claude", false).modes.contains(&"bypass".to_string()));
        assert!(knobs("claude", true).modes.contains(&"bypass".to_string()));
        assert!(!knobs("claude-free", true).model && !knobs("claude-free", true).efforts.is_empty());
        assert!(knobs("gemini", true).efforts.is_empty());
        assert_eq!(knobs("shell", true), Knobs::default());
        let all: Vec<&str> = MODES.iter().map(|m| m.0).collect();
        for agent in ["claude", "codex", "gemini"] {
            let k = knobs(agent, true);
            let mut order = k.modes.iter().map(|m| all.iter().position(|a| a == m).expect("a known mode"));
            assert!(order.clone().zip(order.by_ref().skip(1)).all(|(a, b)| a < b), "{agent} modes in MODES order");
        }
    }

    #[test]
    fn defaults_fill_gaps() {
        let d = c(Some("edits"), Some("opus"), None);
        assert_eq!(c(None, Some("haiku"), Some("low")).or(&d), c(Some("edits"), Some("haiku"), Some("low")));
        assert!(Controls::default().is_empty());
    }
}
