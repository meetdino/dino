//! Agent controls: permission mode, model and effort, in dino's own words, and how each agent
//! takes them on its command line. An agent without a knob simply doesn't offer it.

use crate::models::{Catalog, ModelInfo};
use serde::{Deserialize, Serialize};

/// What a session was asked to run with; `None` leaves it to the agent's own default.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Controls {
    /// One of `MODES`' ids.
    pub mode: Option<String>,
    /// A model the agent lists, an alias it knows ("opus"), or a name typed by hand.
    pub model: Option<String>,
    /// One of the efforts the model takes.
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
    /// The models the agent's own files list, in its order; empty when it keeps no list, and
    /// the model is typed by hand.
    pub models: Vec<ModelInfo>,
    /// The model it starts with when none is chosen, when its settings say.
    pub default_model: Option<String>,
    /// Every effort level its models take, lowest first; empty when there's no such knob.
    pub efforts: Vec<String>,
    /// Changing a control restarts the agent (resuming its conversation) rather than applying live.
    pub restart: bool,
}

impl Knobs {
    /// The efforts `model` takes (the agent's default when `None`); all of them when the model
    /// isn't one it lists.
    pub fn efforts_for(&self, model: Option<&str>) -> &[String] {
        match model.or(self.default_model.as_deref()).and_then(|m| self.models.iter().find(|x| x.named(m))) {
            Some(m) => &m.efforts,
            None => &self.efforts,
        }
    }

    /// `effort` for `model`: as is when it takes it, else the nearest level below that it does
    /// (the way Claude falls back itself), else none.
    pub fn effort(&self, model: Option<&str>, effort: &str) -> Option<String> {
        let takes = self.efforts_for(model);
        if takes.iter().any(|e| e == effort) {
            return Some(effort.to_string());
        }
        let rank = |e: &str| self.efforts.iter().position(|x| x == e);
        let wanted = rank(effort)?;
        takes.iter().filter(|e| rank(e).is_some_and(|r| r < wanted)).max_by_key(|e| rank(e)).cloned()
    }
}

fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| s.to_string()).collect()
}

/// The controls `agent_id` offers, its models and efforts from `catalog` (see `models`): without
/// one, the model is typed and there's no effort to pick. `bypass` is left out unless the
/// policies allow it.
pub fn knobs(agent_id: &str, allow_bypass: bool, catalog: Option<&Catalog>) -> Knobs {
    let listed = catalog.map(|c| Knobs { models: c.models.clone(), default_model: c.default_model.clone(), efforts: c.efforts(), ..Knobs::default() }).unwrap_or_default();
    let mut k = match agent_id {
        "claude" => Knobs { modes: strings(&["ask", "edits", "plan", "auto", "bypass"]), model: true, restart: true, ..listed },
        // The free tier picks the model for each turn.
        "claude-free" => Knobs { model: false, models: vec![], default_model: None, ..knobs("claude", true, catalog) },
        "codex" => Knobs { modes: strings(&["ask", "edits", "auto", "bypass"]), model: true, restart: true, ..listed },
        "gemini" => Knobs { modes: strings(&["ask", "edits", "plan", "bypass"]), model: true, restart: true, ..Knobs::default() },
        _ => Knobs::default(),
    };
    if !allow_bypass {
        k.modes.retain(|m| m != "bypass");
    }
    k
}

/// Command-line arguments that apply `c` to an agent offering `k` (see `knobs`). Values it
/// doesn't offer are dropped rather than passed on to fail; an effort the model doesn't take
/// becomes the nearest one below that it does.
pub fn args(agent_id: &str, c: &Controls, k: &Knobs) -> Vec<String> {
    let mode = c.mode.as_deref().filter(|m| k.modes.iter().any(|x| x == m));
    let model = c.model.as_deref().map(str::trim).filter(|m| k.model && !m.is_empty());
    let effort = c.effort.as_deref().and_then(|e| k.effort(model, e));
    let effort = effort.as_deref();
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

/// One of Claude's permission modes in dino's words; `dontAsk` has none.
pub fn claude_mode(m: &str) -> Option<String> {
    let id = match m {
        "default" | "manual" => "ask",
        "acceptEdits" => "edits",
        "plan" => "plan",
        "auto" => "auto",
        "bypassPermissions" => "bypass",
        _ => return None,
    };
    Some(id.into())
}

/// A flag in `args` at `i`: its name and value, and how many arguments it takes up. `value_flags`
/// take a value, as the next argument or after `=`.
fn flag_at<'a>(args: &'a [String], i: usize, value_flags: &[&str]) -> Option<(&'a str, Option<&'a str>, usize)> {
    let a = args[i].as_str();
    if let Some((name, v)) = a.split_once('=').filter(|(n, _)| n.starts_with("--")) {
        return Some((name, Some(v), 1));
    }
    if value_flags.contains(&a) {
        return Some((a, args.get(i + 1).map(String::as_str), 2));
    }
    a.starts_with('-').then_some((a, None, 1))
}

/// Which control a flag of `agent_id`'s sets, if any.
fn control_of(agent_id: &str, name: &str, value: Option<&str>) -> Option<ControlKind> {
    use ControlKind::*;
    match (agent_id, name) {
        ("claude" | "claude-free", "--permission-mode" | "--dangerously-skip-permissions") => Some(Mode),
        ("claude" | "claude-free", "--model") => Some(Model),
        ("claude" | "claude-free", "--effort") => Some(Effort),
        ("codex", "-s" | "--sandbox" | "-a" | "--ask-for-approval" | "--approve-for-me" | "--full-auto" | "--dangerously-bypass-approvals-and-sandbox" | "--yolo") => Some(Mode),
        ("codex", "-m" | "--model") => Some(Model),
        ("codex", "-c" | "--config") if value.is_some_and(|v| v.trim_start().starts_with("model_reasoning_effort")) => Some(Effort),
        ("gemini", "--approval-mode" | "-y" | "--yolo") => Some(Mode),
        ("gemini", "-m" | "--model") => Some(Model),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ControlKind {
    Mode,
    Model,
    Effort,
}

const VALUE_FLAGS: &[&str] = &["--permission-mode", "--model", "--effort", "-s", "--sandbox", "-a", "--ask-for-approval", "-m", "-c", "--config", "--approval-mode"];

/// What `args` ask `agent_id` for, in dino's words: a session started with
/// `--dangerously-skip-permissions` is in bypass. A mode dino can't name stays `None`.
pub fn from_args(agent_id: &str, args: &[String]) -> Controls {
    let mut c = Controls::default();
    let (mut sandbox, mut approval, mut mode_flags) = (None, None, 0);
    let mut i = 0;
    while i < args.len() {
        let Some((name, value, n)) = flag_at(args, i, VALUE_FLAGS) else {
            i += 1;
            continue;
        };
        let kind = control_of(agent_id, name, value);
        match (agent_id, kind, name) {
            (_, Some(ControlKind::Model), _) => c.model = value.map(String::from),
            ("codex", Some(ControlKind::Effort), _) => {
                c.effort = value.and_then(|v| v.split_once('=')).map(|(_, e)| e.trim().trim_matches('"').to_string())
            }
            (_, Some(ControlKind::Effort), _) => c.effort = value.map(String::from),
            ("claude" | "claude-free", Some(ControlKind::Mode), "--dangerously-skip-permissions") => c.mode = Some("bypass".into()),
            ("claude" | "claude-free", Some(ControlKind::Mode), _) => c.mode = value.and_then(claude_mode),
            ("codex", Some(ControlKind::Mode), _) => {
                mode_flags += 1;
                match name {
                    "-s" | "--sandbox" => sandbox = value,
                    "-a" | "--ask-for-approval" => approval = value,
                    "--approve-for-me" => c.mode = Some("auto".into()),
                    "--full-auto" => c.mode = Some("edits".into()),
                    _ => c.mode = Some("bypass".into()),
                }
            }
            ("gemini", Some(ControlKind::Mode), _) => {
                c.mode = match (name, value) {
                    ("--approval-mode", Some("default")) => Some("ask".into()),
                    ("--approval-mode", Some("auto_edit")) => Some("edits".into()),
                    ("--approval-mode", Some("plan")) => Some("plan".into()),
                    ("--approval-mode", Some("yolo")) | ("-y" | "--yolo", _) => Some("bypass".into()),
                    _ => None,
                }
            }
            _ => {}
        }
        i += if kind.is_some() { n } else { 1 };
    }
    // Codex's sandbox and approval flags name a mode only together, the way `args` writes them.
    if agent_id == "codex" && (sandbox.is_some() || approval.is_some()) {
        c.mode = match (sandbox, approval, mode_flags) {
            (Some("read-only"), Some("on-request"), 2) => Some("ask".into()),
            (Some("workspace-write"), Some("on-request"), 2) => Some("edits".into()),
            _ => None,
        };
    }
    c
}

/// `args` without the flags for what `c` sets, so a control chosen later isn't overruled by the
/// flag the session was started with.
pub fn without(agent_id: &str, args: &[String], c: &Controls) -> Vec<String> {
    let set = |k: ControlKind| match k {
        ControlKind::Mode => c.mode.is_some(),
        ControlKind::Model => c.model.is_some(),
        ControlKind::Effort => c.effort.is_some(),
    };
    let mut out = vec![];
    let mut i = 0;
    while i < args.len() {
        match flag_at(args, i, VALUE_FLAGS) {
            Some((name, value, n)) if control_of(agent_id, name, value).is_some_and(set) => i += n,
            _ => {
                out.push(args[i].clone());
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(mode: Option<&str>, model: Option<&str>, effort: Option<&str>) -> Controls {
        Controls { mode: mode.map(Into::into), model: model.map(Into::into), effort: effort.map(Into::into) }
    }

    /// What the agent's own files list, from copies of real ones.
    fn catalog(agent: &str) -> Option<Catalog> {
        match agent {
            "claude" | "claude-free" => crate::models::claude(include_str!("../tests/fixtures/claude-catalog.json"), None, None),
            "codex" => crate::models::codex(include_str!("../tests/fixtures/codex-models.json"), None),
            _ => None,
        }
    }

    fn k(agent: &str) -> Knobs {
        knobs(agent, true, catalog(agent).as_ref())
    }

    #[test]
    fn claude_flags() {
        let claude = k("claude");
        assert_eq!(args("claude", &Controls::default(), &claude), Vec::<String>::new());
        assert_eq!(
            args("claude", &c(Some("ask"), Some("sonnet"), Some("high")), &claude),
            ["--permission-mode", "manual", "--model", "sonnet", "--effort", "high"]
        );
        assert_eq!(args("claude", &c(Some("bypass"), None, None), &claude), ["--permission-mode", "bypassPermissions"]);
        // Haiku takes no effort; an older Opus has no xhigh, so it gets the level below.
        assert_eq!(args("claude", &c(None, Some("haiku"), Some("high")), &claude), ["--model", "haiku"]);
        assert_eq!(args("claude", &c(None, Some("claude-opus-4-6"), Some("xhigh")), &claude), ["--model", "claude-opus-4-6", "--effort", "high"]);
        // A model typed by hand takes any level its models do.
        assert_eq!(args("claude", &c(None, Some("claude-next"), Some("max")), &claude), ["--model", "claude-next", "--effort", "max"]);
        // The free tier routes models itself.
        assert_eq!(args("claude-free", &c(Some("plan"), Some("opus"), None), &k("claude-free")), ["--permission-mode", "plan"]);
        // Unknown values never reach the agent.
        assert_eq!(args("claude", &c(Some("yolo"), Some("  "), Some("huge")), &claude), Vec::<String>::new());
        // Without its catalog there's no effort to be sure of.
        assert_eq!(args("claude", &c(None, Some("opus"), Some("high")), &knobs("claude", true, None)), ["--model", "opus"]);
    }

    #[test]
    fn codex_and_gemini_flags() {
        let codex = k("codex");
        assert_eq!(args("codex", &c(Some("edits"), Some("gpt-5.5"), Some("low")), &codex), ["-s", "workspace-write", "-a", "on-request", "-m", "gpt-5.5", "-c", "model_reasoning_effort=\"low\""]);
        assert_eq!(args("codex", &c(None, Some("gpt-5.5"), Some("ultra")), &codex), ["-m", "gpt-5.5", "-c", "model_reasoning_effort=\"xhigh\""], "the most it takes");
        assert_eq!(args("codex", &c(Some("plan"), None, None), &codex), Vec::<String>::new(), "codex has no plan mode");
        assert_eq!(args("codex", &c(Some("bypass"), None, None), &codex), ["--dangerously-bypass-approvals-and-sandbox"]);
        assert_eq!(args("gemini", &c(Some("edits"), Some("gemini-2.5-pro"), Some("high")), &k("gemini")), ["--approval-mode", "auto_edit", "-m", "gemini-2.5-pro"]);
        assert_eq!(args("aider", &c(Some("ask"), Some("x"), Some("high")), &k("aider")), Vec::<String>::new());
    }

    #[test]
    fn efforts_follow_the_model() {
        let claude = k("claude");
        assert!(claude.efforts_for(Some("haiku")).is_empty());
        assert_eq!(claude.efforts_for(None), claude.efforts_for(Some("claude-opus-5-5")), "the agent's own default model");
        assert_eq!(claude.efforts, ["low", "medium", "high", "xhigh", "max"]);
        let codex = k("codex");
        assert_eq!(codex.efforts_for(Some("gpt-5.6-terra")).last().map(String::as_str), Some("ultra"));
        assert_eq!(codex.effort(Some("gpt-5.5"), "max").as_deref(), Some("xhigh"));
        assert_eq!(codex.effort(Some("gpt-5.5"), "minimal"), None, "no level of Codex's");
    }

    #[test]
    fn knobs_follow_policy() {
        assert!(!knobs("claude", false, None).modes.contains(&"bypass".to_string()));
        assert!(k("claude").modes.contains(&"bypass".to_string()));
        assert!(!k("claude-free").model && k("claude-free").models.is_empty() && !k("claude-free").efforts.is_empty());
        assert!(k("gemini").efforts.is_empty() && k("gemini").models.is_empty());
        assert!(knobs("codex", true, None).models.is_empty() && knobs("codex", true, None).efforts.is_empty(), "nothing it doesn't list");
        assert_eq!(k("shell"), Knobs::default());
        let all: Vec<&str> = MODES.iter().map(|m| m.0).collect();
        for agent in ["claude", "codex", "gemini"] {
            let k = k(agent);
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

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn controls_in_the_args() {
        assert_eq!(from_args("claude", &v(&["--dangerously-skip-permissions"])), c(Some("bypass"), None, None));
        assert_eq!(from_args("claude", &v(&["--permission-mode=plan", "--model", "opus", "--effort", "high", "fix it"])), c(Some("plan"), Some("opus"), Some("high")));
        assert_eq!(from_args("claude", &v(&["--allow-dangerously-skip-permissions"])), Controls::default(), "only allows it");
        assert_eq!(from_args("codex", &v(&["--yolo", "-m", "gpt-5", "-c", "model_reasoning_effort=\"high\""])), c(Some("bypass"), Some("gpt-5"), Some("high")));
        assert_eq!(from_args("codex", &v(&["-s", "read-only", "-a", "on-request"])), c(Some("ask"), None, None));
        assert_eq!(from_args("codex", &v(&["-s", "danger-full-access"])), Controls::default(), "no mode of dino's");
        assert_eq!(from_args("gemini", &v(&["-y"])), c(Some("bypass"), None, None));
        // Every mode dino passes reads back as itself.
        for agent in ["claude", "codex", "gemini"] {
            let k = k(agent);
            for m in &k.modes {
                let want = c(Some(m), None, None);
                assert_eq!(from_args(agent, &args(agent, &want, &k)), want, "{agent} {m}");
            }
        }
    }

    #[test]
    fn chosen_controls_replace_the_args_flags() {
        let started = v(&["--dangerously-skip-permissions", "--model=opus", "--verbose", "hi"]);
        assert_eq!(without("claude", &started, &c(Some("ask"), None, None)), v(&["--model=opus", "--verbose", "hi"]));
        assert_eq!(without("claude", &started, &c(None, Some("haiku"), None)), v(&["--dangerously-skip-permissions", "--verbose", "hi"]));
        let codex = v(&["-s", "read-only", "-a", "on-request", "-c", "model_reasoning_effort=high", "-c", "x=1"]);
        assert_eq!(without("codex", &codex, &c(Some("bypass"), None, None)), v(&["-c", "model_reasoning_effort=high", "-c", "x=1"]));
        assert_eq!(without("codex", &codex, &c(None, None, Some("low"))), v(&["-s", "read-only", "-a", "on-request", "-c", "x=1"]));
    }
}
