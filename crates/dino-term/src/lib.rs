//! Terminal panes: an emulator (`alacritty_terminal`) fed with a program's output, rendered as a
//! ratatui widget, with input encoded the way the program asked for. Output arrives either from a
//! local PTY (the daemon) or over a socket from dinod (clients); see [`Transport`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
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
}

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

pub struct Pane {
    term: Arc<FairMutex<Term<Listener>>>,
    processor: Mutex<Processor>,
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
        });
        let config = Config { kitty_keyboard: true, ..Config::default() };
        let term = Term::new(config, &TermSize { cols: cols as usize, rows: rows as usize }, Listener(shared.clone()));
        Self { term: Arc::new(FairMutex::new(term)), processor: Mutex::new(Processor::new()), shared, killer: Mutex::new(None), pid: OnceLock::new() }
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
                pane.processor.lock().unwrap().advance(&mut *term, &buf[..n]);
                pane.shared.dirty.store(true, Ordering::Relaxed);
                tap(&buf[..n]);
            }
            tap(&[]);
        })?;
        let shared = pane.shared.clone();
        std::thread::Builder::new().name("pty-wait".into()).spawn(move || {
            let code = child.wait().map_or(1, |s| s.exit_code());
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
        self.processor.lock().unwrap().advance(&mut *term, bytes);
        self.shared.dirty.store(true, Ordering::Relaxed);
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
            let row = &grid[Line(line)];
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
            lines.push(text.trim_end().to_string());
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
            let row = &grid[Line(line)];
            // Skip trailing default blanks; a fresh terminal is already blank there.
            let end = (0..cols).rev().find(|&c| !is_blank(&row[Column(c)])).map_or(0, |c| c + 1);
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

    pub fn paste(&self, text: &str) {
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
