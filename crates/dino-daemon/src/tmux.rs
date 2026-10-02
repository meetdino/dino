//! A real tmux running in one of dino's shells. tmux stays in charge: dino never puts its own
//! `tmux` on PATH or edits a config. It asks the tmux client's own server, with read-only format
//! queries, what the client is showing, so the tab can follow the active pane: its folder, what
//! runs in it, a name. Closing the tab detaches the client and leaves the server as it was.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What a tmux client in the foreground of a dino shell is showing.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// The tmux the client runs, so the server is asked by its own version.
    pub bin: PathBuf,
    /// The server's socket.
    pub socket: PathBuf,
    /// The client's terminal, which names it to its server.
    pub tty: String,
    /// `session:window.pane` of the active pane.
    pub target: String,
    /// `session:window name`, for the tab.
    pub label: String,
    /// The active pane's folder, as tmux knows it: from its process, else (a process macOS won't
    /// let tmux look at, like `top`) from the pane's own OSC 7.
    pub path: Option<String>,
    /// Something other than a shell runs in the active pane.
    pub busy: bool,
}

/// The tmux and the server socket of `fg`, when it's a tmux client. Its arguments don't change, so
/// this is looked up once per client; [`view`] then asks the server.
pub fn client(fg: u32) -> Option<(PathBuf, PathBuf)> {
    let (bin, args) = command_of(fg)?;
    if bin.file_name()? != "tmux" {
        return None;
    }
    let socket = socket(&args)?;
    Some((bin, socket))
}

/// What tmux client `fg` (of `bin` on `socket`) shows now; `None` when its server can't be asked
/// (gone, or an older tmux without these formats).
pub fn view(fg: u32, bin: &Path, socket: &Path) -> Option<View> {
    // The server's answer for the client with this pid: what its session shows.
    const FMT: &str = "#{client_pid}\t#{client_tty}\t#{session_name}\t#{window_index}\t#{pane_index}\t#{window_name}\t#{?pane_current_path,#{pane_current_path},#{pane_path}}\t#{pane_current_command}";
    let out = Command::new(bin).arg("-S").arg(socket).args(["list-clients", "-F", FMT]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.split('\t').next() == Some(&fg.to_string()))?;
    parse(line).map(|(tty, target, label, path, busy)| View { bin: bin.into(), socket: socket.into(), tty, target, label, path, busy })
}

/// One `list-clients` line: tty, target, label, folder and whether the pane is busy.
fn parse(line: &str) -> Option<(String, String, String, Option<String>, bool)> {
    let f: Vec<&str> = line.split('\t').collect();
    let [_, tty, session, window, pane, name, path, command] = f[..] else { return None };
    let path = (!path.is_empty()).then(|| std::fs::canonicalize(path).map_or(path.to_string(), |p| p.display().to_string()));
    Some((tty.into(), format!("{session}:{window}.{pane}"), format!("{session}:{name}"), path, !is_shell(command)))
}

/// A login shell or a plain one: the pane is at a prompt (or waiting on a job it put back).
fn is_shell(command: &str) -> bool {
    matches!(command.trim_start_matches('-'), "" | "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "elvish" | "xonsh")
}

impl View {
    /// The window showing, as `session:index name`: how alerts say where they came from.
    pub fn window(&self) -> String {
        let window = self.target.split('.').next().unwrap_or(&self.target);
        let name = self.label.split_once(':').map_or("", |(_, n)| n);
        format!("{window} {name}")
    }
}

/// The windows of the client's session that rang their bell since someone last looked at them, as
/// `session:index name`. tmux passes every window's bell on to its client, but not which window.
pub fn rang(v: &View) -> Vec<String> {
    let session = v.target.split(':').next().unwrap_or_default();
    let out = Command::new(&v.bin)
        .arg("-S")
        .arg(&v.socket)
        .args(["list-windows", "-t", session, "-F", "#{window_bell_flag}\t#{session_name}:#{window_index} #{window_name}"])
        .output();
    let Ok(out) = out else { return vec![] };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.strip_prefix("1\t")).map(String::from).collect()
}

/// Detach the client: its server and everything in it keep running.
pub fn detach(v: &View) {
    let _ = Command::new(&v.bin).arg("-S").arg(&v.socket).args(["detach-client", "-t", &v.tty]).output();
}

/// The executable and arguments of `pid`.
fn command_of(pid: u32) -> Option<(PathBuf, Vec<String>)> {
    let out = Command::new("ps").args(["-o", "comm=", "-p", &pid.to_string()]).output().ok()?;
    let bin = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let out = Command::new("ps").args(["-o", "args=", "-p", &pid.to_string()]).output().ok()?;
    let args = String::from_utf8_lossy(&out.stdout).split_whitespace().skip(1).map(String::from).collect();
    // A bare `tmux` found on PATH: ask the same one.
    let bin = if bin.is_absolute() { bin } else { which(&bin)? };
    Some((bin, args))
}

fn which(name: &Path) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// The server's socket from the client's `-S path` or `-L name`, else tmux's default
/// (`$TMUX_TMPDIR`, else /tmp, `tmux-<uid>/default`).
fn socket(args: &[String]) -> Option<PathBuf> {
    let mut name = "default".to_string();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        // Flags come before the command; the first word that isn't one ends them.
        if !a.starts_with('-') {
            break;
        }
        let value = |i: usize| if a.len() > 2 { Some(a[2..].to_string()) } else { args.get(i + 1).cloned() };
        match &a[..a.len().min(2)] {
            "-S" => return value(i).map(PathBuf::from),
            "-L" => name = value(i)?,
            // Flags that take a value: skip it.
            "-c" | "-f" | "-T" if a.len() == 2 => i += 1,
            _ => {}
        }
        i += 1;
    }
    let dir = std::env::var_os("TMUX_TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let path = dir.join(format!("tmux-{}", unsafe { libc::getuid() })).join(name);
    path.exists().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_server_a_client_talks_to() {
        let s = |a: &[&str]| socket(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&["-S", "/x/sock", "attach"]), Some(PathBuf::from("/x/sock")));
        assert_eq!(s(&["-S/x/sock"]), Some(PathBuf::from("/x/sock")));
        assert_eq!(s(&["-f", "/dev/null", "-S", "/y", "new"]), Some(PathBuf::from("/y")));
        // A `-S` after the command is the command's, not tmux's.
        assert_eq!(s(&["new", "-S", "/z"]).filter(|p| p == Path::new("/z")), None);
    }

    #[test]
    fn reads_what_the_client_shows() {
        let (tty, target, label, path, busy) = parse("123\t/dev/ttys004\tmain\t2\t1\tvim\t/tmp\tvim").unwrap();
        assert_eq!((tty.as_str(), target.as_str(), label.as_str(), busy), ("/dev/ttys004", "main:2.1", "main:vim", true));
        assert_eq!(path, std::fs::canonicalize("/tmp").ok().map(|p| p.display().to_string()));
        assert!(!parse("1\tt\tm\t0\t0\tzsh\t\t-zsh").unwrap().4);
        let v = View { bin: "tmux".into(), socket: "/s".into(), tty, target, label, path, busy };
        assert_eq!(v.window(), "main:2 vim");
        assert_eq!(parse("1\tt\tm\t0\t0\tzsh\t\t-zsh").unwrap().3, None);
    }
}
