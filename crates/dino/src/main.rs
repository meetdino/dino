use std::collections::HashMap;
use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal;
use std::sync::{Arc, Mutex};

use dino_core::discover::{self, Inventory};
use dino_core::{Config, Detected, detect_agents, load_keys, proxy_wiring, user_shell};
use dino_proxy::{Activity, Proxy, SessionStats, Usage};
use dino_term::{Pane, SpawnSpec};
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

struct Session {
    id: String,
    name: String,
    stats: SessionStats,
    pane: Pane,
    last_output: Option<Instant>,
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
    fn status(&self) -> Status {
        if self.pane.is_exited() {
            Status::Exited
        } else if self.attention || matches!(self.stats.activity, Some(Activity::NeedsPermission(_))) {
            Status::Attention
        } else if self.stats.in_flight > 0 {
            Status::Thinking
        } else if self.stats.activity == Some(Activity::Working)
            || self.last_output.is_some_and(|t| t.elapsed() < ACTIVE_WINDOW)
        {
            Status::Working
        } else if self.unseen_done {
            Status::Done
        } else {
            Status::Idle
        }
    }
}

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

struct Launcher {
    agent_id: String,
    /// Session name stem, e.g. "claude", "free".
    short: String,
    label: String,
    program: String,
}

struct App {
    sessions: Vec<Session>,
    focused: usize,
    mode: Mode,
    launchers: Vec<Launcher>,
    pane_size: (u16, u16),
    quit: bool,
    started: Instant,
    proxy: Proxy,
    next_id: u64,
    config: Config,
    inventory: Arc<Mutex<Option<Inventory>>>,
    welcome_opened: Instant,
}

impl App {
    fn new(detected: Vec<Detected>, proxy: Proxy, free_tier: bool) -> Self {
        let mut launchers = vec![];
        for d in detected {
            let program: String = d.path.to_string_lossy().into();
            if d.kind.id == "claude" && free_tier {
                launchers.push(Launcher { agent_id: "claude-free".into(), short: "free".into(), label: "Claude Code · free models".into(), program: program.clone() });
            }
            launchers.push(Launcher { agent_id: d.kind.id.into(), short: d.kind.id.into(), label: d.kind.name.into(), program });
        }
        let shell = user_shell();
        let shell_name = shell.rsplit('/').next().unwrap_or("shell").to_string();
        launchers.push(Launcher { agent_id: "shell".into(), short: "shell".into(), label: format!("Shell ({shell_name})"), program: shell });
        Self { sessions: vec![], focused: 0, mode: Mode::Picker { selected: 0 }, launchers, pane_size: (80, 24), quit: false, started: Instant::now(), proxy, next_id: 1, config: Config::load(), inventory: Arc::default(), welcome_opened: Instant::now() }
    }

    fn spawn(&mut self, launcher: usize, extra_args: &[String]) {
        let l = &self.launchers[launcher];
        let id = self.next_id.to_string();
        self.next_id += 1;
        let (env, mut args) = proxy_wiring(&l.agent_id, self.config.route, &|provider| self.proxy.base_url(&id, provider));
        args.extend_from_slice(extra_args);
        let spec = SpawnSpec {
            program: l.program.clone(),
            args,
            cwd: std::env::current_dir().ok(),
            env: env.into_iter().collect::<HashMap<_, _>>(),
        };
        match Pane::spawn(spec, self.pane_size.0, self.pane_size.1) {
            Ok(pane) => {
                let base = l.short.clone();
                let n = self.sessions.iter().filter(|s| s.name.starts_with(&base)).count();
                let name = if n == 0 { base } else { format!("{base}-{}", n + 1) };
                self.sessions.push(Session { id, name, stats: SessionStats::default(), pane, last_output: None, attention: false, unseen_done: false });
                self.focus(self.sessions.len() - 1);
            }
            Err(e) => eprintln!("spawn failed: {e}"),
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
        self.sessions.get(self.focused).map(|s| &s.pane)
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
            self.sessions.remove(self.focused);
            self.focused = self.focused.min(self.sessions.len().saturating_sub(1));
        }
        if self.sessions.is_empty() {
            self.mode = Mode::Picker { selected: 0 };
        }
    }

    /// Drain PTY-side signals. Returns true if anything needs a redraw.
    fn poll_sessions(&mut self) -> bool {
        let mut redraw = false;
        for (i, s) in self.sessions.iter_mut().enumerate() {
            let stats = self.proxy.stats.session(&s.id);
            if (stats.requests, stats.in_flight, stats.usage.output) != (s.stats.requests, s.stats.in_flight, s.stats.usage.output)
                || stats.activity != s.stats.activity
            {
                redraw = true;
            }
            let finished = stats.activity == Some(Activity::Done)
                && matches!(s.stats.activity, Some(Activity::Working | Activity::NeedsPermission(_)));
            if finished && i != self.focused {
                s.unseen_done = true;
            }
            s.stats = stats;
            if s.pane.shared.dirty.swap(false, Ordering::Relaxed) {
                s.last_output = Some(Instant::now());
                redraw = true;
            }
            if s.pane.shared.bell.swap(false, Ordering::Relaxed) && i != self.focused {
                s.attention = true;
                redraw = true;
            }
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
                let first_run = !self.config.onboarded;
                match key.code {
                    KeyCode::Char('y' | 'Y') | KeyCode::Enter if first_run => self.finish_onboarding(true),
                    KeyCode::Char('n' | 'N') if first_run => self.finish_onboarding(false),
                    KeyCode::Char('r') if !first_run => {
                        self.config.route = !self.config.route;
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

    fn finish_onboarding(&mut self, route: bool) {
        self.config.onboarded = true;
        self.config.route = route;
        let _ = self.config.save();
        self.leave_modal();
    }

    fn draw(&mut self, f: &mut Frame) {
        if self.mode == Mode::Welcome && !self.config.onboarded {
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
                let mut spans = vec![Span::from(format!(" {} ", s.name)).bold().fg(ACCENT)];
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
                f.render_widget(Paragraph::new(" no sessions").fg(MUTED), header);
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

    fn draw_sidebar(&self, f: &mut Frame, area: Rect) {
        let block = Block::new().borders(Borders::RIGHT).border_style(Style::new().fg(Color::Rgb(0x33, 0x33, 0x3a)));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let mut lines = vec![
            Line::from(vec![Span::from(" dino").bold().fg(ACCENT), Span::from("  agent control").fg(MUTED)]),
            Line::default(),
            Line::from(" AGENTS").fg(MUTED).add_modifier(Modifier::BOLD),
        ];
        let blink = self.started.elapsed().as_millis() / 400 % 2 == 0;
        for (i, s) in self.sessions.iter().enumerate() {
            let (dot, label, color) = match s.status() {
                Status::Thinking => (if blink { "◆" } else { "◇" }, "thinking", Color::LightMagenta),
                Status::Working => (if blink { "●" } else { "◉" }, "working", ACCENT),
                Status::Idle => ("○", "idle", MUTED),
                Status::Done => ("✓", "done", Color::LightCyan),
                Status::Attention => ("!", "needs you", Color::Yellow),
                Status::Exited => ("✕", "exited", Color::Red),
            };
            let name = format!("{:<12}", truncate(&s.name, 12));
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
            if let Some(Activity::NeedsPermission(what)) = &s.stats.activity {
                lines.push(Line::from(format!("    ⚠ {}", truncate(what, 22))).fg(Color::Yellow));
            }
            if s.stats.requests > 0 {
                let model = s.stats.last_model.as_deref().map(short_model).unwrap_or_default();
                if let Some(tier) = &s.stats.tier {
                    // Free tier: which tier the router picked and which model actually answered.
                    lines.push(Line::from(vec![
                        Span::from(format!("    {tier} → ")).fg(Color::LightCyan),
                        Span::from(truncate(&model, 16)).fg(Color::LightCyan),
                    ]));
                }
                let model = if s.stats.tier.is_some() { String::new() } else { model };
                let u = &s.stats.usage;
                lines.push(Line::from(format!("    ↑{} ↓{} {model}", tokens(u.total_input()), tokens(u.output))).fg(MUTED));
            }
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
            let Some(q) = self.proxy.stats.quota(provider) else { continue };
            any = true;
            for (i, (name, w)) in q.windows.iter().filter(|(n, _)| n.ends_with(['h', 'd'])).enumerate() {
                let pct = (w.utilization * 100.0).round() as u16;
                let color = match pct {
                    0..=59 => ACCENT,
                    60..=84 => Color::Yellow,
                    _ => Color::Red,
                };
                let bar_w = width.saturating_sub(24).clamp(3, 8) as usize;
                let filled = ((w.utilization.clamp(0.0, 1.0) * bar_w as f32).round() as usize).min(bar_w);
                let reset = w.resets_in_secs().map(duration).unwrap_or_default();
                let who = if i == 0 { label } else { "" };
                lines.push(Line::from(vec![
                    Span::from(format!(" {who:<6}{name:<4}")).fg(MUTED),
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
        let mut total = Usage::default();
        for s in &self.sessions {
            let u = &s.stats.usage;
            total.input += u.total_input();
            total.output += u.output;
        }
        let free: u64 = self.sessions.iter().filter(|s| s.stats.tier.is_some()).map(|s| s.stats.usage.total_input() + s.stats.usage.output).sum();
        if free > 0 {
            lines.push(Line::from(vec![Span::from(" free  ").fg(MUTED), Span::from(format!("{} tok", tokens(free))), Span::from("  $0.00").fg(ACCENT)]));
        }
        if total.input + total.output > 0 {
            lines.push(Line::from(format!(" tokens ↑{} ↓{}", tokens(total.input), tokens(total.output))).fg(MUTED));
        }
        lines
    }

    fn draw_welcome(&self, f: &mut Frame, area: Rect) {
        let first_run = !self.config.onboarded;
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
                let state = if self.config.route { Span::from("on").fg(ACCENT).bold() } else { Span::from("off").fg(Color::Yellow).bold() };
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
                    Event::Mouse(m) => {
                        let lines = match m.kind {
                            MouseEventKind::ScrollUp => 3,
                            MouseEventKind::ScrollDown => -3,
                            _ => 0,
                        };
                        if lines != 0 && m.column >= SIDEBAR_WIDTH {
                            if let Some(p) = app.focused_pane() {
                                p.scroll(lines, m.column - SIDEBAR_WIDTH, m.row.saturating_sub(1));
                            }
                        }
                    }
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

fn main() -> anyhow::Result<()> {
    let keys = load_keys();
    let free_tier = keys.contains_key("NVIDIA_API_KEY");
    let mut app = App::new(detect_agents(), Proxy::start(keys)?, free_tier);
    // `dino <agent> [agent args...]`, or `dino --welcome` to replay onboarding.
    let mut cli: Vec<String> = std::env::args().skip(1).collect();
    if cli.first().is_some_and(|a| a == "--welcome") {
        cli.remove(0);
        app.config.onboarded = false;
    }
    if !app.config.onboarded && cli.is_empty() {
        app.open_welcome();
    }
    if let Some((arg, extra)) = cli.split_first() {
        let arg = arg.to_lowercase();
        let found = app.launchers.iter().position(|l| l.short == arg).or_else(|| app.launchers.iter().position(|l| l.label.to_lowercase().starts_with(&arg)));
        if let Some(i) = found {
            app.mode = Mode::Pane;
            let (cols, rows) = terminal::size()?;
            app.pane_size = (cols.saturating_sub(SIDEBAR_WIDTH), rows.saturating_sub(1));
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
