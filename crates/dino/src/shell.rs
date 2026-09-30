//! Shell integration: `dino init <shell>` prints the script, `dino shell install|uninstall` adds or
//! removes the one line that loads it from the shell's startup file.

use std::path::PathBuf;

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
    let text = script(&shell).ok_or_else(|| anyhow::anyhow!("dino has no integration for {shell}: zsh, bash and fish"))?;
    print!("{}", text.replace("__DINO_BIN__", &this_dino()));
    Ok(())
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

/// `dino shell install|uninstall [zsh|bash|fish]`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let usage = "usage: dino shell install|uninstall [zsh|bash|fish]";
    let shell = args.get(1).cloned().or_else(current).ok_or_else(|| anyhow::anyhow!(usage))?;
    anyhow::ensure!(script(&shell).is_some(), "dino has no integration for {shell}: zsh, bash and fish");
    let rc = rc_file(&shell)?;
    let text = std::fs::read_to_string(&rc).unwrap_or_default();
    let (rest, had) = without_block(&text);
    match args.first().map(String::as_str) {
        Some("install") => {
            let sep = if rest.is_empty() || rest.ends_with('\n') { "" } else { "\n" };
            if let Some(dir) = rc.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&rc, format!("{rest}{sep}{}", block(&shell)))?;
            println!("{} dino in {}; open a new {shell} to use it", if had { "Updated" } else { "Added" }, rc.display());
            if shell == "bash" && cfg!(target_os = "macos") {
                println!("macOS starts bash as a login shell, which reads ~/.bash_profile: make sure it sources ~/.bashrc");
            }
        }
        Some("uninstall") => {
            if had {
                std::fs::write(&rc, rest)?;
                println!("Removed dino from {}", rc.display());
            } else {
                println!("dino isn't in {}", rc.display());
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
    fn every_script_names_this_dino() {
        for s in ["zsh", "bash", "fish"] {
            assert!(script(s).unwrap().contains("__DINO_BIN__"), "{s}");
        }
    }
}
