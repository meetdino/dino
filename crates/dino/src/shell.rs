//! Shell integration: `dino init <shell>` prints the script, `dino shell install|uninstall` adds or
//! removes the one line that loads it from the shell's startup file.

use std::path::{Path, PathBuf};

const ZSH: &str = include_str!("../shell/dino.zsh");
const BASH: &str = include_str!("../shell/dino.bash");
const FISH: &str = include_str!("../shell/dino.fish");

const BEGIN: &str = "# >>> dino >>>";
const END: &str = "# <<< dino <<<";

/// The shell `$SHELL` names, when dino has an integration for it.
fn current() -> Option<String> {
    let name = std::env::var("SHELL").ok()?.rsplit('/').next()?.to_string();
    script(&name).map(|_| name)
}

fn script(shell: &str) -> Option<&'static str> {
    match shell {
        "zsh" => Some(ZSH),
        "bash" => Some(BASH),
        "fish" => Some(FISH),
        _ => None,
    }
}

/// This dino, quoted for the shell, so the script works without dino on the PATH.
fn this_dino() -> String {
    let path = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "dino".into());
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// `dino init zsh|bash|fish`: the script to source (`eval "$(dino init zsh)"`).
pub fn init(shell: Option<&str>) -> anyhow::Result<()> {
    let shell = shell.map(String::from).or_else(current).ok_or_else(|| anyhow::anyhow!("usage: dino init zsh|bash|fish"))?;
    let text = script(&shell).ok_or_else(|| anyhow::anyhow!("dino's shell integration supports zsh, bash and fish, not {shell}"))?;
    print!("{}", with_completions(&shell, text).replace("__DINO_BIN__", &this_dino()));
    Ok(())
}

/// The integration with dino's Tab completion after it. fish's only when no completions folder of
/// fish's has a dino.fish (Homebrew's, the user's own): fish would load that one too.
fn with_completions(shell: &str, text: &str) -> String {
    let completion = crate::complete::script(shell).unwrap_or_default();
    match shell {
        "fish" => format!(
            "{text}\n# Tab completion, unless a completions folder has dino's already.\n\
             set -l _dino_has_completions\n\
             for d in $fish_complete_path\n    test -f $d/dino.fish; and set _dino_has_completions 1; and break\nend\n\
             if not set -q _dino_has_completions[1]\n{completion}end\n"
        ),
        _ => format!("{text}\n{completion}"),
    }
}

fn rc_file(shell: &str) -> anyhow::Result<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME isn't set"))?);
    Ok(match shell {
        "zsh" => std::env::var("ZDOTDIR").map(PathBuf::from).unwrap_or(home).join(".zshrc"),
        "bash" => home.join(".bashrc"),
        _ => std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| home.join(".config")).join("fish/config.fish"),
    })
}

fn block(shell: &str) -> String {
    let load = match shell {
        "fish" => format!("{} init fish | source", this_dino()),
        _ => format!("eval \"$({} init {shell})\"", this_dino()),
    };
    format!("{BEGIN}\n{load}\n{END}\n")
}

/// `text` without dino's block, and whether it had one.
fn without_block(text: &str) -> (String, bool) {
    let (Some(start), Some(end)) = (text.find(BEGIN), text.find(END)) else { return (text.to_string(), false) };
    if end < start {
        return (text.to_string(), false);
    }
    let after = text[end + END.len()..].strip_prefix('\n').unwrap_or(&text[end + END.len()..]);
    (format!("{}{}", &text[..start], after), true)
}

/// The startup file's text: empty when there isn't one yet, and an error, never an empty file to
/// write over, when it can't be read or isn't UTF-8.
fn read_rc(rc: &Path) -> anyhow::Result<String> {
    match std::fs::read(rc) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{} isn't UTF-8 text, so dino didn't change it: add the line yourself", rc.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => anyhow::bail!("couldn't read {}, so dino didn't change it: {e}", rc.display()),
    }
}

/// Where `rc` really is: through any symlinks (a dotfiles repo's, say), so writing it keeps the link.
fn resolved(rc: &Path) -> PathBuf {
    let mut path = rc.to_path_buf();
    // A loop of links ends somewhere; the write then fails and says so.
    for _ in 0..40 {
        let Ok(target) = std::fs::read_link(&path) else { break };
        path = path.parent().map(|dir| dir.join(&target)).unwrap_or(target);
    }
    path
}

/// `text` into `path` whole or not at all: a new file beside it, with its permissions, renamed
/// over it.
fn write_whole(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| anyhow::anyhow!("{} isn't a file", path.display()))?;
    let tmp = dir.join(format!(".{}.dino-{}", name.to_string_lossy(), std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        if let Ok(meta) = std::fs::metadata(path) {
            f.set_permissions(meta.permissions())?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("couldn't write {}: {e}", path.display())
    })
}

/// `dino shell install|uninstall [zsh|bash|fish]`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let usage = "usage: dino shell install|uninstall [zsh|bash|fish]";
    let shell = args.get(1).cloned().or_else(current).ok_or_else(|| anyhow::anyhow!(usage))?;
    anyhow::ensure!(script(&shell).is_some(), "dino's shell integration supports zsh, bash and fish, not {shell}");
    let rc = rc_file(&shell)?;
    let target = resolved(&rc);
    let text = read_rc(&target)?;
    let (rest, had) = without_block(&text);
    match args.first().map(String::as_str) {
        Some("install") => {
            let sep = if rest.is_empty() || rest.ends_with('\n') { "" } else { "\n" };
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir)?;
            }
            write_whole(&target, &format!("{rest}{sep}{}", block(&shell)))?;
            println!("{} dino in {}. Open a new {shell} to use it.", if had { "Updated" } else { "Added" }, rc.display());
            if shell == "bash" && cfg!(target_os = "macos") {
                println!("On macOS, bash reads ~/.bash_profile, not ~/.bashrc. Make sure ~/.bash_profile sources ~/.bashrc.");
            }
        }
        Some("uninstall") => {
            if had {
                write_whole(&target, &rest)?;
                println!("Removed dino from {}.", rc.display());
            } else {
                println!("dino isn't in {}.", rc.display());
            }
        }
        _ => anyhow::bail!(usage),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_comes_out_whole() {
        let text = format!("export A=1\n{}alias x=y\n", block("zsh"));
        let (rest, had) = without_block(&text);
        assert!(had);
        assert_eq!(rest, "export A=1\nalias x=y\n");
        assert_eq!(without_block("plain\n"), ("plain\n".to_string(), false));
    }

    #[test]
    fn a_startup_file_is_rewritten_whole_or_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dino-rc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dotfiles")).unwrap();
        // None yet is empty; one that isn't text, or can't be read, is an error, not an empty file.
        assert_eq!(read_rc(&dir.join("none")).unwrap(), "");
        std::fs::write(dir.join("latin1"), b"export A=\xe9\n").unwrap();
        assert!(read_rc(&dir.join("latin1")).is_err());
        assert!(read_rc(&dir).is_err());
        // Through a symlink to the real file, which keeps its permissions; the link stays a link.
        let real = dir.join("dotfiles/zshrc");
        std::fs::write(&real, "export A=1\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join(".zshrc");
        std::os::unix::fs::symlink("dotfiles/zshrc", &link).unwrap();
        assert_eq!(resolved(&link), real);
        write_whole(&resolved(&link), "export A=2\n").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "export A=2\n");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        // A new one, and no temp file left behind.
        write_whole(&dir.join("new"), "x\n").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("new")).unwrap(), "x\n");
        let left = std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".dino-")).count();
        assert_eq!(left, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_script_names_this_dino() {
        for s in ["zsh", "bash", "fish"] {
            assert!(script(s).unwrap().contains("__DINO_BIN__"), "{s}");
            assert!(crate::complete::script(s).unwrap().contains("__DINO_BIN__"), "{s}");
        }
    }

    #[test]
    fn every_script_registers_its_completion() {
        let registers = [("zsh", "compdef _dino_complete dino"), ("bash", "complete -F _dino_complete dino"), ("fish", "complete -c dino -f -a '(__dino_complete)'")];
        for (shell, line) in registers {
            let init = with_completions(shell, script(shell).unwrap());
            assert!(init.starts_with(script(shell).unwrap()), "{shell}");
            assert!(init.contains(line), "{shell}: dino init registers it");
            assert!(crate::complete::script(shell).unwrap().contains(line), "{shell}: so does the standalone script");
        }
        // Each leaves completions someone else registered alone.
        assert!(crate::complete::script("zsh").unwrap().contains("[[ -n ${_comps[dino]-} ]] ||"));
        assert!(crate::complete::script("bash").unwrap().contains("if ! complete -p dino &>/dev/null"));
        assert!(with_completions("fish", "").contains("test -f $d/dino.fish"));
    }

    #[test]
    fn every_shell_passes_the_last_command_and_status_to_ai() {
        for (name, text) in [("zsh", ZSH), ("bash", BASH), ("fish", FISH)] {
            assert!(text.contains("--last"), "{name} must include the last command");
            assert!(text.contains("--status"), "{name} must include the last command's status");
        }
        assert!(FISH.contains("fish_postexec"), "fish must refresh context after a command");
    }

    #[test]
    fn shell_context_preserves_history_and_handoffs() {
        let bash_guard = BASH.find("[[ $last != \"$_DINO_ASKED\" ]] || return 0").unwrap();
        let bash_capture = BASH.find("_DINO_LAST=$last").unwrap();
        assert!(bash_guard < bash_capture);
        assert!(BASH.contains("if [[ -n $last && $last != \\#*"));
        assert!(FISH.contains("string match -q -- '#*' (string trim -l -- $argv[1])"));
        assert!(BASH.contains("ai agent -- $(printf '%q' \"$1\")"));
        assert!(FISH.contains("ai agent -- \"(string escape -- $line)"));
    }

    /// bash's # line, as an interactive bash runs it: each line goes into history, runs, and the
    /// prompt command follows. What the stand-in dino is asked shows the context it got.
    #[test]
    fn bash_asks_with_the_last_command_entered_and_its_status() {
        let dir = std::env::temp_dir().join(format!("dino-bash-context-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (stub, log, script) = (dir.join("dino"), dir.join("asked"), dir.join("dino.bash"));
        std::fs::write(&stub, "#!/bin/sh\nprintf '%s|' \"$@\" >> \"$STUBLOG\"; echo >> \"$STUBLOG\"\n[ \"$2\" = suggest ] && echo 'echo fake-answer'\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::write(&script, BASH).unwrap();
        // A failed command, a # request, an empty Enter (no new history entry: the answer pushed
        // into history never ran), another request; then a comment that isn't one, and a request.
        let lines = ["sh -c 'exit 7'", "# list files", "", "# again", "true", "#! not a request", "# third"];
        let run = format!(
            "source {}; for l in {}; do [[ -n $l ]] && history -s -- \"$l\"; eval \"$l\"; _dino_prompt_command; done",
            script.display(),
            lines.iter().map(|l| format!("'{}'", l.replace('\'', r"'\''"))).collect::<Vec<_>>().join(" ")
        );
        let ok = std::process::Command::new("bash")
            .args(["--norc", "-i", "-c", &run])
            .env("DINO_BIN", &stub)
            .env("STUBLOG", &log)
            .env("HISTFILE", dir.join("history"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(ok.success());
        let asked = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let context: Vec<_> = asked.lines().map(|l| l.split("--last|").nth(1).and_then(|r| r.split("|--|").next()).unwrap_or(l)).collect();
        assert_eq!(context, ["sh -c 'exit 7'|--status|7", "sh -c 'exit 7'|--status|7", "true|--status|0"], "{asked}");
    }

    #[test]
    fn fish_uses_the_configured_ai_key_or_alt_i() {
        assert!(FISH.contains("bind (string unescape -- \"$DINO_AI_KEY\") __dino_ai_line"));
        assert!(FISH.contains("bind \\ei __dino_ai_line"));
    }
}
