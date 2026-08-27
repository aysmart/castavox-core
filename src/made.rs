//! How much a church has made, counted locally and sent when it can be.
//!
//! # Informational, and that word is the whole contract
//!
//! The service is unlimited. Nothing here is billed, nothing is compared
//! against a plan, and nothing may ever refuse work because of a number in it.
//! It answers a question we were guessing at -- whether the summaries and the
//! slides are used at all -- and it must not quietly become a meter, because
//! the privacy policy describes it as this.
//!
//! # It rides on the check-in's consent, and nothing else
//!
//! The check-in is off until an operator switches it on, and this is the same
//! switch. A church that never touches it sends nothing here either, which
//! keeps the policy's sentence about the local engine true of them: nothing
//! leaves the building at all.
//!
//! It reuses the check-in's install id for the same reason -- a second
//! identifier would be a second thing to explain, and a second thing to be
//! wrong about in a breach.
//!
//! # Counted locally first
//!
//! A summary written on a Sunday morning in a hall with no working wifi still
//! happened. So the tally is kept in a file and sent when something can reach
//! the broker, which is why the endpoint adds rather than replaces: two reports
//! in a day are two batches of work, not a correction.
//!
//! Nothing about *what* was made is recorded. There is no field for a title, a
//! transcript, a verse or a prompt, and no code path that could put one here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Short enough that a broker having a bad morning costs a launch nothing.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What has been made and not yet reported.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Tally {
    pub summaries: u32,
    pub slides: u32,
}

impl Tally {
    fn empty(&self) -> bool {
        self.summaries == 0 && self.slides == 0
    }
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("made")
}

fn read(data_dir: &Path) -> Tally {
    std::fs::read_to_string(path(data_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Adds one thing made, whether or not it will ever be reported.
///
/// Counted even when the check-in is off. The switch decides what is *sent*,
/// and a church that turns it on next month should not have this month read as
/// a month of nothing -- while a church that never turns it on has a small file
/// on its own disk that nobody ever reads, which is the same as not counting.
pub fn record(data_dir: &Path, summaries: u32, slides: u32) {
    let mut tally = read(data_dir);
    tally.summaries = tally.summaries.saturating_add(summaries);
    tally.slides = tally.slides.saturating_add(slides);
    if let Ok(text) = serde_json::to_string(&tally) {
        let _ = std::fs::write(path(data_dir), text);
    }
}

/// Sends what has accumulated, and forgets it only if the broker took it.
///
/// Everything after the boolean happens on its own thread, for the reason the
/// check-in gives at length: a "small file read" on a roaming profile or behind
/// an antivirus scanner is where an application stops for four seconds on
/// launch, and the operator is watching a splash screen.
pub fn send(enabled: bool, endpoint: &str, data_dir: &Path, app: &str) {
    if !enabled {
        return;
    }

    let endpoint = endpoint.to_string();
    let data_dir = data_dir.to_path_buf();
    let app = app.to_string();

    std::thread::Builder::new()
        .name("castavox-made".into())
        .spawn(move || {
            // Nothing in here may reach the application, by any route -- see
            // the check-in, which explains what this is guarding against.
            let sent = std::panic::catch_unwind(|| {
                let tally = read(&data_dir);
                if tally.empty() {
                    return None;
                }

                let body = serde_json::json!({
                    "install": crate::checkin::install(&data_dir),
                    "app": app,
                    "summaries": tally.summaries,
                    "slides": tally.slides,
                });

                let client = crate::tls::client().timeout(TIMEOUT).build().ok()?;
                let ok = client
                    .post(&endpoint)
                    .json(&body)
                    .send()
                    .is_ok_and(|reply| reply.status().is_success());
                ok.then_some(tally)
            });

            /*
             * Subtracted rather than zeroed, and the difference is a Sunday.
             *
             * A summary written while this request was in flight is in the file
             * and not in the request. Zeroing would throw it away; subtracting
             * what was actually sent leaves it to go next time.
             */
            if let Ok(Some(sent)) = sent {
                let mut left = read(&data_dir);
                left.summaries = left.summaries.saturating_sub(sent.summaries);
                left.slides = left.slides.saturating_sub(sent.slides);
                if let Ok(text) = serde_json::to_string(&left) {
                    let _ = std::fs::write(path(&data_dir), text);
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, the way the check-in's tests do it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("made-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_tally_accumulates_across_launches() {
        let dir = scratch("accumulates");
        record(&dir, 1, 0);
        record(&dir, 0, 2);
        record(&dir, 1, 0);

        let tally = read(&dir);
        assert_eq!(tally.summaries, 2);
        assert_eq!(tally.slides, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_file_reads_as_nothing_made() {
        // A church whose disk lost this file has made nothing as far as we are
        // concerned, which is the only answer that cannot be wrong in a way
        // that matters.
        let dir = scratch("broken");
        assert_eq!(read(&dir), Tally::default());

        std::fs::write(path(&dir), "not json at all").unwrap();
        assert_eq!(read(&dir), Tally::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
