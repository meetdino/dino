mod account;
mod ai;
mod client;
mod mcp;
mod out;
mod search;
mod shell;

use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal;
use std::sync::{Arc, Mutex};

use dino_core::discover::{self, Inventory};
use dino_core::settings::Settings;
use dino_core::ipc::{LauncherInfo, QuotaInfo, Request, Response, SessionInfo};
use dino_core::providers::ProviderRoute;
use dino_core::status::{self, Status as SessionStatus};
use out::{Cell, Column, Paint};
use dino_term::Pane;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

const SIDEBAR_WIDTH: u16 = 28;
const ACCENT: Color = Color::Rgb(0x75, 0xb3, 0x40);
const SPIKE: Color = Color::Rgb(0xfc, 0x4f, 0x26);
const MUTED: Color = Color::DarkGray;
/// Output within this window counts as "working".
const ACTIVE_WINDOW: Duration = Duration::from_millis(1500);

/// A session living in dinod, mirrored here by a local emulator.
struct Session {
    info: SessionInfo,
    pane: Arc<Pane>,
    /// Rang the bell while in the background.
    attention: bool,
    /// Finished a turn while in the background; cleared when focused.
    unseen_done: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    /// A model request is streaming.
    Thinking,
    /// Recent terminal output, e.g. a tool running.
    Working,
    /// The turn ended on subagents or background commands that still run.
    Waiting,
    Idle,
    Done,
    Attention,
    Exited,
}

impl Session {
    fn needs(&self) -> Option<&str> {
        self.info.activity.as_deref()?.strip_prefix("needs:")
    }

    fn status(&self) -> Status {
        if self.info.exited || self.pane.is_exited() {
            Status::Exited
        } else if self.attention || self.needs().is_some() {
            Status::Attention
        } else if self.info.activity.as_deref().is_some_and(|a| a.starts_with("waiting:")) {
            Status::Waiting
        } else if self.info.activity.as_deref().is_some_and(|a| a != "working") {
            // Hooks say the turn ended; side calls and redraws since then aren't work.
            if self.unseen_done { Status::Done } else { Status::Idle }
        } else if self.info.in_flight > 0 {
            Status::Thinking
        } else if self.info.activity.as_deref() == Some("working")
            || self.info.output_ms_ago.is_some_and(|ms| ms < ACTIVE_WINDOW.as_millis() as u64)
        {
            Status::Working
        } else if self.unseen_done {
            Status::Done
        } else {
            Status::Idle
        }
    }
}

type Snapshot = (Vec<SessionInfo>, Vec<QuotaInfo>);

#[derive(PartialEq)]
enum Mode {
    /// Keys go to the focused pane.
    Pane,
    /// After the prefix key: next key is a dino command.
    Command,
    /// New-session picker is open.
    Picker { selected: usize },
    /// Machine scan: agents, logins, keys, local models. Full-screen on first run.
    Welcome,
}

struct App {
    sessions: Vec<Session>,
    focused: usize,
    mode: Mode,
    launchers: Vec<LauncherInfo>,
    pane_size: (u16, u16),
    quit: bool,
    started: Instant,
    /// Latest state from dinod, refreshed by a background poller.
    snapshot: Arc<Mutex<Option<Snapshot>>>,
    quotas: Vec<QuotaInfo>,
    /// Status line for errors talking to dinod.
    notice: Option<String>,
    config: Settings,
    /// Screen row → session index, rebuilt every frame for sidebar clicks.
    sidebar_rows: Vec<(u16, usize)>,
    inventory: Arc<Mutex<Option<Inventory>>>,
    welcome_opened: Instant,
}

impl App {
    fn new(launchers: Vec<LauncherInfo>, snapshot: Arc<Mutex<Option<Snapshot>>>) -> Self {
        Self {
            sessions: vec![],
            focused: 0,
            mode: Mode::Picker { selected: 0 },
            launchers,
            pane_size: (80, 24),
            quit: false,
            started: Instant::now(),
            snapshot,
            quotas: vec![],
            notice: None,
            config: Settings::load(),
            sidebar_rows: vec![],
            inventory: Arc::default(),
            welcome_opened: Instant::now(),
        }
    }

    fn spawn(&mut self, launcher: usize, extra_args: &[String]) {
        let l = &self.launchers[launcher];
        let req = Request::New {
            launcher: l.short.clone(),
            args: extra_args.to_vec(),
            cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
            cols: self.pane_size.0,
            rows: self.pane_size.1,
            worktree: false,
            controls: Default::default(),
            host: None,
            prompt: None,
            by: None,
            route: None,
            reveal: false,
        };
        match client::request(&req) {
            Ok(Response::Created { id }) => {
                let info = SessionInfo { id: id.clone(), name: l.short.clone(), agent_id: l.agent_id.clone(), ..Default::default() };
                if let Some(i) = self.adopt(info) {
                    self.focus(i);
                }
            }
            Ok(Response::Error { message }) => self.notice = Some(message),
            Ok(_) => {}
            Err(e) => self.notice = Some(format!("dinod: {e}")),
        }
    }

    /// Attach to a session we aren't mirroring yet. Returns its index.
    fn adopt(&mut self, info: SessionInfo) -> Option<usize> {
        if let Some(i) = self.sessions.iter().position(|s| s.info.id == info.id) {
            return Some(i);
        }
        match client::attach_pane(&info.id, self.pane_size.0, self.pane_size.1) {
            Ok(pane) => {
                self.sessions.push(Session { info, pane, attention: false, unseen_done: false });
                Some(self.sessions.len() - 1)
            }
            Err(e) => {
                self.notice = Some(format!("attach {}: {e}", info.id));
                None
            }
        }
    }

    fn open_welcome(&mut self) {
        let inv = self.inventory.clone();
        *inv.lock().unwrap() = None;
        std::thread::spawn(move || *inv.lock().unwrap() = Some(discover::scan()));
        self.welcome_opened = Instant::now();
        self.mode = Mode::Welcome;
    }

    fn leave_modal(&mut self) {
        self.mode = if self.sessions.is_empty() { Mode::Picker { selected: 0 } } else { Mode::Pane };
    }

    fn focus(&mut self, i: usize) {
        if i < self.sessions.len() {
            self.focused = i;
            self.sessions[i].attention = false;
            self.sessions[i].unseen_done = false;
        }
    }

    fn focused_pane(&self) -> Option<&Pane> {
        self.sessions.get(self.focused).map(|s| &*s.pane)
    }

    /// Jump to the next session that needs the user, falling back to plain cycling.
    fn next_attention(&mut self) {
        let n = self.sessions.len();
        let find = |want: Status| (1..=n).map(|d| (self.focused + d) % n).find(|&i| self.sessions[i].status() == want);
        let target = find(Status::Attention).or_else(|| find(Status::Done)).unwrap_or((self.focused + 1) % n.max(1));
        self.focus(target);
    }

    fn close_focused(&mut self) {
        if self.focused < self.sessions.len() {
            let s = self.sessions.remove(self.focused);
            let _ = client::request(&Request::Kill { id: s.info.id });
            self.focused = self.focused.min(self.sessions.len().saturating_sub(1));
        }
        if self.sessions.is_empty() {
            self.mode = Mode::Picker { selected: 0 };
        }
    }

    /// Merge dinod's latest state and local redraw signals. Returns true if anything changed.
    fn poll_sessions(&mut self) -> bool {
        let mut redraw = false;
        let latest = self.snapshot.lock().unwrap().take();
        if let Some((infos, quotas)) = latest {
            self.quotas = quotas;
            // Sessions killed elsewhere disappear; new ones (another client, `dino new`) appear.
            let before = self.sessions.len();
            let focused_id = self.sessions.get(self.focused).map(|s| s.info.id.clone());
            self.sessions.retain(|s| infos.iter().any(|i| i.id == s.info.id));
            for info in infos {
                let focused = focused_id.as_deref() == Some(&info.id);
                match self.sessions.iter_mut().find(|s| s.info.id == info.id) {
                    Some(s) => {
                        // Our stream ended but the session lives (dinod restarted): attach again.
                        if s.pane.is_exited() && !info.exited {
                            if let Ok(pane) = client::attach_pane(&info.id, self.pane_size.0, self.pane_size.1) {
                                s.pane = pane;
                            }
                        }
                        let finished = info.activity.as_deref() == Some("done")
                            && s.info.activity.as_deref().is_some_and(|a| a == "working" || a.starts_with("needs:") || a.starts_with("waiting:"));
                        if !focused && finished {
                            s.unseen_done = true;
                            notify(&format!("{} finished", info.name), info.title.as_deref().unwrap_or("Ready for your review"));
                        }
                        let now_needs = info.activity.as_deref().and_then(|a| a.strip_prefix("needs:"));
                        let was_needing = s.info.activity.as_deref().is_some_and(|a| a.starts_with("needs:"));
                        if !focused && !was_needing {
                            if let Some(what) = now_needs {
                                notify(&format!("{} needs you", info.name), what);
                            }
                        }
                        if !focused && info.bells > s.info.bells {
                            s.attention = true;
                        }
                        s.info = info;
                    }
                    None => {
                        self.adopt(info);
                    }
                }
            }
            if let Some(id) = focused_id {
                self.focused = self.sessions.iter().position(|s| s.info.id == id).unwrap_or(0);
            }
            if self.sessions.len() != before {
                self.focused = self.focused.min(self.sessions.len().saturating_sub(1));
            }
            redraw = true;
        }
        for s in &self.sessions {
            redraw |= s.pane.shared.dirty.swap(false, Ordering::Relaxed);
        }
        redraw
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        match &mut self.mode {
            Mode::Pane => {
                if is_prefix(&key) {
                    self.mode = Mode::Command;
                } else if let Some(p) = self.focused_pane() {
                    p.send_key(key);
                }
            }
            Mode::Command => {
                self.mode = Mode::Pane;
                match key.code {
                    _ if is_prefix(&key) => {
                        if let Some(p) = self.focused_pane() {
                            p.write(vec![0x1d]);
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('c') => self.mode = Mode::Picker { selected: 0 },
                    KeyCode::Char('j') | KeyCode::Down => self.focus((self.focused + 1) % self.sessions.len().max(1)),
                    KeyCode::Char('k') | KeyCode::Up => {
                        let n = self.sessions.len().max(1);
                        self.focus((self.focused + n - 1) % n)
                    }
                    KeyCode::Char(' ') | KeyCode::Tab => self.next_attention(),
                    KeyCode::Char(c @ '1'..='9') => self.focus(c as usize - '1' as usize),
                    KeyCode::Char('x') => self.close_focused(),
                    KeyCode::Char('d') => self.open_welcome(),
                    KeyCode::Char('q') => self.quit = true,
                    _ => {}
                }
            }
            Mode::Welcome => {
                let first_run = !self.config.machine.onboarded;
                match key.code {
                    KeyCode::Char('y' | 'Y') | KeyCode::Enter if first_run => self.finish_onboarding(true),
                    KeyCode::Char('n' | 'N') if first_run => self.finish_onboarding(false),
                    KeyCode::Char('r') if !first_run => {
                        self.config.routing.proxy = !self.config.routing.proxy;
                        let _ = self.config.save();
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.leave_modal(),
                    _ => {}
                }
            }
            Mode::Picker { selected } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1).min(self.launchers.len() - 1),
                KeyCode::Char(c @ '1'..='9') => {
                    let i = c as usize - '1' as usize;
                    if i < self.launchers.len() {
                        self.mode = Mode::Pane;
                        self.spawn(i, &[]);
                    }
                }
                KeyCode::Enter => {
                    let i = *selected;
                    self.mode = Mode::Pane;
                    self.spawn(i, &[]);
                }
                KeyCode::Esc if !self.sessions.is_empty() => self.mode = Mode::Pane,
                KeyCode::Char('q') | KeyCode::Esc if self.sessions.is_empty() => self.quit = true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
                _ => {}
            },
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let in_sidebar = m.column < SIDEBAR_WIDTH;
        if in_sidebar {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                if let Some(&(_, i)) = self.sidebar_rows.iter().find(|(y, _)| *y == m.row) {
                    self.focus(i);
                    if self.mode == Mode::Command {
                        self.mode = Mode::Pane;
                    }
                }
            }
            return;
        }
        if self.mode != Mode::Pane {
            return;
        }
        let Some(p) = self.focused_pane() else { return };
        // Pane-local coordinates; clamp so a drag or release off the edge still reaches the app.
        let (w, h) = self.pane_size;
        let col = (m.column - SIDEBAR_WIDTH).min(w.saturating_sub(1));
        let row = m.row.saturating_sub(1).min(h.saturating_sub(1));
        let lines = match m.kind {
            MouseEventKind::ScrollUp => 3,
            MouseEventKind::ScrollDown => -3,
            _ => 0,
        };
        if lines != 0 {
            p.scroll(lines, col, row);
        } else if m.row >= 1 || matches!(m.kind, MouseEventKind::Up(_) | MouseEventKind::Drag(_)) {
            p.mouse(m, col, row);
        }
    }

    fn finish_onboarding(&mut self, route: bool) {
        self.config.machine.onboarded = true;
        self.config.routing.proxy = route;
        let _ = self.config.save();
        self.leave_modal();
    }

    fn draw(&mut self, f: &mut Frame) {
        if self.mode == Mode::Welcome && !self.config.machine.onboarded {
            self.draw_welcome(f, f.area());
            return;
        }
        let [sidebar, main] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)]).areas(f.area());
        self.draw_sidebar(f, sidebar);

        let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main);
        self.pane_size = (body.width, body.height);
        for s in &mut self.sessions {
            s.pane.resize(body.width, body.height);
        }

        match self.sessions.get(self.focused) {
            Some(s) => {
                let title = s.pane.title().unwrap_or_default();
                let mut spans = vec![Span::from(format!(" {} ", s.info.name)).bold().fg(ACCENT)];
                if !title.is_empty() {
                    spans.push(Span::from(title).fg(MUTED));
                }
                let offset = s.pane.display_offset();
                if offset > 0 {
                    spans.push(Span::from(format!("  ↑ scrollback {offset}")).fg(Color::Yellow));
                }
                f.render_widget(Paragraph::new(Line::from(spans)).bg(Color::Rgb(0x22, 0x22, 0x28)), header);
                if let Some(pos) = s.pane.render(body, f.buffer_mut()) {
                    if self.mode == Mode::Pane {
                        f.set_cursor_position(pos);
                    }
                }
            }
            None => {
                let text = self.notice.clone().unwrap_or_else(|| " no sessions".into());
                f.render_widget(Paragraph::new(text).fg(MUTED), header);
            }
        }

        match self.mode {
            Mode::Picker { selected } => self.draw_picker(f, main, selected),
            Mode::Welcome => {
                f.render_widget(Clear, main);
                self.draw_welcome(f, main);
            }
            _ => {}
        }
    }

    fn draw_sidebar(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::new().borders(Borders::RIGHT).border_style(Style::new().fg(Color::Rgb(0x33, 0x33, 0x3a)));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let mut lines = vec![
            Line::from(vec![Span::from(" dino").bold().fg(ACCENT), Span::from("  agent control").fg(MUTED)]),
            Line::default(),
            Line::from(" AGENTS").fg(MUTED).add_modifier(Modifier::BOLD),
        ];
        let blink = self.started.elapsed().as_millis() / 400 % 2 == 0;
        self.sidebar_rows.clear();
        for (i, s) in self.sessions.iter().enumerate() {
            let first_row = inner.y + lines.len() as u16;
            let (dot, label, color) = match s.status() {
                Status::Thinking => (if blink { "◆" } else { "◇" }, "thinking", Color::LightMagenta),
                Status::Working => (if blink { "●" } else { "◉" }, "working", ACCENT),
                Status::Waiting => (if blink { "◌" } else { "○" }, "waiting", ACCENT),
                Status::Idle => ("○", "idle", MUTED),
                Status::Done => ("✓", "done", Color::LightCyan),
                Status::Attention => ("!", "needs you", Color::Yellow),
                Status::Exited => ("✕", "exited", Color::Red),
            };
            let name = format!("{:<12}", truncate(&s.info.name, 12));
            let mut line = Line::from(vec![
                Span::from(format!(" {} ", i + 1)).fg(MUTED),
                Span::from(dot).fg(color),
                Span::from(format!(" {name}")),
                Span::from(label).fg(color),
            ]);
            if i == self.focused {
                line = line.bg(Color::Rgb(0x2a, 0x2a, 0x33)).bold();
            }
            lines.push(line);
            if let Some(what) = s.needs() {
                lines.push(Line::from(format!("    ⚠ {}", truncate(what, 22))).fg(Color::Yellow));
            }
            if s.info.requests > 0 {
                let model = s.info.last_model.as_deref().map(short_model).unwrap_or_default();
                if let Some(tier) = &s.info.tier {
                    // Free tier: which tier the router picked and which model actually answered.
                    lines.push(Line::from(vec![
                        Span::from(format!("    {tier} → ")).fg(Color::LightCyan),
                        Span::from(truncate(&model, 16)).fg(Color::LightCyan),
                    ]));
                }
                let model = if s.info.tier.is_some() { String::new() } else { model };
                lines.push(Line::from(format!("    ↑{} ↓{} {model}", tokens(s.info.input_tokens), tokens(s.info.output_tokens))).fg(MUTED));
            }
            let last_row = inner.y + lines.len() as u16;
            self.sidebar_rows.extend((first_row..last_row).map(|y| (y, i)));
        }
        if self.sessions.is_empty() {
            lines.push(Line::from("   none yet").fg(MUTED));
        }
        f.render_widget(Paragraph::new(lines), inner);

        let hint = match self.mode {
            Mode::Command => vec![
                Line::from(" n new   x close   q quit").fg(Color::Yellow),
                Line::from(" d your machine").fg(Color::Yellow),
                Line::from(" j/k  1-9  space: needs-you").fg(Color::Yellow),
            ],
            _ => vec![Line::from(" ^] command").fg(MUTED)],
        };
        let h = hint.len() as u16;
        let hint_area = Rect { y: inner.bottom().saturating_sub(h), height: h, ..inner };
        f.render_widget(Paragraph::new(hint), hint_area);

        let usage = self.usage_lines(inner.width);
        let uh = usage.len() as u16;
        let usage_area = Rect { y: hint_area.y.saturating_sub(uh + 1), height: uh, ..inner };
        f.render_widget(Paragraph::new(usage), usage_area);
    }

    fn usage_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(" USAGE").fg(MUTED).add_modifier(Modifier::BOLD)];
        let mut any = false;
        for (provider, label) in [("anthropic", "claude"), ("chatgpt", "codex")] {
            let Some(q) = self.quotas.iter().find(|q| q.provider == provider) else { continue };
            any = true;
            for (i, w) in q.windows.iter().filter(|w| w.name.ends_with(['h', 'd'])).enumerate() {
                let name = &w.name;
                let pct = (w.utilization * 100.0).round() as u16;
                let color = match pct {
                    0..=59 => ACCENT,
                    60..=84 => Color::Yellow,
                    _ => Color::Red,
                };
                let bar_w = width.saturating_sub(24).clamp(3, 8) as usize;
                let filled = ((w.utilization.clamp(0.0, 1.0) * bar_w as f32).round() as usize).min(bar_w);
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let reset = w.resets_at.map(|t| duration(t.saturating_sub(now))).unwrap_or_default();
                let who = if i == 0 { label } else { "" };
                lines.push(Line::from(vec![
                    Span::from(format!(" {who:<7}{name:<4}")).fg(MUTED),
                    Span::from("█".repeat(filled)).fg(color),
                    Span::from("░".repeat(bar_w - filled)).fg(Color::Rgb(0x3a, 0x3a, 0x44)),
                    Span::from(format!(" {pct:>3}% ")).fg(color),
                    Span::from(reset).fg(MUTED),
                ]));
            }
        }
        if !any {
            lines.push(Line::from(" no quota data yet").fg(MUTED));
        }
        let (mut total_in, mut total_out) = (0, 0);
        for s in &self.sessions {
            total_in += s.info.input_tokens;
            total_out += s.info.output_tokens;
        }
        let free: u64 = self.sessions.iter().filter(|s| s.info.tier.is_some()).map(|s| s.info.input_tokens + s.info.output_tokens).sum();
        if free > 0 {
            lines.push(Line::from(vec![Span::from(" free  ").fg(MUTED), Span::from(format!("{} tok", tokens(free))), Span::from("  $0.00").fg(ACCENT)]));
        }
        if total_in + total_out > 0 {
            lines.push(Line::from(format!(" tokens ↑{} ↓{}", tokens(total_in), tokens(total_out))).fg(MUTED));
        }
        lines
    }

    fn draw_welcome(&self, f: &mut Frame, area: Rect) {
        let first_run = !self.config.machine.onboarded;
        let inv = self.inventory.lock().unwrap().clone();
        let mut lines: Vec<Line> = vec![
            Line::from(vec![Span::from("▲▲ ").fg(SPIKE), Span::from("dino").bold().fg(ACCENT)]),
            Line::from("one place for every agent, account and model on this machine").fg(MUTED),
            Line::default(),
        ];
        let Some(inv) = inv else {
            let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][(self.started.elapsed().as_millis() / 80 % 10) as usize];
            lines.push(Line::from(format!("{spin} scanning your machine…")).fg(MUTED));
            return render_centered(f, area, lines);
        };

        let section = |t: &str| Line::from(t.to_string()).fg(MUTED).add_modifier(Modifier::BOLD);
        let ok = || Span::from("✓ ").fg(ACCENT);
        let no = || Span::from("· ").fg(MUTED);

        lines.push(section("AGENTS"));
        for a in inv.agents.iter().filter(|a| a.path.is_some()) {
            let auth = a.auth.clone().unwrap_or_default();
            let auth_color = if auth.contains("signed out") || auth.contains("not signed") { Color::Yellow } else { Color::Reset };
            let routed = if a.meterable { Span::from("● metered").fg(ACCENT) } else { Span::from("○ direct").fg(MUTED) };
            lines.push(Line::from(vec![
                ok(),
                Span::from(format!("{:<14}", a.kind.name)).bold(),
                Span::from(format!("{:<11}", a.version.clone().unwrap_or_default())).fg(MUTED),
                Span::from(format!("{auth:<16}")).fg(auth_color),
                routed,
            ]));
        }
        for a in inv.agents.iter().filter(|a| a.path.is_none()).take(4) {
            lines.push(Line::from(vec![
                no(),
                Span::from(format!("{:<14}", a.kind.name)).fg(MUTED),
                Span::from(a.install).fg(Color::Rgb(0x55, 0x55, 0x60)),
            ]));
        }

        lines.push(Line::default());
        lines.push(section("API KEYS"));
        if inv.keys.is_empty() {
            lines.push(Line::from("  none in env, shell rc files or ./.env").fg(MUTED));
        }
        for k in &inv.keys {
            lines.push(Line::from(vec![
                ok(),
                Span::from(format!("{:<14}", k.provider)).bold(),
                Span::from(format!("{:<12}", k.masked)).fg(MUTED),
                Span::from(k.source.clone()).fg(MUTED),
            ]));
        }

        lines.push(Line::default());
        lines.push(section("LOCAL MODELS"));
        for l in &inv.local {
            let (mark, state) = if l.up { (ok(), Span::from("running").fg(ACCENT)) } else { (no(), Span::from("not running").fg(MUTED)) };
            lines.push(Line::from(vec![mark, Span::from(format!("{:<14}", l.name)), Span::from(format!("{:<17}", l.addr)).fg(MUTED), state]));
        }

        // Reveal the scan a line at a time; it's the first thing a new user sees.
        let revealed = 3 + (self.welcome_opened.elapsed().as_millis() / 45) as usize;
        let all_shown = revealed >= lines.len();
        lines.truncate(revealed);
        if all_shown {
            lines.push(Line::default());
            if first_run {
                lines.push(Line::from(vec![
                    Span::from("Route agents through dino's local proxy?  ").bold(),
                    Span::from("[Y/n]").fg(ACCENT).bold(),
                ]));
                lines.push(Line::from("Metering, quotas and live status. Your logins and keys stay where they are.").fg(MUTED));
            } else {
                let state = if self.config.routing.proxy { Span::from("on").fg(ACCENT).bold() } else { Span::from("off").fg(Color::Yellow).bold() };
                lines.push(Line::from(vec![Span::from("Routing through proxy: "), state, Span::from("   r toggle · esc close").fg(MUTED)]));
                lines.push(Line::from("Applies to newly started sessions.").fg(MUTED));
            }
        }
        render_centered(f, area, lines);
    }

    fn draw_picker(&self, f: &mut Frame, area: Rect, selected: usize) {
        let w = 44.min(area.width);
        let h = (self.launchers.len() as u16 + 4).min(area.height);
        let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 3, width: w, height: h };
        f.render_widget(Clear, r);
        let block = Block::bordered()
            .title(" new session ")
            .title_bottom(Line::from(" ↵ start  esc cancel ").right_aligned())
            .border_style(Style::new().fg(ACCENT));
        let inner = block.inner(r);
        f.render_widget(block, r);
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        let mut lines = vec![Line::from(format!(" in {}", truncate_left(&cwd, w as usize - 6))).fg(MUTED)];
        for (i, l) in self.launchers.iter().enumerate() {
            let mut line = Line::from(format!(" {}  {}", i + 1, l.label));
            if i == selected {
                line = line.bg(Color::Rgb(0x2a, 0x2a, 0x33)).fg(ACCENT).bold();
            }
            lines.push(line);
        }
        f.render_widget(Paragraph::new(lines), inner);
    }
}

/// Left-aligned block, centered as a whole in `area`.
fn render_centered(f: &mut Frame, area: Rect, lines: Vec<Line>) {
    let w = (lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16).min(area.width);
    // Fixed minimum so the block doesn't drift upward while lines are revealed.
    let h = (lines.len() as u16).max(24).min(area.height);
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 3, width: w, height: h };
    f.render_widget(Paragraph::new(lines), r);
}

/// Ask the host terminal for a desktop notification: OSC 777 (Ghostty, WezTerm, Warp, VTE) or
/// OSC 9 (iTerm2 and most others). Terminals that support neither ignore it.
fn notify(title: &str, body: &str) {
    use std::io::Write;
    let clean = |s: &str| s.replace(['\x07', '\x1b', ';'], " ");
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
    let seq = if ["ghostty", "wezterm", "warpterminal"].iter().any(|p| program.contains(p)) {
        format!("\x1b]777;notify;{};{}\x07", clean(title), clean(body))
    } else {
        format!("\x1b]9;{}: {}\x07", clean(title), clean(body))
    };
    let mut out = io::stdout();
    let _ = out.write_all(seq.as_bytes()).and_then(|_| out.flush());
}

fn is_prefix(key: &KeyEvent) -> bool {
    // Ctrl-] arrives as ']' with kitty-style reporting, or as '5' from the legacy 0x1d byte.
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
}

fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn duration(secs: u64) -> String {
    match secs {
        0..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{}h", secs / 86400, secs % 86400 / 3600),
    }
}

/// `claude-opus-5-5-20260101` → `opus-5-5`
fn short_model(m: &str) -> String {
    let m = m.strip_prefix("claude-").unwrap_or(m);
    let parts: Vec<&str> = m.split('-').filter(|p| !(p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))).collect();
    truncate(&parts.join("-"), 14)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.into() } else { s.chars().take(n - 1).chain(['…']).collect() }
}

fn truncate_left(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n { s.into() } else { std::iter::once('…').chain(s.chars().skip(len - n + 1)).collect() }
}

/// `s` safe to print to a terminal: control characters (C0, DEL and C1), with which a title or
/// a folder name could move the cursor, retitle the window or write to the clipboard, become `?`.
fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut redraw = true;
    let mut last_tick = Instant::now();
    while !app.quit {
        if redraw {
            terminal.draw(|f| app.draw(f))?;
            redraw = false;
        }
        if event::poll(Duration::from_millis(16))? {
            // Drain everything queued so a paste or fast typing costs one frame.
            loop {
                match event::read()? {
                    Event::Key(k) => app.on_key(k),
                    Event::Paste(text) => {
                        if app.mode == Mode::Pane {
                            if let Some(p) = app.focused_pane() {
                                p.paste(&text);
                            }
                        }
                    }
                    Event::Mouse(m) => app.on_mouse(m),
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            redraw = true;
        }
        redraw |= app.poll_sessions();
        redraw |= app.mode == Mode::Welcome;
        // Periodic tick keeps status labels (working → idle) and the spinner fresh.
        if last_tick.elapsed() > Duration::from_millis(400) {
            last_tick = Instant::now();
            redraw = true;
        }
    }
    Ok(())
}

/// What `dino --help` says before the full list: what dino is, and where to start.
const INTRO: &str = "dino runs your coding agents (Claude Code, Codex, …) and keeps them running:
in the dino app, or here.

  dino                every session, full screen (starts dinod if needed)
  dino claude         start Claude Code here, or any agent dino knows
  dino .              a shell in this folder, in the dino app
  dino status         what's working and what needs you
  dino found          agents already running on this Mac, in any terminal
";

const USAGE: &str = "Sessions
  dino [<agent> [args...]]          full screen; with an agent, that agent in it
  dino <folder> [<agent> [args...]] a shell or that agent there, in the dino app
  dino ls [--usage] [--json]        every session, what needs you first
  dino status [--tmux]              in a line; --tmux for tmux's status-right
  dino new [--worktree] <agent> [--on <provider> <model>] [args...]
                                    start one in the background; prints its id
  dino attach | resume | kill <id>
  dino found [--all] [--json]       agents dino didn't start, to continue here
  dino continue <id>                continue one of those in dino

Fan-out: one prompt to several agents, a git worktree each
  dino fan [--agents claude,codex,...] <prompt>
  dino groups | diff <id> | keep <id> | discard <group>

Setup
  dino login [--email | --device] | logout | sync [status|now|resolve|undo]
  dino login openrouter|chatgpt     connect a provider in your browser
  dino claude-token [status|create|set|remove]
  dino power [status|setup|remove]  keep agents running with the lid closed
  dino init zsh|bash|fish | shell install|uninstall [zsh|bash|fish]
  dino ai suggest|agent -- <request> | search [--json|--pick]
                                    the shell's AI line and history search
  dino mcp [--read-only]            an MCP server on stdio, for agents
  dino ping | stop | daemon | --version

`dino <command> --help` says more about ls, found, login and fan.";

fn main() {
    // Piped into `head`, stop quietly when it has enough, as other commands do; Rust otherwise
    // ignores SIGPIPE and the next print panics. Not dinod: a client hanging up mustn't end it.
    if !matches!(std::env::args().nth(1).as_deref(), Some("daemon" | "lid-watchdog")) {
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    if let Err(e) = dino() {
        out::error(&e);
        std::process::exit(1);
    }
}

fn dino() -> anyhow::Result<()> {
    let mut cli: Vec<String> = std::env::args().skip(1).collect();
    match cli.first().map(String::as_str) {
        Some("daemon") => return dino_daemon::run(),
        Some("lid-watchdog") => {
            dino_daemon::lid_watchdog(cli.get(1).and_then(|p| p.parse().ok()).unwrap_or(0));
            return Ok(());
        }
        Some("power") => return cmd_power(cli.get(1).map(String::as_str).unwrap_or("status")),
        Some("claude-token") => return cmd_claude_token(cli.get(1).map(String::as_str).unwrap_or("status")),
        Some("attach") => {
            let fresh = cli.iter().any(|a| a == "--fresh");
            let id = cli.iter().skip(1).find(|a| *a != "--fresh").ok_or_else(|| anyhow::anyhow!("usage: dino attach <id>\n`dino ls` lists the sessions."))?;
            return client::attach_raw(id, fresh);
        }
        Some("ls") => return cmd_ls(&cli[1..]),
        Some("status") => return cmd_status(cli.iter().any(|a| a == "--tmux")),
        Some("mcp") => return mcp::serve(cli.iter().any(|a| a == "--read-only")),
        // Wired in by dinod around the user's own statusline (see `dino_core::statusline`).
        Some("statusline") => std::process::exit(dino_core::statusline::run(cli.get(1).map(String::as_str))),
        Some("--version" | "-V" | "version") => {
            println!("dino {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("found") => return cmd_found(&cli[1..]),
        Some("ai") => return ai::run(&cli[1..]),
        Some("search") => return search::run(&cli[1..]),
        Some("init") => return shell::init(cli.get(1).map(String::as_str)),
        Some("shell") => return shell::run(&cli[1..]),
        Some("login") if matches!(cli.get(1).map(String::as_str), Some("openrouter" | "chatgpt")) => return cmd_login(cli.get(1).map(String::as_str)),
        Some("login") => return account::login(&cli[1..]),
        Some("logout") => {
            let Some(provider) = cli.get(1).cloned() else { return account::logout() };
            done(client::request(&Request::DisconnectProvider { provider: provider.clone() })?)?;
            say(&format!("Disconnected {}.", printable(&provider)));
            return Ok(());
        }
        Some("sync") => return account::sync(&cli[1..]),
        Some("fan") => return cmd_fan(&cli[1..]),
        Some("groups") => return cmd_groups(),
        Some("diff") => {
            let session = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino diff <id>\n`dino groups` lists fan-outs and their sessions."))?.clone();
            let Response::Diff { text, .. } = client::request(&Request::Diff { session })? else { return Err(unexpected()) };
            print!("{text}");
            return Ok(());
        }
        Some("keep") => {
            let session = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino keep <session id>\n`dino groups` lists fan-outs and their sessions."))?.clone();
            done(client::request(&Request::Keep { session: session.clone() })?)?;
            say(&format!("Applied session {}'s changes to your checkout, and closed its group.", printable(&session)));
            return Ok(());
        }
        Some("discard") => {
            let group = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino discard <group>\n`dino groups` lists them."))?.clone();
            done(client::request(&Request::Discard { group: group.clone() })?)?;
            say(&format!("Closed group {}: its agents are stopped and their worktrees removed.", printable(&group)));
            return Ok(());
        }
        Some("continue") => return cmd_continue(cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino continue <session-id prefix>"))?),
        // Start dinod if needed; used by the app before it attaches surfaces.
        Some("ping") => {
            client::connect()?;
            println!("{}", dino_core::ipc::socket_path().display());
            return Ok(());
        }
        Some("new") => {
            let worktree = cli.get(1).is_some_and(|a| a == "-w" || a == "--worktree");
            let rest = &cli[if worktree { 2 } else { 1 }..];
            let agent = rest.first().ok_or_else(|| anyhow::anyhow!("usage: dino new [--worktree] <agent> [--on <provider> <model>] [args...]"))?.clone();
            let (cols, rows) = terminal::size().unwrap_or((120, 40));
            let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
            // `--on <provider> <model>`: a provider's model instead of the agent's own account.
            let (route, args) = match &rest[1..] {
                [on, provider, model, args @ ..] if on == "--on" => (Some(ProviderRoute { provider: provider.clone(), model: model.clone(), format: None, name: String::new() }), args.to_vec()),
                args => (None, args.to_vec()),
            };
            let req = Request::New { launcher: agent.clone(), args, cwd, cols, rows, worktree, controls: Default::default(), host: None, prompt: None, by: None, route, reveal: false };
            let id = created(client::request(&req)?)?;
            // Piped, only the id, for `id=$(dino new claude)`.
            if out::tty() {
                println!("Started {} as session {id}. `dino attach {id}` opens it here.", printable(&agent));
            } else {
                println!("{id}");
            }
            return Ok(());
        }
        Some("kill") => {
            let id = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino kill <id>\n`dino ls` lists the sessions."))?.clone();
            done(client::request(&Request::Kill { id: id.clone() })?)?;
            say(&format!("Closed session {}.", printable(&id)));
            return Ok(());
        }
        Some("resume") => {
            let id = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino resume <id>\n`dino ls` lists the sessions; Ended ones resume."))?.clone();
            done(client::request(&Request::Resume { id: id.clone() })?)?;
            say(&format!("Resumed session {id}. `dino attach {id}` opens it here.", id = printable(&id)));
            return Ok(());
        }
        Some("stop") => {
            if std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()).is_err() {
                println!("dinod isn't running.");
                return Ok(());
            }
            done(client::request(&Request::Shutdown)?)?;
            say("Stopped dinod. Its sessions resume when it next starts.");
            return Ok(());
        }
        Some(arg) if is_folder(arg) => return cmd_open(arg, &cli[1..]),
        Some("-h" | "--help" | "help") => {
            println!("{INTRO}\n{USAGE}");
            return Ok(());
        }
        _ => {}
    }
    // The full-screen client inside a dino session would attach to the session it runs in, and its
    // close kills sessions: refuse rather than nest.
    if std::env::var_os("DINO_SESSION").is_some_and(|s| !s.is_empty()) {
        // `dino claude` in a dino shell: that agent here, as `dino . claude`.
        if let (Some(a), Ok(Response::Launchers { launchers })) = (cli.first(), client::request(&Request::Launchers)) {
            if launchers.iter().any(|l| &l.short == a) {
                return cmd_open(".", &cli);
            }
        }
        match cli.first() {
            Some(a) => return Err(unknown_agent(a, true)),
            None => anyhow::bail!("this is a dino session already: the full-screen view would show itself.\n`dino ls` lists the sessions, and `dino --help` the commands."),
        }
    }

    let launchers = match client::request(&Request::Launchers)? {
        Response::Launchers { launchers } => launchers,
        _ => return Err(unexpected()),
    };
    // Not a command, and no agent by that name: say so, rather than open the full-screen view.
    if let Some(a) = cli.first().filter(|a| *a != "--welcome") {
        let a = a.to_lowercase();
        if !launchers.iter().any(|l| l.short == a || l.label.to_lowercase().starts_with(&a)) {
            return Err(unknown_agent(&a, true));
        }
    }
    let snapshot: Arc<Mutex<Option<Snapshot>>> = Arc::default();
    let mut app = App::new(launchers, snapshot.clone());
    let (cols, rows) = terminal::size()?;
    app.pane_size = (cols.saturating_sub(SIDEBAR_WIDTH), rows.saturating_sub(1));

    // Pick up sessions that kept running while no client was open.
    if let Response::State { sessions, quotas, .. } = client::request(&Request::State)? {
        *snapshot.lock().unwrap() = Some((sessions, quotas));
        app.poll_sessions();
    }
    // Poll forever; if dinod goes away (`dino stop`, crash), keep retrying without restarting it.
    std::thread::spawn(move || {
        loop {
            if let Ok(mut control) = client::Control::open_existing() {
                while let Ok(Response::State { sessions, quotas, .. }) = control.request(&Request::State) {
                    *snapshot.lock().unwrap() = Some((sessions, quotas));
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    if cli.first().is_some_and(|a| a == "--welcome") {
        cli.remove(0);
        app.config.machine.onboarded = false;
    }
    if !app.sessions.is_empty() {
        app.mode = Mode::Pane;
    }
    if !app.config.machine.onboarded && cli.is_empty() {
        app.open_welcome();
    }
    if let Some((arg, extra)) = cli.split_first() {
        let arg = arg.to_lowercase();
        let found = app.launchers.iter().position(|l| l.short == arg).or_else(|| app.launchers.iter().position(|l| l.label.to_lowercase().starts_with(&arg)));
        if let Some(i) = found {
            app.mode = Mode::Pane;
            app.spawn(i, extra);
        }
    }

    let mut terminal = ratatui::init();
    let mut stdout = io::stdout();
    execute!(stdout, EnableBracketedPaste, EnableMouseCapture)?;
    let kitty = terminal::supports_keyboard_enhancement().unwrap_or(false);
    if kitty {
        execute!(stdout, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }

    let result = run(&mut terminal, &mut app);

    if kitty {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout, DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    Ok(result?)
}

/// `.`, `~/x`, `a/b`: a folder, not an agent's name. A bare word is one only when it's a folder
/// here and no agent is called that.
fn is_folder(arg: &str) -> bool {
    if arg == "." || arg == ".." || arg.starts_with('~') || arg.contains('/') {
        return true;
    }
    !arg.starts_with('-')
        && std::path::Path::new(arg).is_dir()
        && !matches!(client::request(&Request::Launchers), Ok(Response::Launchers { launchers }) if launchers.iter().any(|l| l.short == arg))
}

/// `dino <folder> [agent [args...]]`: a session in that folder, shown in the terminal app. Inside
/// it, the app switches to it; elsewhere this opens the app, or attaches here if there isn't one.
fn cmd_open(folder: &str, rest: &[String]) -> anyhow::Result<()> {
    let expanded = match folder.strip_prefix('~') {
        Some(tail) => format!("{}{tail}", std::env::var("HOME").unwrap_or_default()),
        None => folder.to_string(),
    };
    let path = std::fs::canonicalize(&expanded).map_err(|e| anyhow::anyhow!("{folder}: {e}"))?;
    anyhow::ensure!(path.is_dir(), "{folder} isn't a folder");
    let (cols, rows) = terminal::size().unwrap_or((120, 40));
    let req = Request::New {
        launcher: rest.first().cloned().unwrap_or_else(|| "shell".into()),
        args: rest.get(1..).unwrap_or_default().to_vec(),
        cwd: Some(path.display().to_string()),
        cols,
        rows,
        worktree: false,
        controls: Default::default(),
        host: None,
        prompt: None,
        by: None,
        route: None,
        reveal: true,
    };
    let id = match client::request(&req)? {
        Response::Created { id } => id,
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    if std::env::var_os("DINO_SESSION").is_some_and(|s| !s.is_empty()) {
        return Ok(());
    }
    let opened = cfg!(target_os = "macos") && std::process::Command::new("open").args(["-b", "dev.dino.app"]).status().is_ok_and(|s| s.success());
    if opened { Ok(()) } else { client::attach_raw(&id, false) }
}

/// What a command did, in a sentence, on a terminal; piped, it says nothing, the way `cp` doesn't.
fn say(what: &str) {
    if out::tty() {
        println!("{what}");
    }
}

/// dinod's answer to a command that makes something: its id.
fn created(resp: Response) -> anyhow::Result<String> {
    match resp {
        Response::Created { id } => Ok(id),
        Response::Error { message } => Err(hinted(message)),
        _ => Err(unexpected()),
    }
}

/// dinod's answer to a command that only does something.
fn done(resp: Response) -> anyhow::Result<()> {
    match resp {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(hinted(message)),
        _ => Err(unexpected()),
    }
}

/// A reply this dino doesn't know: dinod is a different version.
fn unexpected() -> anyhow::Error {
    anyhow::anyhow!("dinod answered in a way this dino doesn't understand: it's probably another version.\n`dino stop` stops it (its sessions resume), and the next command starts this one's.")
}

/// dinod's error, with what to do about it when the command line knows better.
fn hinted(message: String) -> anyhow::Error {
    if message.starts_with("no session ") {
        return anyhow::anyhow!("{message}\n`dino ls` lists them.");
    }
    if let Some(name) = message.strip_prefix("unknown agent ") {
        return unknown_agent(name, false);
    }
    anyhow::anyhow!(message)
}

/// `name` isn't an agent dino can start: how to install it, when it's one dino knows, or the ones it
/// can. `command`: it was the first word, so it could have been meant as a command.
fn unknown_agent(name: &str, command: bool) -> anyhow::Error {
    let name = printable(name);
    if let Some(k) = dino_core::KNOWN_AGENTS.iter().find(|k| k.id == name || k.bin == name) {
        let hint = discover::install_hint(k.id);
        return anyhow::anyhow!("{} isn't installed (no `{}` on the PATH). Install it with\n\n    {hint}\n\nthen run this again.", k.name, k.bin);
    }
    let agents = match client::request(&Request::Launchers) {
        Ok(Response::Launchers { launchers }) => launchers.into_iter().map(|l| l.short).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    };
    if command {
        anyhow::anyhow!("`{name}` isn't a dino command or an agent dino knows.\nAgents here: {agents}. `dino --help` lists the commands.")
    } else {
        anyhow::anyhow!("`{name}` isn't an agent dino knows. Agents here: {agents}.")
    }
}

/// What the agents are up to, in a line: `dino status --tmux` is short, for tmux's status bar
/// (`set -g status-right '#(dino status --tmux)'`), and prints nothing when nothing needs saying.
/// Without it, the agents that need you, with what they ask, and those working, under the line.
/// It never starts dinod: a status bar asks every few seconds.
fn cmd_status(tmux: bool) -> anyhow::Result<()> {
    if std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()).is_err() {
        if !tmux {
            println!("No agents: dinod isn't running. Start one with `dino claude`.");
        }
        return Ok(());
    }
    let Response::State { sessions, .. } = client::request(&Request::State)? else { return Err(unexpected()) };
    let mut agents: Vec<_> = sessions.iter().filter(|s| status::is_agent(s) && !s.exited).map(|s| (SessionStatus::of(s), s)).collect();
    agents.sort_by_key(|(st, s)| (*st, id_order(&s.id)));
    let count = |want: SessionStatus| agents.iter().filter(|(st, _)| *st == want).count();
    let line = status_line(tmux, agents.len(), count(SessionStatus::NeedsYou), count(SessionStatus::Working));
    if tmux {
        println!("{line}");
        return Ok(());
    }
    let cols = [Column::keep("ID"), Column::keep("STATUS"), Column::end("NAME", 12), Column::end("ASKS", 12)];
    let rows: Vec<_> = agents
        .iter()
        .filter(|(st, _)| matches!(st, SessionStatus::NeedsYou | SessionStatus::Working))
        .map(|(st, s)| vec![Cell::new(printable(&s.id)), Cell::status(*st), Cell::new(name(s)), Cell::new(printable(status::needs(s).unwrap_or("")))])
        .collect();
    if out::tty() {
        println!("{line}");
        if !rows.is_empty() {
            print!("\n{}", out::table(&cols, &rows, false).lines().map(|l| format!("  {l}\n")).collect::<String>());
        }
        let asking: Vec<_> = agents.iter().filter(|(st, _)| *st == SessionStatus::NeedsYou).collect();
        match asking.as_slice() {
            [] => {}
            [(_, s)] => println!("\n{}", out::paint(&format!("`dino attach {}` to answer it.", printable(&s.id)), Paint::Dim)),
            _ => println!("\n{}", out::paint("`dino attach <id>` to answer one.", Paint::Dim)),
        }
    } else {
        println!("{line}");
        print!("{}", out::table(&cols, &rows, false));
    }
    Ok(())
}

fn status_line(tmux: bool, agents: usize, needs: usize, working: usize) -> String {
    let mut parts = vec![];
    if needs > 0 {
        parts.push(format!("{needs} {}", if needs == 1 { "needs you" } else { "need you" }));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    match (tmux, parts.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("dino: {}", parts.join(" · ")),
        (false, true) if agents == 0 => "No agents running. Start one with `dino claude`.".into(),
        (false, true) => format!("{agents} {}, none working", if agents == 1 { "agent" } else { "agents" }),
        (false, false) => format!("{agents} {}: {}", if agents == 1 { "agent" } else { "agents" }, parts.join(", ")),
    }
}

/// Sessions in the order dinod numbered them: `10` after `9`.
fn id_order(id: &str) -> (usize, String) {
    (id.parse().unwrap_or(usize::MAX), id.to_string())
}

/// What the app calls a session: the name you gave it, else what its agent calls the conversation,
/// else its own name (`claude-2`).
fn name(s: &SessionInfo) -> String {
    let title = |t: &str| {
        let t = t.trim_start_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '~' | '/' | '.'))).trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    // Until it has a topic, an agent's title is only its own name ("Claude Code").
    let topic = |t: String| {
        let words: Vec<_> = t.split_whitespace().collect();
        let only_agent = words.len() <= 2 && words.first().is_some_and(|w| s.agent_id.to_lowercase().starts_with(&w.to_lowercase()));
        (!only_agent).then_some(t)
    };
    let shown = s.label.clone().or_else(|| match &s.inside {
        Some(f) => title(&f.title),
        None if s.agent_id != "shell" => s.title.as_deref().and_then(title).and_then(topic),
        None => None,
    });
    printable(&shown.unwrap_or_else(|| s.name.clone()))
}

/// Where a session runs: its folder (a shell's, where it is now), `host:` before it over SSH.
fn folder(s: &SessionInfo) -> String {
    let path = s.shell_cwd.as_deref().unwrap_or(&s.cwd);
    printable(&match &s.host {
        Some(h) => format!("{h}:{path}"),
        None => out::short_path(path),
    })
}

const LS_HELP: &str = "usage: dino ls [--usage] [--json]

Every session dinod runs, what needs you first, then what's working, done, idle and ended.
Statuses are the app's, worked out the same way; a turn that ended is Done until the next one.

  -u, --usage   add the tokens each one has used, in and out
      --json    a JSON array for scripts, one object per session, with
                  id, name, agent, status (needs_you, working, done, idle, ended, exited),
                  needs (what it asks for, or null), folder, host (or null),
                  last_active (ISO 8601 UTC, or null), input_tokens, output_tokens
                Fields may be added; these keep their names and meanings.

Piped, it prints a tab-separated line per session and no header: id, name, agent, status,
folder (in full) and last active, with the values --json has.";

/// `dino ls [--usage] [--json]`: every session, what wants you first.
fn cmd_ls(args: &[String]) -> anyhow::Result<()> {
    let usage = args.iter().any(|a| a == "-u" || a == "--usage");
    let json = args.iter().any(|a| a == "--json");
    if let Some(a) = args.iter().find(|a| !matches!(a.as_str(), "-u" | "--usage" | "--json")) {
        if matches!(a.as_str(), "-h" | "--help") {
            println!("{LS_HELP}");
            return Ok(());
        }
        anyhow::bail!("dino ls doesn't take `{}`\n`dino ls --help` says what it does.", printable(a));
    }
    // Never starts dinod: without it there are no sessions to list.
    let mut sessions = match std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()) {
        Err(_) => vec![],
        Ok(_) => match client::request(&Request::State)? {
            Response::State { sessions, .. } => sessions,
            _ => return Err(unexpected()),
        },
    };
    sessions.sort_by_key(|s| (SessionStatus::of(s), id_order(&s.id)));
    let now_ms = out::now() * 1000;
    let last_active = |s: &SessionInfo| s.output_ms_ago.map(|ms| now_ms.saturating_sub(ms) / 1000);
    if json {
        let list: Vec<_> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "name": name(s),
                    "agent": s.inside.as_ref().map_or(&s.agent_id, |f| &f.agent),
                    "status": SessionStatus::of(s).key(),
                    "needs": status::needs(s),
                    "folder": s.shell_cwd.as_deref().unwrap_or(&s.cwd),
                    "host": s.host,
                    "last_active": last_active(s).map(out::iso),
                    "input_tokens": s.input_tokens,
                    "output_tokens": s.output_tokens,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if sessions.is_empty() {
        eprintln!("No sessions. Start one with `dino claude`, or `dino .` for a shell here.");
        return Ok(());
    }
    let mut cols = vec![Column::keep("ID"), Column::end("NAME", 12), Column::keep("AGENT"), Column::keep("STATUS"), Column::path("FOLDER", 12), Column::keep("ACTIVE")];
    if usage {
        cols.extend([Column::right("IN"), Column::right("OUT")]);
    }
    let rows: Vec<_> = sessions
        .iter()
        .map(|s| {
            let st = SessionStatus::of(s);
            let agent = printable(s.inside.as_ref().map_or(&s.agent_id, |f| &f.agent));
            let when = last_active(s).map_or_else(|| Cell::new("-").raw(""), |t| Cell::new(out::ago(out::now().saturating_sub(t))).raw(out::iso(t)));
            let mut row = vec![
                Cell::new(printable(&s.id)),
                Cell::new(name(s)).paint(if st == SessionStatus::NeedsYou { Paint::Bold } else { Paint::Plain }),
                Cell::new(agent),
                Cell::status(st),
                Cell::new(folder(s)).raw(printable(s.shell_cwd.as_deref().unwrap_or(&s.cwd))),
                when.paint(Paint::Dim),
            ];
            if usage {
                row.push(Cell::new(tokens(s.input_tokens)).raw(s.input_tokens.to_string()));
                row.push(Cell::new(tokens(s.output_tokens)).raw(s.output_tokens.to_string()));
            }
            row
        })
        .collect();
    print!("{}", out::table(&cols, &rows, true));
    Ok(())
}

/// `dino power [status|setup|remove]`: keeping agents running with the lid closed.
fn cmd_power(action: &str) -> anyhow::Result<()> {
    if !matches!(action, "status" | "setup" | "remove") {
        println!("usage: dino power [status|setup|remove]\n\nsetup asks for an administrator's password once, so dino can keep the Mac awake with its lid closed while agents work (Settings → General).");
        return Ok(());
    }
    let p = match client::request(&Request::Power { action: action.into() })? {
        Response::Power { power } => power,
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    let lid = dino_core::settings::Settings::load().machine.lid;
    let mut rows = vec![
        ("Lid closed", if lid.enabled { "agents keep running".to_string() } else { "the Mac sleeps (Settings → General keeps agents running)".into() }),
        ("Permission", if p.ready == Some(true) { "set up".into() } else { "not set up: `dino power setup`".into() }),
    ];
    if p.holding {
        rows.push(("Now", "awake with the lid closed".into()));
    } else if p.external {
        rows.push(("Now", "sleep is off, but not by dino; dino leaves it alone".into()));
    }
    if let Some(note) = p.note {
        rows.push(("Last time", printable(&note)));
    }
    if let Some(e) = p.error {
        rows.push(("Error", printable(&e)));
    }
    print!("{}", out::fields(&rows));
    Ok(())
}

/// The Claude subscription token (`claude setup-token`), for the Claude Code dino starts where it
/// isn't signed in. `set` reads the token from stdin, so it never sits on a command line.
fn cmd_claude_token(action: &str) -> anyhow::Result<()> {
    if !matches!(action, "status" | "create" | "set" | "remove") {
        println!(
            "usage: dino claude-token [status|create|set|remove]\n\n\
             create runs `claude setup-token` in a dino shell and keeps the token it prints; set reads one from stdin.\n\
             Only Claude Code gets it: on SSH environments, and on this Mac when Claude Code here isn't signed in (Settings → Agents)."
        );
        return Ok(());
    }
    let value = if action == "set" {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)?;
        Some(t.trim().to_string())
    } else {
        None
    };
    let t = match client::request(&Request::ClaudeToken { action: action.into(), value })? {
        Response::ClaudeToken { token } => token,
        Response::Error { message } => return Err(hinted(message)),
        _ => return Err(unexpected()),
    };
    let when = |s: u64| match s.saturating_sub(out::now()) / 86_400 {
        0 => "today".to_string(),
        1 => "tomorrow".into(),
        days => format!("in {days} days"),
    };
    let mut rows = vec![(
        "Token",
        match (&t.masked, t.expires) {
            (Some(m), Some(e)) => format!("{} (runs out {})", printable(m), when(e)),
            (Some(m), None) => printable(m),
            _ => "none: `dino claude-token create` makes one".into(),
        },
    )];
    if let Some(s) = t.signed_in {
        rows.push(("Claude Code here", if s { "signed in on its own".into() } else { "not signed in: sessions here use the token".into() }));
    }
    if let Some(id) = t.creating {
        rows.push(("Creating", format!("in session {}: finish signing in in your browser", printable(&id))));
    }
    if let Some(e) = t.error {
        rows.push(("Error", printable(&e)));
    }
    print!("{}", out::fields(&rows));
    Ok(())
}

const FOUND_HELP: &str = "usage: dino found [--all] [--json]

Agent sessions dino didn't start, that `dino continue <id>` continues in dino: running in other
terminals (tmux too), recent ones on this Mac, and Claude Code on the web.

  --all    every recent one, not only the newest 25
  --json   a JSON array, one object per session

Piped, it prints a tab-separated line per session and no header: where it was found (running,
recent or cloud), its full id, agent, title, folder, status and when it last changed.";

/// Agent sessions outside dino that it can continue: running elsewhere, recent, cloud.
fn cmd_found(args: &[String]) -> anyhow::Result<()> {
    use dino_core::found::Source;
    let all = args.iter().any(|a| a == "--all");
    let json = args.iter().any(|a| a == "--json");
    if let Some(a) = args.iter().find(|a| !matches!(a.as_str(), "--all" | "--json")) {
        if matches!(a.as_str(), "-h" | "--help") {
            println!("{FOUND_HELP}");
            return Ok(());
        }
        anyhow::bail!("dino found doesn't take `{}`\n`dino found --help` says what it does.", printable(a));
    }
    // Through dinod, so its own sessions aren't listed as "elsewhere".
    let Response::Found { sessions } = client::request(&Request::Found { cloud: true, running_only: false })? else { return Err(unexpected()) };
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    let now = out::now();
    let shown = |f: &dino_core::found::FoundSession| f.session_id.get(..8).unwrap_or(&f.session_id).to_string();
    if !out::tty() {
        for f in &sessions {
            let source = match f.source {
                Source::Running => "running",
                Source::Recent => "recent",
                Source::Cloud => "cloud",
            };
            let when = if f.updated_at > 0 { out::iso(f.updated_at) } else { String::new() };
            let fields = [source, &f.session_id, &f.agent, &f.title, f.cwd.as_deref().unwrap_or(""), f.status.as_deref().unwrap_or(""), &when];
            println!("{}", fields.map(printable).join("\t"));
        }
        return Ok(());
    }
    // The rest are in the app's session browser, and `dino continue` finds them all.
    const NEWEST: usize = 25;
    let mut first = true;
    let mut section = |heading: &str| {
        if !first {
            println!();
        }
        first = false;
        println!("{}", out::paint(heading, Paint::Bold));
    };
    let row = |f: &dino_core::found::FoundSession| {
        vec![
            Cell::new(printable(&shown(f))),
            Cell::new(printable(&f.agent)),
            Cell::new(printable(&f.title)),
            Cell::new(printable(&out::short_path(f.cwd.as_deref().unwrap_or("")))),
        ]
    };
    let running: Vec<_> = sessions.iter().filter(|f| f.source == Source::Running).collect();
    if !running.is_empty() {
        section("Running in other terminals");
        let place = |f: &dino_core::found::FoundSession| f.tmux.as_ref().map(|t| format!("tmux {}", t.label)).or_else(|| f.terminal.clone());
        // Which terminal, when dino can tell for any of them.
        let any_place = running.iter().any(|f| place(f).is_some());
        let mut cols = vec![Column::keep("ID"), Column::keep("AGENT"), Column::end("TITLE", 16), Column::path("FOLDER", 12), Column::keep("STATUS")];
        if any_place {
            cols.push(Column::end("TERMINAL", 8));
        }
        let rows: Vec<_> = running
            .iter()
            .map(|f| {
                let state = match f.status.as_deref() {
                    Some("busy") => SessionStatus::Working,
                    Some("needs") => SessionStatus::NeedsYou,
                    _ => SessionStatus::Idle,
                };
                let mut r = row(f);
                r.push(Cell::status(state));
                if any_place {
                    r.push(Cell::new(printable(&place(f).unwrap_or_default())).paint(Paint::Dim));
                }
                r
            })
            .collect();
        print!("{}", out::table(&cols, &rows, true));
    }
    let recent: Vec<_> = sessions.iter().filter(|f| f.source == Source::Recent).collect();
    if !recent.is_empty() {
        section("Recent");
        let more = if all { 0 } else { recent.len().saturating_sub(NEWEST) };
        let cols = [Column::keep("ID"), Column::keep("AGENT"), Column::end("TITLE", 16), Column::path("FOLDER", 12), Column::keep("UPDATED")];
        let rows: Vec<_> = recent
            .iter()
            .take(recent.len() - more)
            .map(|f| {
                let mut r = row(f);
                r.push(Cell::new(out::ago(now.saturating_sub(f.updated_at))).paint(Paint::Dim));
                r
            })
            .collect();
        print!("{}", out::table(&cols, &rows, true));
        if more > 0 {
            println!("{}", out::paint(&format!("… and {more} older: dino found --all"), Paint::Dim));
        }
    }
    let cloud: Vec<_> = sessions.iter().filter(|f| f.source == Source::Cloud).collect();
    if !cloud.is_empty() {
        section("Claude Code on the web");
        for f in cloud {
            // Not a session yet: the app's session browser lists the web's to pick from.
            let what = if f.session_id.is_empty() { "Pick a web session to teleport in the dino app: Session → Continue a Session… (⌘K)".into() } else { format!("{}  {}", printable(&shown(f)), printable(&f.title)) };
            println!("{what}");
        }
    }
    if first {
        println!("No agent sessions outside dino on this Mac.");
    } else {
        println!("\n{}", out::paint("`dino continue <id>` continues one in dino.", Paint::Dim));
    }
    Ok(())
}

/// One prompt to several agents, each in its own worktree. Without `--agents`, every agent dino has.
fn cmd_fan(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() || args.first().is_some_and(|a| a.starts_with('-') && a != "--agents") {
        println!("usage: dino fan [--agents claude,codex,…] <prompt>\n\nOne prompt, several agents, each in its own git worktree of the current repo.");
        return Ok(());
    }
    let (agents, prompt) = match args {
        [flag, list, rest @ ..] if flag == "--agents" => (list.split(',').map(String::from).collect(), rest.join(" ")),
        rest => {
            let Response::Launchers { launchers } = client::request(&Request::Launchers)? else { return Err(unexpected()) };
            (launchers.into_iter().filter(|l| l.agent_id != "shell").map(|l| l.short).collect::<Vec<_>>(), rest.join(" "))
        }
    };
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    let group = created(client::request(&Request::Fanout { prompt, launchers: agents.clone(), cwd })?)?;
    if out::tty() {
        println!("Fanned out to {} as group {group}. `dino groups` shows how each is doing.", agents.join(", "));
    } else {
        println!("{group}");
    }
    Ok(())
}


/// Connect a hosted provider in the browser (OpenRouter's sign-in, or Sign in with ChatGPT; no key
/// to paste), then wait until dinod has what it gave.
fn cmd_login(provider: Option<&str>) -> anyhow::Result<()> {
    let Some(provider) = provider else {
        println!("usage: dino login openrouter|chatgpt\n\n  openrouter  Connect OpenRouter in your browser; dino keeps the key it gets, and never shows it.\n  chatgpt     Sign in with ChatGPT, so agents in dino can use your ChatGPT plan (up to the weekly\n              cap you set for dino in ChatGPT → Settings → Usage).");
        return Ok(());
    };
    if let Response::Providers { providers } = client::request(&Request::Providers)?
        && let Some(p) = providers.iter().find(|p| p.id == provider && p.connected)
    {
        println!("{} is already connected (dino logout {provider} disconnects it).", p.name);
        return Ok(());
    }
    let Response::Connect { url } = client::request(&Request::ConnectProvider { provider: provider.into() })? else { return Err(unexpected()) };
    println!("Opening your browser to connect {provider}. If it doesn't open, go to:\n\n  {url}\n");
    let _ = std::process::Command::new("open").arg(&url).status();
    let until = Instant::now() + std::time::Duration::from_secs(10 * 60);
    while Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let Response::Providers { providers } = client::request(&Request::Providers)? else { continue };
        let Some(p) = providers.into_iter().find(|p| p.id == provider) else { continue };
        if p.connected {
            match p.account.and_then(|a| a.label) {
                Some(label) if provider == "chatgpt" => println!("Signed in with ChatGPT. {label}."),
                _ => println!("Connected {}.", p.name),
            }
            return Ok(());
        }
        if let Some(e) = p.error {
            anyhow::bail!("{e}");
        }
    }
    anyhow::bail!("gave up waiting for the browser")
}

/// Fan-outs: each group's prompt, then how each of its agents is doing and what it changed.
fn cmd_groups() -> anyhow::Result<()> {
    let Response::Groups { groups } = client::request(&Request::Groups)? else { return Err(unexpected()) };
    if groups.is_empty() {
        eprintln!("No fan-outs. `dino fan <prompt>` starts one: one prompt, several agents, a worktree each.");
        return Ok(());
    }
    let sessions = match client::request(&Request::State)? {
        Response::State { sessions, .. } => sessions,
        _ => vec![],
    };
    let cols = [Column::keep("ID"), Column::keep("AGENT"), Column::keep("STATUS"), Column::keep("CHANGES"), Column::path("WORKTREE", 12)];
    let rows: Vec<_> = groups
        .iter()
        .flat_map(|g| &g.members)
        .map(|m| {
            let st = sessions.iter().find(|s| s.id == m.session).map_or(SessionStatus::Ended, SessionStatus::of);
            let changes = match &m.stat {
                Some(s) if s.files == 0 => Cell::new("none").raw("0 +0 -0").paint(Paint::Dim),
                Some(s) => Cell::new(format!("{} {}  +{} -{}", s.files, if s.files == 1 { "file" } else { "files" }, s.added, s.removed)).raw(format!("{} +{} -{}", s.files, s.added, s.removed)),
                None => Cell::new("worktree missing").raw("missing").paint(Paint::Red),
            };
            vec![Cell::new(printable(&m.session)), Cell::new(printable(&m.launcher)), Cell::status(st), changes, Cell::new(printable(&out::short_path(&m.worktree))).raw(printable(&m.worktree))]
        })
        .collect();
    // One table for all of them, so their columns line up; each group's rows under its prompt.
    let table = out::table(&cols, &rows, out::tty());
    let mut lines = table.lines();
    let header = if out::tty() { lines.next().unwrap_or_default() } else { "" };
    for (i, g) in groups.iter().enumerate() {
        if out::tty() {
            if i > 0 {
                println!();
            }
            let prompt = out::fit_end(&printable(&g.prompt.split_whitespace().collect::<Vec<_>>().join(" ")), out::width().saturating_sub(2).max(20));
            println!("{}", out::paint(&format!("“{prompt}”"), Paint::Bold));
            println!("{}", out::paint(&format!("group {} in {}", printable(&g.id), printable(&out::short_path(&g.repo))), Paint::Dim));
            println!("  {header}");
        }
        for l in lines.by_ref().take(g.members.len()) {
            if out::tty() { println!("  {l}") } else { println!("{}\t{l}", printable(&g.id)) }
        }
    }
    if out::tty() {
        println!("\n{}", out::paint("`dino diff <id>` shows what one changed, `dino keep <id>` applies it.", Paint::Dim));
        println!("{}", out::paint("`dino discard <group>` stops a group and removes its worktrees.", Paint::Dim));
    }
    Ok(())
}

/// Continue a session dino didn't start (see `dino found`).
fn cmd_continue(prefix: &str) -> anyhow::Result<()> {
    let Response::Found { sessions } = client::request(&Request::Found { cloud: false, running_only: false })? else { return Err(unexpected()) };
    let session = sessions
        .into_iter()
        .find(|f| !f.session_id.is_empty() && f.session_id.starts_with(prefix))
        .ok_or_else(|| anyhow::anyhow!("no session found starting with {}\n`dino found` lists the ones dino can continue.", printable(prefix)))?;
    let title = printable(&session.title);
    if session.pid.is_some() {
        eprintln!("Moving “{title}” into dino; it waits for its current turn to finish…");
    }
    let id = created(client::request(&Request::Adopt { session, cwd: None })?)?;
    if out::tty() {
        println!("Continuing “{title}” as session {id}. `dino attach {id}` opens it here.");
    } else {
        println!("{id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_not_printed() {
        assert_eq!(printable("fix \x1b]0;pwned\x07login\r\n\x7f\u{9b}2J done"), "fix ?]0;pwned?login????2J done");
        assert_eq!(printable("~/src/app · café ✓\t"), "~/src/app · café ✓?");
    }

    #[test]
    fn status_says_what_needs_saying() {
        // tmux's status bar: nothing when nothing is going on.
        assert_eq!(status_line(true, 3, 0, 0), "");
        assert_eq!(status_line(true, 3, 1, 2), "dino: 1 needs you · 2 working");
        assert_eq!(status_line(true, 3, 2, 0), "dino: 2 need you");
        assert_eq!(status_line(false, 1, 0, 0), "1 agent, none working");
        assert_eq!(status_line(false, 4, 0, 1), "4 agents: 1 working");
    }
}
