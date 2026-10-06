//! The models an agent offers and the effort levels each takes, read from the agent's own files
//! so dino never keeps a list of its own: Codex's `models_cache.json`, Claude Code's model
//! catalog. Without one, the model is typed by hand and there's no effort list.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One model an agent offers.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct ModelInfo {
    /// What the agent takes on its command line.
    pub id: String,
    pub label: String,
    /// Effort levels it takes, lowest first; empty when it has none (Haiku).
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    /// Listed after the main ones, under this heading ("More models").
    pub group: Option<String>,
    /// Other names the agent takes for it: Claude's "haiku" is its current Haiku.
    pub aliases: Vec<String>,
}

impl ModelInfo {
    pub fn named(&self, name: &str) -> bool {
        self.id == name || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    }
}

/// What an agent's own files say it offers.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Catalog {
    pub models: Vec<ModelInfo>,
    /// The model it starts with when none is chosen, as the agent's settings say.
    pub default_model: Option<String>,
    /// Effort levels the agent says it takes, whatever the model (Claude's `--effort`, as its help
    /// lists them): for an agent that keeps no list of its models here.
    pub efforts: Vec<String>,
}

impl Catalog {
    pub fn find(&self, name: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.named(name))
    }

    /// Every model's effort levels in one order, lowest first.
    pub fn efforts(&self) -> Vec<String> {
        let mut out: Vec<String> = self.efforts.clone();
        for m in &self.models {
            for (i, e) in m.efforts.iter().enumerate() {
                if out.contains(e) {
                    continue;
                }
                // After the level below it in this model, or first.
                let at = i.checked_sub(1).and_then(|p| out.iter().position(|x| x == &m.efforts[p])).map_or(0, |p| p + 1);
                out.insert(at, e.clone());
            }
        }
        out
    }
}

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// The files Codex's catalog is read from, to tell when to read it again.
pub fn codex_sources() -> Vec<PathBuf> {
    vec![codex_home().join("models_cache.json"), codex_home().join("config.toml")]
}

/// The files Claude's catalog is read from, to tell when to read it again.
pub fn claude_sources() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = claude_catalog_file().into_iter().collect();
    v.push(crate::claude_config::home().join("settings.json"));
    v.push(PathBuf::from(CLAUDE_MANAGED));
    v
}

pub(crate) const CLAUDE_MANAGED: &str = "/Library/Application Support/ClaudeCode/managed-settings.json";

// ---- Codex ----

/// Codex's models, from its cache (or `codex debug models`, the same JSON): the listed ones,
/// in its own order.
pub fn codex(cache: &str, config: Option<&str>) -> Option<Catalog> {
    let v: Value = serde_json::from_str(cache).ok()?;
    let mut listed: Vec<(i64, ModelInfo)> = v
        .get("models")?
        .as_array()?
        .iter()
        .filter(|m| m.get("visibility").and_then(Value::as_str).is_none_or(|v| v == "list"))
        .filter_map(|m| {
            let id = m.get("slug")?.as_str()?.to_string();
            let efforts = m
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(|e| e.as_str().or_else(|| e.get("effort")?.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            let info = ModelInfo {
                label: m.get("display_name").and_then(Value::as_str).unwrap_or(&id).to_string(),
                default_effort: m.get("default_reasoning_level").and_then(Value::as_str).map(String::from),
                id,
                efforts,
                ..Default::default()
            };
            Some((m.get("priority").and_then(Value::as_i64).unwrap_or(i64::MAX), info))
        })
        .collect();
    if listed.is_empty() {
        return None;
    }
    listed.sort_by_key(|(p, _)| *p);
    let default_model = config.and_then(|c| toml::from_str::<toml::Table>(c).ok()).and_then(|t| t.get("model")?.as_str().map(String::from));
    Some(Catalog { models: listed.into_iter().map(|(_, m)| m).collect(), default_model, ..Catalog::default() })
}

/// Codex's model cache as it's on disk (the models its ChatGPT account may use).
pub fn codex_cache() -> Option<String> {
    std::fs::read_to_string(codex_home().join("models_cache.json")).ok()
}

pub fn codex_from_files() -> Option<Catalog> {
    let cache = std::fs::read_to_string(codex_home().join("models_cache.json")).ok()?;
    codex(&cache, std::fs::read_to_string(codex_home().join("config.toml")).ok().as_deref())
}

/// When there's no cache yet: ask Codex itself (`program debug models`).
pub fn codex_from_program(program: &str) -> Option<Catalog> {
    let out = std::process::Command::new(program).args(["debug", "models"]).stdin(std::process::Stdio::null()).output().ok()?;
    let config = std::fs::read_to_string(codex_home().join("config.toml")).ok();
    codex(std::str::from_utf8(&out.stdout).ok()?, config.as_deref())
}

// ---- Claude Code ----

/// The newest `*-cc.json` in Claude Code's model catalog cache: what `/model` offers this account.
fn claude_catalog_file() -> Option<PathBuf> {
    std::fs::read_dir(crate::claude_config::home().join("cache/model-catalog"))
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with("-cc.json"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// "2.1.285" is at least "2.1.280".
fn at_least(version: &str, min: &str) -> bool {
    let parts = |s: &str| s.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(version) >= parts(min)
}

/// Claude's models from its catalog: the main ones, then the rest as "More models". Models the
/// installed Claude (`version`) is too old for, or that `allowed` (settings' `availableModels`)
/// leaves out, aren't offered.
pub fn claude(catalog: &str, version: Option<&str>, allowed: Option<&[String]>) -> Option<Catalog> {
    let v: Value = serde_json::from_str(catalog).ok()?;
    let c = v.get("catalog")?;
    let models: Vec<ModelInfo> = c
        .pointer("/config/models")?
        .as_array()?
        .iter()
        .filter(|m| match (version, m.get("min_claude_code_version").and_then(Value::as_str)) {
            (Some(have), Some(min)) => at_least(have, min),
            _ => true,
        })
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let main = m.get("section").and_then(Value::as_str).is_none_or(|s| s == "main");
            let options = m.pointer("/thinking/effort_options").and_then(Value::as_array);
            let efforts: Vec<String> = options.map(|o| o.iter().filter_map(|e| e.get("id")?.as_str().map(String::from)).collect()).unwrap_or_default();
            let default_effort = options
                .and_then(|o| o.iter().find(|e| e.get("badge").is_some_and(|b| !b.is_null())))
                .and_then(|e| e.get("id")?.as_str().map(String::from));
            // The alias ("haiku") names the main entry, not an older one in the overflow.
            let aliases = main.then(|| m.get("short_name").and_then(Value::as_str).map(str::to_lowercase)).flatten().into_iter().collect();
            Some(ModelInfo {
                label: m.get("name").and_then(Value::as_str).unwrap_or(&id).to_string(),
                id,
                efforts,
                default_effort,
                group: (!main).then(|| "More models".to_string()),
                aliases,
            })
        })
        .filter(|m| allowed.is_none_or(|a| a.iter().any(|x| m.named(x))))
        .collect();
    if models.is_empty() {
        return None;
    }
    let default_model = c.pointer("/state/model").and_then(Value::as_str).map(String::from);
    Some(Catalog { models, default_model, ..Catalog::default() })
}

/// `availableModels` from Claude's settings: the organization's, else the user's.
fn claude_allowed() -> Option<Vec<String>> {
    let read = |p: &Path| -> Option<Vec<String>> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        v.get("availableModels")?.as_array().map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect())
    };
    read(Path::new(CLAUDE_MANAGED)).or_else(|| read(&crate::claude_config::home().join("settings.json")))
}

/// Claude's catalog; `version` is what `claude --version` says.
pub fn claude_from_files(version: Option<&str>) -> Option<Catalog> {
    let text = std::fs::read_to_string(claude_catalog_file()?).ok()?;
    claude(&text, version, claude_allowed().as_deref())
}

/// The levels Claude's `--effort` takes, as `claude --help` lists them after it: "(low, medium,
/// high, xhigh, max)". Empty when it lists none.
pub fn claude_help_efforts(help: &str) -> Vec<String> {
    let Some(at) = help.find("--effort ") else { return vec![] };
    // Its description runs to the next option.
    let desc = &help[at..];
    let desc = &desc[..desc.find("\n  -").unwrap_or(desc.len())];
    let Some(list) = desc.split_once('(').and_then(|(_, r)| r.split_once(')')).map(|(l, _)| l) else { return vec![] };
    let levels: Vec<String> = list.split(',').map(|l| l.trim().to_string()).collect();
    if levels.iter().any(|l| l.is_empty() || !l.chars().all(|c| c.is_ascii_alphanumeric())) {
        return vec![];
    }
    levels
}

/// "2.1.285 (Claude Code)" → "2.1.285".
pub fn version_of(output: &str) -> Option<String> {
    output.split_whitespace().next().filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX: &str = include_str!("../tests/fixtures/codex-models.json");
    const CLAUDE: &str = include_str!("../tests/fixtures/claude-catalog.json");

    #[test]
    fn codex_lists_its_listed_models_with_their_efforts() {
        let c = codex(CODEX, Some("model = \"gpt-5.5\"\n")).unwrap();
        let ids: Vec<&str> = c.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["gpt-6-luna", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"], "hidden ones left out, in Codex's order");
        assert_eq!(c.find("gpt-5.6-terra").unwrap().efforts.last().unwrap(), "ultra");
        assert_eq!(c.find("gpt-5.5").unwrap().efforts, ["low", "medium", "high", "xhigh"]);
        assert_eq!(c.find("gpt-6-luna").unwrap().default_effort.as_deref(), Some("medium"));
        assert_eq!(c.find("gpt-6-luna").unwrap().label, "GPT-6-Luna");
        assert_eq!(c.default_model.as_deref(), Some("gpt-5.5"));
        assert_eq!(c.efforts(), ["low", "medium", "high", "xhigh", "max", "ultra"]);
        assert!(codex("{}", None).is_none() && codex("not json", None).is_none());
    }

    #[test]
    fn claude_lists_its_catalog() {
        let c = claude(CLAUDE, Some("2.1.285"), None).unwrap();
        assert_eq!(c.models[0].id, "claude-opus-5-5");
        assert_eq!(c.models[0].label, "Opus 5.5");
        assert_eq!(c.models[0].default_effort.as_deref(), Some("medium"), "the recommended one");
        let haiku = c.find("haiku").expect("the alias names the main Haiku");
        assert_eq!(haiku.id, "claude-haiku-4-5-20251001");
        assert!(haiku.efforts.is_empty(), "Haiku takes no effort");
        assert_eq!(c.find("opus").unwrap().id, "claude-opus-5-5", "not an older Opus in the overflow");
        let older = c.find("claude-opus-4-6").unwrap();
        assert_eq!(older.group.as_deref(), Some("More models"));
        assert!(!older.efforts.contains(&"xhigh".to_string()));
        assert_eq!(c.default_model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(c.efforts(), ["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn claude_leaves_out_what_it_cant_run_or_isnt_allowed() {
        let old = claude(CLAUDE, Some("2.1.260"), None).unwrap();
        assert!(old.find("claude-opus-5-5").is_none(), "needs 2.1.280");
        assert!(old.find("claude-fable-5-1").is_some(), "needs 2.1.251");
        let allowed = vec!["sonnet".to_string(), "claude-haiku-4-5-20251001".to_string()];
        let c = claude(CLAUDE, None, Some(&allowed)).unwrap();
        let ids: Vec<&str> = c.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["claude-sonnet-5-5", "claude-haiku-4-5-20251001"]);
        assert!(claude("{}", None, None).is_none());
    }

    /// Claude Code 2.1.292's help, as it prints it when not on a terminal.
    #[test]
    fn claudes_effort_levels_from_its_help() {
        let help = "  --disallowedTools <tools...>          Comma or space-separated list of tool names\n                                        to deny (e.g. \"Bash(git:*) Edit\")\n  --effort <level>                      Effort level for the current session\n                                        (low, medium, high, xhigh, max)\n  --environment <environment_id>        Create a new cloud session that runs on\n                                        the given self-hosted environment (see\n";
        assert_eq!(claude_help_efforts(help), ["low", "medium", "high", "xhigh", "max"]);
        assert!(claude_help_efforts("  --model <model>  Model for the current session (e.g. sonnet)\n").is_empty(), "no --effort");
        assert!(claude_help_efforts("  --effort <level>  Effort level\n  --environment <id>  (see docs)\n").is_empty(), "not the next option's");
        // Without its models: those levels for any of them.
        let c = Catalog { efforts: claude_help_efforts(help), ..Default::default() };
        assert_eq!(c.efforts(), ["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn versions() {
        assert_eq!(version_of("2.1.285 (Claude Code)").as_deref(), Some("2.1.285"));
        assert!(version_of("error: nope").is_none());
        assert!(at_least("2.1.285", "2.1.280") && !at_least("2.1.99", "2.1.280") && at_least("3.0", "2.9.9"));
    }
}
