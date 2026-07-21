//! Downloading and unpacking — the plumbing both runtimes share.
//!
//! The Linux (Proton) and macOS (Wine) runtimes fetch and unpack different
//! things from different places, but the mechanics are identical: stream a URL to
//! disk with progress, unpack a tarball or a zip, find a file in a directory by a
//! set of name prefixes. Those live here so there is one copy, tested once,
//! rather than a subtly-different pair drifting apart across `proton.rs` and
//! `wine.rs`. Nothing here is platform-specific — it is ureq, the zip crate and
//! flate2/tar, all of which build everywhere.

use crate::setup::{Progress, SetupStep};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Stream a URL to a file, reporting download progress. Writes to a `.part` file
/// and renames on success so an interrupted download is never mistaken for a
/// complete one.
///
/// Progress is reported at most [`REPORT_EVERY`] apart rather than once per 64 KB
/// chunk: a 571 MB file is ~9000 chunks, and every one of those crossed a channel
/// to the GTK main loop or took the FFI state mutex for a line no one could read
/// at that rate. Ten a second is smooth and free.
pub(crate) fn download(
    url: &str,
    dest: &Path,
    step: SetupStep,
    on: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let resp = ureq::get(url)
        .set("User-Agent", "betterac")
        .call()
        .map_err(|e| format!("download failed ({url}): {e}"))?;
    let total: u64 = resp.header("Content-Length").and_then(|s| s.parse().ok()).unwrap_or(0);

    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut got: u64 = 0;

    let started = Instant::now();
    let mut last_report = started;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        // The one place a cancel can land promptly: everything else in setup is an
        // external command we have to let finish. Drop the partial file rather
        // than leave half a gigabyte of nothing behind.
        if crate::setup::cancel_requested() {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(crate::setup::CANCELLED.to_string());
        }
        if last_report.elapsed() >= REPORT_EVERY {
            last_report = Instant::now();
            on(Progress::new(step, fraction(got, total), transferred(got, total, started)));
        }
    }
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    on(Progress::new(step, 1.0, transferred(got, got, started)));
    Ok(())
}

/// How often a running download reports itself.
const REPORT_EVERY: Duration = Duration::from_millis(100);

/// How far through we are, or 0.0 when the server didn't say how big the file is
/// (an unknown total is better shown as an indeterminate bar than a fake one).
fn fraction(got: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        got as f32 / total as f32
    }
}

/// The line under the bar: "312 MB of 571 MB · 8.4 MB/s". The size is what tells
/// you whether a stalled-looking bar is actually moving, and the rate is what
/// tells you whether it is worth waiting for.
fn transferred(got: u64, total: u64, started: Instant) -> String {
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    let secs = started.elapsed().as_secs_f64();
    let rate =
        if secs > 0.5 { format!(" · {:.1} MB/s", mb(got) / secs) } else { String::new() };
    if total > 0 {
        format!("{:.0} MB of {:.0} MB{rate}", mb(got), mb(total))
    } else {
        format!("{:.0} MB{rate}", mb(got))
    }
}

pub(crate) fn extract_tar_gz(tarball: &Path, dest: &Path) -> Result<(), String> {
    let f = std::fs::File::open(tarball).map_err(|e| e.to_string())?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(dest).map_err(|e| format!("unpacking {}: {e}", tarball.display()))
}

pub(crate) fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let f = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("{}: {e}", zip_path.display()))?;
    zip.extract(dest).map_err(|e| format!("unzipping {}: {e}", zip_path.display()))
}

/// A file in `dir` whose lowercased name starts with any of `prefixes` and ends
/// with `.ext`. Case-insensitive, matching the shell installer's nocaseglob.
pub(crate) fn find_in_dir(dir: &Path, prefixes: &[&str], ext: &str) -> Option<PathBuf> {
    let want_ext = format!(".{ext}");
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let n = p.file_name()?.to_string_lossy().to_ascii_lowercase();
        if n.ends_with(&want_ext) && prefixes.iter().any(|pre| n.starts_with(pre)) {
            return Some(p);
        }
    }
    None
}

/// Fail unless the file's SHA-256 matches `expected_hex` (case-insensitive).
/// Streams the file so a multi-hundred-MB download isn't slurped into memory.
pub(crate) fn verify_sha256(path: &Path, expected_hex: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch for {} — expected {expected_hex}, got {got}",
            path.display()
        ))
    }
}
