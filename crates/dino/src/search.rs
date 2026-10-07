//! `dino search`: one list of the shell's history and dino's sessions, for the shell's
//! search key. Picking a command or a session puts a command on the prompt; nothing runs.

use std::io::{Read, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{cursor, queue, style, terminal};
use dino_core::ipc::{Request, Response};

#[derive(Clone, Debug, PartialEq)]
struct Item {
    /// "session" or "command".
    kind: &'static str,
    /// What's shown and matched.
    text: String,
    detail: String,
    /// What goes on the prompt when it's picked.
    command: String,
}

pub(crate) const USAGE: &str = "usage: dino search [--json | --pick] [--query TEXT] [--history FILE|-]
  --history     the shell's history, newest first, from a file or stdin (the shell widget passes it);
                without it, Atuin's when Atuin is installed";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let (mut json, mut pick, mut query, mut from) = (false, false, String::new(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--pick" => pick = true,
            "--query" => query = it.next().cloned().unwrap_or_default(),
            "--history" => from = it.next().cloned(),
            _ => {
                println!("{USAGE}");
                return Ok(());
            }
        }
    }
    // The picker reads keys from stdin, so its history comes in a file.
    let history: Vec<String> = match from.as_deref() {
        Some("-") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s.lines().map(str::to_string).collect()
        }
        Some(path) => std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect(),
        None => atuin().unwrap_or_default(),
    };
    let items = merge(sessions(), &history);
    if pick {
        if let Some(i) = picker(&items, &query)? {
            println!("{}", i.command);
        }
        return Ok(());
    }
    let shown: Vec<&Item> = items.iter().filter(|i| matches(i, &query)).collect();
    if json {
        let v: Vec<_> = shown.iter().map(|i| serde_json::json!({"kind": i.kind, "text": i.text, "detail": i.detail, "command": i.command})).collect();
        println!("{}", serde_json::to_string(&v)?);
    } else {
        for i in shown {
            println!("{:<8} {}  {}", i.kind, crate::printable(&i.text), crate::printable(&i.detail));
        }
    }
    Ok(())
}

/// Atuin's history when it's installed, newest first; read through its own CLI.
fn atuin() -> Option<Vec<String>> {
    let out = std::process::Command::new("atuin").args(["search", "--cmd-only", "--limit", "2000"]).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().rev().map(str::to_string).collect())
}

/// dino's sessions, only if dinod is running: the live ones, and conversations it can continue.
fn sessions() -> (Vec<Item>, Vec<Item>) {
    let Ok(mut c) = crate::client::Control::open_existing() else { return (vec![], vec![]) };
    let (mut live, mut past) = (vec![], vec![]);
    if let Ok(Response::State { sessions, .. }) = c.request(&Request::State) {
        for s in sessions.into_iter().filter(|s| !s.exited && s.agent_id != "shell") {
            let text = s.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| s.name.clone());
            live.push(Item { kind: "session", text, detail: format!("{} · {}", s.agent_id, tilde(&s.cwd)), command: format!("dino attach {}", s.id) });
        }
    }
    if let Ok(Response::Found { sessions }) = c.request(&Request::Found { cloud: false, running_only: false }) {
        for f in sessions.into_iter().filter(|f| !f.session_id.is_empty()).take(200) {
            let prefix: String = f.session_id.chars().take(8).collect();
            past.push(Item { kind: "session", text: f.title.clone(), detail: format!("{} · {}", f.agent, f.cwd.as_deref().map(tilde).unwrap_or_default()), command: format!("dino continue {prefix}") });
        }
    }
    (live, past)
}

pub(crate) fn tilde(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

/// Live sessions first (there are few), then commands newest first, each once, then past
/// conversations.
fn merge((live, past): (Vec<Item>, Vec<Item>), history: &[String]) -> Vec<Item> {
    let mut seen = std::collections::HashSet::new();
    let commands = history
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && seen.insert(l.to_string()))
        .map(|l| Item { kind: "command", text: l.to_string(), detail: String::new(), command: l.to_string() });
    live.into_iter().chain(commands).chain(past).collect()
}

/// Every word of `query` appears in the item, ignoring case.
fn matches(i: &Item, query: &str) -> bool {
    let hay = format!("{} {}", i.text, i.detail).to_lowercase();
    query.to_lowercase().split_whitespace().all(|w| hay.contains(w))
}

/// A full-screen list on the terminal: type to narrow, ↑↓ to move, Enter picks, Esc leaves.
fn picker(items: &[Item], query: &str) -> anyhow::Result<Option<Item>> {
    let mut tty = std::io::BufWriter::new(std::fs::OpenOptions::new().write(true).open("/dev/tty")?);
    terminal::enable_raw_mode()?;
    queue!(tty, terminal::EnterAlternateScreen, cursor::Hide)?;
    let mut query = query.to_string();
    let mut at = 0usize;
    let picked = loop {
        let shown: Vec<&Item> = items.iter().filter(|i| matches(i, &query)).collect();
        at = at.min(shown.len().saturating_sub(1));
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let room = rows.saturating_sub(2) as usize;
        let top = at.saturating_sub(room.saturating_sub(1));
        queue!(tty, cursor::MoveTo(0, 0), terminal::Clear(terminal::ClearType::All))?;
        queue!(tty, style::Print(format!("search: {query}")), cursor::MoveTo(0, 1))?;
        queue!(tty, style::SetAttribute(style::Attribute::Dim), style::Print(format!("{} of {} · ↑↓ move · ⏎ put on the prompt · esc cancel", shown.len(), items.len())), style::SetAttribute(style::Attribute::Reset))?;
        for (row, (n, i)) in shown.iter().enumerate().skip(top).take(room).enumerate() {
            let mark = if i.kind == "session" { "◆ " } else { "  " };
            // A title, a folder or a history line could hold escapes that restyle or retitle the terminal.
            let line: String = format!("{mark}{}  {}", crate::printable(&i.text.replace('\n', " ")), crate::printable(&i.detail)).chars().take(cols as usize).collect();
            queue!(tty, cursor::MoveTo(0, row as u16 + 2))?;
            if n == at {
                queue!(tty, style::SetAttribute(style::Attribute::Reverse), style::Print(line), style::SetAttribute(style::Attribute::Reset))?;
            } else {
                queue!(tty, style::Print(line))?;
            }
        }
        tty.flush()?;
        let Event::Key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, .. }) = event::read()? else { continue };
        match code {
            KeyCode::Esc => break None,
            KeyCode::Char('c' | 'g') if modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Enter => break shown.get(at).map(|i| (*i).clone()),
            KeyCode::Up => at = at.saturating_sub(1),
            KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => at = at.saturating_sub(1),
            KeyCode::Down => at += 1,
            KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => at += 1,
            KeyCode::Backspace => {
                query.pop();
                at = 0;
            }
            KeyCode::Char(c) => {
                query.push(c);
                at = 0;
            }
            _ => {}
        }
    };
    queue!(tty, cursor::Show, terminal::LeaveAlternateScreen)?;
    tty.flush()?;
    terminal::disable_raw_mode()?;
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_sessions_then_history_once_then_past() {
        let s = |t: &str| Item { kind: "session", text: t.into(), detail: "claude · ~/app".into(), command: "dino attach 3".into() };
        let items = merge((vec![s("Fix login")], vec![s("Old chat")]), &["git status".into(), "ls".into(), "git status".into(), " ".into()]);
        assert_eq!(items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(), ["Fix login", "git status", "ls", "Old chat"]);
        assert!(matches(&items[0], "LOGIN claude"));
        assert!(!matches(&items[1], "login"));
    }
}
