//! Terminal panes: an emulator (`alacritty_terminal`) fed with a program's output, rendered as a
//! ratatui widget, with input encoded the way the program asked for. Output arrives either from a
//! local PTY (the daemon) or over a socket from dinod (clients); see [`Transport`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Row, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor, Processor, Rgb};
use alacritty_terminal::Term;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use terminput::{Encoding, KittyFlags};

/// Colors reported to apps that query them (OSC 10/11); agents use this to pick a light/dark theme.
const DEFAULT_FG: Rgb = Rgb { r: 0xd8, g: 0xd8, b: 0xd8 };
const DEFAULT_BG: Rgb = Rgb { r: 0x16, g: 0x16, b: 0x1a };

/// Env markers from a parent agent session that would confuse a child agent.
const STRIP_ENV: &[&str] = &["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SSE_PORT", "CLAUDE_CODE_ENTRYPOINT"];

/// The ways a program switches back from the alternate screen (DECRST 1049, 1047, 47).
const ALT_OFF: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];

/// A program that exits this soon after leaving the alternate screen (a fullscreen agent) ends on
/// what it showed there, not on the main screen it switched back to on the way out.
const KEEP_ALT_WITHIN: Duration = Duration::from_secs(10);

/// The OSC sequences dino reads, which the parser drops: `ESC ] <kind> ; text`, ended by BEL or
/// ST. 9 is a desktop notification, 7 the folder a shell is in, 133 a shell's prompt marks.
const OSC: &[u8] = b"\x1b]";
const READ: [&str; 3] = ["9", "7", "133"];

/// Longer than any of those worth reading: an unended one this long is dropped, not kept waiting.
const OSC_MAX: usize = 4096;

pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
}

/// Where keystrokes and resizes go: a local PTY, or a socket to dinod.
pub trait Transport: Send + Sync {
    fn write(&self, bytes: Vec<u8>);
    fn resize(&self, cols: u16, rows: u16);
    /// The terminal's foreground process group, when it's a local PTY.
    fn foreground(&self) -> Option<u32> {
        None
    }
}

/// State shared between the output pump and whoever owns the pane.
pub struct Shared {
    pub dirty: AtomicBool,
    pub bell: AtomicBool,
    /// Total bells rung, for observers that poll.
    pub bells: AtomicU64,
    pub exited: AtomicBool,
    /// The program's exit code, once it has exited (a signal counts as 1).
    pub exit_code: Mutex<Option<u32>>,
    pub title: Mutex<Option<String>>,
    /// Answer the app's terminal queries (cursor position, colors). Turn off while a real
    /// terminal downstream receives the same bytes, or the app gets two answers.
    pub answer_queries: AtomicBool,
    transport: OnceLock<Arc<dyn Transport>>,
    size: Mutex<(u16, u16)>,
    /// All output up to EOF has been processed.
    drained: AtomicBool,
    /// At exit the screen became the alternate screen as last seen (see [`KEEP_ALT_WITHIN`]),
    /// so what clients streamed no longer matches it.
    pub kept_alt: AtomicBool,
    /// Desktop notifications the program sent (OSC 9), counted for observers that poll, and the
    /// last one's text. Codex sends one when it waits on the user.
    pub notices: AtomicU64,
    pub notice: Mutex<Option<String>>,
    /// From a shell with shell integration: the folder it's in (OSC 7), the exit code of the last
    /// command it ran (OSC 133 D), and how many prompts it has shown (OSC 133 A).
    pub cwd: Mutex<Option<String>>,
    pub last_exit: Mutex<Option<i32>>,
    pub prompts: AtomicU64,
    /// What the last command printed, between its OSC 133 C and D (see [`OUTPUT_MAX_LINES`]).
    pub last_output: Mutex<Option<String>>,
}

/// The most of a command's output kept: its last lines, up to this many characters.
pub const OUTPUT_MAX_LINES: usize = 200;
pub const OUTPUT_MAX_CHARS: usize = 16_000;

#[derive(Clone)]
struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let s = &self.0;
        let answer = s.answer_queries.load(Ordering::Relaxed);
        match event {
            Event::Wakeup => s.dirty.store(true, Ordering::Relaxed),
            Event::Bell => {
                s.bell.store(true, Ordering::Relaxed);
                s.bells.fetch_add(1, Ordering::Relaxed);
            }
            Event::Title(t) => *s.title.lock().unwrap() = Some(t),
            Event::ResetTitle => *s.title.lock().unwrap() = None,
            Event::PtyWrite(text) if answer => write_to(s, text.into_bytes()),
            Event::ColorRequest(index, fmt) if answer => {
                let rgb = if index == NamedColor::Background as usize { DEFAULT_BG } else { DEFAULT_FG };
                write_to(s, fmt(rgb).into_bytes());
            }
            Event::TextAreaSizeRequest(fmt) if answer => {
                let (cols, rows) = *s.size.lock().unwrap();
                write_to(s, fmt(window_size(cols, rows)).into_bytes());
            }
            _ => {}
        }
    }
}

fn write_to(s: &Shared, bytes: Vec<u8>) {
    if let Some(t) = s.transport.get() {
        t.write(bytes);
    }
}

struct TermSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

struct PtyTransport {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
}

impl Transport for PtyTransport {
    fn write(&self, bytes: Vec<u8>) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(&bytes).and_then(|_| w.flush());
    }
    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.lock().unwrap().resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }
    fn foreground(&self) -> Option<u32> {
        self.master.lock().unwrap().process_group_leader().and_then(|p| u32::try_from(p).ok())
    }
}

/// The parser, and what it needs to notice the program leaving the alternate screen.
struct Feed {
    processor: Processor,
    /// The end of the last chunk, for a switch back split across two.
    carry: Vec<u8>,
    left_alt: Option<LeftAlt>,
    /// The start of an OSC dino reads (see [`READ`]) whose end hasn't arrived yet.
    osc: Vec<u8>,
    /// Where the running command's output began (OSC 133 C), counted from the top of the
    /// scrollback, and the scrollback size then (so a full scrollback dropping lines is seen).
    output_from: Option<(usize, usize)>,
}

/// The alternate screen as it was when the program last left it.
struct LeftAlt {
    at: Instant,
    /// Its rows, styled, without the blank ones at the bottom.
    rows: Vec<String>,
    /// The main screen's scrollback size and cursor line right after switching back: what the
    /// program printed from there on came after the alternate screen.
    history: usize,
    line: i32,
}

pub struct Pane {
    term: Arc<FairMutex<Term<Listener>>>,
    feed: Mutex<Feed>,
    pub shared: Arc<Shared>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
    /// The program's process, when it runs on a local PTY.
    pid: OnceLock<u32>,
}

impl Pane {
    fn emulator(cols: u16, rows: u16, answer_queries: bool) -> Self {
        let shared = Arc::new(Shared {
            dirty: AtomicBool::new(false),
            bell: AtomicBool::new(false),
            bells: AtomicU64::new(0),
            exited: AtomicBool::new(false),
            exit_code: Mutex::new(None),
            title: Mutex::new(None),
            answer_queries: AtomicBool::new(answer_queries),
            transport: OnceLock::new(),
            size: Mutex::new((cols, rows)),
            drained: AtomicBool::new(false),
            kept_alt: AtomicBool::new(false),
            notices: AtomicU64::new(0),
            notice: Mutex::new(None),
            cwd: Mutex::new(None),
            last_exit: Mutex::new(None),
            prompts: AtomicU64::new(0),
            last_output: Mutex::new(None),
        });
        let term = new_term(&shared, cols, rows);
        let feed = Feed { processor: Processor::new(), carry: Vec::new(), left_alt: None, osc: Vec::new(), output_from: None };
        Self { term: Arc::new(FairMutex::new(term)), feed: Mutex::new(feed), shared, killer: Mutex::new(None), pid: OnceLock::new() }
    }

    /// Run a program on a local PTY. `tap` sees every chunk of raw output (dinod forwards it to
    /// attached clients), under the same lock as [`Pane::replay_then`], and an empty chunk at EOF.
    pub fn spawn(spec: SpawnSpec, cols: u16, rows: u16, mut tap: impl FnMut(&[u8]) + Send + 'static) -> anyhow::Result<Arc<Self>> {
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.cwd(cwd);
        }
        for var in STRIP_ENV {
            cmd.env_remove(var);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let pane = Arc::new(Self::emulator(cols, rows, true));
        *pane.killer.lock().unwrap() = Some(child.clone_killer());
        if let Some(pid) = child.process_id() {
            let _ = pane.pid.set(pid);
        }
        let mut reader = pair.master.try_clone_reader()?;
        let transport = PtyTransport { writer: Mutex::new(pair.master.take_writer()?), master: Mutex::new(pair.master) };
        let _ = pane.shared.transport.set(Arc::new(transport));

        let weak: Weak<Self> = Arc::downgrade(&pane);
        std::thread::Builder::new().name("pty-read".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let Some(pane) = weak.upgrade() else { break };
                let mut term = pane.term.lock();
                pane.advance(&mut term, &buf[..n]);
                pane.shared.dirty.store(true, Ordering::Relaxed);
                tap(&buf[..n]);
            }
            if let Some(pane) = weak.upgrade() {
                let mut term = pane.term.lock();
                pane.keep_alt(&mut term);
                pane.shared.drained.store(true, Ordering::Relaxed);
            }
            tap(&[]);
        })?;
        let shared = pane.shared.clone();
        std::thread::Builder::new().name("pty-wait".into()).spawn(move || {
            let code = child.wait().map_or(1, |s| s.exit_code());
            // The last screen is final once the output is read, unless something the program left
            // behind keeps the terminal open.
            let since = Instant::now();
            while !shared.drained.load(Ordering::Relaxed) && since.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(10));
            }
            *shared.exit_code.lock().unwrap() = Some(code);
            shared.exited.store(true, Ordering::Relaxed);
            shared.dirty.store(true, Ordering::Relaxed);
        })?;
        Ok(pane)
    }

    /// An emulator whose bytes come from elsewhere (see [`Pane::feed`]) and whose input goes to `transport`.
    pub fn remote(transport: Arc<dyn Transport>, cols: u16, rows: u16) -> Arc<Self> {
        let pane = Arc::new(Self::emulator(cols, rows, true));
        let _ = pane.shared.transport.set(transport);
        pane
    }

    /// A program that has already exited, its last screen restored from `replay` bytes (see
    /// [`Pane::replay`]). Input goes nowhere.
    pub fn ended(replay: &[u8], cols: u16, rows: u16, exit_code: Option<u32>) -> Arc<Self> {
        let pane = Arc::new(Self::emulator(cols, rows, false));
        pane.feed(replay);
        *pane.shared.exit_code.lock().unwrap() = exit_code;
        pane.mark_exited();
        pane
    }

    pub fn exit_code(&self) -> Option<u32> {
        *self.shared.exit_code.lock().unwrap()
    }

    /// Process program output.
    pub fn feed(&self, bytes: &[u8]) {
        let mut term = self.term.lock();
        self.advance(&mut term, bytes);
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    /// Parse output, noting what the alternate screen showed each time the program leaves it.
    fn advance(&self, term: &mut Term<Listener>, bytes: &[u8]) {
        let mut guard = self.feed.lock().unwrap();
        let feed = &mut *guard;
        // Up to each command's start and end mark first: where the cursor is then is where its
        // output begins and ends.
        let mut fed = 0;
        for (kind, text, end) in oscs(&mut feed.osc, bytes) {
            let s = &self.shared;
            match kind {
                // `9;4;…` is a progress report, not a notice.
                "9" if !text.is_empty() && !text.starts_with("4;") => {
                    *s.notice.lock().unwrap() = Some(text);
                    s.notices.fetch_add(1, Ordering::Relaxed);
                }
                "7" => {
                    if let Some(path) = file_url_path(&text) {
                        *s.cwd.lock().unwrap() = Some(path);
                    }
                }
                "133" => match text.split(';').collect::<Vec<_>>()[..] {
                    ["A", ..] => {
                        s.prompts.fetch_add(1, Ordering::Relaxed);
                    }
                    ["C", ..] => {
                        Self::parse(term, feed, &bytes[fed..end.max(fed)]);
                        fed = end.max(fed);
                        let grid = term.grid();
                        let alt = term.mode().contains(TermMode::ALT_SCREEN);
                        feed.output_from = (!alt).then(|| (grid.history_size() + grid.cursor.point.line.0.max(0) as usize, grid.history_size()));
                    }
                    ["D", ref rest @ ..] => {
                        *s.last_exit.lock().unwrap() = rest.first().and_then(|c| c.parse().ok());
                        Self::parse(term, feed, &bytes[fed..end.max(fed)]);
                        fed = end.max(fed);
                        if let Some(from) = feed.output_from.take() {
                            *s.last_output.lock().unwrap() = output_since(term, from);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Self::parse(term, feed, &bytes[fed..]);
    }

    /// Feed the parser, noting what the alternate screen showed each time the program leaves it.
    fn parse(term: &mut Term<Listener>, feed: &mut Feed, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some((cut, end)) = alt_off(&feed.carry, rest) {
            feed.processor.advance(term, &rest[..cut]);
            let rows = term.mode().contains(TermMode::ALT_SCREEN).then(|| screen_rows(term));
            feed.processor.advance(term, &rest[cut..end]);
            if let Some(rows) = rows.filter(|_| !term.mode().contains(TermMode::ALT_SCREEN)) {
                let grid = term.grid();
                feed.left_alt = Some(LeftAlt { at: Instant::now(), rows, history: grid.history_size(), line: grid.cursor.point.line.0 });
            }
            feed.carry.clear();
            rest = &rest[end..];
        }
        feed.processor.advance(term, rest);
        let keep = ALT_OFF.iter().map(|p| p.len()).max().unwrap_or(1) - 1;
        feed.carry.extend_from_slice(&rest[rest.len().saturating_sub(keep)..]);
        let drop = feed.carry.len().saturating_sub(keep);
        feed.carry.drain(..drop);
    }

    /// At exit: a program that left the alternate screen just before (a fullscreen agent quitting)
    /// ends on that screen, followed by what it printed on the way out, instead of on the main
    /// screen alone.
    fn keep_alt(&self, term: &mut Term<Listener>) {
        let Some(left) = self.feed.lock().unwrap().left_alt.take() else { return };
        if left.at.elapsed() > KEEP_ALT_WITHIN || term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let grid = term.grid();
        let history = grid.history_size() as i32;
        let from = (left.line - (history - left.history as i32)).max(-history);
        let mut after: Vec<String> = (from..grid.screen_lines() as i32).map(|line| styled_row(&grid[Line(line)], grid.columns())).collect();
        while after.last().is_some_and(|r| r.is_empty()) {
            after.pop();
        }
        let screen: Vec<String> = left.rows.into_iter().chain(after).map(|r| r + "\x1b[0m").collect();
        let (cols, rows) = self.size();
        *term = new_term(&self.shared, cols, rows);
        let mut feed = self.feed.lock().unwrap();
        feed.processor = Processor::new();
        feed.processor.advance(term, screen.join("\r\n").as_bytes());
        self.shared.kept_alt.store(true, Ordering::Relaxed);
    }

    pub fn mark_exited(&self) {
        self.shared.exited.store(true, Ordering::Relaxed);
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    pub fn kill(&self) {
        if let Some(mut k) = self.killer.lock().unwrap().take() {
            let _ = k.kill();
        }
    }

    /// The program it was started with (a shell, an agent), on this Mac.
    pub fn pid(&self) -> Option<u32> {
        self.pid.get().copied()
    }

    /// The process group in the foreground: the program itself, or whatever it's running now
    /// (a shell's current command).
    pub fn foreground(&self) -> Option<u32> {
        self.shared.transport.get()?.foreground()
    }

    pub fn size(&self) -> (u16, u16) {
        *self.shared.size.lock().unwrap()
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 || (cols, rows) == self.size() {
            return;
        }
        *self.shared.size.lock().unwrap() = (cols, rows);
        self.term.lock().resize(TermSize { cols: cols as usize, rows: rows as usize });
        if let Some(t) = self.shared.transport.get() {
            t.resize(cols, rows);
        }
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        write_to(&self.shared, bytes.into());
    }

    pub fn is_exited(&self) -> bool {
        self.shared.exited.load(Ordering::Relaxed)
    }

    pub fn title(&self) -> Option<String> {
        self.shared.title.lock().unwrap().clone()
    }

    /// Bytes that bring a fresh terminal of the same size to this pane's state: up to `history`
    /// scrollback lines, the screen, cursor, title, and the input modes the app turned on.
    pub fn replay(&self, history: usize) -> Vec<u8> {
        let term = self.term.lock();
        self.replay_of(&term, history)
    }

    /// Compute the replay and run `f` with it before any further output is processed, so a
    /// subscriber added in `f` sees exactly the bytes after the replay.
    pub fn replay_then<R>(&self, history: usize, f: impl FnOnce(Vec<u8>) -> R) -> R {
        let term = self.term.lock();
        let bytes = self.replay_of(&term, history);
        f(bytes)
    }

    /// What the screen shows as plain text, with up to `history` lines of scrollback above it;
    /// trailing blanks and blank lines at the end left out.
    pub fn text(&self, history: usize) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        let hist = if alt { 0 } else { grid.history_size().min(history) as i32 };
        let cols = grid.columns();
        let mut lines: Vec<String> = Vec::new();
        for line in -hist..grid.screen_lines() as i32 {
            lines.push(plain_row(&grid[Line(line)], cols));
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn replay_of(&self, term: &Term<Listener>, history: usize) -> Vec<u8> {
        let grid = term.grid();
        let mode = *term.mode();
        let mut out = String::new();
        let alt = mode.contains(TermMode::ALT_SCREEN);
        if alt {
            out.push_str("\x1b[?1049h");
        }
        let hist = if alt { 0 } else { grid.history_size().min(history) as i32 };
        let rows = grid.screen_lines() as i32;
        let cols = grid.columns();
        for (n, line) in (-hist..rows).enumerate() {
            if n > 0 {
                out.push_str("\r\n");
            }
            out.push_str(&styled_row(&grid[Line(line)], cols));
            out.push_str("\x1b[0m");
        }
        let cursor = term.grid().cursor.point;
        out.push_str(&format!("\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1));
        let set = |on: bool, code: &str, out: &mut String| {
            if on {
                out.push_str(&format!("\x1b[?{code}h"));
            }
        };
        set(mode.contains(TermMode::APP_CURSOR), "1", &mut out);
        set(mode.contains(TermMode::BRACKETED_PASTE), "2004", &mut out);
        set(mode.contains(TermMode::MOUSE_REPORT_CLICK), "1000", &mut out);
        set(mode.contains(TermMode::MOUSE_DRAG), "1002", &mut out);
        set(mode.contains(TermMode::MOUSE_MOTION), "1003", &mut out);
        set(mode.contains(TermMode::SGR_MOUSE), "1006", &mut out);
        set(mode.contains(TermMode::FOCUS_IN_OUT), "1004", &mut out);
        if mode.contains(TermMode::APP_KEYPAD) {
            out.push_str("\x1b=");
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.push_str("\x1b[?25l");
        }
        let kitty = [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ]
        .iter()
        .filter(|(m, _)| mode.contains(*m))
        .map(|(_, bit)| bit)
        .sum::<u8>();
        if kitty > 0 {
            out.push_str(&format!("\x1b[={kitty};1u"));
        }
        if let Some(title) = self.title() {
            out.push_str(&format!("\x1b]2;{title}\x07"));
        }
        out.into_bytes()
    }

    /// Forward a host key event, honoring the modes the child app has enabled.
    pub fn send_key(&self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mode = *self.term.lock().mode();
        // Leaving scrollback on any keypress, like most terminals.
        self.term.lock().scroll_display(Scroll::Bottom);

        if mode.contains(TermMode::APP_CURSOR) && key.modifiers == KeyModifiers::NONE {
            let c = match key.code {
                KeyCode::Up => Some('A'),
                KeyCode::Down => Some('B'),
                KeyCode::Right => Some('C'),
                KeyCode::Left => Some('D'),
                KeyCode::Home => Some('H'),
                KeyCode::End => Some('F'),
                _ => None,
            };
            if let Some(c) = c {
                return self.write(format!("\x1bO{c}"));
            }
        }

        let Ok(ev) = terminput_crossterm::to_terminput(crossterm::event::Event::Key(key)) else { return };
        let mut buf = [0u8; 32];
        if let Ok(n) = ev.encode(&mut buf, encoding(mode)) {
            self.write(buf[..n].to_vec());
        }
    }

    /// `text` as typed text, never keys: escapes and other control characters (C0 but tab and line
    /// breaks, DEL, C1) are dropped, so it can't end the bracketed paste early (`ESC[201~`) and go
    /// on to press keys in the program (`ESC[Z`, Shift+Tab).
    pub fn paste(&self, text: &str) {
        let text: String = text.chars().filter(|&c| matches!(c, '\t' | '\n' | '\r') || !c.is_control()).collect();
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            self.write(format!("\x1b[200~{text}\x1b[201~"));
        } else {
            self.write(text.replace("\r\n", "\r").replace('\n', "\r"));
        }
    }

    /// Mouse wheel: report to the app if it asked for mouse events, otherwise scroll our history.
    pub fn scroll(&self, lines: i32, col: u16, row: u16) {
        let mut term = self.term.lock();
        let mode = *term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) {
            let button = if lines > 0 { 64 } else { 65 };
            drop(term);
            for _ in 0..lines.unsigned_abs() {
                self.write(format!("\x1b[<{button};{};{}M", col + 1, row + 1));
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            drop(term);
            let seq = if lines > 0 { "\x1bOA" } else { "\x1bOB" };
            self.write(seq.repeat(lines.unsigned_abs() as usize));
        } else {
            term.scroll_display(Scroll::Delta(lines));
            self.shared.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Forward a click, drag, release or move at pane-local (`col`, `row`) if the app asked for
    /// mouse reporting. Returns false when the app isn't listening, so the host can use the event.
    pub fn mouse(&self, ev: crossterm::event::MouseEvent, col: u16, row: u16) -> bool {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind as K};
        let mode = *self.term.lock().mode();
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        let button = |b: MouseButton| match b {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        };
        // (button code, is release)
        let (mut code, release) = match ev.kind {
            K::Down(b) => (button(b), false),
            K::Up(b) => (button(b), true),
            K::Drag(b) if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) => (button(b) + 32, false),
            K::Moved if mode.contains(TermMode::MOUSE_MOTION) => (35, false),
            _ => return true,
        };
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            code += 4;
        }
        if ev.modifiers.contains(KeyModifiers::ALT) {
            code += 8;
        }
        if ev.modifiers.contains(KeyModifiers::CONTROL) {
            code += 16;
        }
        let (x, y) = (col as u32 + 1, row as u32 + 1);
        if mode.contains(TermMode::SGR_MOUSE) {
            self.write(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }));
        } else {
            // Legacy X10 encoding: release has no button, coordinates cap at 223.
            let code = if release { 3 + (code & !3) } else { code };
            let enc = |v: u32| (32 + v.min(223)) as u8;
            self.write(vec![0x1b, b'[', b'M', 32 + code as u8, enc(x), enc(y)]);
        }
        true
    }

    /// Draw the visible grid into `area`. Returns where the host cursor should go, if visible.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let term = self.term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;

        for cell in content.display_iter {
            let row = cell.point.line.0 + offset;
            let col = cell.point.column.0 as u16;
            if row < 0 || row as u16 >= area.height || col >= area.width {
                continue;
            }
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let Some(out) = buf.cell_mut((area.x + col, area.y + row as u16)) else { continue };
            let mut symbol = String::new();
            symbol.push(if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth() {
                symbol.extend(extra);
            }
            out.set_symbol(&symbol);
            out.set_style(style(cell.fg, cell.bg, cell.flags));
        }

        let cursor = content.cursor.point;
        let visible = content.mode.contains(TermMode::SHOW_CURSOR) && offset == 0;
        let (x, y) = (cursor.column.0 as u16, cursor.line.0);
        (visible && y >= 0 && (y as u16) < area.height && x < area.width)
            .then(|| Position::new(area.x + x, area.y + y as u16))
    }

    /// Lines scrolled back from the bottom (0 = live).
    pub fn display_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.kill();
    }
}

fn new_term(shared: &Arc<Shared>, cols: u16, rows: u16) -> Term<Listener> {
    let config = Config { kitty_keyboard: true, ..Config::default() };
    Term::new(config, &TermSize { cols: cols as usize, rows: rows as usize }, Listener(shared.clone()))
}

/// The OSCs dino reads (see [`READ`]) in `bytes`, as their kind, text and where in `bytes` they
/// end, with `pending` the
/// unended start of one from earlier chunks, left holding the start of one still unended.
fn oscs(pending: &mut Vec<u8>, bytes: &[u8]) -> Vec<(&'static str, String, usize)> {
    let before = pending.len();
    let joined;
    let data = if pending.is_empty() {
        bytes
    } else {
        joined = [pending.as_slice(), bytes].concat();
        &joined
    };
    let mut out = vec![];
    let mut at = 0;
    let mut rest = None;
    while let Some(i) = data[at..].windows(OSC.len()).position(|w| w == OSC).map(|i| at + i) {
        let body = &data[i + OSC.len()..];
        let Some((end, stop)) = body.iter().enumerate().find_map(|(k, &b)| match b {
            0x07 => Some((k, 1)),
            0x1b if body.get(k + 1) == Some(&b'\\') => Some((k, 2)),
            _ => None,
        }) else {
            rest = Some(i);
            break;
        };
        let text = String::from_utf8_lossy(&body[..end]);
        let (kind, text) = text.split_once(';').unwrap_or((&text, ""));
        at = i + OSC.len() + end + stop;
        if let Some(kind) = READ.iter().find(|k| **k == kind) {
            out.push((*kind, text.trim().to_string(), at.saturating_sub(before)));
        }
    }
    pending.clear();
    match rest {
        Some(i) if data.len() - i <= OSC_MAX => pending.extend_from_slice(&data[i..]),
        Some(_) => {}
        // One may begin at the very end.
        None if data.ends_with(&OSC[..1]) => pending.push(OSC[0]),
        None => {}
    }
    out
}

/// The path in an OSC 7 `file://host/path` URL, percent-decoded.
fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = rest[rest.find('/')?..].as_bytes();
    let mut out = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        let hex = path.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) if path[i] == b'%' => {
                out.push(b);
                i += 3;
            }
            _ => {
                out.push(path[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Where `bytes` switch back from the alternate screen, as the range to cut out; it starts at 0
/// when the switch began at the end of the last chunk (`carry`).
fn alt_off(carry: &[u8], bytes: &[u8]) -> Option<(usize, usize)> {
    let split = ALT_OFF.iter().find_map(|p| (1..p.len()).find(|&k| carry.ends_with(&p[..k]) && bytes.starts_with(&p[k..])).map(|k| (0, p.len() - k)));
    split.or_else(|| ALT_OFF.iter().filter_map(|p| bytes.windows(p.len()).position(|w| w == *p).map(|i| (i, i + p.len()))).min())
}

/// A row as plain text, without trailing blanks.
fn plain_row(row: &alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>, cols: usize) -> String {
    let mut text = String::new();
    for c in 0..cols {
        let cell = &row[Column(c)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        text.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            text.extend(extra);
        }
    }
    text.trim_end().to_string()
}

/// What a command printed: the lines from `from` (where its output began, counted from the top
/// of the scrollback, with the scrollback size then) up to the cursor, its last
/// [`OUTPUT_MAX_LINES`] and [`OUTPUT_MAX_CHARS`]. None if it ran on the alternate screen (an
/// editor, a pager) or printed nothing.
fn output_since(term: &Term<Listener>, (from, history_then): (usize, usize)) -> Option<String> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return None;
    }
    let grid = term.grid();
    let history = grid.history_size();
    // A full scrollback keeps its size as it drops lines; a growing one tells how far it moved.
    let first = from as i64 - history as i64 - (history as i64 - history_then as i64).min(0);
    let first = first.max(-(history as i64)) as i32;
    let last = grid.cursor.point.line.0;
    let cols = grid.columns();
    let mut lines: Vec<String> = (first..=last).filter(|l| *l < grid.screen_lines() as i32).map(|l| plain_row(&grid[Line(l)], cols)).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    let start = lines.len().saturating_sub(OUTPUT_MAX_LINES);
    let mut text = lines[start..].join("\n");
    if text.len() > OUTPUT_MAX_CHARS {
        let cut = text.len() - OUTPUT_MAX_CHARS;
        let cut = (cut..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
        text = text[cut..].to_string();
    }
    (!text.is_empty()).then_some(text)
}

/// The screen's rows, styled, without the blank ones at the bottom.
fn screen_rows(term: &Term<Listener>) -> Vec<String> {
    let grid = term.grid();
    let mut rows: Vec<String> = (0..grid.screen_lines() as i32).map(|line| styled_row(&grid[Line(line)], grid.columns())).collect();
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

/// A row as text with its colors and attributes, without trailing default blanks (a fresh
/// terminal is already blank there); empty when the row is blank.
fn styled_row(row: &Row<Cell>, cols: usize) -> String {
    let end = (0..cols).rev().find(|&c| !is_blank(&row[Column(c)])).map_or(0, |c| c + 1);
    let mut out = String::new();
    let mut last = String::new();
    for c in 0..end {
        let cell = &row[Column(c)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let sgr = sgr(cell);
        if sgr != last {
            out.push_str(&sgr);
            last = sgr;
        }
        out.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            out.extend(extra);
        }
    }
    out
}

fn window_size(cols: u16, rows: u16) -> WindowSize {
    WindowSize { num_cols: cols, num_lines: rows, cell_width: 8, cell_height: 16 }
}

fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == AColor::Named(NamedColor::Background)
        && !cell.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
}

/// Full SGR sequence (reset first) for a cell's colors and attributes.
fn sgr(cell: &Cell) -> String {
    let mut codes = vec!["0".to_string()];
    let f = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if f.contains(flag) {
            codes.push(code.into());
        }
    }
    if let Some(c) = sgr_color(cell.fg, 30, 90, 38) {
        codes.push(c);
    }
    if let Some(c) = sgr_color(cell.bg, 40, 100, 48) {
        codes.push(c);
    }
    format!("\x1b[{}m", codes.join(";"))
}

fn sgr_color(c: AColor, base: u8, bright: u8, ext: u8) -> Option<String> {
    match c {
        AColor::Spec(Rgb { r, g, b }) => Some(format!("{ext};2;{r};{g};{b}")),
        AColor::Indexed(i) => Some(format!("{ext};5;{i}")),
        AColor::Named(n) => {
            let i = n as usize;
            match i {
                0..8 => Some((base as usize + i).to_string()),
                8..16 => Some((bright as usize + i - 8).to_string()),
                _ => None,
            }
        }
    }
}

fn encoding(mode: TermMode) -> Encoding {
    if !mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
        return Encoding::Xterm;
    }
    let mut flags = KittyFlags::empty();
    flags.set(KittyFlags::DISAMBIGUATE_ESCAPE_CODES, mode.contains(TermMode::DISAMBIGUATE_ESC_CODES));
    flags.set(KittyFlags::REPORT_EVENT_TYPES, mode.contains(TermMode::REPORT_EVENT_TYPES));
    flags.set(KittyFlags::REPORT_ALTERNATE_KEYS, mode.contains(TermMode::REPORT_ALTERNATE_KEYS));
    flags.set(KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES, mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC));
    Encoding::Kitty(flags)
}

fn style(fg: AColor, bg: AColor, flags: Flags) -> Style {
    let mut s = Style::default().fg(color(fg)).bg(color(bg));
    let mut m = Modifier::empty();
    m.set(Modifier::BOLD, flags.contains(Flags::BOLD));
    m.set(Modifier::ITALIC, flags.contains(Flags::ITALIC));
    m.set(Modifier::DIM, flags.contains(Flags::DIM));
    m.set(Modifier::UNDERLINED, flags.intersects(Flags::ALL_UNDERLINES));
    m.set(Modifier::CROSSED_OUT, flags.contains(Flags::STRIKEOUT));
    m.set(Modifier::REVERSED, flags.contains(Flags::INVERSE));
    s = s.add_modifier(m);
    s
}

fn color(c: AColor) -> Color {
    match c {
        AColor::Spec(Rgb { r, g, b }) => Color::Rgb(r, g, b),
        AColor::Indexed(i) => Color::Indexed(i),
        AColor::Named(n) => match n {
            NamedColor::Black | NamedColor::DimBlack => Color::Black,
            NamedColor::Red | NamedColor::DimRed => Color::Red,
            NamedColor::Green | NamedColor::DimGreen => Color::Green,
            NamedColor::Yellow | NamedColor::DimYellow => Color::Yellow,
            NamedColor::Blue | NamedColor::DimBlue => Color::Blue,
            NamedColor::Magenta | NamedColor::DimMagenta => Color::Magenta,
            NamedColor::Cyan | NamedColor::DimCyan => Color::Cyan,
            NamedColor::White | NamedColor::DimWhite => Color::Gray,
            NamedColor::BrightBlack => Color::DarkGray,
            NamedColor::BrightRed => Color::LightRed,
            NamedColor::BrightGreen => Color::LightGreen,
            NamedColor::BrightYellow => Color::LightYellow,
            NamedColor::BrightBlue => Color::LightBlue,
            NamedColor::BrightMagenta => Color::LightMagenta,
            NamedColor::BrightCyan => Color::LightCyan,
            NamedColor::BrightWhite => Color::White,
            _ => Color::Reset,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> Pane {
        Pane::emulator(40, 6, false)
    }

    fn exit(p: &Pane) {
        let mut term = p.term.lock();
        p.keep_alt(&mut term);
    }

    const FULLSCREEN: &[&[u8]] = &[b"before\r\n", b"\x1b[?1049h\x1b[Hconversation\r\nPELICAN", b"\x1b[?1049l", b"\r\nResume this session with:\r\n"];

    #[test]
    fn a_fullscreen_program_ends_on_its_last_screen() {
        let p = pane();
        for chunk in FULLSCREEN {
            p.feed(chunk);
        }
        assert_eq!(p.text(100), "before\n\nResume this session with:");
        exit(&p);
        assert_eq!(p.text(100), "conversation\nPELICAN\n\nResume this session with:");
        assert!(p.shared.kept_alt.load(Ordering::Relaxed));
        // What a fresh attach, or a saved screen, brings back.
        let again = Pane::ended(&p.replay(100), 40, 6, Some(0));
        assert_eq!(again.text(100), "conversation\nPELICAN\n\nResume this session with:");
    }

    #[test]
    fn the_switch_back_is_found_across_chunks() {
        assert_eq!(alt_off(b"", b"ab\x1b[?1049lcd"), Some((2, 10)));
        assert_eq!(alt_off(b"xx\x1b[?10", b"49lcd"), Some((0, 3)));
        assert_eq!(alt_off(b"", b"\x1b[?47l"), Some((0, 6)));
        assert_eq!(alt_off(b"", b"\x1b[?1049h"), None);
        let p = pane();
        p.feed(b"\x1b[?1049hPELICAN\x1b[?1");
        p.feed(b"049l\r\nbye\r\n");
        exit(&p);
        assert_eq!(p.text(100), "PELICAN\n\nbye");
    }

    #[test]
    fn a_long_exit_message_follows_the_screen() {
        let p = pane();
        p.feed(b"\x1b[?1049hPELICAN\x1b[?1049l");
        for n in 0..10 {
            p.feed(format!("line {n}\r\n").as_bytes());
        }
        exit(&p);
        let text = p.text(100);
        assert!(text.starts_with("PELICAN\nline 0\nline 1"), "{text}");
        assert!(text.ends_with("line 9"), "{text}");
    }

    #[test]
    fn otherwise_the_screen_is_left_alone() {
        // Never on the alternate screen.
        let p = pane();
        p.feed(b"plain output\r\n");
        exit(&p);
        assert_eq!(p.text(100), "plain output");
        assert!(!p.shared.kept_alt.load(Ordering::Relaxed));
        // Still on it at exit: the replay puts the alternate screen back as it is.
        let p = pane();
        p.feed(b"\x1b[?1049hfullscreen");
        exit(&p);
        assert!(!p.shared.kept_alt.load(Ordering::Relaxed));
        assert!(p.replay(100).starts_with(b"\x1b[?1049h"));
        assert_eq!(p.text(100), "fullscreen");
        // Left it long before exiting, like vim in a shell: the shell's screen stays.
        let p = pane();
        p.feed(b"$ vim\r\n\x1b[?1049hfile\x1b[?1049l$ exit\r\n");
        p.feed.lock().unwrap().left_alt.as_mut().unwrap().at = Instant::now() - KEEP_ALT_WITHIN - Duration::from_secs(1);
        exit(&p);
        assert_eq!(p.text(100), "$ vim\n$ exit");
        assert!(!p.shared.kept_alt.load(Ordering::Relaxed));
    }

    #[test]
    fn notices_are_read_even_split_across_chunks() {
        let p = pane();
        p.feed(b"working\x1b]9;Approval requested: touch x\x07more");
        assert_eq!(p.shared.notices.load(Ordering::Relaxed), 1);
        assert_eq!(p.shared.notice.lock().unwrap().as_deref(), Some("Approval requested: touch x"));
        // Split in the introducer, then in the text, ended by ST.
        p.feed(b"\x1b]");
        p.feed(b"9;Plan qu");
        assert_eq!(p.shared.notices.load(Ordering::Relaxed), 1);
        p.feed(b"estion\x1b\\after");
        assert_eq!(p.shared.notices.load(Ordering::Relaxed), 2);
        assert_eq!(p.shared.notice.lock().unwrap().as_deref(), Some("Plan question"));
        // Progress reports and titles aren't notices; the text around them still reaches the screen.
        p.feed(b"\x1b]9;4;1;50\x07\x1b]0;title\x07");
        assert_eq!(p.shared.notices.load(Ordering::Relaxed), 2);
        assert!(p.text(100).contains("workingmore"));
        // One that never ends isn't kept forever.
        let mut pending = vec![];
        assert!(oscs(&mut pending, &[b"\x1b]9;".as_slice(), &[b'x'; OSC_MAX]].concat()).is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn a_shell_reports_its_folder_and_its_last_exit_code() {
        let p = pane();
        assert_eq!(*p.shared.cwd.lock().unwrap(), None);
        // What shell integration prints around a prompt, split mid-sequence.
        p.feed(b"\x1b]133;D;1\x07\x1b]133;A\x07\x1b]7;file://Mac/tmp/a%20b");
        p.feed(b"/c%C3%A9\x07$ ");
        assert_eq!(p.shared.cwd.lock().unwrap().as_deref(), Some("/tmp/a b/c\u{e9}"));
        assert_eq!(*p.shared.last_exit.lock().unwrap(), Some(1));
        assert_eq!(p.shared.prompts.load(Ordering::Relaxed), 1);
        p.feed(b"true\r\n\x1b]133;C\x07\x1b]133;D;0\x1b\\\x1b]133;A\x07\x1b]7;file:///Users/me\x07$ ");
        assert_eq!(*p.shared.last_exit.lock().unwrap(), Some(0));
        assert_eq!(p.shared.cwd.lock().unwrap().as_deref(), Some("/Users/me"));
        assert_eq!(p.shared.prompts.load(Ordering::Relaxed), 2);
        // An introducer split at the very end of a chunk still counts.
        p.feed(b"\x1b");
        p.feed(b"]133;A\x07");
        assert_eq!(p.shared.prompts.load(Ordering::Relaxed), 3);
        // The marks don't show on the screen.
        assert_eq!(p.text(100), "$ true\n$");
        // What a command printed: from its start mark to its end mark, even in one chunk.
        p.feed(b"false\r\n\x1b]133;C\x07oops: it broke\r\nsecond line\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ ");
        assert_eq!(p.shared.last_output.lock().unwrap().as_deref(), Some("oops: it broke\nsecond line"));
        assert_eq!(*p.shared.last_exit.lock().unwrap(), Some(1));
        // Marks split across chunks.
        p.feed(b"make\r\n\x1b]13");
        p.feed(b"3;C\x07built\r\n\x1b]133;D");
        p.feed(b";0\x07$ ");
        assert_eq!(p.shared.last_output.lock().unwrap().as_deref(), Some("built"));
        // An editor on the alternate screen leaves nothing to report.
        p.feed(b"vi\r\n\x1b]133;C\x07\x1b[?1049hediting\x1b[?1049l\x1b]133;D;0\x07$ ");
        assert_eq!(p.shared.last_output.lock().unwrap().as_deref(), None);
        assert_eq!(file_url_path("file://host"), None);
        assert_eq!(file_url_path("http://x/y"), None);
    }

    /// What the pane sends to its program.
    #[derive(Default)]
    struct Sent(Mutex<Vec<u8>>);

    impl Transport for Sent {
        fn write(&self, bytes: Vec<u8>) {
            self.0.lock().unwrap().extend(bytes);
        }
        fn resize(&self, _cols: u16, _rows: u16) {}
    }

    impl Sent {
        fn drain(&self) -> String {
            String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
        }
    }

    #[test]
    fn a_paste_cant_end_itself_or_press_keys() {
        let sent = Arc::new(Sent::default());
        let p = Pane::remote(sent.clone(), 40, 6);
        // Unbracketed: line breaks become Return, and nothing else is a key.
        p.paste("one\x1b[Z\u{9b}Z\x03\r\ntwo\n");
        assert_eq!(sent.drain(), "one[ZZ\rtwo\r");
        // Bracketed: an embedded end of paste can't close it early.
        p.feed(b"\x1b[?2004h");
        p.paste("fix it\x1b[201~\x1b[Z\x7f\u{85}\tnow\n");
        assert_eq!(sent.drain(), "\x1b[200~fix it[201~[Z\tnow\n\x1b[201~");
    }
}
