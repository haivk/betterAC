//! Drive the real update path: check, then install.
//!   BETTERAC_BASE_URL=http://... cargo run -p ac-core --example update_smoke
fn main() {
    println!("running version = {}", ac_core::VERSION);
    println!("install source  = {:?}", ac_core::update::source());
    println!("this exe        = {:?}", std::env::current_exe().unwrap());

    match ac_core::update::check() {
        Err(e) => {
            eprintln!("check FAILED: {e}");
            std::process::exit(1);
        }
        Ok(None) => println!("\nalready up to date"),
        Ok(Some(r)) => {
            println!("\nupdate available: {} ({})", r.version, r.asset);
            if std::env::args().any(|a| a == "--install") {
                let mut last = String::new();
                match ac_core::update::install(&r, &mut |p| {
                    if p.message != last {
                        last = p.message.clone();
                        println!("  {}", p.message);
                    }
                }) {
                    Ok(applied) => {
                        println!("\ninstalled: {applied:?}");
                        // --hold proves the safety property: this process is the
                        // binary that was just replaced, and must survive it.
                        if std::env::args().any(|a| a == "--hold") {
                            println!("holding for 20s to prove the running process survives");
                            std::thread::sleep(std::time::Duration::from_secs(20));
                            println!("still alive after being replaced");
                        }
                    }
                    Err(e) => {
                        eprintln!("\ninstall FAILED: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
