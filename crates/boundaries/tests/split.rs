//! Which dino crate may use which (ARCHITECTURE.md): the public crates others build on stay free of
//! daemon, proxy and terminal code, and the pieces stay separable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Each crate, the repository it goes to, and the other dino crates it may use.
const RULES: &[(&str, &str, &[&str])] = &[
    // Public crates others build on (dino-cloud): no daemon, proxy or terminal code.
    ("dino-core", "dino", &[]),
    ("dino-sync", "dino", &["dino-core"]),
    // The terminal emulation and the proxy stand on their own, so either can move out.
    ("dino-term", "dino", &[]),
    ("dino-router", "dino", &[]),
    ("dino-proxy", "dino", &["dino-router"]),
    // dinod and its CLI put the pieces together.
    ("dino-daemon", "dino", &["dino-core", "dino-sync", "dino-term", "dino-proxy", "dino-router"]),
    ("dino", "dino", &["dino-core", "dino-sync", "dino-term", "dino-daemon", "dino-proxy", "dino-router"]),
    ("boundaries", "dino", &[]),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn metadata() -> serde_json::Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo).args(["metadata", "--format-version", "1", "--no-deps"]).current_dir(root()).output().expect("cargo metadata");
    assert!(out.status.success(), "cargo metadata: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("cargo metadata json")
}

#[test]
fn crates_only_use_what_their_repository_allows() {
    let meta = metadata();
    let packages = meta["packages"].as_array().unwrap();
    let ours: BTreeSet<&str> = packages.iter().map(|p| p["name"].as_str().unwrap()).collect();
    let rules: BTreeMap<&str, &[&str]> = RULES.iter().map(|(name, _, allowed)| (*name, *allowed)).collect();
    let mut wrong = vec![];
    for p in packages {
        let name = p["name"].as_str().unwrap();
        let Some(allowed) = rules.get(name) else {
            wrong.push(format!("{name} is new: add it to RULES and to ARCHITECTURE.md"));
            continue;
        };
        for d in p["dependencies"].as_array().unwrap() {
            let dep = d["name"].as_str().unwrap();
            if ours.contains(dep) && dep != name && !allowed.contains(&dep) {
                wrong.push(format!("{name} uses {dep}, which ARCHITECTURE.md doesn't allow"));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// dino-terminal (the Swift app in app/) talks to dinod over its socket and nothing else: no
/// Rust sources or crates of this workspace in its package.
#[test]
fn the_terminal_app_stands_alone() {
    let package = std::fs::read_to_string(root().join("app/Package.swift")).expect("app/Package.swift");
    for bad in ["../crates", "path: \"..", ".package(path:"] {
        assert!(!package.contains(bad), "app/Package.swift reaches outside app/ ({bad}); it must only talk to dinod over IPC");
    }
    let mut stack = vec![root().join("app/Sources")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                panic!("{} is Rust inside the terminal app", p.display());
            }
        }
    }
}
