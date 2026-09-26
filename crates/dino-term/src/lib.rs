//! Embedded terminal panes: a child process on a PTY, emulated by `alacritty_terminal`,
//! rendered as a ratatui widget.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor, Rgb};
use alacritty_terminal::Term;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use terminput::{Encoding, KittyFlags};

/// Colors reported to apps that query them (OSC 10/11); agents use this to pick a light/dark theme.
const DEFAULT_FG: Rgb = Rgb { r: 0xd8, g: 0xd8, b: 0xd8 };
const DEFAULT_BG: Rgb = Rgb { r: 0x16, g: 0x16, b: 0x1a };

pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
}

/// State the PTY thread shares with the UI.
#[derive(Default)]
pub struct Shared {
    pub dirty: AtomicBool,
    pub bell: AtomicBool,
    pub exited: AtomicBool,
    pub title: Mutex<Option<String>>,
    sender: OnceLock<EventLoopSender>,
    size: Mutex<Option<WindowSize>>,
}

#[derive(Clone)]
struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let s = &self.0;
        match event {
            Event::Wakeup => s.dirty.store(true, Ordering::Relaxed),
            Event::Bell => s.bell.store(true, Ordering::Relaxed),
            Event::Title(t) => *s.title.lock().unwrap() = Some(t),
            Event::ResetTitle => *s.title.lock().unwrap() = None,
            Event::ChildExit(_) | Event::Exit => {
                s.exited.store(true, Ordering::Relaxed);
                s.dirty.store(true, Ordering::Relaxed);
            }
            Event::PtyWrite(text) => write_pty(s, text.into_bytes()),
            Event::ColorRequest(index, fmt) => {
                let rgb = if index == NamedColor::Background as usize { DEFAULT_BG } else { DEFAULT_FG };
                write_pty(s, fmt(rgb).into_bytes());
            }
            Event::TextAreaSizeRequest(fmt) => {
                if let Some(size) = *s.size.lock().unwrap() {
                    write_pty(s, fmt(size).into_bytes());
                }
            }
            _ => {}
        }
    }
}

fn write_pty(s: &Shared, bytes: Vec<u8>) {
    if let Some(tx) = s.sender.get() {
        let _ = tx.send(Msg::Input(bytes.into()));
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

pub struct Pane {
    term: Arc<FairMutex<Term<Listener>>>,
    pub shared: Arc<Shared>,
    size: (u16, u16),
}

impl Pane {
    pub fn spawn(spec: SpawnSpec, cols: u16, rows: u16) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared::default());
        let listener = Listener(shared.clone());
        let config = Config { kitty_keyboard: true, ..Config::default() };
        let term = Term::new(config, &TermSize { cols: cols as usize, rows: rows as usize }, listener.clone());
        let term = Arc::new(FairMutex::new(term));

        let mut env = spec.env;
        env.entry("TERM".into()).or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into()).or_insert_with(|| "truecolor".into());
        let options = Options {
            shell: Some(Shell::new(spec.program, spec.args)),
            working_directory: spec.cwd,
            drain_on_exit: true,
            env,
        };
        let window = window_size(cols, rows);
        *shared.size.lock().unwrap() = Some(window);
        let pty = tty::new(&options, window, 0)?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let _ = shared.sender.set(event_loop.channel());
        event_loop.spawn();

        Ok(Self { term, shared, size: (cols, rows) })
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == self.size || cols == 0 || rows == 0 {
            return;
        }
        self.size = (cols, rows);
        self.term.lock().resize(TermSize { cols: cols as usize, rows: rows as usize });
        let window = window_size(cols, rows);
        *self.shared.size.lock().unwrap() = Some(window);
        if let Some(tx) = self.shared.sender.get() {
            let _ = tx.send(Msg::Resize(window));
        }
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        write_pty(&self.shared, bytes.into());
    }

    pub fn is_exited(&self) -> bool {
        self.shared.exited.load(Ordering::Relaxed)
    }

    pub fn title(&self) -> Option<String> {
        self.shared.title.lock().unwrap().clone()
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
        if let Some(tx) = self.shared.sender.get() {
            let _ = tx.send(Msg::Shutdown);
        }
    }
}

fn window_size(cols: u16, rows: u16) -> WindowSize {
    WindowSize { num_cols: cols, num_lines: rows, cell_width: 8, cell_height: 16 }
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
