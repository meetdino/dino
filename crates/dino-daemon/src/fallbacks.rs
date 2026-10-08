//! Settings → Agents' "When it hits a limit", put to work: each session's chain of routes goes to
//! the proxy (which does the falling back, see `dino_proxy::fallback`), sessions say where they
//! are, and new sessions start with another agent while theirs is at its limit.

use std::collections::{BTreeSet, HashMap};

use dino_core::agent::agent;
use dino_core::ipc::{AgentLimit, FallbackInfo, InsteadOf, LauncherInfo, RouteUsageInfo};
use dino_core::providers::{Format, ProviderInfo, ProviderRoute, provider_of_route, route_path};
use dino_core::settings::{AgentSwitch, Settings};
use dino_proxy::SessionStats;
use dino_proxy::fallback::{Chain, Kind, Step};

use crate::{Daemon, Session, providers};

/// What a session of `agent_id` falls back to: the steps of its chain that serve the API it talks
/// in (on `route`, or its own account) as `providers` (`providers::list`) say, that the policies
/// allow, and that are on: the free models only while they're turned on. A session over SSH, or
/// an agent that doesn't talk through dino, has none.
pub(crate) fn chain(settings: &Settings, providers: &[ProviderInfo], agent_id: &str, route: Option<&ProviderRoute>, host: Option<&str>) -> Option<Chain> {
    let f = settings.fallbacks.get(agent_id).filter(|f| !f.steps.is_empty())?;
    if host.is_some() || !settings.routing.proxy && route.is_none() {
        return None;
    }
    let a = agent(agent_id)?;
    // The API it talks in: its route's, or for its own account, the one it speaks.
    let api = match route {
        Some(r) => r.format?,
        None if a.metered() => *a.provider_formats().first()?,
        None => return None,
    };
    let steps: Vec<Step> = f
        .steps
        .iter()
        .filter(|s| !s.model.trim().is_empty() && settings.policies.allows_fallback(&s.provider))
        .filter(|s| route.is_none_or(|r| r.provider != s.provider))
        .filter_map(|s| {
            let name = if s.provider == "free" {
                if !settings.experimental.free_models || !matches!(api, Format::Anthropic | Format::Chat) {
                    return None;
                }
                "free models".to_string()
            } else {
                let p = providers.iter().find(|p| p.id == s.provider)?;
                // What it serves isn't known yet (a model server not running when the session
                // started): tried anyway; the proxy goes past a route that turns the call down.
                if !p.formats.is_empty() && !p.formats.contains(&api) {
                    return None;
                }
                p.name.clone()
            };
            Some(Step { route: route_path(&s.provider), model: s.model.trim().to_string(), name })
        })
        .collect();
    (!steps.is_empty()).then_some(Chain { steps, on_outage: f.on_outage })
}

/// Give every session its chain as the settings now have it.
pub(crate) fn sync(d: &Daemon) {
    let settings = Settings::load();
    let providers = providers::list();
    for s in d.sessions.lock().unwrap().iter() {
        set(d, &settings, &providers, s);
    }
}

pub(crate) fn set(d: &Daemon, settings: &Settings, providers: &[ProviderInfo], s: &Session) {
    d.proxy.set_fallback(&s.id, chain(settings, providers, &s.agent_id, s.route.as_ref(), s.host.as_deref()));
}

/// Where session `st` is answered from, when it's a fallback: another route, or another of its
/// Codex account's models while ChatGPT rejects the one it asks for (reason "unavailable").
pub(crate) fn info(st: &SessionStats) -> Option<FallbackInfo> {
    let Some(f) = st.fallback.as_ref() else {
        let s = st.substitute.as_ref()?;
        return Some(FallbackInfo {
            provider: "chatgpt".into(),
            name: "ChatGPT".into(),
            model: s.using.clone(),
            from: s.rejected.clone(),
            reason: "unavailable".into(),
            said: s.said.clone(),
            resets_at: None,
            retry_at: None,
            since: s.since,
        });
    };
    Some(FallbackInfo {
        provider: provider_of_route(&f.route).unwrap_or_else(|| f.route.clone()),
        name: f.name.clone(),
        model: f.model.clone(),
        from: f.from_name.clone(),
        reason: f.kind.word().into(),
        said: f.said.clone(),
        resets_at: f.resets_at,
        retry_at: Some(f.retry_at),
        since: f.since,
    })
}

/// What each route answered for it, most first.
pub(crate) fn usage(st: &SessionStats) -> Vec<RouteUsageInfo> {
    let mut out: Vec<RouteUsageInfo> =
        st.by_route.iter().map(|r| RouteUsageInfo { route: r.route.clone(), name: r.name.clone(), input_tokens: r.usage.total_input(), output_tokens: r.usage.output }).collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.input_tokens + r.output_tokens));
    out
}

/// The routes each agent's sessions use, by agent id, as seen so far: a spent one means the
/// agent is at its limit, for its new sessions too.
#[derive(Default)]
pub(crate) struct Seen(HashMap<String, BTreeSet<String>>);

impl Seen {
    pub(crate) fn note(&mut self, agent_id: &str, st: &SessionStats) {
        if let Some((key, _)) = &st.primary {
            self.add(agent_id, key);
        }
    }

    pub(crate) fn add(&mut self, agent_id: &str, key: &str) {
        if self.0.get(agent_id).is_none_or(|k| !k.contains(key)) {
            self.0.entry(agent_id.to_string()).or_default().insert(key.to_string());
        }
    }
}

/// Agents at their limit (not merely down; Claude Code not while another of the user's Claude
/// accounts answers), and what their new sessions start with meanwhile.
/// Looked at with every state a client asks for: the settings are only read when one is.
pub(crate) fn limits(d: &Daemon) -> Vec<AgentLimit> {
    let spent = |switch: bool| -> Vec<(String, dino_proxy::fallback::Limited)> {
        let seen = d.fallback_seen.lock().unwrap();
        seen.0.iter().filter_map(|(agent_id, keys)| Some((agent_id.clone(), keys.iter().filter_map(|k| d.proxy.spent(k, switch)).find(|l| l.kind != Kind::Outage)?))).collect()
    };
    // As usual nothing is: settings unread. Another Claude account stands in only for sessions
    // that talk through dino (see `dino_proxy::accounts`).
    if spent(false).is_empty() {
        return vec![];
    }
    let settings = Settings::load();
    let spent = spent(settings.routing.proxy);
    if spent.is_empty() {
        return vec![];
    }
    let mut out: Vec<AgentLimit> = spent
        .into_iter()
        .map(|(agent_id, l)| {
            let switch = switch_to(d, &settings, &agent_id);
            AgentLimit {
                name: l.name,
                reason: l.kind.word().into(),
                said: l.said,
                resets_at: l.resets_at,
                retry_at: l.retry_at,
                instead: switch.as_ref().map(|(l, _)| l.agent_id.clone()),
                instead_model: switch.and_then(|(_, s)| s.model),
                agent_id,
            }
        })
        .collect();
    out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    out
}

/// The agent new sessions of `agent_id` start with while it's at its limit, if one is set and
/// allowed, with its launcher.
pub(crate) fn switch_to(d: &Daemon, settings: &Settings, agent_id: &str) -> Option<(LauncherInfo, AgentSwitch)> {
    let s = settings.fallbacks.get(agent_id)?.new_sessions.clone().filter(|s| s.agent != agent_id)?;
    let l = d.offered().into_iter().find(|l| l.agent_id == s.agent)?;
    Some((l, s))
}

/// A new session of `agent_id` on its own account, while it's at its limit: the agent to start
/// instead, its model, and why.
pub(crate) fn instead(d: &Daemon, settings: &Settings, agent_id: &str) -> Option<(LauncherInfo, AgentSwitch, InsteadOf)> {
    let limit = limits(d).into_iter().find(|l| l.agent_id == agent_id)?;
    let (l, s) = switch_to(d, settings, agent_id)?;
    Some((l, s, InsteadOf { agent_id: agent_id.into(), name: limit.name, resets_at: limit.resets_at }))
}

/// What session `s` uses, when it's its agent's own account: a new session on that account
/// would hit the same limit. A session on a provider's model says nothing about the agent's own.
pub(crate) fn note(d: &Daemon, s: &Session, st: &SessionStats) {
    if st.primary.is_some() && s.route.is_none() && s.host.is_none() {
        d.fallback_seen.lock().unwrap().note(&s.agent_id, st);
    }
}
