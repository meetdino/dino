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
    /// Coupled, multi-file technical work.
    Complex,
    /// Planning, debugging, architecture.
    Reason,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Code => "code",
            Tier::Complex => "complex",
            Tier::Reason => "reason",
        }
    }
}

/// One of the provider's models.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub id: String,
}

impl Model {
    /// `z-ai/glm-5.3` → `glm-5.3`
    pub fn short(&self) -> &str {
        self.id.rsplit('/').next().unwrap_or(&self.id)
    }
}

/// What dino knows about one of the provider's models: what it learned by trying it, and the
/// provider's own ranking and output limit where the provider publishes them. No table.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Probe {
    pub id: String,
    /// It answered a chat request.
    pub answers: bool,
    /// It called a tool when given one.
    pub tools: bool,
    /// How long its answer took.
    pub ms: f64,
    /// Its place in the provider's featured list, best first.
    pub rank: Option<usize>,
    /// It has been tried: what `answers`, `tools` and `ms` say was learned, not assumed.
    pub tried: bool,
    /// Most output tokens it takes, as the provider publishes it or as it said when refusing more.
    pub max_output: Option<u64>,
}

#[derive(Default)]
struct Health {
    cool_until: Option<Instant>,
    /// Smoothed time to first byte.
    latency_ms: Option<f64>,
    failures: u32,
}

#[derive(Default)]
pub struct Router {
    health: Mutex<HashMap<String, Health>>,
    /// The provider's models as the proxy last found them.
    probes: Mutex<Vec<Probe>>,
    /// Tier chosen for each session's current turn, keyed by the classifier input it came from.
    sticky: Mutex<HashMap<String, (u64, Tier)>>,
}

impl Router {
    pub fn set_probes(&self, probes: Vec<Probe>) {
        *self.probes.lock().unwrap() = probes;
    }

    pub fn probes(&self) -> Vec<Probe> {
        self.probes.lock().unwrap().clone()
    }

    /// Preference order for `tier`: only models that call tools; quick chores to the quickest,
    /// everything else by the provider's own ranking, then speed. After them, the provider's own
    /// picks not tried yet, by its ranking: until they have been (the free tier was just turned on,
    /// and trying them all takes a minute or more), a request goes to them rather than nowhere.
    fn pool(&self, tier: Tier) -> Vec<Model> {
        let probes = self.probes.lock().unwrap();
        let mut usable: Vec<&Probe> = probes.iter().filter(|p| p.tools).collect();
        let by_speed = |a: &&Probe, b: &&Probe| a.ms.total_cmp(&b.ms);
        match tier {
            Tier::Fast => usable.sort_by(by_speed),
            _ => usable.sort_by(|a, b| a.rank.unwrap_or(usize::MAX).cmp(&b.rank.unwrap_or(usize::MAX)).then(by_speed(a, b))),
        }
        let mut untried: Vec<&Probe> = probes.iter().filter(|p| !p.tried && p.rank.is_some()).collect();
        untried.sort_by_key(|p| p.rank);
        usable.into_iter().chain(untried).map(|p| Model { id: p.id.clone() }).collect()
    }

    /// The quickest model that answers, to classify requests with.
    pub fn classifier(&self) -> Option<Model> {
        let probes = self.probes.lock().unwrap();
        probes.iter().filter(|p| p.answers).min_by(|a, b| a.ms.total_cmp(&b.ms)).map(|p| Model { id: p.id.clone() })
    }

    /// Most output tokens `model` takes, when known.
    pub fn max_output(&self, model: &Model) -> Option<u64> {
        self.probes.lock().unwrap().iter().find(|p| p.id == model.id).and_then(|p| p.max_output)
    }

    /// `model` refused more than `tokens` of output.
    pub fn learn_max_output(&self, model: &Model, tokens: u64) {
        if let Some(p) = self.probes.lock().unwrap().iter_mut().find(|p| p.id == model.id) {
            p.max_output = Some(tokens);
        }
    }

    /// Models to try for `tier`, best first: healthy before cooling, then preference,
    /// demoting a model that has been much slower than its peers.
    pub fn candidates(&self, tier: Tier) -> Vec<Model> {
        let mut ranked: Vec<(usize, Model)> = self.pool(tier).into_iter().enumerate().collect();
        let health = self.health.lock().unwrap();
        let now = Instant::now();
        ranked.sort_by_key(|(pref, m)| {
            let h = health.get(&m.id);
            let cooling = h.and_then(|h| h.cool_until).is_some_and(|t| t > now);
            let slow = h.and_then(|h| h.latency_ms).is_some_and(|l| l > 15_000.0);
            (cooling, slow, *pref)
        });
        ranked.into_iter().map(|(_, m)| m).collect()
    }

    pub fn record_ok(&self, model: &Model, ttfb: Duration) {
        let mut health = self.health.lock().unwrap();
        let h = health.entry(model.id.clone()).or_default();
        let ms = ttfb.as_secs_f64() * 1000.0;
        h.latency_ms = Some(h.latency_ms.map_or(ms, |l| l * 0.7 + ms * 0.3));
        h.failures = 0;
        h.cool_until = None;
    }

    /// Back off a failing model: 30s, 60s, 120s… capped at 10 minutes.
    pub fn record_failure(&self, model: &Model) {
        let mut health = self.health.lock().unwrap();
        let h = health.entry(model.id.clone()).or_default();
        h.failures += 1;
        let secs = (30u64 << (h.failures - 1).min(5)).min(600);
        h.cool_until = Some(Instant::now() + Duration::from_secs(secs));
    }

    pub fn set_turn_tier(&self, session: &str, state: &str, tier: Tier) {
        self.sticky.lock().unwrap().insert(session.to_string(), (hash(state), tier));
    }

    /// The current turn's tier; with `state`, only if it was decided for that exact input.
    pub fn turn_tier(&self, session: &str, state: Option<&str>) -> Option<Tier> {
        let sticky = self.sticky.lock().unwrap();
        let &(h, tier) = sticky.get(session)?;
        state.is_none_or(|s| hash(s) == h).then_some(tier)
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
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

impl Tier {
    /// One step stronger, for when the classifier isn't sure.
    pub fn up(self) -> Tier {
        match self {
            Tier::Fast => Tier::Code,
            Tier::Code => Tier::Complex,
            Tier::Complex | Tier::Reason => Tier::Reason,
        }
    }
}

/// What the classifier sees: the current request plus up to 3 earlier user turns, newest last,
/// capped at 8k chars. Assistant turns are left out (LiteLLM's tested default for Jev).
pub fn classifier_state(anthropic_req: &Value) -> Option<String> {
    let current = new_turn_text(anthropic_req)?;
    let msgs = anthropic_req["messages"].as_array()?;
    let mut earlier: Vec<String> = msgs.iter().filter(|m| m["role"] == "user").filter_map(|m| new_turn_text(&json!({ "messages": [m] }))).collect();
    earlier.pop(); // that's `current`
    let start = earlier.len().saturating_sub(3);
    let mut state = String::new();
    for (i, t) in earlier[start..].iter().enumerate() {
        state.push_str(&format!("Earlier request {}:\n{t}\n\n", i + 1));
    }
    state.push_str(&format!("Current request:\n{current}"));
    let chars = state.chars().count();
    // Keep the end: the current request matters most.
    Some(if chars > 8000 { state.chars().skip(chars - 8000).collect() } else { state })
}

pub const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
/// Pinned so behavior doesn't shift when the `jev-latest` alias moves.
pub const JEV_MODEL: &str = "jev-1.13.0";

/// TypeSafe Jev "choice" request: picks a tier with probabilities instead of generating text.
pub fn jev_request(state: &str) -> Value {
    json!({
        "model": JEV_MODEL,
        "state": state,
        "questions": {
            "tier": {
                "type": "choice",
                "instructions": "A developer is talking to an AI coding agent. Decide how capable a model the agent needs to handle the current request well.",
                "criteria": {
                    "simple": "Greetings, lookups, trivial questions, tiny mechanical edits.",
                    "medium": "Routine programming: implement a small feature, edit a function, write a test, explain code.",
                    "complex": "Coupled technical work across several files or systems, refactors, non-trivial bugs.",
                    "reasoning": "Architecture and design decisions, tricky debugging, proofs, trade-offs between conflicting goals."
                }
            }
        }
    })
}

/// Tier from a Jev response, bumped up a step when confidence is below 0.5.
pub fn parse_jev(resp: &Value) -> Option<(Tier, f64)> {
    let answer = &resp["answers"]["tier"];
    let tier = match answer["choice"].as_str()? {
        "simple" => Tier::Fast,
        "medium" => Tier::Code,
        "complex" => Tier::Complex,
        "reasoning" => Tier::Reason,
        _ => return None,
    };
    let confidence = answer["confidence"].as_f64().unwrap_or(1.0);
    Some((if confidence < 0.5 { tier.up() } else { tier }, confidence))
}

/// OpenAI chat request asking classifier `model` for a one-word label.
pub fn classifier_request(model: &Model, user_text: &str) -> Value {
    let text: String = user_text.chars().take(2000).collect();
    json!({
        "model": model.id,
        "max_tokens": 200,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": "You route requests for a coding agent. Reply with exactly one word:\n\
                fast - greetings, trivial questions, tiny edits\n\
                code - normal programming work: implement, edit, test, explain code\n\
                complex - coupled work across several files or systems, refactors, non-trivial bugs\n\
                reason - hard problems: architecture, planning, tricky debugging, deep analysis"},
            {"role": "user", "content": text}
        ]
    })
}

pub fn parse_label(text: &str) -> Option<Tier> {
    let t = text.to_lowercase();
    // Last mention wins, so reasoning-model preambles don't confuse it.
    [("fast", Tier::Fast), ("code", Tier::Code), ("complex", Tier::Complex), ("reason", Tier::Reason)]
        .into_iter()
        .filter_map(|(w, tier)| t.rfind(w).map(|i| (i, tier)))
        .max_by_key(|(i, _)| *i)
        .map(|(_, tier)| tier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_is_what_was_learned() {
        let r = Router::default();
        assert!(r.candidates(Tier::Code).is_empty() && r.classifier().is_none(), "nothing known, nothing offered");
        let p = |id: &str, tools: bool, ms: f64, rank: Option<usize>| Probe { id: id.into(), answers: true, tools, ms, rank, tried: true, max_output: None };
        r.set_probes(vec![p("a/slow-top", true, 9000.0, Some(0)), p("b/quick", true, 800.0, None), p("c/no-tools", false, 200.0, None), p("d/mid", true, 2000.0, None)]);
        let ids = |t| r.candidates(t).into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(Tier::Fast), ["b/quick", "d/mid", "a/slow-top"], "chores to the quickest");
        assert_eq!(ids(Tier::Complex), ["a/slow-top", "b/quick", "d/mid"], "the provider's pick first");
        assert_eq!(r.classifier().unwrap().id, "c/no-tools", "classifying needs no tools");
        r.learn_max_output(&Model { id: "b/quick".into() }, 4096);
        assert_eq!(r.max_output(&Model { id: "b/quick".into() }), Some(4096));
        r.record_failure(&Model { id: "b/quick".into() });
        assert_eq!(ids(Tier::Fast), ["d/mid", "a/slow-top", "b/quick"], "a failing model waits");
    }

    /// Just turned on, before its models have been tried: the provider's own picks, by its
    /// ranking, rather than nothing; once tried, only those that called a tool.
    #[test]
    fn before_its_models_are_tried_the_providers_picks_are_offered() {
        let r = Router::default();
        let untried = |id: &str, rank: Option<usize>| Probe { id: id.into(), ms: f64::MAX, rank, ..Probe::default() };
        r.set_probes(vec![untried("x/embed", None), untried("b/second", Some(1)), untried("a/first", Some(0))]);
        let ids = |t| r.candidates(t).into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(Tier::Code), ["a/first", "b/second"]);
        assert_eq!(ids(Tier::Fast), ["a/first", "b/second"]);
        let worked = Probe { id: "c/tried".into(), answers: true, tools: true, ms: 900.0, rank: None, tried: true, max_output: None };
        let failed = Probe { id: "b/second".into(), ms: 45_000.0, rank: Some(1), tried: true, ..Probe::default() };
        r.set_probes(vec![untried("a/first", Some(0)), failed, worked]);
        assert_eq!(ids(Tier::Code), ["c/tried", "a/first"], "known to work first; one that didn't, never");
    }

    #[test]
    fn jev_response_parsing_and_confidence_bump() {
        let sure = json!({"answers": {"tier": {"type": "choice", "choice": "medium", "confidence": 0.9, "probabilities": {}}}});
        assert_eq!(parse_jev(&sure), Some((Tier::Code, 0.9)));
        let unsure = json!({"answers": {"tier": {"choice": "medium", "confidence": 0.4}}});
        assert_eq!(parse_jev(&unsure).map(|t| t.0), Some(Tier::Complex));
        assert_eq!(parse_jev(&json!({"answers": {}})), None);
    }

    #[test]
    fn state_skips_tool_results_and_trailing_system() {
        let req = json!({"messages": [
            {"role": "user", "content": "first ask"},
            {"role": "assistant", "content": "ok"},
            {"role": "user", "content": [{"type": "tool_result", "content": "x"}]},
            {"role": "user", "content": [{"type": "text", "text": "<system-reminder>ctx</system-reminder>"}, {"type": "text", "text": "second ask"}]},
            {"role": "system", "content": "env"}
        ]});
        let state = classifier_state(&req).unwrap();
        assert!(state.contains("Earlier request 1:\nfirst ask"));
        assert!(state.ends_with("Current request:\nsecond ask"));
        assert!(!state.contains("ok") && !state.contains("ctx"));
    }
}
