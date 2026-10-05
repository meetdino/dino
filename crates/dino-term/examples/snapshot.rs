//! Headless check: run a program in a pane and print what it rendered.
//! usage: cargo run -p dino-term --example snapshot -- <secs> <program> [args...]

use std::time::Duration;

use dino_term::{Pane, SpawnSpec};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args.next().unwrap_or("3".into()).parse()?;
    let program = args.next().unwrap_or("/bin/sh".into());
    let spec = SpawnSpec { program, args: args.collect(), cwd: std::env::current_dir().ok(), env: Default::default() };
    let (w, h) = (110, 36);
    let pane = Pane::spawn(spec, w, h, |_| {})?;
    std::thread::sleep(Duration::from_secs(secs));
    // DINO_SNAP_KEYS: text to type, "\n" = Enter, "~" = Down arrow,
    // "{click:x,y}" = left click at that cell (SGR mouse), "{ctrl:c}" = Ctrl+c.
    if let Ok(keys) = std::env::var("DINO_SNAP_KEYS") {
        let mut rest = keys.as_str();
        while let Some(c) = rest.chars().next() {
            if let Some(after) = rest.strip_prefix("{click:") {
                let (spec, tail) = after.split_once('}').unwrap();
                let (x, y) = spec.split_once(',').unwrap();
                let (x, y): (u16, u16) = (x.parse()?, y.parse()?);
                pane.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1));
                std::thread::sleep(Duration::from_millis(300));
                rest = tail;
                continue;
            }
            if let Some(after) = rest.strip_prefix("{ctrl:") {
                let (k, tail) = after.split_once('}').unwrap();
                pane.write(vec![k.as_bytes()[0] & 0x1f]);
                std::thread::sleep(Duration::from_millis(300));
                rest = tail;
                continue;
            }
            rest = &rest[c.len_utf8()..];
            match c {
                '\n' => pane.write("\r"),
                '~' => pane.write("\x1b[B"),
                c => pane.write(c.to_string()),
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        // DINO_SNAP_HOLD: seconds to stay alive after typing (default: same as the initial wait).
        let hold = std::env::var("DINO_SNAP_HOLD").ok().and_then(|h| h.parse().ok()).unwrap_or(secs);
        std::thread::sleep(Duration::from_secs(hold));
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
    println!("+{}+", "-".repeat(w as usize));
    let text = pane.text(0);
    let mut lines = text.lines();
    for _ in 0..h {
        let line = lines.next().unwrap_or("");
        println!("|{line}{}|", " ".repeat((w as usize).saturating_sub(line.chars().count())));
    }
    println!("+{}+  exited={}", "-".repeat(w as usize), pane.is_exited());
    Ok(())
}
