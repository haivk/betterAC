//! Drive the real Decal setup step against the installed prefix, then read back
//! what it produced.
//!
//!   AC_DECAL_MSI=/path/to/Decal.msi cargo run -p ac-core --example decal_smoke
//!
//! Needs `decal.enabled` true in the config, which is the same switch the settings
//! UI flips. Safe to re-run: the step no-ops once Decal is present.
use ac_core::setup::{Runtime, SetupStep};

fn main() {
    let prefix = ac_core::install::default_prefix();
    let cfg = ac_core::config::Config::load();
    println!("decal.enabled = {}", cfg.decal.enabled);
    println!("already installed = {}", ac_core::decal::is_installed(&prefix));

    let rt = ac_core::runtime::for_prefix(prefix.clone());
    if let Err(e) = rt.run_step(SetupStep::InstallDecal, &mut |p| println!("  {}", p.message)) {
        eprintln!("FAILED: {e}");
        std::process::exit(1);
    }

    println!("\ninstalled  = {}", ac_core::decal::is_installed(&prefix));
    println!("injector   = {}", ac_core::decal::injector_path(&prefix).is_file());
    let install = rt.discover().expect("discover");
    println!("plugins:");
    for p in ac_core::decal::plugins(&prefix, &|args| ac_core::runtime::query_in_prefix(&install, args))
    {
        println!("  [{}] {}  {}", if p.enabled { "on " } else { "off" }, p.clsid, p.name);
    }
}
