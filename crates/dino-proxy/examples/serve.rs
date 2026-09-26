//! Run the proxy alone and print usage as it happens. `cargo run -p dino-proxy --example serve`
fn main() -> anyhow::Result<()> {
    let proxy = dino_proxy::Proxy::start()?;
    println!("listening on http://127.0.0.1:{}  (base url: {})", proxy.port, proxy.base_url("test", "<provider>"));
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let s = proxy.stats.session("test");
        if s.requests > 0 {
            println!("requests={} in_flight={} errors={} usage={:?} model={:?}", s.requests, s.in_flight, s.errors, s.usage, s.last_model);
        }
    }
}
