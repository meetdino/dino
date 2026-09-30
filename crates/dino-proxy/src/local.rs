//! `/s/<session>/local/<runtime>/…`: a model server on this Mac, at its default address. They
//! take no key, so the agent's own credentials (a claude.ai login among them) stay here.

/// Model servers people run on their Macs: id, name, where they listen by default.
pub const RUNTIMES: &[(&str, &str, &str)] = &[
    ("ollama", "Ollama", "http://127.0.0.1:11434"),
    ("lmstudio", "LM Studio", "http://127.0.0.1:1234"),
    ("llamacpp", "llama.cpp", "http://127.0.0.1:8080"),
    ("vllm", "vLLM", "http://127.0.0.1:8000"),
];

pub(crate) const PROVIDER: &str = "local";

/// Runtime `id`: its name and address. Ollama's is `OLLAMA_HOST` when set, as its own CLI reads it.
pub fn runtime(id: &str) -> Option<(&'static str, &'static str)> {
    static OLLAMA: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let (_, name, default) = RUNTIMES.iter().find(|r| r.0 == id)?;
    let host = if id == "ollama" { OLLAMA.get_or_init(|| std::env::var("OLLAMA_HOST").ok().and_then(|h| ollama_base(&h))).as_deref() } else { None };
    Some((name, host.unwrap_or(default)))
}

/// `OLLAMA_HOST` as a base URL: "host:port", ":port", "0.0.0.0" (listen everywhere, so ask this
/// Mac) or a URL.
fn ollama_base(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('/');
    if host.is_empty() {
        return None;
    }
    let (scheme, rest) = host.split_once("://").unwrap_or(("http", host));
    let (h, port) = rest.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).unwrap_or((rest, "11434"));
    let h = if h.is_empty() || h == "0.0.0.0" { "127.0.0.1" } else { h };
    Some(format!("{scheme}://{h}:{port}"))
}

/// The agent's credentials, not sent on.
pub(crate) fn is_credential(name: &str) -> bool {
    matches!(name, "authorization" | "x-api-key")
}

/// Why a call to runtime `name` failed, said so the user knows what to do.
pub(crate) fn refused(id: &str, name: &str, status: u16, message: &str, model: Option<&str>) -> String {
    let missing = status == 404 && message.to_lowercase().contains("not found");
    match model {
        Some(m) if missing && id == "ollama" => format!("{name} doesn't have {m}: run `ollama pull {m}`"),
        Some(m) if missing => format!("{name} hasn't loaded {m}: load it there first"),
        _ => format!("{name}: {status} {message}"),
    }
}

/// Couldn't reach it at all.
pub(crate) fn unreachable(name: &str, base: &str) -> String {
    format!("{name} isn't running at {base}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_model_says_how_to_get_it() {
        assert_eq!(runtime("ollama"), Some(("Ollama", "http://127.0.0.1:11434")));
        assert!(runtime("nope").is_none());
        assert_eq!(refused("ollama", "Ollama", 404, "model 'qwen9:1b' not found", Some("qwen9:1b")), "Ollama doesn't have qwen9:1b: run `ollama pull qwen9:1b`");
        assert_eq!(refused("lmstudio", "LM Studio", 404, "Model not found", Some("q")), "LM Studio hasn't loaded q: load it there first");
        assert_eq!(refused("ollama", "Ollama", 500, "boom", Some("q")), "Ollama: 500 boom");
        assert!(is_credential("authorization") && !is_credential("content-type"));
    }

    #[test]
    fn ollama_host_reads_as_ollama_reads_it() {
        assert_eq!(ollama_base("127.0.0.1:11535").as_deref(), Some("http://127.0.0.1:11535"));
        assert_eq!(ollama_base("0.0.0.0").as_deref(), Some("http://127.0.0.1:11434"));
        assert_eq!(ollama_base(":9000").as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(ollama_base("https://box.local:443/").as_deref(), Some("https://box.local:443"));
        assert_eq!(ollama_base(" "), None);
    }
}
