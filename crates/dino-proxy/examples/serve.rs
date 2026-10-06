//! Run the proxy alone and print usage as it happens. `cargo run -p dino-proxy --example serve`
//!
//! Its base URL carries the proxy's secret, so it goes in a file only you can read, not on the
//! terminal or in its scrollback.
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

fn main() -> anyhow::Result<()> {
    let proxy = dino_proxy::Proxy::start(Default::default())?;
    let file = std::env::temp_dir().join(format!("dino-proxy-{}.url", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file)?;
    writeln!(f, "{}", proxy.base_url("test", "<provider>"))?;
    println!("listening on http://127.0.0.1:{}  (base url, with its secret: {})", proxy.port, file.display());
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let s = proxy.stats.session("test");
        if s.requests > 0 {
            println!("requests={} in_flight={} errors={} usage={:?} model={:?}", s.requests, s.in_flight, s.errors, s.usage, s.last_model);
        }
    }
}
