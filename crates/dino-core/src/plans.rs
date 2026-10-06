//! Coding plans: a subscription (or a pay-as-you-go key of the same shape) that serves its models
//! over an Anthropic- and/or OpenAI-compatible API, for coding agents to use: the GLM Coding Plan,
//! Kimi Code, MiniMax's Token Plan, DeepSeek's API… The user pastes the plan's key; any agent
//! dino runs can then use the plan through dino's proxy (`plan/<id>`), in a shape it speaks.
//!
//! One provider type with presets, not a module per plan: where each plan is served comes from
//! `plans.json`, with the vendor's docs as its source. What a plan serves (its models, their
//! context) is asked of the plan; when it has no list to ask, the models its docs name are shown
//! as such. A preset never names a model's size or price.
//!
//! The plan's key stays in dino's key store, which never syncs. A Claude subscription token is
//! never taken as a plan's key: it is for Claude Code alone (`crate::claude_token`).

use serde::Deserialize;
use serde_json::Value;

use crate::providers::{Format, ProviderModel};

/// Provider ids of coding plans start with this: `plan-zai`, `plan-other`.
pub const PREFIX: &str = "plan-";
/// The generic entry: any Anthropic- or OpenAI-compatible endpoint, by its base URL.
pub const OTHER: &str = "other";
/// Every coding plan's entries in the key store start with this. Settings → Models & Providers
/// keeps them, so Settings → Keys doesn't list them.
pub const KEY_PREFIX: &str = "CODING_PLAN_";
/// Where the generic entry's base URL is kept, next to its key: both are this Mac's only.
pub const OTHER_URL_KEY: &str = "CODING_PLAN_OTHER_URL";

/// One coding plan, as its docs describe it.
#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Preset {
    /// "zai", "kimi"…; its provider id is `plan-<id>`.
    pub id: String,
    pub name: String,
    /// Where it serves Anthropic Messages: `{anthropic}/v1/messages`, as `ANTHROPIC_BASE_URL`.
    pub anthropic: Option<String>,
    /// Its OpenAI-compatible base, as an OpenAI client's `base_url` (its version included, if the
    /// plan's paths have one): `{openai}/chat/completions`.
    pub openai: Option<String>,
    /// The OpenAI base also serves the Responses API (`{openai}/responses`), which Codex needs.
    pub responses: bool,
    /// Where its models are listed: "openai" (`{openai}/models`), "anthropic"
    /// (`{anthropic}/v1/models`), or none.
    pub list: Option<String>,
    /// The models its docs say the endpoint serves: shown, as such, when its list can't be had.
    pub documented_models: Vec<String>,
    /// What to know about its list.
    pub models_note: Option<String>,
    /// The plan is made for coding agents: its models call tools, as its docs say.
    pub tools: bool,
    /// Where to make a key.
    pub keys_page: Option<String>,
    /// The page these URLs come from.
    pub docs: String,
    /// What its terms say about using it from coding agents.
    pub terms: Option<String>,
    /// Shown under its name.
    pub blurb: Option<String>,
}

impl Preset {
    pub fn provider_id(&self) -> String {
        format!("{PREFIX}{}", self.id)
    }

    /// The key store's name for its key.
    pub fn key_name(&self) -> String {
        key_name(&self.id)
    }

    /// What it serves, as its docs say.
    pub fn formats(&self) -> Vec<Format> {
        let mut out = vec![];
        if self.anthropic.is_some() {
            out.push(Format::Anthropic);
        }
        if self.openai.is_some() {
            out.push(Format::Chat);
            if self.responses {
                out.push(Format::Responses);
            }
        }
        out
    }

    /// The generic entry, pointed at `base`: Anthropic Messages under it, and the OpenAI API at
    /// it when it ends in a version (`…/v1`, `…/paas/v4`), else under `/v1`. Which of them it
    /// really serves is asked (dinod probes it). Connected, it goes by its host.
    pub fn other(base: &str) -> Preset {
        let base = base.trim().trim_end_matches('/');
        let versioned = base.rsplit('/').next().is_some_and(is_version);
        let anthropic = if versioned && base.ends_with("/v1") { base.trim_end_matches("/v1").to_string() } else { base.to_string() };
        let openai = if versioned { base.to_string() } else { format!("{base}/v1") };
        let other = presets().iter().find(|p| p.id == OTHER).cloned().unwrap_or_default();
        let host = base.split_once("://").map_or(base, |(_, rest)| rest).split('/').next().unwrap_or_default();
        let name = if host.is_empty() { other.name.clone() } else { host.to_string() };
        Preset { name, anthropic: Some(anthropic), openai: Some(openai), responses: true, list: Some("openai".into()), ..other }
    }
}

fn is_version(segment: &str) -> bool {
    segment.len() > 1 && segment.starts_with('v') && segment[1..].chars().all(|c| c.is_ascii_digit())
}

/// The key store's name for plan `id`'s key: `CODING_PLAN_ZAI`.
pub fn key_name(id: &str) -> String {
    format!("{KEY_PREFIX}{}", id.to_ascii_uppercase().replace(['-', '.'], "_"))
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
struct File {
    plans: Vec<Preset>,
}

const BUNDLED: &str = include_str!("../plans.json");

/// Every preset, the generic entry last.
pub fn presets() -> &'static [Preset] {
    static PRESETS: std::sync::OnceLock<Vec<Preset>> = std::sync::OnceLock::new();
    PRESETS.get_or_init(|| serde_json::from_str::<File>(BUNDLED).expect("bundled plans.json").plans)
}

/// The plan provider `provider_id` (`plan-zai`) is, if it is one.
pub fn preset(provider_id: &str) -> Option<&'static Preset> {
    let id = provider_id.strip_prefix(PREFIX)?;
    presets().iter().find(|p| p.id == id)
}

/// A Claude subscription token (`claude setup-token`'s, or a claude.ai sign-in's): Claude Code's
/// alone, never a plan's key.
pub fn is_subscription_token(key: &str) -> bool {
    key.trim().starts_with("sk-ant-oat")
}

/// `key` as a plan's key, or why it can't be one.
pub fn check_key(key: &str) -> Result<String, String> {
    let k = key.trim();
    if k.is_empty() {
        return Err("Paste the plan's API key".into());
    }
    if is_subscription_token(k) {
        return Err("That's a Claude subscription token, which only Claude Code can use. Paste the plan's own API key".into());
    }
    if k.chars().any(|c| c.is_control() || c.is_whitespace() || c == '=') {
        return Err("That doesn't look like an API key".into());
    }
    Ok(k.to_string())
}

/// `url` as the generic entry's base, or why not: https, or plain http to this Mac only (a key
/// sent in the clear to another machine could be read on the way).
pub fn check_base(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/');
    let (scheme, rest) = u.split_once("://").ok_or("The base URL starts with https://")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let name = host.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map_or(host, |(h, _)| h);
    if name.is_empty() || u.contains(['?', '#', ' ']) {
        return Err("That isn't a base URL".into());
    }
    let here = matches!(name, "localhost" | "127.0.0.1" | "[::1]");
    match scheme {
        "https" => Ok(u.to_string()),
        "http" if here => Ok(u.to_string()),
        "http" => Err("Use https: over plain http the key could be read on the way (only a server on this Mac may use http)".into()),
        _ => Err("The base URL starts with https://".into()),
    }
}

/// A plan's model list (`{openai}/models` or `{anthropic}/v1/models`): ids, and what each says
/// about itself, under the names plans use for it.
pub fn listed_models(v: &Value, provider: &str, tools: bool) -> Vec<ProviderModel> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let context = ["context_length", "context_window", "max_context_length", "max_model_len", "max_input_tokens"].iter().find_map(|k| m[*k].as_u64());
            let features: Option<Vec<&str>> = m["supported_features"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
            Some(ProviderModel {
                name: m["display_name"].as_str().or(m["name"].as_str()).unwrap_or(&id).to_string(),
                provider: provider.into(),
                context,
                max_output: ["max_output_tokens", "max_output_length", "max_tokens"].iter().find_map(|k| m[*k].as_u64()),
                tools: if tools { Some(true) } else { features.map(|f| f.contains(&"tools")) },
                id,
                ..Default::default()
            })
        })
        .collect()
}

/// The models a plan's docs name, for a plan with no list to ask.
pub fn documented_models(p: &Preset) -> Vec<ProviderModel> {
    p.documented_models.iter().map(|id| ProviderModel { id: id.clone(), name: id.clone(), provider: p.provider_id(), tools: p.tools.then_some(true), ..Default::default() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_bundled_presets_read_and_each_has_its_source() {
        let all = presets();
        assert!(all.len() >= 2 && all.last().unwrap().id == OTHER, "the generic entry last");
        for p in all.iter().filter(|p| p.id != OTHER) {
            assert!(p.anthropic.is_some() || p.openai.is_some(), "{} is served somewhere", p.id);
            assert!(p.docs.starts_with("https://"), "{} says where it's from", p.id);
            for url in p.anthropic.iter().chain(&p.openai) {
                assert!(url.starts_with("https://") && !url.ends_with('/'), "{url}");
            }
            assert!(p.list.is_some() || !p.documented_models.is_empty(), "{}: its models come from somewhere", p.id);
            assert_eq!(preset(&p.provider_id()), Some(p));
        }
        assert_eq!(key_name("zai"), "CODING_PLAN_ZAI");
        assert_eq!(key_name("opencode-go"), "CODING_PLAN_OPENCODE_GO");
    }

    #[test]
    fn a_claude_subscription_token_is_never_a_plans_key() {
        assert!(check_key("sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789").is_err());
        assert!(check_key("  sk-ant-oat01-x  ").unwrap_err().contains("only Claude Code can use"));
        assert!(check_key("").is_err() && check_key("a b").is_err() && check_key("a=b").is_err());
        assert_eq!(check_key(" sk-1234abcd \n").as_deref(), Ok("sk-1234abcd"));
    }

    #[test]
    fn the_generic_entry_takes_https_or_this_mac() {
        assert_eq!(check_base("https://api.example.com/anthropic/").as_deref(), Ok("https://api.example.com/anthropic"));
        assert!(check_base("http://127.0.0.1:11434").is_ok() && check_base("http://localhost:8080/v1").is_ok());
        assert!(check_base("http://api.example.com").is_err());
        assert!(check_base("ftp://x").is_err() && check_base("example.com").is_err() && check_base("https://").is_err());

        let o = Preset::other("http://127.0.0.1:11434");
        assert_eq!((o.anthropic.as_deref(), o.openai.as_deref()), (Some("http://127.0.0.1:11434"), Some("http://127.0.0.1:11434/v1")));
        let o = Preset::other("http://127.0.0.1:11434/v1/");
        assert_eq!((o.anthropic.as_deref(), o.openai.as_deref()), (Some("http://127.0.0.1:11434"), Some("http://127.0.0.1:11434/v1")));
        let o = Preset::other("https://api.example.com/api/coding/paas/v4");
        assert_eq!(o.openai.as_deref(), Some("https://api.example.com/api/coding/paas/v4"));
        assert_eq!(o.provider_id(), "plan-other");
        assert_eq!(o.name, "api.example.com", "connected, it goes by its host");
    }

    #[test]
    fn a_plans_list_says_what_it_says() {
        let v = json!({"object": "list", "data": [
            {"id": "glm-x", "object": "model", "owned_by": "z-ai"},
            {"id": "kimi-y", "display_name": "Kimi Y", "context_length": 262144},
            {"type": "model", "id": "m-z", "max_input_tokens": 200000},
            {"id": "q", "context_length": 131072, "max_output_length": 8192, "supported_features": ["json_mode", "tools"]}
        ]});
        let m = listed_models(&v, "plan-zai", true);
        assert_eq!(m.len(), 4);
        assert_eq!((m[0].id.as_str(), m[0].context, m[0].tools), ("glm-x", None, Some(true)));
        assert_eq!((m[1].name.as_str(), m[1].context), ("Kimi Y", Some(262144)));
        assert_eq!(m[2].context, Some(200000));
        let other = listed_models(&v, "plan-other", false);
        assert_eq!(other[0].tools, None, "unknown, not no");
        assert_eq!((other[3].tools, other[3].max_output), (Some(true), Some(8192)), "as the list says");
    }
}
