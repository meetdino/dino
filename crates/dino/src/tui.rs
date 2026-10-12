//! `dino tui`: dino-tui, dino's terminal UI (on Linux, its only one), from PATH or where the
//! installer puts it (~/.local/bin). When it's in neither, it's installed there first by the
//! installer compiled in here, scripts/install.sh with `--tui`: the newest dino-tui release for
//! this machine, checked against its SHA256SUMS, the same download and checks the installer makes
//! when it installs dino on Linux.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// scripts/install.sh, as this dino was built with it.
const INSTALLER: &str = include_str!("../../../scripts/install.sh");

/// Where the installer puts it: `$DINO_INSTALL_DIR`, else ~/.local/bin.
fn install_dir() -> PathBuf {
    match std::env::var_os("DINO_INSTALL_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/bin"),
    }
}

/// dino-tui, on PATH or where the installer puts it.
pub(crate) fn find() -> Option<PathBuf> {
    dino_core::which("dino-tui").or_else(|| dino_core::which_in("dino-tui", install_dir().as_os_str()))
}

/// Install dino-tui with the installer; it says how it goes. Its path, once it's there.
pub(crate) fn install() -> anyhow::Result<PathBuf> {
    let mut sh = Command::new("/bin/sh").args(["-s", "--", "--tui"]).stdin(Stdio::piped()).spawn()?;
    sh.stdin.take().expect("piped").write_all(INSTALLER.as_bytes())?;
    let ok = sh.wait()?.success();
    match find() {
        Some(p) if ok => Ok(p),
        _ => anyhow::bail!("dino-tui isn't installed"),
    }
}

/// `dino tui [args...]`: dino-tui in this terminal, in place of this process. dinod first, started
/// as any `dino` command starts it: dino-tui never starts it, so on Linux, without the systemd
/// service, nothing else would.
pub(crate) fn run(args: &[String]) -> anyhow::Result<()> {
    let exe = match find() {
        Some(p) => p,
        None => install()?,
    };
    // When dinod can't start, dino-tui says so and how to start it.
    let _ = crate::client::connect();
    let e = Command::new(&exe).args(args).exec();
    anyhow::bail!("couldn't run {}: {e}", exe.display())
}
