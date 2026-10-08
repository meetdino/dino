//! `awake` on Linux: nothing yet. A server doesn't sleep, and a laptop's sleep goes through
//! logind's inhibitors (`systemd-inhibit`), which a later phase can take. dinod holds nothing and
//! lists no holders.

use dino_core::ipc::AwakeHolder;

use crate::Daemon;

#[derive(Default)]
pub(crate) struct Awake;

impl Awake {
    pub(crate) fn holders(&self) -> Vec<AwakeHolder> {
        vec![]
    }

    pub(crate) fn set_scheduled(&self, _on: bool) {}
}

pub(crate) fn apply(_d: &Daemon, _settings: &dino_core::settings::Settings) {}

pub(crate) fn stop(_d: &Daemon) {}
