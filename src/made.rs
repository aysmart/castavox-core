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

/// What a machine has made, and how much of it the broker has heard about.
///
/// Two counters that only ever go up, rather than one that goes down as it is
/// reported. What is waiting to be sent is the difference between them, which
/// means there is no subtraction to get wrong -- and it means the number an
/// operator is shown is the one they would say out loud: "we have made
/// forty-seven summaries", not "seven are waiting to be reported", which is a
/// sentence about our plumbing rather than about their church.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Tally {
    /// Made on this machine, ever.
    pub summaries: u32,
    pub slides: u32,
    /// How much of that has reached the broker.
    ///
    /// A file written before these existed reads them as zero, so the first
    /// report sends everything counted so far and then keeps pace. That is the
    /// right answer: nothing had been reported.
    pub sent_summaries: u32,
    pub sent_slides: u32,
}

impl Tally {
    /// What has not reached the broker yet.
    ///
    /// Saturating, because a file edited by hand or restored from a backup can
    /// claim more sent than made, and the answer to that is "nothing to send"
    /// rather than an enormous number.
    pub fn unsent(&self) -> (u32, u32) {
        (
            self.summaries.saturating_sub(self.sent_summaries),
            self.slides.saturating_sub(self.sent_slides),
        )
    }

    fn nothing_to_send(&self) -> bool {
        self.unsent() == (0, 0)
    }
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("made")
}

/// What this machine has made, for a screen that shows it.
///
/// The same file the reporting reads, so an operator's number and ours cannot
/// drift apart. The totals do not move when a report succeeds -- only the sent
/// counters do -- so what an operator sees is what their church has made and
/// not a queue length.
pub fn tally(data_dir: &Path) -> Tally {
    read(data_dir)
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
pub fn send(endpoint: &str, data_dir: &Path, app: &str) {

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
                if tally.nothing_to_send() {
                    return None;
                }

                let (summaries, slides) = tally.unsent();
                let body = serde_json::json!({
                    "install": crate::checkin::install(&data_dir),
                    "app": app,
                    "summaries": summaries,
                    "slides": slides,
                });

                let client = crate::tls::client().timeout(TIMEOUT).build().ok()?;
                let ok = client
                    .post(&endpoint)
                    .json(&body)
                    .send()
                    .is_ok_and(|reply| reply.status().is_success());
                ok.then_some((summaries, slides))
            });

            /*
             * The sent counters advance; the totals are never touched.
             *
             * A summary written while this request was in flight is in the file
             * and not in the request, and it stays waiting rather than being
             * lost -- which is what a "zero the tally" would have cost, and a
             * Sunday's work is exactly when that would have happened.
             */
            if let Ok(Some((summaries, slides))) = sent {
                let mut tally = read(&data_dir);
                tally.sent_summaries = tally.sent_summaries.saturating_add(summaries);
                tally.sent_slides = tally.sent_slides.saturating_add(slides);
                if let Ok(text) = serde_json::to_string(&tally) {
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
    fn a_total_survives_being_reported() {
        // The number an operator is shown is what the church has made, not
        // what is queued: reporting must not take it away from them.
        let dir = scratch("reported");
        record(&dir, 3, 1);

        let mut tally = read(&dir);
        assert_eq!(tally.unsent(), (3, 1));

        // What a successful send does, without the network.
        tally.sent_summaries = 3;
        tally.sent_slides = 1;
        std::fs::write(path(&dir), serde_json::to_string(&tally).unwrap()).unwrap();

        let after = read(&dir);
        assert_eq!(after.summaries, 3, "the total was taken away by reporting");
        assert_eq!(after.unsent(), (0, 0), "something was reported twice");

        // And a summary written after that report is waiting, not lost.
        record(&dir, 1, 0);
        let later = read(&dir);
        assert_eq!(later.summaries, 4);
        assert_eq!(later.unsent(), (1, 0));
        let _ = std::fs::remove_dir_all(&dir);
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
