//! The server's own pages. Plain HTML, one stylesheet, no JavaScript (Turnstile's widget is the
//! only script, and only when it's turned on).

use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::AppState;

pub const CSS: &str = include_str!("../../static/app.css");

pub fn layout(state: &AppState, title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="dark light";
                meta name="referrer" content="no-referrer";
                title { (title) " · dino" }
                link rel="stylesheet" href="/static/app.css";
                @if state.cfg.turnstile.is_some() {
                    script src="https://challenges.cloudflare.com/turnstile/v0/api.js" async defer {}
                }
            }
            body {
                main {
                    a.brand href="/account" aria-label="dino account" { (dino_mark()) span { "dino" } }
                    (body)
                }
            }
        }
    }
}

/// The pixel dino, the brand's green with its orange spikes.
fn dino_mark() -> Markup {
    html! {
        svg viewBox="0 0 26 24" width="26" height="24" aria-hidden="true" shape-rendering="crispEdges" {
            path d="M9 20h3v4h-3zM17 20h2v4h-2z" fill="#75B340" {}
            path d="M4 10h2v2H4zM2 12h2v2H2z" fill="#FC4F26" {}
            path d="M15 2h10v8h-5v2h4v2h-4v3h-2v3H8v-2H6v-2H4v-2h2v-2h3V8h6z" fill="#75B340" {}
            path d="M17 4h2v2h-2z" fill="#0A0C09" {}
        }
    }
}

pub fn csrf(token: &str) -> Markup {
    html! { input type="hidden" name="csrf" value=(token); }
}

pub fn turnstile(state: &AppState) -> Markup {
    html! {
        @if let Some(t) = &state.cfg.turnstile {
            div.cf-turnstile data-sitekey=(t.site_key) data-theme="auto" {}
        }
    }
}

pub fn message(state: &AppState, title: &str, text: &str) -> Markup {
    layout(state, title, html! {
        h1 { (title) }
        p { (text) }
    })
}

/// A user code with its separator, large and easy to compare.
pub fn user_code(code: &str) -> Markup {
    html! { p.code aria-label=(format!("Code {}", code.chars().map(String::from).collect::<Vec<_>>().join(" "))) { (code) } }
}

pub fn raw(s: &'static str) -> PreEscaped<&'static str> {
    PreEscaped(s)
}
