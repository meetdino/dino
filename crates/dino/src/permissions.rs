//! `dino permissions`: whether macOS lets programs here record the screen, control the Mac
//! (Accessibility) and read every file (Full Disk Access). macOS answers for the app responsible
//! for this process: in a dino terminal that's dino (dinod runs as its launch agent), in another
//! terminal that terminal. The app asks this way too (Settings → General → Permissions): a
//! process's own answer for Screen Recording never changes after its first, and this is a new one
//! each time.
//!
//! The frameworks are opened only here, not linked: every `dino` (dinod, the statusline, the
//! compiler wrapper) would otherwise load them at start.

use std::ffi::CStr;

use serde_json::json;

/// `(id, name, allowed)`, in the order Settings shows them; None where macOS didn't say.
pub fn check() -> Vec<(&'static str, &'static str, Option<bool>)> {
    vec![
        ("screen_recording", "Screen & System Audio Recording", call(c"/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", c"CGPreflightScreenCaptureAccess")),
        ("accessibility", accessibility_name(), call(c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices", c"AXIsProcessTrusted")),
        ("full_disk_access", "Full Disk Access", full_disk()),
    ]
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let checked = check();
    if args.iter().any(|a| a == "--json") {
        let map: serde_json::Map<_, _> = checked.iter().map(|(id, _, ok)| (id.to_string(), json!(ok))).collect();
        println!("{}", serde_json::Value::Object(map));
        return Ok(());
    }
    if !args.is_empty() {
        println!(
            "usage: dino permissions [--json]\n\nWhat macOS lets programs here do. In a dino terminal they get dino's permissions; Settings → General → Permissions in the dino app asks for them."
        );
        return Ok(());
    }
    let rows: Vec<(&str, String)> = checked
        .iter()
        .map(|(_, name, ok)| {
            (
                *name,
                match ok {
                    Some(true) => "allowed".to_string(),
                    Some(false) => "not allowed".into(),
                    None => "unknown".into(),
                },
            )
        })
        .collect();
    print!("{}", crate::out::fields(&rows));
    Ok(())
}

/// macOS 27 calls Accessibility "Device Control and Data Access".
fn accessibility_name() -> &'static str {
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    let ok = unsafe { libc::sysctlbyname(c"kern.osproductversion".as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } == 0;
    let major = ok.then(|| String::from_utf8_lossy(&buf[..len.saturating_sub(1)]).split('.').next().and_then(|m| m.parse::<u32>().ok())).flatten();
    if major.is_some_and(|m| m >= 27) { "Device Control and Data Access" } else { "Accessibility" }
}

/// A framework's `bool f(void)`.
fn call(framework: &CStr, symbol: &CStr) -> Option<bool> {
    unsafe {
        let lib = libc::dlopen(framework.as_ptr(), libc::RTLD_LAZY);
        if lib.is_null() {
            return None;
        }
        let f = libc::dlsym(lib, symbol.as_ptr());
        if f.is_null() {
            return None;
        }
        let f: extern "C" fn() -> bool = std::mem::transmute(f);
        Some(f())
    }
}

/// macOS's privacy database opens only with Full Disk Access (and asks nothing). macOS 27 has no
/// copy in the user's Library; earlier ones have both.
fn full_disk() -> Option<bool> {
    let mut paths = vec![std::path::PathBuf::from("/Library/Application Support/com.apple.TCC/TCC.db")];
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(std::path::Path::new(&home).join("Library/Application Support/com.apple.TCC/TCC.db"));
    }
    paths.iter().find_map(|db| match std::fs::File::open(db) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(false),
        Err(_) => None,
    })
}
