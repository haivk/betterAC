//! Launch the game through the real launcher path (`runtime::launch`) using the
//! saved config — the server last played, with its account and password — so a
//! human can log in and play while the launch is traced. Set BETTERAC_WINEDEBUG
//! to control tracing.
//!
//!   BETTERAC_WINEDEBUG=+seh,err+all cargo run -p ac-core --example play_logged
use ac_core::config::Config;
use ac_core::runtime::launch;
use ac_core::setup::Runtime;

#[allow(clippy::zombie_processes)]
fn main() {
    let cfg = Config::load();
    println!("decal.enabled = {}", cfg.decal.enabled);

    // Prefer the last-played server; otherwise the first saved one.
    let entry = cfg
        .last
        .as_ref()
        .and_then(|id| cfg.find(id))
        .or_else(|| cfg.servers.first())
        .expect("no saved servers in config — add one in the app first");
    println!("launching: {} ({}:{}) as {}", entry.name, entry.host, entry.port, entry.account);

    let rt = ac_core::runtime::for_prefix(cfg.prefix.clone());
    let install = rt.discover().expect("discover install");
    let child = launch(&install, &entry.to_server(), &entry.account, &entry.password, None)
        .expect("launch");
    println!("launched, pid {}", child.id());
    // Stay alive so the game keeps running; Ctrl-C to stop.
    std::thread::sleep(std::time::Duration::from_secs(600));
}
