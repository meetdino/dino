//! `lid` on Linux: keeping agents running with the lid closed is macOS's `pmset disablesleep` for
//! now. On Linux, `dino power` reports and changes nothing. A server has no lid. A laptop's lid
//! goes through logind (`HandleLidSwitch`, a `handle-lid-switch` inhibitor), which a later phase can take.

use dino_core::ipc::PowerInfo;

use crate::Daemon;

#[derive(Default)]
pub(crate) struct Lid;

impl Lid {
    /// Nothing held: dinod doesn't keep a Linux machine awake.
    pub(crate) fn info(&self) -> PowerInfo {
        PowerInfo::default()
    }
}

impl Daemon {
    pub(crate) fn power_info(&self) -> PowerInfo {
        PowerInfo { awake: self.awake.holders(), ..self.lid.info() }
    }
}

pub(crate) fn start(_d: std::sync::Arc<Daemon>) {}

pub(crate) fn stop(_d: &Daemon) {}

/// `dino lid-watchdog <pid>`: never started on Linux.
pub fn watchdog(_pid: i32) {}

/// `Power { action }`: the state; setting it up is macOS-only.
pub(crate) fn serve(d: &Daemon, action: &str) -> Result<PowerInfo, String> {
    match action {
        "status" => Ok(PowerInfo { ready: Some(false), ..d.power_info() }),
        "setup" | "remove" => Err("Running agents with the lid closed is macOS-only for now.".into()),
        other => Err(format!("unknown power action “{other}”")),
    }
}
