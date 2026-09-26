//! The `auto` model: picks a free model per request from a tiered pool, learning from failures
//! and latency as it goes. Pure logic; the proxy does the I/O.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Background chores, titles, trivial questions.
    Fast,
    /// Normal agentic coding.
    Code,
    /// Planning, debugging, architecture.
    Reason,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Code => "code",
            Tier::Reason => "reason",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Model {
    pub provider: &'static str,
    pub id: &'static str,
}

impl Model {
    /// `z-ai/glm-5.3` → `glm-5.3`
    pub fn short(&self) -> &'static str {
        self.id.rsplit('/').next().unwrap_or(self.id)
    }
}

const fn nim(id: &'static str) -> Model {
    Model { provider: "nvidia", id }
}

/// Preference order per tier, from probing tool-call support and latency on NVIDIA's free tier.
fn pool(tier: Tier) -> Vec<Model> {
    match tier {
        Tier::Fast => vec![nim("nvidia/nemotron-3-super-120b-a12b"), nim("openai/gpt-oss-20b")],
        Tier::Code => vec![nim("z-ai/glm-5.3"), nim("moonshotai/kimi-k3"), nim("nvidia/nemotron-3-super-120b-a12b")],
        Tier::Reason => vec![nim("nvidia/nemotron-3-ultra-550b-a55b"), nim("moonshotai/kimi-k3"), nim("z-ai/glm-5.3")],
    }
}

pub const CLASSIFIER: Model = nim("openai/gpt-oss-20b");

#[derive(Default)]
struct Health {
    cool_until: Option<Instant>,
    /// Smoothed time to first byte.
    latency_ms: Option<f64>,
    failures: u32,
}

#[derive(Default)]
pub struct Router {
    health: Mutex<HashMap<&'static str, Health>>,
    /// Tier chosen at the start of each session's current turn.
    sticky: Mutex<HashMap<String, Tier>>,
}

impl Router {
    /// Models to try for `tier`, best first: healthy before cooling, then preference,
    /// demoting a model that has been much slower than its peers.
    pub fn candidates(&self, tier: Tier) -> Vec<Model> {
        let health = self.health.lock().unwrap();
        let now = Instant::now();
        let mut ranked: Vec<(usize, Model)> = pool(tier).into_iter().enumerate().collect();
        ranked.sort_by_key(|(pref, m)| {
            let h = health.get(m.id);
            let cooling = h.and_then(|h| h.cool_until).is_some_and(|t| t > now);
            let slow = h.and_then(|h| h.latency_ms).is_some_and(|l| l > 15_000.0);
            (cooling, slow, *pref)
        });
        ranked.into_iter().map(|(_, m)| m).collect()
    }

    pub fn record_ok(&self, model: &Model, ttfb: Duration) {
        let mut health = self.health.lock().unwrap();
        let h = health.entry(model.id).or_default();
        let ms = ttfb.as_secs_f64() * 1000.0;
        h.latency_ms = Some(h.latency_ms.map_or(ms, |l| l * 0.7 + ms * 0.3));
        h.failures = 0;
        h.cool_until = None;
    }

    /// Back off a failing model: 30s, 60s, 120s… capped at 10 minutes.
    pub fn record_failure(&self, model: &Model) {
        let mut health = self.health.lock().unwrap();
        let h = health.entry(model.id).or_default();
        h.failures += 1;
        let secs = (30u64 << (h.failures - 1).min(5)).min(600);
        h.cool_until = Some(Instant::now() + Duration::from_secs(secs));
    }

    pub fn set_turn_tier(&self, session: &str, tier: Tier) {
        self.sticky.lock().unwrap().insert(session.to_string(), tier);
    }

    pub fn turn_tier(&self, session: &str) -> Option<Tier> {
        self.sticky.lock().unwrap().get(session).copied()
    }
}

/// Decide without a model call when the request makes it obvious.
pub fn heuristic_tier(anthropic_req: &Value) -> Option<Tier> {
    let model = anthropic_req["model"].as_str().unwrap_or_default();
    let has_tools = anthropic_req["tools"].as_array().is_some_and(|t| !t.is_empty());
    // Claude Code routes its background chores (titles, summaries) to the small model.
    if model.contains("fast") || model.contains("haiku") || !has_tools {
        return Some(Tier::Fast);
    }
    None
}

/// The user's text if this request starts a new turn (the last message is typed by the user,
/// not a tool result coming back mid-turn).
pub fn new_turn_text(anthropic_req: &Value) -> Option<String> {
    // Some clients append environment context as a trailing system message; skip past it.
    let last = anthropic_req["messages"].as_array()?.iter().rev().find(|m| m["role"] != "system")?;
    if last["role"] != "user" {
        return None;
    }
    match &last["content"] {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b["type"] == "tool_result") {
                return None;
            }
            let text: Vec<&str> = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
            // Claude Code prepends <system-reminder> blocks; judge by what the human wrote.
            let human: Vec<&str> = text.iter().copied().filter(|t| !t.trim_start().starts_with("<system-reminder>")).collect();
            let chosen = if human.is_empty() { text } else { human };
            (!chosen.is_empty()).then(|| chosen.join("\n"))
        }
        _ => None,
    }
}

/// OpenAI chat request asking the classifier for a one-word label.
pub fn classifier_request(user_text: &str) -> Value {
    let text: String = user_text.chars().take(2000).collect();
    json!({
        "model": CLASSIFIER.id,
        "max_tokens": 200,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": "You route requests for a coding agent. Reply with exactly one word:\n\
                fast - greetings, trivial questions, tiny edits\n\
                code - normal programming work: implement, edit, test, explain code\n\
                reason - hard problems: architecture, planning, tricky debugging, deep analysis"},
            {"role": "user", "content": text}
        ]
    })
}

pub fn parse_label(text: &str) -> Option<Tier> {
    let t = text.to_lowercase();
    // Last mention wins, so reasoning-model preambles don't confuse it.
    [("fast", Tier::Fast), ("code", Tier::Code), ("reason", Tier::Reason)]
        .into_iter()
        .filter_map(|(w, tier)| t.rfind(w).map(|i| (i, tier)))
        .max_by_key(|(i, _)| *i)
        .map(|(_, tier)| tier)
}
