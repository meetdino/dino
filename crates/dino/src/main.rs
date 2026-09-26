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
use dino_core::{Detected, detect_agents, proxy_wiring, user_shell};
use dino_proxy::{Proxy, SessionStats, Usage};
use dino_term::{Pane, SpawnSpec};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

const SIDEBAR_WIDTH: u16 = 28;
const ACCENT: Color = Color::Rgb(0x7a, 0xd3, 0x8f);
const MUTED: Color = Color::DarkGray;
/// Output within this window counts as "working".
const ACTIVE_WINDOW: Duration = Duration::from_millis(1500);

struct Session {
    id: String,
    name: String,
    stats: SessionStats,
    pane: Pane,
    last_output: Option<Instant>,
    attention: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    /// A model request is streaming.
    Thinking,
    /// Recent terminal output, e.g. a tool running.
    Working,
    Idle,
    Attention,
    Exited,
}

impl Session {
    fn status(&self) -> Status {
        if self.pane.is_exited() {
            Status::Exited
        } else if self.attention {
            Status::Attention
        } else if self.stats.in_flight > 0 {
            Status::Thinking
        } else if self.last_output.is_some_and(|t| t.elapsed() < ACTIVE_WINDOW) {
            Status::Working
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
}

struct Launcher {
    agent_id: String,
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
}

impl App {
    fn new(detected: Vec<Detected>, proxy: Proxy) -> Self {
        let mut launchers: Vec<Launcher> = detected
            .into_iter()
            .map(|d| Launcher { agent_id: d.kind.id.into(), label: d.kind.name.into(), program: d.path.to_string_lossy().into() })
            .collect();
        let shell = user_shell();
        let shell_name = shell.rsplit('/').next().unwrap_or("shell").to_string();
        launchers.push(Launcher { agent_id: "shell".into(), label: format!("Shell ({shell_name})"), program: shell });
        Self { sessions: vec![], focused: 0, mode: Mode::Picker { selected: 0 }, launchers, pane_size: (80, 24), quit: false, started: Instant::now(), proxy, next_id: 1 }
    }

    fn spawn(&mut self, launcher: usize) {
        let l = &self.launchers[launcher];
        let id = self.next_id.to_string();
        self.next_id += 1;
        let (env, args) = proxy_wiring(&l.agent_id, &|provider| self.proxy.base_url(&id, provider));
        let spec = SpawnSpec {
            program: l.program.clone(),
            args,
            cwd: std::env::current_dir().ok(),
            env: env.into_iter().collect::<HashMap<_, _>>(),
        };
        match Pane::spawn(spec, self.pane_size.0, self.pane_size.1) {
            Ok(pane) => {
                let base = l.label.split(' ').next().unwrap_or("agent").to_lowercase();
                let n = self.sessions.iter().filter(|s| s.name.starts_with(&base)).count();
                let name = if n == 0 { base } else { format!("{base}-{}", n + 1) };
                self.sessions.push(Session { id, name, stats: SessionStats::default(), pane, last_output: None, attention: false });
                self.focus(self.sessions.len() - 1);
            }
            Err(e) => eprintln!("spawn failed: {e}"),
        }
    }

    fn focus(&mut self, i: usize) {
        if i < self.sessions.len() {
            self.focused = i;
            self.sessions[i].attention = false;
        }
    }

    fn focused_pane(&self) -> Option<&Pane> {
        self.sessions.get(self.focused).map(|s| &s.pane)
    }

    /// Jump to the next session that needs the user, falling back to plain cycling.
    fn next_attention(&mut self) {
        let n = self.sessions.len();
        let target = (1..=n)
            .map(|d| (self.focused + d) % n)
            .find(|&i| self.sessions[i].status() == Status::Attention)
            .unwrap_or((self.focused + 1) % n.max(1));
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
            if (stats.requests, stats.in_flight, stats.usage.output) != (s.stats.requests, s.stats.in_flight, s.stats.usage.output) {
                redraw = true;
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
                    KeyCode::Char('q') => self.quit = true,
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
                        self.spawn(i);
                    }
                }
                KeyCode::Enter => {
                    let i = *selected;
                    self.mode = Mode::Pane;
                    self.spawn(i);
                }
                KeyCode::Esc if !self.sessions.is_empty() => self.mode = Mode::Pane,
                KeyCode::Char('q') | KeyCode::Esc if self.sessions.is_empty() => self.quit = true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
                _ => {}
            },
        }
    }

    fn draw(&mut self, f: &mut Frame) {
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

        if let Mode::Picker { selected } = self.mode {
            self.draw_picker(f, main, selected);
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
            if s.stats.requests > 0 {
                let model = s.stats.last_model.as_deref().map(short_model).unwrap_or_default();
                let u = &s.stats.usage;
                lines.push(Line::from(format!("      ↑{} ↓{}  {model}", tokens(u.total_input()), tokens(u.output))).fg(MUTED));
            }
        }
        if self.sessions.is_empty() {
            lines.push(Line::from("   none yet").fg(MUTED));
        }
        f.render_widget(Paragraph::new(lines), inner);

        let hint = match self.mode {
            Mode::Command => vec![
                Line::from(" n new   x close   q quit").fg(Color::Yellow),
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
        if let Some(q) = self.proxy.stats.quota("anthropic") {
            for (name, w) in q.windows.iter().filter(|(n, _)| n.ends_with('h') || n.ends_with('d')) {
                let pct = (w.utilization * 100.0).round() as u16;
                let color = match pct {
                    0..=59 => ACCENT,
                    60..=84 => Color::Yellow,
                    _ => Color::Red,
                };
                let bar_w = width.saturating_sub(20).clamp(4, 10) as usize;
                let filled = ((w.utilization.clamp(0.0, 1.0) * bar_w as f32).round() as usize).min(bar_w);
                let reset = w.resets_in_secs().map(duration).unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::from(format!(" {name:<3}")).fg(MUTED),
                    Span::from("█".repeat(filled)).fg(color),
                    Span::from("░".repeat(bar_w - filled)).fg(Color::Rgb(0x3a, 0x3a, 0x44)),
                    Span::from(format!(" {pct:>3}% ")).fg(color),
                    Span::from(reset).fg(MUTED),
                ]));
            }
        } else {
            lines.push(Line::from(" claude  no data yet").fg(MUTED));
        }
        let mut total = Usage::default();
        for s in &self.sessions {
            let u = &s.stats.usage;
            total.input += u.total_input();
            total.output += u.output;
        }
        if total.input + total.output > 0 {
            lines.push(Line::from(format!(" tokens ↑{} ↓{}", tokens(total.input), tokens(total.output))).fg(MUTED));
        }
        lines
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
        // Periodic tick keeps status labels (working → idle) and the spinner fresh.
        if last_tick.elapsed() > Duration::from_millis(400) {
            last_tick = Instant::now();
            redraw = true;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut app = App::new(detect_agents(), Proxy::start()?);
    if let Some(arg) = std::env::args().nth(1) {
        if let Some(i) = app.launchers.iter().position(|l| l.label.to_lowercase().starts_with(&arg.to_lowercase())) {
            app.mode = Mode::Pane;
            let (cols, rows) = terminal::size()?;
            app.pane_size = (cols.saturating_sub(SIDEBAR_WIDTH), rows.saturating_sub(1));
            app.spawn(i);
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
