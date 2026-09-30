//! Where models come from besides the agents' own accounts: OpenRouter, and model servers running
//! on this Mac (Ollama, LM Studio, llama.cpp, vLLM). What each one serves and what each model can
//! do is read from the provider itself; dino keeps no list of its own. This is the pure half:
//! shapes and parsers. dinod fetches.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An API shape an agent speaks and a provider serves.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// Anthropic Messages (`/v1/messages`): Claude Code.
    Anthropic,
    /// OpenAI Chat Completions (`/v1/chat/completions`).
    Chat,
    /// OpenAI Responses (`/v1/responses`): Codex.
    Responses,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Anthropic, Format::Chat, Format::Responses];

    /// Where it's served, under a provider's base.
    pub fn path(self) -> &'static str {
        match self {
            Format::Anthropic => "v1/messages",
            Format::Chat => "v1/chat/completions",
            Format::Responses => "v1/responses",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Anthropic => "Anthropic Messages",
            Format::Chat => "Chat Completions",
            Format::Responses => "Responses",
        }
    }
}

/// A place models come from.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct ProviderInfo {
    /// "openrouter", "ollama", "lmstudio", "llamacpp", "vllm".
    pub id: String,
    pub name: String,
    /// Under which the formats' paths are served, e.g. `https://openrouter.ai/api`.
    pub base: String,
    /// What it answered to when asked; empty until it has been.
    pub formats: Vec<Format>,
    /// Runs on this Mac.
    pub local: bool,
    /// Reachable now: a local server that answers, or a hosted one dino has a key for.
    pub connected: bool,
    /// The key it takes from dino's key store, for hosted ones.
    pub key: Option<String>,
    /// The server's own version, when it says.
    pub version: Option<String>,
    /// Spent and allowed on this key, in dollars, as the provider reports it.
    pub account: Option<Account>,
    /// Why it isn't usable, when it isn't.
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Account {
    pub label: Option<String>,
    pub usage: Option<f64>,
    /// None: no limit on this key.
    pub limit: Option<f64>,
    /// What the account has bought and spent overall.
    pub credits: Option<f64>,
    pub credits_used: Option<f64>,
    pub free_tier: Option<bool>,
}

/// One model a provider serves, as the provider describes it. `None` means it doesn't say.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context: Option<u64>,
    pub max_output: Option<u64>,
    pub tools: Option<bool>,
    pub reasoning: Option<bool>,
    pub vision: bool,
    /// Costs nothing to call.
    pub free: bool,
    pub local: bool,
    /// Dollars per million tokens.
    pub price_in: Option<f64>,
    pub price_out: Option<f64>,
}

/// OpenRouter's `/api/v1/models`.
pub fn openrouter_models(v: &Value) -> Vec<ProviderModel> {
    let per_million = |s: &Value| s.as_str().and_then(|s| s.parse::<f64>().ok()).map(|p| p * 1e6);
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let params: Vec<&str> = m["supported_parameters"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let price_in = per_million(&m["pricing"]["prompt"]);
            let price_out = per_million(&m["pricing"]["completion"]);
            let inputs: Vec<&str> = m["architecture"]["input_modalities"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            Some(ProviderModel {
                name: m["name"].as_str().unwrap_or(&id).to_string(),
                provider: "openrouter".into(),
                context: m["context_length"].as_u64().or(m["top_provider"]["context_length"].as_u64()),
                max_output: m["top_provider"]["max_completion_tokens"].as_u64(),
                tools: m["supported_parameters"].is_array().then(|| params.contains(&"tools")),
                reasoning: Some(m["reasoning"].is_object() || params.contains(&"reasoning")),
                vision: inputs.contains(&"image"),
                free: id.ends_with(":free") || (price_in == Some(0.0) && price_out == Some(0.0)),
                local: false,
                price_in,
                price_out,
                id,
            })
        })
        .collect()
}

/// OpenRouter's `/api/v1/key`: what this key has spent and may spend.
pub fn openrouter_account(key: &Value, credits: Option<&Value>) -> Account {
    let d = &key["data"];
    Account {
        label: d["label"].as_str().map(String::from),
        usage: d["usage"].as_f64(),
        limit: d["limit"].as_f64(),
        credits: credits.and_then(|c| c["data"]["total_credits"].as_f64()),
        credits_used: credits.and_then(|c| c["data"]["total_usage"].as_f64()),
        free_tier: d["is_free_tier"].as_bool(),
    }
}

/// One of Ollama's `/api/tags` models, with what its `/api/show` says (`show` may be null).
pub fn ollama_model(tag: &Value, show: &Value) -> Option<ProviderModel> {
    let id = tag["name"].as_str().or(tag["model"].as_str())?.to_string();
    // Newer Ollamas put capabilities and context in `/api/tags` too; `/api/show` for older ones.
    let caps: Option<Vec<&str>> = tag["capabilities"].as_array().or(show["capabilities"].as_array()).map(|a| a.iter().filter_map(Value::as_str).collect());
    // `model_info` keys are per architecture: "qwen3.context_length", "llama.context_length".
    let context = tag["details"]["context_length"].as_u64().or_else(|| {
        show["model_info"].as_object().and_then(|o| o.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| v.as_u64()))
    });
    Some(ProviderModel {
        name: id.clone(),
        provider: "ollama".into(),
        context,
        tools: caps.as_ref().map(|c| c.contains(&"tools")),
        reasoning: caps.as_ref().map(|c| c.contains(&"thinking")),
        vision: caps.as_ref().is_some_and(|c| c.contains(&"vision")),
        free: true,
        local: true,
        id,
        ..Default::default()
    })
}

/// LM Studio's `/api/v0/models`: language models only.
pub fn lmstudio_models(v: &Value) -> Vec<ProviderModel> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| matches!(m["type"].as_str(), Some("llm" | "vlm") | None))
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let caps: Option<Vec<&str>> = m["capabilities"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
            Some(ProviderModel {
                name: id.clone(),
                provider: "lmstudio".into(),
                context: m["loaded_context_length"].as_u64().or(m["max_context_length"].as_u64()),
                tools: caps.as_ref().map(|c| c.contains(&"tool_use")),
                vision: m["type"].as_str() == Some("vlm"),
                free: true,
                local: true,
                id,
                ..Default::default()
            })
        })
        .collect()
}

/// llama.cpp's server: its one model from `/v1/models`, with `/props` (context it runs with,
/// and whether its chat template does tools).
pub fn llamacpp_models(models: &Value, props: &Value) -> Vec<ProviderModel> {
    let context = props["default_generation_settings"]["n_ctx"].as_u64().or(props["n_ctx"].as_u64());
    let tools = props["chat_template_caps"]["supports_tools"].as_bool();
    openai_ids(models)
        .into_iter()
        .map(|id| ProviderModel { name: id.clone(), provider: "llamacpp".into(), context, tools, free: true, local: true, id, ..Default::default() })
        .collect()
}

/// vLLM's `/v1/models` (`max_model_len` is the context it serves).
pub fn vllm_models(v: &Value) -> Vec<ProviderModel> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            Some(ProviderModel { name: id.clone(), provider: "vllm".into(), context: m["max_model_len"].as_u64(), free: true, local: true, id, ..Default::default() })
        })
        .collect()
}

/// The models a ChatGPT sign-in may use: the account's own `/v1/models`. That list can come back
/// empty; then the catalog Codex keeps for the same ChatGPT account (`models_cache.json`) says
/// which models there are, with their context. Codex lists the models it runs with its tools.
pub fn chatgpt_models(api: &Value, codex_cache: Option<&str>) -> Vec<ProviderModel> {
    let plan = |id: String| ProviderModel { name: id.clone(), provider: "chatgpt".into(), id, ..Default::default() };
    let listed: Vec<ProviderModel> = openai_ids(api).into_iter().map(plan).collect();
    if !listed.is_empty() {
        return listed;
    }
    let Some(cache) = codex_cache.and_then(|c| serde_json::from_str::<Value>(c).ok()) else { return vec![] };
    cache["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"].as_str().is_none_or(|v| v == "list"))
        .filter_map(|m| {
            let id = m["slug"].as_str()?.to_string();
            Some(ProviderModel {
                name: m["display_name"].as_str().unwrap_or(&id).to_string(),
                context: m["context_window"].as_u64(),
                tools: Some(true),
                reasoning: m["supported_reasoning_levels"].as_array().map(|l| !l.is_empty()),
                ..plan(id)
            })
        })
        .collect()
}

fn openai_ids(v: &Value) -> Vec<String> {
    v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(String::from)).collect()
}

/// A probe's answer: whether `format` is served. Asked with an empty body, a server that has the
/// route turns the request down (400, 401, 422…); one that doesn't says it isn't there.
pub fn serves(status: u16) -> bool {
    !matches!(status, 404 | 405 | 501) && status != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OPENROUTER: &str = include_str!("../tests/fixtures/openrouter-models.json");

    #[test]
    fn openrouter_says_what_each_model_does() {
        let models = openrouter_models(&serde_json::from_str(OPENROUTER).unwrap());
        assert_eq!(models.len(), 6);
        let q = models.iter().find(|m| m.id == "qwen/qwen3.8-27b").unwrap();
        assert_eq!((q.context, q.tools, q.reasoning, q.vision, q.free), (Some(1_000_000), Some(true), Some(true), true, false));
        assert!((q.price_in.unwrap() - 0.42).abs() < 1e-9);
        let free = models.iter().find(|m| m.id == "qwen/qwen3.8-27b:free").unwrap();
        assert!(free.free);
        let small = models.iter().find(|m| m.id == "tencent/hy-mt2-7b").unwrap();
        assert_eq!((small.context, small.tools), (Some(8192), Some(false)));
    }

    #[test]
    fn local_servers_describe_their_models() {
        // A real Ollama 0.34 `/api/tags`: all it takes, no `/api/show`.
        let tags: Value = serde_json::from_str(include_str!("../tests/fixtures/ollama-tags.json")).unwrap();
        let m = ollama_model(&tags["models"][0], &Value::Null).unwrap();
        assert_eq!((m.id.as_str(), m.context, m.tools, m.reasoning), ("qwen3:4b", Some(262144), Some(true), Some(true)));

        let tag = json!({"name": "qwen3.8:27b", "model": "qwen3.8:27b"});
        let show = json!({"capabilities": ["completion", "tools", "thinking"], "model_info": {"general.architecture": "qwen3", "qwen3.context_length": 262144}});
        let m = ollama_model(&tag, &show).unwrap();
        assert_eq!((m.context, m.tools, m.reasoning, m.local), (Some(262144), Some(true), Some(true), true));
        // An older Ollama without capabilities: unknown, not "no".
        assert_eq!(ollama_model(&tag, &Value::Null).unwrap().tools, None);

        let lm = lmstudio_models(&json!({"data": [
            {"id": "qwen3-4b", "type": "llm", "max_context_length": 32768, "capabilities": ["tool_use"]},
            {"id": "nomic-embed", "type": "embeddings", "max_context_length": 2048}
        ]}));
        assert_eq!(lm.len(), 1);
        assert_eq!((lm[0].context, lm[0].tools), (Some(32768), Some(true)));

        let cpp = llamacpp_models(&json!({"data": [{"id": "Qwen3.8-27B-Q4_K_M.gguf"}]}), &json!({"default_generation_settings": {"n_ctx": 65536}, "chat_template_caps": {"supports_tools": true}}));
        assert_eq!((cpp[0].context, cpp[0].tools), (Some(65536), Some(true)));

        let v = vllm_models(&json!({"data": [{"id": "Qwen/Qwen3.8-27B", "max_model_len": 131072}]}));
        assert_eq!((v[0].context, v[0].tools), (Some(131072), None));
    }

    #[test]
    fn a_probe_tells_a_route_from_none() {
        assert!(serves(400) && serves(401) && serves(422) && serves(200));
        assert!(!serves(404) && !serves(405) && !serves(0));
    }

    #[test]
    fn a_chatgpt_sign_in_lists_its_models_or_codexs() {
        let api = json!({"object": "list", "data": [{"id": "gpt-5.5", "object": "model"}]});
        let m = chatgpt_models(&api, None);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].id.as_str(), m[0].provider.as_str(), m[0].tools), ("gpt-5.5", "chatgpt", None));

        // Empty, as it came back for a real sign-in: Codex's catalog for the same account.
        let cache = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex-models.json")).unwrap();
        let empty = json!({"object": "list", "data": []});
        let m = chatgpt_models(&empty, Some(&cache));
        assert!(!m.is_empty() && m.iter().all(|m| m.provider == "chatgpt" && m.tools == Some(true) && m.context.is_some()), "{m:?}");
        assert!(chatgpt_models(&empty, None).is_empty());
    }
}
