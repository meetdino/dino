//! Keeping agents running with the lid closed.
//!
//! Only `pmset disablesleep 1` keeps a MacBook awake with its lid closed and no display attached;
//! `caffeinate` and IOPM assertions stop idle sleep, not lid sleep. The flag needs root and
//! survives restarts, so dino installs, once and with the user's consent, two things that can do
//! nothing else: a sudoers rule that lets this user run exactly `pmset -a disablesleep 0` and
//! `… 1`, and a launch daemon that runs `pmset -a disablesleep 0` at every boot. dinod turns the
//! flag on only while [`step`] says to, and back off as soon as it doesn't.

use std::time::Duration;

use crate::settings::{Lid, LidWhen};

/// What the sudoers rule allows, word for word.
pub const PMSET: &str = "/usr/bin/pmset";
pub const SUDOERS_PATH: &str = "/etc/sudoers.d/dino-lid";
pub const BOOT_LABEL: &str = "com.meetdino.restore-sleep";
pub const BOOT_PATH: &str = "/Library/LaunchDaemons/com.meetdino.restore-sleep.plist";

/// How things stand when dinod decides.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// An agent is working, or waiting on its own background work.
    pub working: bool,
    /// An agent session is open at all.
    pub open: bool,
    /// On the power adapter; `None` when the Mac has no battery (a desktop).
    pub on_adapter: Option<bool>,
    pub battery: Option<u8>,
    /// The Mac reports thermal pressure.
    pub thermal: bool,
    /// How long the lid has been kept awake so far, if it is.
    pub held_for: Option<Duration>,
}

/// Why the lid isn't kept awake. The ones that stop it while it's held are worth telling the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Off {
    Disabled,
    Idle,
    Unplugged,
    Battery(u8),
    /// After this many minutes.
    MaxDuration(u32),
    Thermal,
}

impl Off {
    /// Said to the user when it ends a stretch awake; `None` when that's just the work being done.
    pub fn note(self) -> Option<String> {
        match self {
            Off::Disabled | Off::Idle => None,
            Off::Unplugged => Some("Unplugged from power, so closing the lid sleeps the Mac again".into()),
            Off::Battery(p) => Some(format!("Battery at {p}%, so closing the lid sleeps the Mac again")),
            Off::MaxDuration(m) => Some(format!("Kept awake for {}, so closing the lid sleeps the Mac again", match m {
                    1 => "a minute".into(),
                    60 => "an hour".into(),
                    m if m % 60 == 0 => format!("{} hours", m / 60),
                    m => format!("{m} minutes"),
                })),
            Off::Thermal => Some("The Mac is running hot, so closing the lid sleeps it again".into()),
        }
    }
}

/// Whether to keep the lid awake now. `latched` holds a safety stop (battery, heat, time) so it
/// isn't undone on the next tick: it clears only once its reason has gone.
pub fn step(lid: &Lid, i: &Inputs, latched: &mut Option<Off>) -> Result<(), Off> {
    let wanted = match lid.when {
        LidWhen::Working => i.working,
        LidWhen::Open => i.open,
    };
    // A stop clears once what caused it is over: plugged in or charged up again, cooled down, or
    // a fresh stretch of work after the old one ended.
    if let Some(l) = *latched {
        let over = match l {
            Off::Unplugged | Off::Battery(_) => i.on_adapter != Some(false) || i.battery.is_some_and(|b| b >= lid.min_battery.saturating_add(5)) && lid.on_battery,
            Off::Thermal => !i.thermal,
            Off::MaxDuration(_) => !wanted,
            Off::Disabled | Off::Idle => true,
        };
        if over {
            *latched = None;
        } else {
            return Err(l);
        }
    }
    if !lid.enabled {
        return Err(Off::Disabled);
    }
    if !wanted {
        return Err(Off::Idle);
    }
    let stop = if i.thermal {
        Some(Off::Thermal)
    } else if i.on_adapter == Some(false) && !lid.on_battery {
        Some(Off::Unplugged)
    } else if i.on_adapter == Some(false) && i.battery.is_some_and(|b| b < lid.min_battery) {
        Some(Off::Battery(i.battery.unwrap_or(0)))
    } else if lid.max_hours > 0.0 && i.held_for.is_some_and(|h| h.as_secs_f64() >= lid.max_hours * 3600.0) {
        Some(Off::MaxDuration(((lid.max_hours * 60.0).round() as u32).max(1)))
    } else {
        None
    };
    match stop {
        Some(s) => {
            *latched = Some(s);
            Err(s)
        }
        None => Ok(()),
    }
}

/// `pmset -g`: whether system sleep is disabled right now.
pub fn sleep_disabled(pmset_g: &str) -> Option<bool> {
    pmset_g.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next()? == "SleepDisabled").then(|| w.next() == Some("1"))
    })
}

/// `pmset -g batt`: on the adapter or on battery, and the charge. `(None, None)` with no battery.
pub fn battery(pmset_batt: &str) -> (Option<bool>, Option<u8>) {
    let adapter = if pmset_batt.contains("'AC Power'") {
        Some(true)
    } else if pmset_batt.contains("'Battery Power'") {
        Some(false)
    } else {
        None
    };
    let percent = pmset_batt.lines().find_map(|l| {
        let (before, _) = l.split_once('%')?;
        before.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    });
    match percent {
        Some(p) => (adapter, Some(p)),
        // No battery line: a desktop, where the adapter question doesn't apply.
        None => (adapter.filter(|_| pmset_batt.contains("InternalBattery")), None),
    }
}

/// `pmset -g therm`: thermal pressure, from a recorded warning level or a CPU speed limit.
pub fn thermal(pmset_therm: &str) -> bool {
    pmset_therm.lines().any(|l| {
        let l = l.trim();
        let value = |key: &str| l.strip_prefix(key).and_then(|r| r.trim_start_matches(|c: char| c.is_whitespace() || c == '=' || c == ':').trim().parse::<u32>().ok());
        value("CPU_Speed_Limit").is_some_and(|v| v < 100) || value("Thermal warning level").is_some_and(|v| v > 0)
    })
}

/// A macOS short user name sudoers can take as is.
pub fn valid_user(user: &str) -> bool {
    !user.is_empty() && user.len() <= 64 && user.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && !user.starts_with('-')
}

/// The sudoers rule: `user` may run exactly these two commands as root, nothing else.
pub fn sudoers(user: &str) -> String {
    format!(
        "# dino: keep agents running with the lid closed. Lets {user} turn system sleep off and on,\n\
         # nothing else. Remove it in dino (Settings → Power) or delete this file.\n\
         {user} ALL=(root) NOPASSWD: {PMSET} -a disablesleep 0, {PMSET} -a disablesleep 1\n"
    )
}

/// The boot-time safety net: sleep is back on after every restart, whatever dinod was doing.
pub fn boot_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{BOOT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{PMSET}</string>
		<string>-a</string>
		<string>disablesleep</string>
		<string>0</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#
    )
}

/// The script run as root, once, with the user's password: installs the rule and the boot daemon
/// (checking the rule with `visudo` before it goes in), or removes both. Either way sleep is on
/// afterwards.
pub fn setup_script(install: bool, user: &str) -> String {
    let mut s = String::from("set -eu\n");
    if install {
        s += &format!(
            "tmp=$(/usr/bin/mktemp /etc/sudoers.d/.dino-lid.XXXXXX)\n\
             /bin/cat > \"$tmp\" <<'DINO_SUDOERS'\n{}DINO_SUDOERS\n\
             /bin/chmod 0440 \"$tmp\"\n\
             /usr/sbin/chown root:wheel \"$tmp\"\n\
             /usr/sbin/visudo -cf \"$tmp\" >/dev/null || {{ /bin/rm -f \"$tmp\"; exit 1; }}\n\
             /bin/mv -f \"$tmp\" {SUDOERS_PATH}\n\
             /bin/cat > {BOOT_PATH} <<'DINO_PLIST'\n{}DINO_PLIST\n\
             /bin/chmod 0644 {BOOT_PATH}\n\
             /usr/sbin/chown root:wheel {BOOT_PATH}\n\
             /bin/launchctl bootout system/{BOOT_LABEL} 2>/dev/null || true\n\
             /bin/launchctl bootstrap system {BOOT_PATH}\n",
            sudoers(user),
            boot_plist()
        );
    } else {
        s += &format!(
            "/bin/rm -f {SUDOERS_PATH}\n\
             /bin/launchctl bootout system/{BOOT_LABEL} 2>/dev/null || true\n\
             /bin/rm -f {BOOT_PATH}\n"
        );
    }
    s += &format!("{PMSET} -a disablesleep 0\n");
    s
}

/// `do shell script` for `osascript`, so macOS asks for an administrator's password itself. The
/// script goes inline: a file the user could edit between writing and running it would run as root.
pub fn osascript(script: &str) -> String {
    let quoted = script.replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{quoted}\" with administrator privileges with prompt \"dino wants to keep agents running with the lid closed.\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lid() -> Lid {
        Lid { enabled: true, ..Lid::default() }
    }

    fn working() -> Inputs {
        Inputs { working: true, open: true, on_adapter: Some(true), battery: Some(80), ..Inputs::default() }
    }

    #[test]
    fn held_only_while_on_and_working_and_safe() {
        let mut latch = None;
        assert_eq!(step(&lid(), &working(), &mut latch), Ok(()));
        assert_eq!(step(&Lid::default(), &working(), &mut latch), Err(Off::Disabled), "off by default");
        assert_eq!(step(&lid(), &Inputs { working: false, ..working() }, &mut latch), Err(Off::Idle));
        let open = Lid { when: LidWhen::Open, ..lid() };
        assert_eq!(step(&open, &Inputs { working: false, ..working() }, &mut latch), Ok(()), "while dino has an agent open");
        assert_eq!(step(&lid(), &Inputs { on_adapter: None, battery: None, ..working() }, &mut latch), Ok(()), "a desktop");
    }

    #[test]
    fn safety_stops_hold_until_their_reason_is_gone() {
        let mut latch = None;
        let unplugged = Inputs { on_adapter: Some(false), battery: Some(90), ..working() };
        assert_eq!(step(&lid(), &unplugged, &mut latch), Err(Off::Unplugged));
        assert_eq!(step(&lid(), &working(), &mut latch), Ok(()), "plugged back in");

        let battery = Lid { on_battery: true, min_battery: 30, ..lid() };
        assert_eq!(step(&battery, &unplugged, &mut latch), Ok(()), "on battery, allowed");
        let low = Inputs { battery: Some(29), ..unplugged.clone() };
        assert_eq!(step(&battery, &low, &mut latch), Err(Off::Battery(29)));
        assert_eq!(step(&battery, &Inputs { battery: Some(31), ..unplugged.clone() }, &mut latch), Err(Off::Battery(29)), "not back on at 31%");
        assert_eq!(step(&battery, &Inputs { battery: Some(35), ..unplugged.clone() }, &mut latch), Ok(()), "charged up again");

        assert_eq!(step(&lid(), &Inputs { thermal: true, ..working() }, &mut latch), Err(Off::Thermal));
        assert_eq!(step(&lid(), &working(), &mut latch), Ok(()), "cooled down");

        let long = Inputs { held_for: Some(Duration::from_secs(8 * 3600)), ..working() };
        assert_eq!(step(&lid(), &long, &mut latch), Err(Off::MaxDuration(480)));
        assert_eq!(step(&lid(), &working(), &mut latch), Err(Off::MaxDuration(480)), "the same stretch of work stays stopped");
        assert_eq!(step(&lid(), &Inputs { working: false, ..working() }, &mut latch), Err(Off::Idle));
        assert_eq!(step(&lid(), &working(), &mut latch), Ok(()), "new work after it ended");
    }

    #[test]
    fn reads_pmset() {
        assert_eq!(sleep_disabled("System-wide power settings:\n SleepDisabled\t\t1\nCurrently in use:\n standby 1\n"), Some(true));
        assert_eq!(sleep_disabled(" SleepDisabled\t\t0\n"), Some(false));
        assert_eq!(sleep_disabled("Currently in use:\n"), None);
        assert_eq!(battery("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n"), (Some(true), Some(100)));
        assert_eq!(battery("Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t27%; discharging; 1:12 remaining present: true\n"), (Some(false), Some(27)));
        assert_eq!(battery("Now drawing from 'AC Power'\n"), (None, None), "no battery");
        assert!(!thermal("Note: No thermal warning level has been recorded\nNote: No performance warning level has been recorded\n"));
        assert!(thermal("CPU_Scheduler_Limit \t= 100\n\tCPU_Available_CPUs \t= 8\n\tCPU_Speed_Limit \t= 70\n"));
        assert!(!thermal("CPU_Speed_Limit \t= 100\n"));
    }

    #[test]
    fn setup_files_hold_nothing_but_pmset() {
        assert!(valid_user("alice") && valid_user("a.b-c_d") && !valid_user("x y") && !valid_user("-x") && !valid_user("a,b"));
        let rule = sudoers("alice");
        let grants: Vec<&str> = rule.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(grants, ["alice ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 0, /usr/bin/pmset -a disablesleep 1"]);
        let plist = boot_plist();
        assert!(plist.contains("<string>/usr/bin/pmset</string>") && plist.contains("<true/>"));
        for install in [true, false] {
            let s = setup_script(install, "alice");
            assert!(s.trim_end().ends_with("/usr/bin/pmset -a disablesleep 0"), "sleep comes back either way");
        }
        let a = osascript("echo \"hi\" \\ there");
        assert!(a.contains(r#"echo \"hi\" \\ there"#) && a.ends_with("lid closed.\""));
    }
}
