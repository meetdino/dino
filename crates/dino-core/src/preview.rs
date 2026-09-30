//! Dev servers a session can preview: `.dino/launch.json`, then Claude's `.claude/launch.json`
//! (the same shape), in the session's folder.
//!
//! ```json
//! { "version": "0.0.1",
//!   "configurations": [{ "name": "web", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"], "port": 5173 }] }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where launch configurations are looked for, dino's own first. A name in both: dino's wins.
pub const FILES: &[&str] = &[".dino/launch.json", ".claude/launch.json"];

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct LaunchFile {
    #[serde(default)]
    configurations: Vec<RawConfig>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawConfig {
    name: String,
    runtime_executable: Option<String>,
    runtime_args: Vec<String>,
    program: Option<String>,
    args: Vec<String>,
    port: Option<u16>,
    url: Option<String>,
    cwd: Option<String>,
    env: HashMap<String, String>,
}

/// A dev server, ready to start.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PreviewConfig {
    pub name: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    /// The port the config names; otherwise found in the server's output.
    pub port: Option<u16>,
    /// A page to open instead of the port's root.
    pub url: Option<String>,
    /// The file it came from, relative to the session's folder.
    pub source: String,
}

/// A session's dev server, as the app sees it.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PreviewInfo {
    pub name: String,
    pub running: bool,
    /// The page to load, once the port is known.
    pub url: Option<String>,
    /// How it ended, when it isn't running and ran at some point.
    pub exit: Option<String>,
}

/// Every configuration in `root`'s launch files. Unreadable files are skipped; a broken one is
/// an error, so the user learns why their server isn't listed.
pub fn configs(root: &Path) -> anyhow::Result<Vec<PreviewConfig>> {
    let mut out: Vec<PreviewConfig> = vec![];
    for file in FILES {
        let Ok(text) = std::fs::read_to_string(root.join(file)) else { continue };
        let parsed: LaunchFile = serde_json::from_str(&strip_jsonc(&text)).map_err(|e| anyhow::anyhow!("{file}: {e}"))?;
        for raw in parsed.configurations {
            if raw.name.is_empty() || out.iter().any(|c| c.name == raw.name) {
                continue;
            }
            if let Some(c) = resolve(raw, root, file) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

fn resolve(raw: RawConfig, root: &Path, source: &str) -> Option<PreviewConfig> {
    let expand = |s: &str| s.replace("${workspaceFolder}", &root.to_string_lossy());
    let mut argv: Vec<String> = match (&raw.runtime_executable, &raw.program) {
        (Some(exe), _) => std::iter::once(exe.as_str()).chain(raw.runtime_args.iter().map(String::as_str)).map(expand).collect(),
        (None, Some(program)) => ["node", program.as_str()].into_iter().map(expand).collect(),
        (None, None) => return None,
    };
    argv.extend(raw.args.iter().map(|a| expand(a)));
    let cwd = match raw.cwd {
        Some(c) => {
            let c = PathBuf::from(expand(&c));
            if c.is_absolute() { c } else { root.join(c) }
        }
        None => root.to_path_buf(),
    };
    let env = raw.env.iter().map(|(k, v)| (k.clone(), expand(v))).collect();
    Some(PreviewConfig { name: raw.name, argv, cwd, env, port: raw.port, url: raw.url, source: source.into() })
}

/// JSON with comments and trailing commas (what VS Code allows in launch.json) to plain JSON.
pub fn strip_jsonc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                out.extend_from_slice(&b[start..i]);
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                continue;
            }
            b',' => {
                // A comma before `]` or `}` (whitespace and comments between) is dropped.
                let mut j = i + 1;
                loop {
                    match (b.get(j), b.get(j + 1)) {
                        (Some(c), _) if c.is_ascii_whitespace() => j += 1,
                        (Some(b'/'), Some(b'/')) => {
                            while j < b.len() && b[j] != b'\n' {
                                j += 1;
                            }
                        }
                        (Some(b'/'), Some(b'*')) => {
                            j += 2;
                            while j < b.len() && !(b[j] == b'*' && b.get(j + 1) == Some(&b'/')) {
                                j += 1;
                            }
                            j += 2;
                        }
                        _ => break,
                    }
                }
                if matches!(b.get(j), Some(b']' | b'}')) {
                    i += 1;
                    continue;
                }
            }
            _ => {}
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// The first local address a server announces: `localhost:5173`, `127.0.0.1:8000`,
/// `0.0.0.0:3000`, `[::]:8080`. Returns the port.
pub fn find_port(text: &str) -> Option<u16> {
    const HOSTS: &[&str] = &["localhost:", "127.0.0.1:", "0.0.0.0:", "[::1]:", "[::]:"];
    let mut best: Option<(usize, u16)> = None;
    for host in HOSTS {
        let mut from = 0;
        while let Some(at) = text[from..].find(host) {
            let start = from + at + host.len();
            let digits: String = text[start..].chars().take_while(char::is_ascii_digit).collect();
            if let Ok(port) = digits.parse::<u16>()
                && port > 0
                && best.is_none_or(|(i, _)| from + at < i)
            {
                best = Some((from + at, port));
                break;
            }
            from = start;
        }
    }
    // `python3 -m http.server` says "Serving HTTP on :: port 8000 (http://[::]:8000/)".
    best.map(|(_, p)| p).or_else(|| {
        let at = text.find(" port ")?;
        text[at + 6..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok().filter(|p| *p > 0)
    })
}

/// The last local web address in `text` (`http://localhost:5173/app`), to offer a preview of.
pub fn find_local_url(text: &str) -> Option<String> {
    const STARTS: &[&str] = &["http://localhost:", "https://localhost:", "http://127.0.0.1:", "https://127.0.0.1:", "http://0.0.0.0:"];
    let mut best: Option<(usize, String)> = None;
    for start in STARTS {
        for (at, _) in text.match_indices(start) {
            let rest = &text[at..];
            let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '│' | '\u{1b}')).unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':']);
            // Up to the path, only a port: `http://localhost:3000@evil.example/` is evil.example's.
            let port = url.get(start.len()..).unwrap_or_default().split(['/', '?', '#']).next().unwrap_or_default();
            let local = !port.is_empty() && port.bytes().all(|c| c.is_ascii_digit());
            if local && best.as_ref().is_none_or(|(i, _)| at > *i) {
                best = Some((at, url.replace("0.0.0.0", "localhost")));
            }
        }
    }
    best.map(|(_, u)| u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_local_urls() {
        assert_eq!(find_local_url("Server running at http://localhost:3000/app.").as_deref(), Some("http://localhost:3000/app"));
        assert_eq!(find_local_url("(see http://127.0.0.1:8000) or http://localhost:5173/"), Some("http://localhost:5173/".into()));
        assert_eq!(find_local_url("│ http://0.0.0.0:8080 │"), Some("http://localhost:8080".into()));
        assert_eq!(find_local_url("http://localhost:abc http://example.com:80"), None);
        assert_eq!(find_local_url("see http://localhost: and http://localhost:"), None);
        assert_eq!(find_local_url("http://localhost:5173?x=1#top"), Some("http://localhost:5173?x=1#top".into()));
    }

    #[test]
    fn look_alike_local_urls_are_not_local() {
        // The host is what's after the `@`, or the port isn't one.
        assert_eq!(find_local_url("http://localhost:3000@evil.example/"), None);
        assert_eq!(find_local_url("https://127.0.0.1:443@evil.example"), None);
        assert_eq!(find_local_url("http://0.0.0.0:80.evil.example/x"), None);
        assert_eq!(find_local_url("http://localhost:3000:4000/"), None);
        // A real one earlier still counts.
        assert_eq!(find_local_url("http://localhost:5173/ then http://localhost:3000@evil.example/"), Some("http://localhost:5173/".into()));
    }

    #[test]
    fn strips_comments_and_trailing_commas() {
        let s = r#"{
            // a comment
            "a": "http://x // not a comment", /* block */
            "b": [1, 2, /* x */ ],
        }"#;
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(s)).unwrap();
        assert_eq!(v["a"], "http://x // not a comment");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }

    #[test]
    fn keeps_escaped_quotes() {
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(r#"{"a": "say \"hi\" // there"}"#)).unwrap();
        assert_eq!(v["a"], "say \"hi\" // there");
    }

    #[test]
    fn reads_both_files_dino_first() {
        let dir = std::env::temp_dir().join(format!("dino-preview-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".dino")).unwrap();
        std::fs::write(
            dir.join(".claude/launch.json"),
            r#"{ "version": "0.0.1", "configurations": [
                { "name": "web", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"], "port": 5173 },
                { "name": "api", "program": "${workspaceFolder}/server.js", "args": ["--x"], "cwd": "api", "env": { "A": "1" } },
                { "name": "nothing" },
            ]}"#,
        )
        .unwrap();
        std::fs::write(dir.join(".dino/launch.json"), r#"{ "configurations": [{ "name": "web", "runtimeExecutable": "python3", "runtimeArgs": ["-m", "http.server"] }] }"#).unwrap();
        let c = configs(&dir).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "web");
        assert_eq!(c[0].argv, ["python3", "-m", "http.server"]);
        assert_eq!(c[0].source, ".dino/launch.json");
        assert_eq!(c[0].port, None);
        assert_eq!(c[1].argv, vec!["node".to_string(), format!("{}/server.js", dir.display()), "--x".into()]);
        assert_eq!(c[1].cwd, dir.join("api"));
        assert_eq!(c[1].env["A"], "1");
        std::fs::write(dir.join(".dino/launch.json"), "{ nope").unwrap();
        assert!(configs(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_files_no_configs() {
        assert!(configs(Path::new("/nonexistent-dino")).unwrap().is_empty());
    }

    #[test]
    fn finds_ports() {
        assert_eq!(find_port("  ➜  Local:   http://localhost:5173/"), Some(5173));
        assert_eq!(find_port("ready - started server on 0.0.0.0:3000, url: http://localhost:3000"), Some(3000));
        assert_eq!(find_port("Serving HTTP on :: port 8000 (http://[::]:8000/) ..."), Some(8000));
        assert_eq!(find_port("Serving HTTP on 0.0.0.0 port 8123 ..."), Some(8123));
        assert_eq!(find_port(" * Running on http://127.0.0.1:5000"), Some(5000));
        assert_eq!(find_port("compiling..."), None);
        assert_eq!(find_port("localhost: nothing"), None);
    }
}
