//! Run the real client-patching setup step against the installed prefix and print
//! what each patch did, then read the patched bytes back.
//!   cargo run -p ac-core --example patch_smoke
//! Safe to re-run: `patches::apply_all` is idempotent, so a second pass must report
//! every patch as `already applied`.
use ac_core::setup::{Runtime, SetupStep};
use ac_core::wine::WineRuntime;

fn main() {
    let rt = WineRuntime::new(ac_core::install::default_prefix());
    rt.run_step(SetupStep::PatchClient, &mut |p| println!("  {}", p.message))
        .expect("patch step");

    let install = rt.discover().expect("discover install");
    let client = install.ac_dir.join("acclient.exe");
    let buf = std::fs::read(&client).expect("read client");
    for p in ac_core::patches::applicable() {
        let here = &buf[p.offset..p.offset + p.patched.len()];
        println!("{:>20}: {} {}", p.name, hex(here), if here == p.patched { "OK" } else { "MISMATCH" });
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
