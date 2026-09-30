//! `/s/<session>/or/…`: OpenRouter, with the key dino got when the user connected it. Agents
//! talk to it in their own shape: Anthropic Messages (`or/v1/messages`, OpenRouter's Anthropic
//! endpoint under `/api`), Chat Completions and Responses (`or/v1/…`). The agent never sees the
//! key: whatever credentials it sends are swapped for dino's.

use std::collections::HashMap;

pub(crate) const PROVIDER: &str = "or";
pub(crate) const UPSTREAM: &str = "https://openrouter.ai/api";
pub(crate) const KEY: &str = "OPENROUTER_API_KEY";

/// The agent's own credentials, dropped on the way out.
pub(crate) fn is_credential(name: &str) -> bool {
    matches!(name, "authorization" | "x-api-key")
}

/// What dino adds instead: its key, and its name for OpenRouter's app attribution.
pub(crate) fn headers(keys: &HashMap<String, String>) -> Option<[(&'static str, String); 2]> {
    let key = keys.get(KEY).filter(|k| !k.is_empty())?;
    Some([("authorization", format!("Bearer {key}")), ("x-title", "dino".into())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dinos_key_replaces_the_agents() {
        assert!(headers(&HashMap::new()).is_none());
        let keys = HashMap::from([(KEY.to_string(), "sk-or-v1-x".to_string())]);
        let h = headers(&keys).unwrap();
        assert_eq!(h[0], ("authorization", "Bearer sk-or-v1-x".to_string()));
        assert!(is_credential("x-api-key") && is_credential("authorization") && !is_credential("anthropic-version"));
    }
}
