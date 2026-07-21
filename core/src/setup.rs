//! First-run setup: the vocabulary both platforms share.
//!
//! The launcher is now the entry point. On start it asks the runtime whether the
//! game is set up; if not, it walks these steps, each reporting `Progress` so a
//! GTK step list or a SwiftUI one can render the same flow. The steps mean
//! slightly different things on each platform -- `Runtime` downloads GE-Proton on
//! Linux and a CrossOver-lineage Wine engine on macOS -- but the sequence and the
//! user-facing labels are identical, so the frontends stay platform-agnostic.
//!
//! ## Why the steps are this fine-grained
//!
//! Setup pulls down ~1.4 GB in three separate files and then does five distinct
//! local jobs. Reported as one bar that is what it looked like: a bar that filled
//! and emptied over and over with no way to tell which pass you were watching. So
//! **every download is its own step**, and the local work is split into the jobs a
//! person would name ("creating the prefix", "installing Asheron's Call"). A
//! frontend renders the whole list up front with a bar per row, and you can always
//! see where you are and what is left.
//!
//! The three downloads run first, back to back, so the long unattended part is
//! over before the one step that needs you (the retail installer wizard).
//!
//! This module is the shared part: the ordered step list, their labels, the
//! per-run state a frontend renders ([`RunState`]), and the stamp files that make
//! setup resumable. The work each step actually does lives in the platform
//! runtimes (`proton`, `wine`), because that is where the external tools and
//! downloads differ. Re-running is safe: a completed step drops a stamp under
//! `<prefix>/.ac-installer/` and is skipped next time, so a failure halfway
//! through never redoes the 1.3 GB client install.

use crate::install::Install;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

// ------------------------------------------------------------------ cancelling

/// Setup runs at most once at a time in a process, so one flag is enough -- and a
/// flag is what the two places that must see it can both reach: `run_all`, which
/// stops between steps, and `fetch::download`, which stops mid-file. Threading a
/// token through every step signature would buy nothing over this.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// The error text a cancelled run stops with. Recognised by [`RunState::finish`],
/// which reports it as a cancellation rather than a failure -- stopping on purpose
/// is not the same as breaking, and the UI should not cry wolf about it.
pub const CANCELLED: &str = "Setup cancelled";

/// Ask the running setup to stop. Takes effect at the next cancellation point:
/// immediately during a download, otherwise when the current step's external
/// command (wineboot, the installer wizard) returns. Steps are idempotent and
/// stamped, so whatever finished stays finished and a later run resumes.
pub fn request_cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Clear the flag before a new run.
pub fn clear_cancel() {
    CANCEL.store(false, Ordering::Relaxed);
}

pub fn cancel_requested() -> bool {
    CANCEL.load(Ordering::Relaxed)
}

/// `Err(CANCELLED)` if a stop was asked for -- the `?`-able form used at every
/// cancellation point.
pub(crate) fn check_cancelled() -> Result<(), String> {
    if cancel_requested() {
        Err(CANCELLED.to_string())
    } else {
        Ok(())
    }
}

/// One step of setup, in the order they run. Same sequence on both platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    /// Host tools that must already be present (umu-run/gamescope on Bazzite;
    /// Rosetta 2 on macOS, which we can install). Re-checked every run.
    Dependencies,
    /// Download the Windows runtime: GE-Proton (Linux) or the Wine engine (macOS).
    DownloadRuntime,
    /// Download the retail client installer, ac1install.exe.
    DownloadClient,
    /// Download the End-of-Retail bundle, ac-updates.zip.
    DownloadUpdates,
    /// Unpack (and on macOS verify) the downloaded runtime into place.
    InstallRuntime,
    /// Create the Wine/Proton prefix.
    Prefix,
    /// Extra runtime components: winetricks vcrun2019 + VC++ 2005 on Linux;
    /// nothing on macOS, where they arrive inside ac-updates.zip.
    Components,
    /// Run the real ac1install.exe wizard. Deliberately not silent.
    InstallClient,
    /// Apply the End-of-Retail dats + patched acclient over the retail install.
    ApplyUpdates,
    /// Write the direct-launch escape hatch and mark the install complete.
    Finalize,
}

impl SetupStep {
    /// Every step, in run order. Downloads first: they are the long unattended
    /// stretch, and getting them done before the installer wizard means the user
    /// is only interrupted once, at a predictable point.
    pub const ALL: [SetupStep; 10] = [
        SetupStep::Dependencies,
        SetupStep::DownloadRuntime,
        SetupStep::DownloadClient,
        SetupStep::DownloadUpdates,
        SetupStep::InstallRuntime,
        SetupStep::Prefix,
        SetupStep::Components,
        SetupStep::InstallClient,
        SetupStep::ApplyUpdates,
        SetupStep::Finalize,
    ];

    /// One-line, user-facing. The title of this step's row in the UI.
    pub fn label(&self) -> &'static str {
        match self {
            SetupStep::Dependencies => "Checking dependencies",
            SetupStep::DownloadRuntime => "Downloading the Windows runtime",
            SetupStep::DownloadClient => "Downloading the game client",
            SetupStep::DownloadUpdates => "Downloading the End-of-Retail update",
            SetupStep::InstallRuntime => "Installing the Windows runtime",
            SetupStep::Prefix => "Creating the Windows prefix",
            SetupStep::Components => "Installing runtime components",
            SetupStep::InstallClient => "Installing Asheron's Call",
            SetupStep::ApplyUpdates => "Applying the End-of-Retail update",
            SetupStep::Finalize => "Finishing up",
        }
    }

    /// The subtitle under the label: what this step is, in plain words, shown
    /// before it runs so the whole list reads as a plan. Once a step is running
    /// its live message replaces this.
    pub fn detail(&self) -> &'static str {
        match self {
            SetupStep::Dependencies => "System requirements and host tools",
            SetupStep::DownloadRuntime => "The compatibility layer that runs Windows games",
            SetupStep::DownloadClient => "The original retail installer, about 570 MB",
            SetupStep::DownloadUpdates => "Data files and the patched client, about 480 MB",
            SetupStep::InstallRuntime => "Unpacking the runtime into place",
            SetupStep::Prefix => "A private Windows environment for the game",
            SetupStep::Components => "Extra libraries the client needs",
            SetupStep::InstallClient => "The original installer opens — you click through it",
            SetupStep::ApplyUpdates => "Copying the update files over the install",
            SetupStep::Finalize => "Last checks and a direct-launch shortcut",
        }
    }

    /// The stamp filename under `<prefix>/.ac-installer/` recording that this step
    /// completed. `None` for steps that self-check every run rather than stamp:
    /// dependencies are re-verified, and the downloads are idempotent by the
    /// presence of the file they fetch.
    ///
    /// The names are the ones install-ac.sh used (and the ones the pre-split step
    /// list wrote), so an install set up by either is recognised as already done.
    pub fn stamp(&self) -> Option<&'static str> {
        match self {
            SetupStep::Dependencies
            | SetupStep::DownloadRuntime
            | SetupStep::DownloadClient
            | SetupStep::DownloadUpdates => None,
            SetupStep::InstallRuntime => Some("runtime"),
            SetupStep::Prefix => Some("prefix"),
            SetupStep::Components => Some("components"),
            SetupStep::InstallClient => Some("client"),
            SetupStep::ApplyUpdates => Some("updates"),
            SetupStep::Finalize => Some("finalize"),
        }
    }
}

/// Where a step is in its life. A frontend draws a row per step and picks its
/// icon and bar from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Not reached yet.
    Pending,
    /// Running now — `fraction` is meaningful if the step can measure itself.
    Running,
    /// Finished, this run.
    Done,
    /// Nothing to do: already installed, already downloaded, or not needed on
    /// this platform. Distinct from `Done` because "we did nothing, on purpose"
    /// is exactly the thing a progress bar cannot say.
    Skipped,
    /// This is the step that broke; `message` says how.
    Failed,
}

impl StepState {
    /// Nothing left to do for this step, either way.
    pub fn is_finished(&self) -> bool {
        matches!(self, StepState::Done | StepState::Skipped)
    }
}

/// A single progress report from a running step. `fraction` is 0.0..=1.0 within
/// the step (best effort -- a download knows it, a wizard does not), and
/// `message` is a short human line ("312 MB of 571 MB").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub step: SetupStep,
    pub state: StepState,
    pub fraction: f32,
    pub message: String,
}

impl Progress {
    /// A running step reporting how far it has got.
    pub fn new(step: SetupStep, fraction: f32, message: impl Into<String>) -> Progress {
        Progress {
            step,
            state: StepState::Running,
            fraction: fraction.clamp(0.0, 1.0),
            message: message.into(),
        }
    }

    /// A step just starting, with its own detail line as the message.
    pub fn starting(step: SetupStep) -> Progress {
        Progress {
            step,
            state: StepState::Running,
            fraction: 0.0,
            message: step.detail().to_string(),
        }
    }

    /// A step that did its work and finished.
    pub fn finished(step: SetupStep) -> Progress {
        Progress { step, state: StepState::Done, fraction: 1.0, message: "Done".into() }
    }

    /// A step that had nothing to do, and why. Runtimes emit this instead of
    /// returning silently, so the list can say "already installed" rather than
    /// flashing a bar that was never really filling.
    pub fn skipped(step: SetupStep, message: impl Into<String>) -> Progress {
        Progress { step, state: StepState::Skipped, fraction: 1.0, message: message.into() }
    }
}

/// The whole run, as a frontend renders it: every step with its own state and
/// fraction, plus the terminal outcome. Built once with all steps `Pending`, so
/// the list can be shown as a plan *before* setup starts, then folded forward
/// with [`RunState::apply`] as progress arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    /// Has setup been kicked off? False means the list is the plan, not history.
    pub started: bool,
    /// Has the run finished, one way or another?
    pub done: bool,
    /// Did it stop because the user asked it to? Then `error` stays empty: this is
    /// a pause, not a fault, and the UI offers to resume rather than apologising.
    pub cancelled: bool,
    /// Why it stopped, if it stopped badly.
    pub error: Option<String>,
    pub steps: Vec<StepStatus>,
}

/// One row of [`RunState`]. Carries its own label and detail so a frontend never
/// needs a parallel copy of the step vocabulary -- it renders whatever it is sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepStatus {
    pub step: SetupStep,
    pub label: String,
    pub detail: String,
    pub state: StepState,
    pub fraction: f32,
    pub message: String,
}

impl Default for RunState {
    fn default() -> RunState {
        RunState::new()
    }
}

impl RunState {
    /// Every step, pending, nothing run yet.
    pub fn new() -> RunState {
        RunState {
            started: false,
            done: false,
            cancelled: false,
            error: None,
            steps: SetupStep::ALL
                .iter()
                .map(|&step| StepStatus {
                    step,
                    label: step.label().to_string(),
                    detail: step.detail().to_string(),
                    state: StepState::Pending,
                    fraction: 0.0,
                    message: step.detail().to_string(),
                })
                .collect(),
        }
    }

    /// Fold one progress report in. A step that has already reported `Skipped`
    /// keeps that state when the run loop later marks it finished -- "nothing to
    /// do" is the more informative of the two.
    pub fn apply(&mut self, p: &Progress) {
        let Some(s) = self.steps.iter_mut().find(|s| s.step == p.step) else { return };
        if s.state == StepState::Skipped && p.state == StepState::Done {
            return;
        }
        s.state = p.state;
        s.fraction = p.fraction;
        s.message = p.message.clone();
    }

    /// Record the run's outcome. On failure the step that was running is the one
    /// that broke, so it takes the error as its message; the rest stay pending,
    /// which is the truth -- they never ran.
    pub fn finish(&mut self, result: &Result<(), String>) {
        self.done = true;
        match result {
            Ok(()) => {
                for s in &mut self.steps {
                    if s.state == StepState::Running || s.state == StepState::Pending {
                        s.state = StepState::Done;
                        s.fraction = 1.0;
                    }
                }
            }
            // Stopped on request: the step we were in never finished, so it goes
            // back to pending rather than being marked failed. Nothing is left
            // half-applied that a resume won't redo -- steps only stamp when they
            // complete.
            Err(e) if e == CANCELLED => {
                self.cancelled = true;
                if let Some(s) = self.steps.iter_mut().find(|s| s.state == StepState::Running) {
                    s.state = StepState::Pending;
                    s.fraction = 0.0;
                    s.message = s.detail.clone();
                }
            }
            Err(e) => {
                self.error = Some(e.clone());
                if let Some(s) = self.steps.iter_mut().find(|s| s.state == StepState::Running) {
                    s.state = StepState::Failed;
                    s.message = e.clone();
                }
            }
        }
    }

    /// The step being worked on right now, if any.
    pub fn current(&self) -> Option<&StepStatus> {
        self.steps.iter().find(|s| s.state == StepState::Running)
    }

    /// How many steps are behind us, for a "step 4 of 10" line.
    pub fn completed(&self) -> usize {
        self.steps.iter().filter(|s| s.state.is_finished()).count()
    }
}

/// A platform's setup + launch backend. Proton on Linux, Wine on macOS. The
/// frontends drive this without knowing which one they hold.
///
/// `run_step` must be idempotent: it is handed every step in order and decides
/// for itself whether the step is already satisfied (by its stamp, or by the
/// presence of what it would produce). When it is, report `Progress::skipped`
/// and return `Ok(())` -- that is what makes setup resumable after a failure
/// without redoing the 1.3 GB client install, and what lets the UI say so.
pub trait Runtime {
    /// Do (or skip, if already done) one setup step, reporting progress.
    fn run_step(&self, step: SetupStep, on: &mut dyn FnMut(Progress)) -> Result<(), String>;

    /// Locate the finished install, or say what is missing. Called on launcher
    /// start to decide whether setup is needed at all.
    fn discover(&self) -> Result<Install, String>;
}

/// Run every setup step in order, streaming progress. Stops at the first failure
/// so the user sees exactly which step broke; re-running resumes from there.
///
/// Each step is bracketed by a `starting` and a `finished` report, so a frontend
/// that only forwards these to a [`RunState`] gets a correct list without knowing
/// anything about the steps themselves. A step that reported `Skipped` is not
/// then marked finished -- see [`RunState::apply`].
pub fn run_all(rt: &dyn Runtime, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
    for step in SetupStep::ALL {
        check_cancelled()?;
        on(Progress::starting(step));
        let mut skipped = false;
        {
            // Watch the stream for the runtime saying "nothing to do" so we don't
            // overwrite that with a generic completion.
            let mut watch = |p: Progress| {
                skipped = p.state == StepState::Skipped;
                on(p);
            };
            rt.run_step(step, &mut watch)?;
        }
        if !skipped {
            on(Progress::finished(step));
        }
    }
    Ok(())
}

/// The `.ac-installer/` directory of stamps inside a prefix. Mirrors the scheme
/// install-ac.sh used, so an install set up by the old script is recognised as
/// already done and not redone.
pub fn stamps_dir(prefix: &Path) -> PathBuf {
    prefix.join(".ac-installer")
}

/// Has this step been completed (or does it not use a stamp)? A stampless step is
/// reported not-done here; the runtime decides idempotency for those itself.
pub fn is_stamped(prefix: &Path, step: SetupStep) -> bool {
    match step.stamp() {
        Some(name) => stamps_dir(prefix).join(name).exists(),
        None => false,
    }
}

/// Record a step as complete.
pub fn mark_stamped(prefix: &Path, step: SetupStep) -> std::io::Result<()> {
    if let Some(name) = step.stamp() {
        let dir = stamps_dir(prefix);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(name), b"")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The cancel flag is process-global (see [`CANCEL`]), and the test harness
    /// runs tests in parallel threads — so every test that calls `run_all` or
    /// touches the flag takes this first, or one test's cancel stops another's run.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        // A panic in one of these tests must not make the rest fail as poisoned.
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn every_step_has_a_label_and_the_order_is_fixed() {
        assert_eq!(SetupStep::ALL.len(), 10);
        assert_eq!(SetupStep::ALL[0], SetupStep::Dependencies);
        assert_eq!(SetupStep::ALL[9], SetupStep::Finalize);
        for s in SetupStep::ALL {
            assert!(!s.label().is_empty());
            assert!(!s.detail().is_empty());
        }
    }

    #[test]
    fn the_three_downloads_run_before_any_local_work() {
        // The point of the split: one uninterrupted download stretch, then the
        // installer wizard. If a local step sneaks in between, the user gets
        // interrupted twice.
        let pos = |want: SetupStep| SetupStep::ALL.iter().position(|&s| s == want).unwrap();
        assert!(pos(SetupStep::DownloadRuntime) < pos(SetupStep::InstallRuntime));
        assert!(pos(SetupStep::DownloadClient) < pos(SetupStep::InstallRuntime));
        assert!(pos(SetupStep::DownloadUpdates) < pos(SetupStep::InstallRuntime));
        assert!(pos(SetupStep::InstallClient) < pos(SetupStep::ApplyUpdates));
    }

    #[test]
    fn stamp_names_are_unique_where_present() {
        let names: Vec<&str> = SetupStep::ALL.iter().filter_map(|s| s.stamp()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "two steps share a stamp filename");
    }

    #[test]
    fn stamp_names_match_the_old_installer_so_existing_installs_are_not_redone() {
        assert_eq!(SetupStep::InstallRuntime.stamp(), Some("runtime"));
        assert_eq!(SetupStep::InstallClient.stamp(), Some("client"));
        assert_eq!(SetupStep::ApplyUpdates.stamp(), Some("updates"));
    }

    #[test]
    fn dependencies_and_downloads_do_not_stamp() {
        // They self-check every run, so a stamp would wrongly skip them.
        assert!(SetupStep::Dependencies.stamp().is_none());
        assert!(SetupStep::DownloadClient.stamp().is_none());
        assert!(SetupStep::DownloadUpdates.stamp().is_none());
        assert!(SetupStep::DownloadRuntime.stamp().is_none());
    }

    #[test]
    fn a_stamped_step_reads_back_as_done() {
        let tmp = std::env::temp_dir().join(format!("ac-setup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(!is_stamped(&tmp, SetupStep::InstallRuntime));
        mark_stamped(&tmp, SetupStep::InstallRuntime).unwrap();
        assert!(is_stamped(&tmp, SetupStep::InstallRuntime));
        // a stampless step never reads as done, even after mark
        mark_stamped(&tmp, SetupStep::DownloadClient).unwrap();
        assert!(!is_stamped(&tmp, SetupStep::DownloadClient));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn progress_fraction_is_clamped() {
        assert_eq!(Progress::new(SetupStep::Prefix, 2.0, "x").fraction, 1.0);
        assert_eq!(Progress::new(SetupStep::Prefix, -1.0, "x").fraction, 0.0);
        assert_eq!(Progress::starting(SetupStep::Prefix).fraction, 0.0);
    }

    #[test]
    fn setup_step_round_trips_through_json_snake_case() {
        // The FFI/progress boundary is JSON; the wire form must be stable.
        let j = serde_json::to_string(&SetupStep::DownloadClient).unwrap();
        assert_eq!(j, "\"download_client\"");
        let back: SetupStep = serde_json::from_str(&j).unwrap();
        assert_eq!(back, SetupStep::DownloadClient);
    }

    #[test]
    fn a_fresh_run_state_lists_every_step_as_pending() {
        let st = RunState::new();
        assert_eq!(st.steps.len(), SetupStep::ALL.len());
        assert!(st.steps.iter().all(|s| s.state == StepState::Pending));
        assert!(st.current().is_none());
        assert_eq!(st.completed(), 0);
    }

    #[test]
    fn applying_progress_moves_only_the_step_it_names() {
        let mut st = RunState::new();
        st.apply(&Progress::new(SetupStep::DownloadClient, 0.5, "285 MB of 571 MB"));
        let row = st.steps.iter().find(|s| s.step == SetupStep::DownloadClient).unwrap();
        assert_eq!(row.state, StepState::Running);
        assert_eq!(row.fraction, 0.5);
        assert_eq!(st.current().map(|s| s.step), Some(SetupStep::DownloadClient));
        // Everything else is untouched.
        assert_eq!(st.steps[0].state, StepState::Pending);
    }

    #[test]
    fn a_skipped_step_stays_skipped_when_the_run_loop_completes_it() {
        let mut st = RunState::new();
        st.apply(&Progress::skipped(SetupStep::DownloadUpdates, "already downloaded"));
        st.apply(&Progress::finished(SetupStep::DownloadUpdates));
        let row = st.steps.iter().find(|s| s.step == SetupStep::DownloadUpdates).unwrap();
        assert_eq!(row.state, StepState::Skipped);
        assert_eq!(row.message, "already downloaded");
        assert_eq!(st.completed(), 1);
    }

    #[test]
    fn a_failure_marks_the_running_step_and_leaves_the_rest_pending() {
        let mut st = RunState::new();
        st.apply(&Progress::finished(SetupStep::Dependencies));
        st.apply(&Progress::new(SetupStep::DownloadRuntime, 0.2, "downloading…"));
        st.finish(&Err("the server hung up".into()));
        assert!(st.done);
        assert_eq!(st.error.as_deref(), Some("the server hung up"));
        assert_eq!(st.steps[1].state, StepState::Failed);
        assert_eq!(st.steps[1].message, "the server hung up");
        assert_eq!(st.steps[2].state, StepState::Pending);
    }

    #[test]
    fn a_successful_finish_completes_every_step() {
        let mut st = RunState::new();
        st.finish(&Ok(()));
        assert!(st.steps.iter().all(|s| s.state == StepState::Done));
        assert_eq!(st.completed(), SetupStep::ALL.len());
    }

    #[test]
    fn a_cancelled_run_is_not_reported_as_a_failure() {
        let mut st = RunState::new();
        st.apply(&Progress::finished(SetupStep::Dependencies));
        st.apply(&Progress::new(SetupStep::DownloadClient, 0.4, "228 MB of 571 MB"));
        st.finish(&Err(CANCELLED.to_string()));
        assert!(st.cancelled);
        assert!(st.error.is_none(), "cancelling is not an error to apologise for");
        // The interrupted step is pending again, ready to be resumed.
        let row = st.steps.iter().find(|s| s.step == SetupStep::DownloadClient).unwrap();
        assert_eq!(row.state, StepState::Pending);
        assert_eq!(row.fraction, 0.0);
        // What already finished stays finished.
        assert_eq!(st.steps[0].state, StepState::Done);
    }

    #[test]
    fn run_all_stops_at_the_next_step_once_cancel_is_requested() {
        struct CancelAfterFirst;
        impl Runtime for CancelAfterFirst {
            fn run_step(&self, _: SetupStep, _: &mut dyn FnMut(Progress)) -> Result<(), String> {
                request_cancel();
                Ok(())
            }
            fn discover(&self) -> Result<Install, String> {
                Err("stub".into())
            }
        }
        let _serial = serial();
        clear_cancel();
        let mut st = RunState::new();
        let result = run_all(&CancelAfterFirst, &mut |p| st.apply(&p));
        clear_cancel();
        assert_eq!(result.clone().unwrap_err(), CANCELLED);
        st.finish(&result);
        assert!(st.cancelled);
        // Exactly one step ran; the rest were never touched.
        assert_eq!(st.completed(), 1);
        assert_eq!(st.steps[1].state, StepState::Pending);
    }

    /// A runtime that reports one skip and otherwise does nothing, so we can
    /// drive `run_all` and check the event stream a frontend actually sees.
    struct StubRuntime;

    impl Runtime for StubRuntime {
        fn run_step(&self, step: SetupStep, on: &mut dyn FnMut(Progress)) -> Result<(), String> {
            if step == SetupStep::Components {
                on(Progress::skipped(step, "not needed here"));
            }
            Ok(())
        }
        fn discover(&self) -> Result<Install, String> {
            Err("stub".into())
        }
    }

    #[test]
    fn run_all_leaves_a_run_state_a_frontend_can_render() {
        let _serial = serial();
        clear_cancel();
        let mut st = RunState::new();
        run_all(&StubRuntime, &mut |p| st.apply(&p)).unwrap();
        assert_eq!(st.completed(), SetupStep::ALL.len());
        let comp = st.steps.iter().find(|s| s.step == SetupStep::Components).unwrap();
        assert_eq!(comp.state, StepState::Skipped, "run_all overwrote a skip with a completion");
        assert_eq!(st.steps[0].state, StepState::Done);
    }
}
