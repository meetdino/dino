//! Which agents can use a model, and which one to use it in. What the provider says about the
//! model comes first (the API shapes it serves, tool calling, context); what the provider can't
//! say comes from one curated file, `compat.json`, which can only add caveats, lower a verdict or
//! fill in a recommendation. Nothing about models lives in code.

use serde::{Deserialize, Deserializer, Serialize};

use crate::providers::{Format, ProviderInfo, ProviderModel};

/// How well an agent does with a model, best first.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Works,
    Caveat,
    No,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Reason {
    pub text: String,
    pub source: Option<String>,
}

/// An agent and a model: whether it works, why not, and whether it's the one to pick.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Verdict {
    pub agent: String,
    /// As people know it: "Qwen Code".
    pub name: String,
    pub status: Status,
    pub reasons: Vec<Reason>,
    /// The format it would talk to the provider in.
    pub via: Option<Format>,
    /// dino translates between what the agent speaks and what the provider serves.
    pub translated: bool,
    /// The agent to run this model in; the first of the list.
    pub recommended: bool,
}

/// `compat.json`: what no provider reports.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Compat {
    pub version: u32,
    pub updated: String,
    /// In the order dino prefers them when nothing else decides.
    #[serde(deserialize_with = "ordered")]
    pub agents: Vec<(String, AgentNeeds)>,
    pub models: Vec<ModelNote>,
    pub runtimes: Vec<RuntimeNote>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct AgentNeeds {
    pub formats: Vec<Format>,
    /// Below this it's cramped; below `min_context_hard` it doesn't work.
    pub min_context: u64,
    pub min_context_hard: Option<u64>,
    /// "tools".
    pub needs: Vec<String>,
    pub notes: Option<String>,
    pub source: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ModelNote {
    /// Globs on the model's id, `|`-separated, case ignored.
    #[serde(rename = "match")]
    pub pattern: String,
    /// Only on these providers ("ollama", "openrouter"…); empty: anywhere.
    #[serde(rename = "where")]
    pub providers: Vec<String>,
    /// Agents to run it in, best first.
    pub recommend: Vec<String>,
    pub why: Option<String>,
    pub source: Option<String>,
    pub agents: std::collections::BTreeMap<String, Note>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Note {
    pub status: Status,
    pub why: String,
    pub source: Option<String>,
    /// Who or what confirmed it, and when.
    pub verified_by: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct RuntimeNote {
    pub runtime: String,
    /// An agent id, or "*".
    pub agent: String,
    pub status: Status,
    pub why: String,
    pub source: Option<String>,
    /// Fixed in this version: older ones (or ones that don't say) still have it.
    pub until_version: Option<String>,
}

fn ordered<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, AgentNeeds)>, D::Error> {
    let map = serde_json::Map::<String, serde_json::Value>::deserialize(d)?;
    map.into_iter().map(|(k, v)| serde_json::from_value(v).map(|n| (k, n)).map_err(serde::de::Error::custom)).collect()
}

const BUNDLED: &str = include_str!("../compat.json");

/// What dino's proxy can translate: an agent speaking the first to a provider serving the second.
const TRANSLATES: &[(Format, Format)] = &[(Format::Anthropic, Format::Chat)];

impl Compat {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    /// The copy shipped with dino, or a newer one in dino's folder. That's where a copy fetched
    /// from dino's own API goes once there is one; nothing fetches it yet.
    pub fn load() -> Self {
        let bundled = Self::parse(BUNDLED).expect("bundled compat.json");
        let newer = std::fs::read_to_string(crate::config_dir().join("compat.json")).ok().and_then(|t| Self::parse(&t).ok());
        newer.filter(|n| n.version > bundled.version).unwrap_or(bundled)
    }

    fn needs(&self, agent: &str) -> Option<&AgentNeeds> {
        self.agents.iter().find(|(a, _)| a == agent).map(|(_, n)| n)
    }

    fn notes<'a>(&'a self, m: &'a ProviderModel) -> impl Iterator<Item = &'a ModelNote> + 'a {
        self.models.iter().filter(move |n| (n.providers.is_empty() || n.providers.contains(&m.provider)) && n.pattern.split('|').any(|g| glob(g.trim(), &m.id)))
    }

    /// Every agent in the file against model `m` on provider `p`, the one to use first.
    pub fn judge(&self, m: &ProviderModel, p: &ProviderInfo) -> Vec<Verdict> {
        let mut out: Vec<Verdict> = self.agents.iter().map(|(a, _)| self.judge_one(a, m, p)).collect();
        // Curated first, else the best verdict, spoken natively, with the fewest caveats.
        let curated: Vec<&String> = self.notes(m).flat_map(|n| n.recommend.iter()).collect();
        let rank = |v: &Verdict| {
            let pick = curated.iter().position(|a| **a == v.agent).unwrap_or(usize::MAX);
            (v.status, pick, v.translated, v.reasons.len())
        };
        let order: Vec<String> = self.agents.iter().map(|(a, _)| a.clone()).collect();
        out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| order.iter().position(|x| *x == a.agent).cmp(&order.iter().position(|x| *x == b.agent))));
        if let Some(first) = out.first_mut().filter(|v| v.status != Status::No) {
            first.recommended = true;
            if let Some(n) = self.notes(m).find(|n| n.recommend.first() == Some(&first.agent)) {
                if let Some(why) = &n.why {
                    first.reasons.insert(0, Reason { text: why.clone(), source: n.source.clone() });
                }
            }
        }
        out
    }

    fn judge_one(&self, agent: &str, m: &ProviderModel, p: &ProviderInfo) -> Verdict {
        let name = crate::KNOWN_AGENTS.iter().find(|k| k.id == agent).map_or(agent, |k| k.name).to_string();
        let mut v = Verdict { agent: agent.to_string(), name, ..Default::default() };
        let Some(needs) = self.needs(agent) else { return v };
        let lower = |v: &mut Verdict, status: Status, text: String, source: Option<String>| {
            v.status = v.status.max(status);
            v.reasons.push(Reason { text, source });
        };

        // 1. A shape both sides speak, or one dino translates.
        if p.formats.is_empty() {
            lower(&mut v, Status::Caveat, format!("dino is still checking which APIs {} supports", p.name), None);
        } else if let Some(f) = needs.formats.iter().find(|f| p.formats.contains(f)) {
            v.via = Some(*f);
        } else if let Some((_, to)) = TRANSLATES.iter().find(|(from, to)| needs.formats.contains(from) && p.formats.contains(to)) {
            v.via = Some(*to);
            v.translated = true;
            lower(&mut v, Status::Caveat, format!("dino translates between {} and {}", needs.formats[0].label(), to.label()), None);
        } else {
            let wants = needs.formats.iter().map(|f| f.label()).collect::<Vec<_>>().join(" or ");
            lower(&mut v, Status::No, format!("Needs {wants}, which {} doesn't support", p.name), needs.source.clone());
        }

        // 2. Tool calling.
        if needs.needs.iter().any(|n| n == "tools") {
            match m.tools {
                Some(false) => lower(&mut v, Status::No, "Doesn't call tools".into(), None),
                None => lower(&mut v, Status::Caveat, format!("{} doesn't say whether it calls tools", p.name), None),
                Some(true) => {}
            }
        }

        // 3. Room for the agent's prompt, tools and answer.
        if let Some(ctx) = m.context {
            let hard = needs.min_context_hard.unwrap_or(0);
            if ctx < hard {
                lower(&mut v, Status::No, format!("{} context; needs at least {}", tokens(ctx), tokens(hard)), needs.source.clone());
            } else if ctx < needs.min_context {
                lower(&mut v, Status::Caveat, format!("{} context is tight; {} or more works best", tokens(ctx), tokens(needs.min_context)), needs.source.clone());
            }
        }

        // 4. What people found that no API says.
        for n in self.notes(m) {
            if let Some(note) = n.agents.get(agent) {
                lower(&mut v, note.status, note.why.clone(), note.source.clone());
            }
        }
        for r in self.runtimes.iter().filter(|r| r.runtime == p.id && (r.agent == agent || r.agent == "*")) {
            let fixed = r.until_version.as_deref().zip(p.version.as_deref()).is_some_and(|(until, have)| older(until, have));
            if !fixed {
                lower(&mut v, r.status, r.why.clone(), r.source.clone());
            }
        }
        v
    }
}

/// `*` matches anything, case ignored.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.to_lowercase(), text.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    let mut at = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match t[at..].find(part) {
            Some(found) if i > 0 || found == 0 => at += found + part.len(),
            _ => return false,
        }
    }
    parts.last().is_some_and(|l| l.is_empty()) || at == t.len()
}

/// Version `a` is older than or the same as `b` ("0.14" vs "0.14.2").
fn older(a: &str, b: &str) -> bool {
    let n = |s: &str| s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    n(a) <= n(b)
}

fn tokens(n: u64) -> String {
    if n >= 1_000_000 && n % 1_000_000 == 0 { format!("{}M", n / 1_000_000) } else { format!("{}k", (n + 512) / 1024) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::openrouter_models;

    const OPENROUTER: &str = include_str!("../tests/fixtures/openrouter-models.json");

    fn openrouter() -> ProviderInfo {
        ProviderInfo { id: "openrouter".into(), name: "OpenRouter".into(), formats: Format::ALL.to_vec(), connected: true, ..Default::default() }
    }

    fn verdict<'a>(vs: &'a [Verdict], agent: &str) -> &'a Verdict {
        vs.iter().find(|v| v.agent == agent).unwrap()
    }

    #[test]
    fn the_bundled_file_reads() {
        let c = Compat::parse(BUNDLED).unwrap();
        assert_eq!(c.agents.first().map(|(a, _)| a.as_str()), Some("claude"), "order kept");
        assert!(c.agents.iter().all(|(_, n)| !n.formats.is_empty() && n.min_context > 0));
    }

    #[test]
    fn qwen_27b_on_openrouter() {
        let c = Compat::parse(BUNDLED).unwrap();
        let models = openrouter_models(&serde_json::from_str(OPENROUTER).unwrap());
        let q = models.iter().find(|m| m.id == "qwen/qwen3.8-27b").unwrap();
        let vs = c.judge(q, &openrouter());
        let claude = verdict(&vs, "claude");
        assert_eq!((claude.status, claude.via, claude.translated), (Status::Works, Some(Format::Anthropic), false));
        let codex = verdict(&vs, "codex");
        assert_eq!((codex.status, codex.via), (Status::Caveat, Some(Format::Responses)));
        assert!(codex.reasons[0].text.contains("awkward"), "{:?}", codex.reasons);
        // Curated: Qwen Code first, then Claude Code; Codex last of the ones that work.
        assert_eq!(vs[0].agent, "qwen");
        assert!(vs[0].recommended && !vs[1].recommended);
        assert_eq!(vs[1].agent, "claude");
        assert!(vs[0].reasons[0].text.contains("Qwen Code"));
    }

    #[test]
    fn what_rules_it_out() {
        let c = Compat::parse(BUNDLED).unwrap();
        // A chat-only local server (MLX): Codex can't, Claude Code can through dino's translation.
        let mlx = ProviderInfo { id: "mlx".into(), name: "MLX".into(), formats: vec![Format::Chat], local: true, connected: true, ..Default::default() };
        let m = ProviderModel { id: "mlx-community/Qwen3-4B".into(), provider: "mlx".into(), context: Some(40960), tools: Some(true), local: true, free: true, ..Default::default() };
        let vs = c.judge(&m, &mlx);
        let codex = verdict(&vs, "codex");
        assert_eq!(codex.status, Status::No);
        assert!(codex.reasons.iter().any(|r| r.text.contains("Needs Responses")), "{:?}", codex.reasons);
        let claude = verdict(&vs, "claude");
        assert!(claude.translated && claude.status == Status::Caveat);
        // Nothing curated: the one that speaks Chat natively comes first, not the translated one.
        assert!(vs[0].recommended && vs[0].via == Some(Format::Chat) && !vs[0].translated, "{:?}", vs[0]);

        // 8k context, no tools.
        let models = openrouter_models(&serde_json::from_str(OPENROUTER).unwrap());
        let small = models.iter().find(|m| m.id == "tencent/hy-mt2-7b").unwrap();
        let vs = c.judge(small, &openrouter());
        let claude = verdict(&vs, "claude");
        assert_eq!(claude.status, Status::No);
        assert!(claude.reasons.iter().any(|r| r.text.contains("8k context")), "{:?}", claude.reasons);
        assert!(vs.iter().all(|v| !v.recommended), "nothing to recommend");
    }

    #[test]
    fn runtimes_and_versions() {
        let c = Compat::parse(BUNDLED).unwrap();
        let ollama = ProviderInfo { id: "ollama".into(), name: "Ollama".into(), formats: Format::ALL.to_vec(), local: true, connected: true, version: Some("0.15.2".into()), ..Default::default() };
        let m = ProviderModel { id: "qwen3:4b".into(), provider: "ollama".into(), context: Some(262144), tools: Some(true), local: true, free: true, ..Default::default() };
        let vs = c.judge(&m, &ollama);
        assert!(verdict(&vs, "claude").reasons.iter().any(|r| r.text.contains("OLLAMA_CONTEXT_LENGTH")));
        assert_eq!(verdict(&vs, "codex").status, Status::Works);
        // A Qwen model: its family's agent, though Codex works as well.
        assert!(vs[0].agent == "qwen" && vs[0].recommended && vs[0].reasons[0].text.contains("made for Qwen"), "{:?}", vs[0]);

        let fixed: Compat = Compat::parse(r#"{"agents": {"claude": {"formats": ["anthropic"], "min_context": 1}}, "runtimes": [{"runtime": "ollama", "agent": "claude", "status": "caveat", "why": "old bug", "until_version": "0.15"}]}"#).unwrap();
        assert_eq!(fixed.judge(&m, &ollama)[0].status, Status::Works, "fixed in 0.15, have 0.15.2");
    }

    #[test]
    fn globs() {
        assert!(glob("qwen/qwen3.8-27b*", "qwen/qwen3.8-27b:free"));
        assert!(glob("*qwen3.8-27b*", "hf.co/unsloth/Qwen3.8-27B-GGUF:Q4"));
        assert!(!glob("qwen/qwen3.8-27b*", "x/qwen/qwen3.8-27b"));
        assert!(glob("qwen3.8:27b*", "qwen3.8:27b"));
    }
}
