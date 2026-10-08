//! Shell integration for the shells dinod starts, the way Ghostty loads it into its own: each
//! prompt is marked (OSC 133, with the last command's exit code), the shell says which folder it's
//! in (OSC 7) and sets the title. zsh, bash, fish, elvish and nushell, as Ghostty's
//! `shell-integration` and `shell-integration-features` say. The scripts are in
//! `shell-integration/`; nothing in the user's startup files changes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

const FILES: [(&str, &str); 16] = [
    ("bash/ghostty.bash", include_str!("../shell-integration/bash/ghostty.bash")),
    ("bash/bash-preexec.sh", include_str!("../shell-integration/bash/bash-preexec.sh")),
    ("bash/dino.bash", include_str!("../shell-integration/bash/dino.bash")),
    ("bash/dino-posix.bash", include_str!("../shell-integration/bash/dino-posix.bash")),
    ("zsh/.zshenv", include_str!("../shell-integration/zsh/.zshenv")),
    ("zsh/ghostty.zshenv", include_str!("../shell-integration/zsh/ghostty.zshenv")),
    ("zsh/ghostty-integration", include_str!("../shell-integration/zsh/ghostty-integration")),
    ("zsh/dino-tmux-start.zsh", include_str!("../shell-integration/zsh/dino-tmux-start.zsh")),
    // Not loaded by dino: for a .zshrc to load in tmux panes (see the file).
    ("zsh/dino-tmux.zsh", include_str!("../shell-integration/zsh/dino-tmux.zsh")),
    ("zsh/dino-term.zsh", include_str!("../shell-integration/zsh/dino-term.zsh")),
    ("bash/dino-term.bash", include_str!("../shell-integration/bash/dino-term.bash")),
    ("zsh/dino-agents.zsh", include_str!("../shell-integration/zsh/dino-agents.zsh")),
    ("bash/dino-agents.bash", include_str!("../shell-integration/bash/dino-agents.bash")),
    // Ghostty 1.3.1's own, found through XDG_DATA_DIRS.
    ("fish/vendor_conf.d/ghostty-shell-integration.fish", include_str!("../shell-integration/fish/vendor_conf.d/ghostty-shell-integration.fish")),
    ("elvish/lib/ghostty-integration.elv", include_str!("../shell-integration/elvish/lib/ghostty-integration.elv")),
    ("nushell/vendor/autoload/ghostty.nu", include_str!("../shell-integration/nushell/vendor/autoload/ghostty.nu")),
];

/// Ghostty's terminfo entries (from libghostty-spm, which draws dino's panes), as ncurses keeps
/// them: by the first letter's hex code.
#[cfg(target_os = "macos")]
const TERMINFO: [(&str, &[u8]); 2] = [
    ("78/xterm-ghostty", include_bytes!("../terminfo/78/xterm-ghostty")),
    ("67/ghostty", include_bytes!("../terminfo/67/ghostty")),
];
/// Linux's ncurses keeps them by the first letter itself, and doesn't look in the hex folders.
#[cfg(not(target_os = "macos"))]
const TERMINFO: [(&str, &[u8]); 2] = [
    ("x/xterm-ghostty", include_bytes!("../terminfo/78/xterm-ghostty")),
    ("g/ghostty", include_bytes!("../terminfo/67/ghostty")),
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

/// The AI line and Tab completion for `dino` (`dino init`), loaded after the user's own startup
/// files; fish finds its completion through XDG_DATA_DIRS, behind the user's own. `__DINO_BIN__`
/// becomes this dino, as `dino init` does it.
const AI: [(&str, &str); 3] = [
    ("zsh/dino-ai.zsh", concat!(include_str!("../../dino/shell/dino.zsh"), "\n", include_str!("../../dino/shell/completions/dino.zsh"))),
    ("bash/dino-ai.bash", concat!(include_str!("../../dino/shell/dino.bash"), "\n", include_str!("../../dino/shell/completions/dino.bash"))),
    ("fish/vendor_completions.d/dino.fish", include_str!("../../dino/shell/completions/dino.fish")),
];

/// What dino turns on when the app hasn't said what the user's Ghostty config does: a bar cursor
/// while editing, and the folder (at a prompt) or the command (while it runs) as the title.
pub const FEATURES: &str = "cursor,title";

/// The shells with an integration, as Ghostty's `shell-integration` names them.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shell {
    Bash,
    Elvish,
    Fish,
    Nushell,
    Zsh,
}

impl Shell {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "bash" => Self::Bash,
            "elvish" => Self::Elvish,
            "fish" => Self::Fish,
            "nushell" | "nu" => Self::Nushell,
            "zsh" => Self::Zsh,
            _ => return None,
        })
    }
}

/// Start shell `program` with the integration loaded, as Ghostty's `shell-integration` (`mode`:
/// `detect` by the shell's name, `none`, or the shell to set up as) and
/// `shell-integration-features` (`features`, as GHOSTTY_SHELL_FEATURES) say. zsh through ZDOTDIR,
/// bash 4 and later through ENV in POSIX mode (Ghostty's way), older bash through --rcfile, fish,
/// elvish and nushell through XDG_DATA_DIRS (nushell told to `use ghostty *`). zsh, bash, fish and
/// nushell as login shells, as Terminal and Ghostty start them on macOS. `env` is what dinod adds
/// to its own environment. True when what loads hands an agent typed there to dino (zsh's and
/// bash's do, `dino-agents.*`).
pub fn wire(program: &str, env: &mut HashMap<String, String>, args: &mut Vec<String>, mode: &str, features: &str) -> bool {
    wire_from(&dino_core::config_dir().join("shell-integration"), program, env, args, mode, features)
}

fn wire_from(dir: &Path, program: &str, env: &mut HashMap<String, String>, args: &mut Vec<String>, mode: &str, features: &str) -> bool {
    let var = |env: &HashMap<String, String>, k: &str| env.get(k).cloned().or_else(|| std::env::var(k).ok()).filter(|v| !v.is_empty());
    // As Ghostty: the features go to every shell, for an integration loaded by hand too.
    if !features.is_empty() {
        env.insert("GHOSTTY_SHELL_FEATURES".into(), features.into());
    }
    let name = Path::new(program).file_name().and_then(|n| n.to_str()).unwrap_or("");
    let shell = match mode {
        "none" => None,
        "detect" | "" => Shell::named(name),
        forced => Shell::named(forced),
    };
    let Some(shell) = shell else { return false };
    if let Err(e) = install(dir) {
        eprintln!("dinod: no shell integration: {e}");
        return false;
    }
    let ours: Vec<String> = match shell {
        Shell::Zsh => {
            // ghostty.zshenv puts the user's own back before anything else reads it.
            if let Some(z) = var(env, "ZDOTDIR") {
                env.insert("GHOSTTY_ZSH_ZDOTDIR".into(), z);
            }
            env.insert("ZDOTDIR".into(), dir.join("zsh").display().to_string());
            vec!["-l".into()]
        }
        Shell::Bash if bash_major(program) >= 4 => {
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
        Shell::Bash => vec!["--rcfile".into(), dir.join("bash/dino.bash").display().to_string()],
        Shell::Fish | Shell::Elvish | Shell::Nushell => {
            // Each finds its part under the first of XDG_DATA_DIRS and takes it back out.
            env.insert("GHOSTTY_SHELL_INTEGRATION_XDG_DIR".into(), dir.display().to_string());
            let data = var(env, "XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            env.insert("XDG_DATA_DIRS".into(), format!("{}:{data}", dir.display()));
            match shell {
                // Nushell's own `--commands` mode, or its language server: nothing to set up.
                Shell::Nushell if args.iter().any(|a| a == "--commands" || a == "--lsp" || (a.starts_with('-') && !a.starts_with("--") && a.contains('c'))) => vec![],
                Shell::Nushell => vec!["-l".into(), "--execute".into(), "use ghostty *".into()],
                Shell::Fish => vec!["-l".into()],
                // Elvish has no login mode; `use ghostty-integration` in rc.elv loads it, as in Ghostty.
                _ => vec![],
            }
        }
    };
    args.splice(0..0, ours);
    matches!(shell, Shell::Zsh | Shell::Bash)
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
        assert!(wire_from(&dir, "/bin/zsh", &mut env, &mut args, "detect", FEATURES));
        assert_eq!(args, ["-l"]);
        assert_eq!(env["ZDOTDIR"], dir.join("zsh").display().to_string());
        assert_eq!(env["GHOSTTY_ZSH_ZDOTDIR"], "/u/zdot");
        assert_eq!(env["GHOSTTY_SHELL_FEATURES"], "cursor,title");
        assert!(dir.join("zsh/.zshenv").exists() && dir.join("zsh/ghostty-integration").exists());

        // macOS's own bash is 3.2.
        if bash_major("/bin/bash") == 3 {
            let (mut env, mut args) = (HashMap::new(), vec![]);
            assert!(wire_from(&dir, "/bin/bash", &mut env, &mut args, "detect", FEATURES));
            assert_eq!(args, ["--rcfile".to_string(), dir.join("bash/dino.bash").display().to_string()]);
            assert!(!env.contains_key("ENV"));
        }

        // fish, elvish and nushell: through XDG_DATA_DIRS, ahead of what was there.
        let (mut env, mut args) = (HashMap::from([("XDG_DATA_DIRS".to_string(), "/opt/share".to_string())]), vec![]);
        // Nothing there hands an agent to dino.
        assert!(!wire_from(&dir, "/opt/homebrew/bin/fish", &mut env, &mut args, "detect", "cursor:blink,path,title"));
        assert_eq!(args, ["-l"]);
        assert_eq!(env["XDG_DATA_DIRS"], format!("{}:/opt/share", dir.display()));
        assert_eq!(env["GHOSTTY_SHELL_INTEGRATION_XDG_DIR"], dir.display().to_string());
        assert_eq!(env["GHOSTTY_SHELL_FEATURES"], "cursor:blink,path,title");
        assert!(dir.join("fish/vendor_conf.d/ghostty-shell-integration.fish").exists());
        // dino's Tab completion, naming this dino; zsh's and bash's come with the AI line.
        let fish = std::fs::read_to_string(dir.join("fish/vendor_completions.d/dino.fish")).unwrap();
        assert!(fish.contains("complete -c dino") && !fish.contains("__DINO_BIN__"));
        for ai in ["zsh/dino-ai.zsh", "bash/dino-ai.bash"] {
            let text = std::fs::read_to_string(dir.join(ai)).unwrap();
            assert!(text.contains("_dino_complete") && !text.contains("__DINO_BIN__"), "{ai}");
        }
        let (mut env, mut args) = (HashMap::new(), vec![]);
        wire_from(&dir, "/usr/local/bin/elvish", &mut env, &mut args, "detect", FEATURES);
        assert!(args.is_empty());
        assert!(env["XDG_DATA_DIRS"].ends_with(":/usr/local/share:/usr/share"));
        assert!(dir.join("elvish/lib/ghostty-integration.elv").exists());
        let (mut env, mut args) = (HashMap::new(), vec![]);
        wire_from(&dir, "/opt/homebrew/bin/nu", &mut env, &mut args, "detect", FEATURES);
        assert_eq!(args, ["-l", "--execute", "use ghostty *"]);
        assert!(dir.join("nushell/vendor/autoload/ghostty.nu").exists());

        // `shell-integration = none`: no integration, but the features still go, for one loaded by hand.
        let (mut env, mut args) = (HashMap::new(), vec![]);
        assert!(!wire_from(&dir, "/bin/zsh", &mut env, &mut args, "none", "sudo,title"));
        assert!(args.is_empty() && !env.contains_key("ZDOTDIR"));
        assert_eq!(env["GHOSTTY_SHELL_FEATURES"], "sudo,title");
        // A shell named there is set up as that one, whatever it's called.
        let (mut env, mut args) = (HashMap::new(), vec![]);
        wire_from(&dir, "/usr/local/bin/my-fish", &mut env, &mut args, "fish", FEATURES);
        assert_eq!(args, ["-l"]);
        assert!(env.contains_key("GHOSTTY_SHELL_INTEGRATION_XDG_DIR"));

        // Not a shell with an integration: untouched but for the features.
        let (mut env, mut args) = (HashMap::new(), vec!["-x".to_string()]);
        wire_from(&dir, "/bin/sh", &mut env, &mut args, "detect", "");
        assert!(env.is_empty() && args == ["-x"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
