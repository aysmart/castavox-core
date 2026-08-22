//! Noticing that a newer version exists.
//!
//! # Why this is not part of the check-in
//!
//! The once-a-day check-in in [`crate::checkin`] is opt-in, and most churches
//! will never switch it on. An update notice that rode on it would reach only
//! the ones who had already agreed to be counted — which is exactly the wrong
//! set, because the churches least likely to opt in are the ones least likely
//! to be watching a downloads page for a new version.
//!
//! So this runs for everybody, and is designed so that running for everybody is
//! defensible.
//!
//! # What it sends: nothing
//!
//! It asks a public list of releases what the newest one is. There is no
//! identifier in the request, no version of ours, no machine facts, no query
//! string of any kind — it is a plain read of a file anybody can read, and it
//! would be identical from every installation on earth.
//!
//! It also does not ask *us*. The releases live on GitHub, which already serves
//! every download, so the request never reaches our servers and there is no log
//! of it for us to hold, lose or be asked to produce. That is a stronger promise
//! than a privacy policy, because it is not a promise about what we do with
//! something — we never receive it.
//!
//! An operator who wants a machine that touches nothing at all can still switch
//! it off, which is why the setting exists rather than being assumed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where the installers are published. Public, so this needs no credential.
const RELEASES: &str = "https://api.github.com/repos/aysmart/castavox-downloads/releases?per_page=30";

/// A day. New versions arrive weekly at most, and a church that opens the app
/// six times on a Sunday should ask once.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Short: nobody is waiting for this, and a slow answer is the same as none.
const TIMEOUT: Duration = Duration::from_secs(6);

/// A newer release than the one running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Newer {
    /// "0.7.0", as it would be written on a page rather than as a tag.
    pub version: String,
    /// Where a person goes to get it.
    pub url: String,
}

fn stamp_path(data_dir: &Path, app: &str) -> PathBuf {
    data_dir.join(format!("last-update-check-{app}"))
}

/// Whether a day has passed since the last look.
fn due(data_dir: &Path, app: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(stamp_path(data_dir, app)) else {
        return true;
    };
    let Ok(then) = raw.trim().parse::<u64>() else {
        return true;
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    now.saturating_sub(then) >= EVERY.as_secs()
}

/// Compares two dotted versions numerically.
///
/// Numerically rather than as strings, because "0.10.0" sorts before "0.9.0"
/// alphabetically and would tell a church on the newest build that it is behind.
fn parts(version: &str) -> Vec<u32> {
    version.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

fn newer_than(candidate: &str, running: &str) -> bool {
    let (a, b) = (parts(candidate), parts(running));
    for at in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(at).copied().unwrap_or(0), b.get(at).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Looks for a newer release of `app`, on a background thread.
///
/// `app` is the tag prefix — "castavox" or "pulpitry" — because both products
/// publish to one repository and each should only notice its own.
///
/// Returns immediately. `found` is called only when there is something to say,
/// so a caller never has to reason about the ordinary case of being up to date.
/// Every failure is silent: no network, a blocked domain, a rate limit, a
/// malformed reply. Nobody asked for this and nobody is waiting on it.
pub fn look(
    enabled: bool,
    data_dir: &Path,
    app: &str,
    running: &str,
    found: impl Fn(Newer) + Send + 'static,
) {
    if !enabled {
        return;
    }

    let data_dir = data_dir.to_path_buf();
    let app = app.to_string();
    let running = running.to_string();

    std::thread::Builder::new()
        .name("castavox-update-check".into())
        .spawn(move || {
            // Caught for the same reason the check-in's is: a courtesy must not
            // be able to take a service down, whatever the cause turns out to
            // be next time.
            let newer = std::panic::catch_unwind(|| {
                if !due(&data_dir, &app) {
                    return None;
                }

                let client = crate::tls::client().timeout(TIMEOUT).build().ok()?;
                let body = client
                    .get(RELEASES)
                    // GitHub refuses a request with no user agent. It names the
                    // product and nothing else -- no version, no machine, no
                    // installation.
                    .header("user-agent", "castavox")
                    .header("accept", "application/vnd.github+json")
                    .send()
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .text()
                    .ok()?;

                let releases: serde_json::Value = serde_json::from_str(&body).ok()?;
                let prefix = format!("{app}-v");

                let mut best: Option<String> = None;
                for release in releases.as_array()? {
                    if release.get("draft").and_then(|d| d.as_bool()).unwrap_or(false) {
                        continue;
                    }
                    let tag = release.get("tag_name")?.as_str()?;
                    let Some(version) = tag.strip_prefix(&prefix) else { continue };
                    if best.as_deref().is_none_or(|held| newer_than(version, held)) {
                        best = Some(version.to_string());
                    }
                }

                // Stamped whatever the answer, because the question was asked
                // and asking again in an hour would not change it.
                let now =
                    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                let _ = std::fs::write(stamp_path(&data_dir, &app), now.to_string());

                let version = best?;
                newer_than(&version, &running).then(|| Newer {
                    url: format!(
                        "https://github.com/aysmart/castavox-downloads/releases/tag/{app}-v{version}"
                    ),
                    version,
                })
            });

            if let Ok(Some(newer)) = newer {
                found(newer);
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers_not_as_text() {
        assert!(newer_than("0.10.0", "0.9.0"), "ten is after nine");
        assert!(newer_than("1.0.0", "0.99.9"));
        assert!(newer_than("0.6.1", "0.6.0"));
        assert!(!newer_than("0.6.0", "0.6.0"), "the same is not newer");
        assert!(!newer_than("0.5.0", "0.6.0"));
        // A shorter version is not a smaller one where the rest is zero.
        assert!(!newer_than("0.6", "0.6.0"));
        assert!(newer_than("0.6.1", "0.6"));
    }

    #[test]
    fn a_switch_that_is_off_does_nothing_at_all() {
        let dir = std::env::temp_dir().join(format!("update-off-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        look(false, &dir, "pulpitry", "0.1.0", |_| panic!("must not run"));
        std::thread::sleep(Duration::from_millis(150));

        // Not merely "asks nothing": records nothing either, so an operator who
        // switched it off leaves no trace of a question never asked.
        assert!(!stamp_path(&dir, "pulpitry").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_caller_is_not_kept_waiting() {
        let dir = std::env::temp_dir().join(format!("update-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let began = std::time::Instant::now();
        look(true, &dir, "pulpitry", "0.1.0", |_| {});
        assert!(began.elapsed() < Duration::from_millis(250));
        std::fs::remove_dir_all(&dir).ok();
    }
}
