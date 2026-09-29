mod client;
mod mcp;

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
                            && s.info.activity.as_deref().is_some_and(|a| a == "working" || a.starts_with("needs:"));
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

const USAGE: &str = "usage: dino [agent [args...]] | --welcome
       dino ls | new [--worktree] <agent> [args...] | attach <id> | resume <id> | kill <id> | ping | stop | daemon
       dino found | continue <session-id prefix>
       dino mcp [--read-only]   (MCP server on stdio: agents list, read, message and start sessions)
       dino fan [--agents claude,codex,...] <prompt> | groups | diff <id> | keep <id> | discard <group>";

fn main() -> anyhow::Result<()> {
    let mut cli: Vec<String> = std::env::args().skip(1).collect();
    match cli.first().map(String::as_str) {
        Some("daemon") => return dino_daemon::run(),
        Some("attach") => return client::attach_raw(cli.get(1).ok_or_else(|| anyhow::anyhow!(USAGE))?),
        Some("ls") => return cmd_ls(),
        Some("mcp") => return mcp::serve(cli.iter().any(|a| a == "--read-only")),
        // Wired in by dinod around the user's own statusline (see `dino_core::statusline`).
        Some("statusline") => std::process::exit(dino_core::statusline::run(cli.get(1).map(String::as_str))),
        Some("found") => return cmd_found(),
        Some("fan") => return cmd_fan(&cli[1..]),
        Some("groups") => return cmd_groups(),
        Some("diff") => {
            let session = cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino diff <session id>"))?.clone();
            let Response::Diff { text, .. } = client::request(&Request::Diff { session })? else { anyhow::bail!("unexpected reply") };
            print!("{text}");
            return Ok(());
        }
        Some("keep") => return print_response(client::request(&Request::Keep { session: cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino keep <session id>"))?.clone() })?),
        Some("discard") => return print_response(client::request(&Request::Discard { group: cli.get(1).ok_or_else(|| anyhow::anyhow!("usage: dino discard <group>"))?.clone() })?),
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
            let agent = rest.first().ok_or_else(|| anyhow::anyhow!(USAGE))?.clone();
            let (cols, rows) = terminal::size().unwrap_or((120, 40));
            let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
            let req = Request::New { launcher: agent, args: rest[1..].to_vec(), cwd, cols, rows, worktree, controls: Default::default(), host: None };
            return print_response(client::request(&req)?);
        }
        Some("kill") => return print_response(client::request(&Request::Kill { id: cli.get(1).ok_or_else(|| anyhow::anyhow!(USAGE))?.clone() })?),
        Some("resume") => return print_response(client::request(&Request::Resume { id: cli.get(1).ok_or_else(|| anyhow::anyhow!(USAGE))?.clone() })?),
        Some("stop") => {
            if std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()).is_err() {
                println!("dinod is not running");
                return Ok(());
            }
            return print_response(client::request(&Request::Shutdown)?);
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => {}
    }

    let launchers = match client::request(&Request::Launchers)? {
        Response::Launchers { launchers } => launchers,
        other => anyhow::bail!("unexpected reply from dinod: {other:?}"),
    };
    let snapshot: Arc<Mutex<Option<Snapshot>>> = Arc::default();
    let mut app = App::new(launchers, snapshot.clone());
    let (cols, rows) = terminal::size()?;
    app.pane_size = (cols.saturating_sub(SIDEBAR_WIDTH), rows.saturating_sub(1));

    // Pick up sessions that kept running while no client was open.
    if let Response::State { sessions, quotas } = client::request(&Request::State)? {
        *snapshot.lock().unwrap() = Some((sessions, quotas));
        app.poll_sessions();
    }
    // Poll forever; if dinod goes away (`dino stop`, crash), keep retrying without restarting it.
    std::thread::spawn(move || {
        loop {
            if let Ok(mut control) = client::Control::open_existing() {
                while let Ok(Response::State { sessions, quotas }) = control.request(&Request::State) {
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

fn print_response(resp: Response) -> anyhow::Result<()> {
    match resp {
        Response::Created { id } => println!("{id}"),
        Response::Ok => {}
        Response::Error { message } => anyhow::bail!(message),
        other => println!("{other:?}"),
    }
    Ok(())
}

fn cmd_ls() -> anyhow::Result<()> {
    if std::os::unix::net::UnixStream::connect(dino_core::ipc::socket_path()).is_err() {
        println!("dinod is not running");
        return Ok(());
    }
    let Response::State { sessions, .. } = client::request(&Request::State)? else { anyhow::bail!("unexpected reply") };
    if sessions.is_empty() {
        println!("no sessions");
    }
    for s in sessions {
        let state = if s.exited { "exited".to_string() } else { s.activity.unwrap_or_else(|| "idle".into()) };
        let title = s.title.unwrap_or_default();
        println!("{:>3}  {:<16} {:<10} ↑{} ↓{}  {title}", s.id, truncate(&s.name, 16), state, tokens(s.input_tokens), tokens(s.output_tokens));
    }
    Ok(())
}

/// Agent sessions outside dino that it can continue: running elsewhere, recent, cloud.
fn cmd_found() -> anyhow::Result<()> {
    use dino_core::found::Source;
    // Through dinod, so its own sessions aren't listed as "elsewhere".
    let Response::Found { sessions } = client::request(&Request::Found { cloud: true, running_only: false })? else { anyhow::bail!("unexpected reply") };
    // The rest are in the app's browser, and `dino continue` finds them all.
    const SHOWN: usize = 25;
    for (label, source) in [("RUNNING ELSEWHERE", Source::Running), ("RECENT", Source::Recent), ("CLOUD", Source::Cloud)] {
        println!("{label}");
        let group: Vec<_> = sessions.iter().filter(|f| f.source == source).collect();
        if group.len() > SHOWN {
            println!("  (newest {SHOWN} of {})", group.len());
        }
        for f in group.into_iter().take(SHOWN) {
            let place = f.terminal.as_deref().map(|t| format!("in {t}")).unwrap_or_default();
            let status = f.status.as_deref().unwrap_or("");
            let cwd = f.cwd.as_deref().unwrap_or("").replace(&std::env::var("HOME").unwrap_or_default(), "~");
            println!("  {:<6} {:<38} {:<32} {:<10} {:<9} {}  {}", f.agent, truncate(&f.title, 38), truncate_left(&cwd, 32), place, status, &f.session_id.get(..8).unwrap_or(""), f.args.join(" "));
        }
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
            let Response::Launchers { launchers } = client::request(&Request::Launchers)? else { anyhow::bail!("unexpected reply") };
            (launchers.into_iter().filter(|l| l.agent_id != "shell").map(|l| l.short).collect::<Vec<_>>(), rest.join(" "))
        }
    };
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    print_response(client::request(&Request::Fanout { prompt, launchers: agents, cwd })?)
}

fn cmd_groups() -> anyhow::Result<()> {
    let Response::Groups { groups } = client::request(&Request::Groups)? else { anyhow::bail!("unexpected reply") };
    if groups.is_empty() {
        println!("no fan-outs");
    }
    for g in groups {
        println!("{}  \"{}\"  in {}", g.id, truncate(&g.prompt, 60), g.repo);
        for m in g.members {
            let stat = m.stat.map_or("worktree missing".into(), |s| format!("{} files +{} -{}", s.files, s.added, s.removed));
            println!("  {:>3}  {:<8} {stat}", m.session, m.launcher);
        }
    }
    Ok(())
}

/// Continue a session dino didn't start (see `dino found`).
fn cmd_continue(prefix: &str) -> anyhow::Result<()> {
    let Response::Found { sessions } = client::request(&Request::Found { cloud: false, running_only: false })? else { anyhow::bail!("unexpected reply") };
    let session = sessions.into_iter().find(|f| f.session_id.starts_with(prefix)).ok_or_else(|| anyhow::anyhow!("no session matching {prefix}"))?;
    if session.pid.is_some() {
        eprintln!("moving \"{}\" into dino (waits for its current turn to finish)…", session.title);
    }
    print_response(client::request(&Request::Adopt { session, cwd: None })?)
}
