//! Shell integration for the shells dinod starts, the way Ghostty loads it into its own: each
//! prompt is marked (OSC 133, with the last command's exit code), the shell says which folder it's
//! in (OSC 7) and sets the title. The scripts are in `shell-integration/`; nothing in the user's
//! startup files changes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

const FILES: [(&str, &str); 13] = [
    ("bash/ghostty.bash", include_str!("../shell-integration/bash/ghostty.bash")),
    ("bash/bash-preexec.sh", include_str!("../shell-integration/bash/bash-preexec.sh")),
    ("bash/dino.bash", include_str!("../shell-integration/bash/dino.bash")),
    ("bash/dino-posix.bash", include_str!("../shell-integration/bash/dino-posix.bash")),
    ("zsh/.zshenv", include_str!("../shell-integration/zsh/.zshenv")),
    ("zsh/ghostty.zshenv", include_str!("../shell-integration/zsh/ghostty.zshenv")),
    ("zsh/ghostty-integration", include_str!("../shell-integration/zsh/ghostty-integration")),
    ("zsh/dino-tmux-start.zsh", include_str!("../shell-integration/zsh/dino-tmux-start.zsh")),
    ("zsh/dino-tmux.zsh", include_str!("../shell-integration/zsh/dino-tmux.zsh")),
    ("zsh/dino-term.zsh", include_str!("../shell-integration/zsh/dino-term.zsh")),
    ("bash/dino-term.bash", include_str!("../shell-integration/bash/dino-term.bash")),
    ("zsh/dino-agents.zsh", include_str!("../shell-integration/zsh/dino-agents.zsh")),
    ("bash/dino-agents.bash", include_str!("../shell-integration/bash/dino-agents.bash")),
];

/// Ghostty's terminfo entries (from libghostty-spm, which draws dino's panes), as ncurses keeps
/// them: by the first letter's hex code.
const TERMINFO: [(&str, &[u8]); 2] = [
    ("78/xterm-ghostty", include_bytes!("../terminfo/78/xterm-ghostty")),
    ("67/ghostty", include_bytes!("../terminfo/67/ghostty")),
];

/// The folder with Ghostty's terminfo, written when missing or from another dino; `None` when it
/// can't be, and sessions then keep `xterm-256color`.
pub fn terminfo() -> Option<std::path::PathBuf> {
    let dir = dino_core::config_dir().join("terminfo");
    for (name, bytes) in TERMINFO {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(bytes) {
            std::fs::create_dir_all(path.parent()?).ok()?;
            std::fs::write(&path, bytes).ok()?;
        }
    }
    Some(dir)
}

/// The AI line (`dino init`), loaded after the user's own startup files. `__DINO_BIN__` becomes
/// this dino, as `dino init` does it.
const AI: [(&str, &str); 2] = [
    ("zsh/dino-ai.zsh", include_str!("../../dino/shell/dino.zsh")),
    ("bash/dino-ai.bash", include_str!("../../dino/shell/dino.bash")),
];

/// What Ghostty turns on unless told otherwise: a bar cursor while editing, and the folder (at a
/// prompt) or the command (while it runs) as the title.
const FEATURES: &str = "cursor,title";

/// Start shell `program` with the integration loaded: zsh through ZDOTDIR, bash 4 and later
/// through ENV in POSIX mode (Ghostty's way), older bash through --rcfile. Both as login shells,
/// as Terminal and Ghostty start them on macOS. `env` is what dinod adds to its own environment;
/// other shells are left as they are.
pub fn wire(program: &str, env: &mut HashMap<String, String>, args: &mut Vec<String>) {
    wire_from(&dino_core::config_dir().join("shell-integration"), program, env, args);
}

fn wire_from(dir: &Path, program: &str, env: &mut HashMap<String, String>, args: &mut Vec<String>) {
    let var = |env: &HashMap<String, String>, k: &str| env.get(k).cloned().or_else(|| std::env::var(k).ok()).filter(|v| !v.is_empty());
    let name = Path::new(program).file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !matches!(name, "zsh" | "bash") {
        return;
    }
    if let Err(e) = install(dir) {
        eprintln!("dinod: no shell integration: {e}");
        return;
    }
    let ours: Vec<String> = match name {
        "zsh" => {
            // ghostty.zshenv puts the user's own back before anything else reads it.
            if let Some(z) = var(env, "ZDOTDIR") {
                env.insert("GHOSTTY_ZSH_ZDOTDIR".into(), z);
            }
            env.insert("ZDOTDIR".into(), dir.join("zsh").display().to_string());
            vec!["-l".into()]
        }
        _ if bash_major(program) >= 4 => {
            // The contract ghostty.bash reads (libghostty's termio/shell_integration.zig).
            env.insert("GHOSTTY_BASH_INJECT".into(), "1".into());
            if let Some(e) = var(env, "ENV") {
                env.insert("GHOSTTY_BASH_ENV".into(), e);
            }
            env.insert("ENV".into(), dir.join("bash/dino-posix.bash").display().to_string());
            // POSIX mode would keep history in ~/.sh_history.
            if var(env, "HISTFILE").is_none() {
                if let Some(home) = var(env, "HOME") {
                    env.insert("HISTFILE".into(), format!("{home}/.bash_history"));
                    env.insert("GHOSTTY_BASH_UNEXPORT_HISTFILE".into(), "1".into());
                }
            }
            vec!["--posix".into(), "-l".into()]
        }
        _ => vec!["--rcfile".into(), dir.join("bash/dino.bash").display().to_string()],
    };
    env.insert("GHOSTTY_SHELL_FEATURES".into(), FEATURES.into());
    args.splice(0..0, ours);
}

/// Writes the scripts to `dir`, where the shells read them, when they're missing or from another dino.
fn install(dir: &Path) -> std::io::Result<()> {
    for (name, text) in FILES {
        let path = dir.join(name);
        write_if_changed(&path, text)?;
    }
    let bin = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "dino".into());
    let bin = format!("'{}'", bin.replace('\'', r"'\''"));
    for (name, text) in AI {
        write_if_changed(&dir.join(name), &text.replace("__DINO_BIN__", &bin))?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(path, text)?;
    }
    Ok(())
}

/// The major version of bash at `program`, asked once.
fn bash_major(program: &str) -> u32 {
    static SEEN: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap();
    *seen.get_or_insert_with(HashMap::new).entry(program.to_string()).or_insert_with(|| {
        std::process::Command::new(program)
            .args(["-c", "echo ${BASH_VERSINFO[0]}"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_shell_gets_its_way_in() {
        let dir = std::env::temp_dir().join(format!("dino-shell-{}", std::process::id()));

        let (mut env, mut args) = (HashMap::from([("ZDOTDIR".to_string(), "/u/zdot".to_string())]), vec![]);
        wire_from(&dir, "/bin/zsh", &mut env, &mut args);
        assert_eq!(args, ["-l"]);
        assert_eq!(env["ZDOTDIR"], dir.join("zsh").display().to_string());
        assert_eq!(env["GHOSTTY_ZSH_ZDOTDIR"], "/u/zdot");
        assert_eq!(env["GHOSTTY_SHELL_FEATURES"], "cursor,title");
        assert!(dir.join("zsh/.zshenv").exists() && dir.join("zsh/ghostty-integration").exists());

        // macOS's own bash is 3.2.
        if bash_major("/bin/bash") == 3 {
            let (mut env, mut args) = (HashMap::new(), vec![]);
            wire_from(&dir, "/bin/bash", &mut env, &mut args);
            assert_eq!(args, ["--rcfile".to_string(), dir.join("bash/dino.bash").display().to_string()]);
            assert!(!env.contains_key("ENV"));
        }

        // Not a shell with an integration: untouched.
        let (mut env, mut args) = (HashMap::new(), vec!["-x".to_string()]);
        wire_from(&dir, "/bin/sh", &mut env, &mut args);
        assert!(env.is_empty() && args == ["-x"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
