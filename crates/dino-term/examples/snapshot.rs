//! Headless check: run a program in a pane and print what it rendered.
//! usage: cargo run -p dino-term --example snapshot -- <secs> <program> [args...]

use std::time::Duration;

use dino_term::{Pane, SpawnSpec};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args.next().unwrap_or("3".into()).parse()?;
    let program = args.next().unwrap_or("/bin/sh".into());
    let spec = SpawnSpec { program, args: args.collect(), cwd: std::env::current_dir().ok(), env: Default::default() };
    let (w, h) = (110, 36);
    let pane = Pane::spawn(spec, w, h, |_| {})?;
    std::thread::sleep(Duration::from_secs(secs));
    // DINO_SNAP_KEYS: text to type as key events, "\n" = Enter, "~" = Down arrow,
    // "{click:x,y}" = left click at that cell, "{ctrl:c}" = Ctrl+c.
    if let Ok(keys) = std::env::var("DINO_SNAP_KEYS") {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut rest = keys.as_str();
        while let Some(c) = rest.chars().next() {
            if let Some(after) = rest.strip_prefix("{click:") {
                let (spec, tail) = after.split_once('}').unwrap();
                let (x, y) = spec.split_once(',').unwrap();
                let (x, y): (u16, u16) = (x.parse()?, y.parse()?);
                for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
                    pane.mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }, x, y);
                }
                std::thread::sleep(Duration::from_millis(300));
                rest = tail;
                continue;
            }
            if let Some(after) = rest.strip_prefix("{ctrl:") {
                let (k, tail) = after.split_once('}').unwrap();
                pane.send_key(KeyEvent::new(KeyCode::Char(k.chars().next().unwrap()), KeyModifiers::CONTROL));
                std::thread::sleep(Duration::from_millis(300));
                rest = tail;
                continue;
            }
            rest = &rest[c.len_utf8()..];
            let code = match c {
                '\n' => KeyCode::Enter,
                '~' => KeyCode::Down,
                c => KeyCode::Char(c),
            };
            pane.send_key(KeyEvent::new(code, KeyModifiers::NONE));
            std::thread::sleep(Duration::from_millis(30));
        }
        std::thread::sleep(Duration::from_secs(secs));
    }

    // DINO_SNAP_REPLAY: render a fresh emulator fed only with the replay, to check attach fidelity.
    let pane = if std::env::var_os("DINO_SNAP_REPLAY").is_some() {
        struct Null;
        impl dino_term::Transport for Null {
            fn write(&self, _: Vec<u8>) {}
            fn resize(&self, _: u16, _: u16) {}
        }
        let copy = Pane::remote(std::sync::Arc::new(Null), w, h);
        copy.feed(&pane.replay(1000));
        copy
    } else {
        pane
    };
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    let cursor = pane.render(area, &mut buf);
    println!("+{}+", "-".repeat(w as usize));
    for y in 0..h {
        let row: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        println!("|{row}|");
    }
    println!("+{}+  cursor={cursor:?} exited={}", "-".repeat(w as usize), pane.is_exited());
    Ok(())
}
