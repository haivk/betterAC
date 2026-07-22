//! betterAC — a native GTK4 launcher for Asheron's Call on Linux.
//!
//! Replaces ThwargLauncher. Thwargle is a Windows WPF app, so it runs inside the
//! Wine prefix, which is where all the DPI pain came from: WPF reads Wine's font
//! metrics, and a prefix scaled for a HiDPI panel either renders the launcher at
//! quarter size or crashes it outright ("Fatal program error") when LogPixels and
//! WindowMetrics disagree. Nothing here runs under Wine except the game itself,
//! so the launcher just scales with GNOME like every other app.
//!
//! The launcher is now the entry point: on first run it detects that the game
//! isn't installed and offers to set it up, so there's no separate install script
//! to run. `--setup` does the same thing headless (no window), which is all the
//! old install-ac.sh now needs to call.

mod launcher;
mod window;

use adw::prelude::*;

const APP_ID: &str = "ac.betterac.BetterAC";

fn main() -> gtk::glib::ExitCode {
    // Headless setup for scripts/CI. Handled before GTK so it never opens a
    // display -- it just runs the same steps the setup screen does and logs them.
    if std::env::args().skip(1).any(|a| a == "--setup") {
        std::process::exit(run_setup_headless());
    }

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(window::build);
    app.run()
}

/// Run the whole setup sequence to stdout and return a process exit code.
fn run_setup_headless() -> i32 {
    use ac_core::setup::{Progress, SetupStep, StepState};

    let cfg = ac_core::config::Config::load();
    let rt = ac_core::proton::ProtonRuntime::new(cfg.prefix.clone());

    let steps = SetupStep::ALL.len();
    let mut last_step: Option<SetupStep> = None;
    let mut on = |p: Progress| {
        // One heading per step ("[3/10] Downloading the game client"), then the
        // step's own progress lines under it -- the terminal shape of the step
        // list the two GUIs draw.
        if last_step != Some(p.step) {
            let n = SetupStep::ALL.iter().position(|&s| s == p.step).unwrap_or(0) + 1;
            println!("\n==> [{n}/{steps}] {}", p.step.label());
            last_step = Some(p.step);
        }
        match p.state {
            StepState::Skipped => println!("    [skip] {}", p.message),
            StepState::Done => println!("    [ ok ] {}", p.message),
            _ => println!("    [{:>3.0}%] {}", p.fraction * 100.0, p.message),
        }
    };

    match ac_core::setup::run_all(&rt, &mut on) {
        Ok(()) => {
            println!("\nSetup complete. Prefix: {}", cfg.prefix.display());
            0
        }
        Err(e) => {
            eprintln!("\nsetup failed: {e}");
            1
        }
    }
}
