//! Throwaway integration smoke test for the macOS fullscreen-Space launch path.
//! Drives the real `wine::launch` against a dead server, exactly as the FFI does,
//! then sleeps so the spawned client + helper stay alive for inspection.
//!   cargo run -p ac-core --example spaces_smoke
use ac_core::servers::{Server, Software};
use ac_core::setup::Runtime;
use ac_core::wine::{launch, WineRuntime};

// The client is meant to outlive this probe -- we spawn it, look at it, and leave it
// running for the operator, so there is deliberately nobody to reap it.
#[allow(clippy::zombie_processes)]
fn main() {
    let rt = WineRuntime::new(ac_core::install::default_prefix());
    let install = rt.discover().expect("discover install");
    let server = Server {
        name: "dead".into(),
        description: String::new(),
        ruleset: "PvE".into(),
        software: Software::Ace,
        host: "127.0.0.1".into(),
        port: "9000".into(),
        players: None,
        website_url: None,
        discord_url: None,
    };
    let child = launch(&install, &server, "test", "test", None).expect("launch");
    println!("launched acclient via wine::launch, pid {}", child.id());
    std::thread::sleep(std::time::Duration::from_secs(45));
}
