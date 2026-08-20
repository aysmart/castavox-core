//! A once-a-day note that an install exists, if its operator agreed to it.
//!
//! # The sentence this has to keep true
//!
//! Our published privacy policy says, of the free local engine, that "nothing
//! leaves the building at all". A check-in on launch makes that false for
//! exactly the people it measures -- and the comment beside that policy records
//! that every claim in it was checked against the code rather than borrowed
//! from a template, so it is not boilerplate to quietly outgrow. It is also
//! what we sell: the competitive position rests on working without a network.
//!
//! So this is **opt-in**, disclosed in one sentence on first run, and the policy
//! changes in the same release. A church that never touches the switch is a
//! church nothing leaves, and the old sentence stays true of them.
//!
//! # What it carries, and what it refuses to
//!
//! An install id, the application and its version, the platform, what the
//! hardware is, and which engine is in use. Enough to answer how many churches
//! run this, on what, and whether a release is live.
//!
//! It refuses two things that were asked for:
//!
//! - **The machine name.** These are overwhelmingly personal -- "Ayo's
//!   MacBook", "Pastor David PC". That is personal data under the NDPA and the
//!   GDPR, it says nothing the install id does not, and it is the single field
//!   most likely to embarrass us in a breach.
//! - **The address.** The country is derived at the edge and the address
//!   discarded, which is what the website's own middleware already does. Nothing
//!   here is precise enough to locate a building.
//!
//! It carries no content of any kind: no transcript, no verse, no reference, no
//! filename, no audio. There is no field for one and no code path that could
//! put one here.
//!
//! # It never blocks anything
//!
//! Sent on a background thread with a short timeout, and every failure is
//! silent. A church with no network, a blocked domain or a broker having a bad
//! morning must open the app and run a service exactly as they would otherwise.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long between check-ins. A day, because the questions this answers are
/// "how many churches" and "which releases are live", and neither moves faster.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Short enough that a broker having a bad morning costs a launch nothing.
const TIMEOUT: Duration = Duration::from_secs(5);

/// What crosses the wire. Every field is here because a business question needs
/// it, and the doc on each says which.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckIn {
    /// A random identifier made once on this machine and kept.
    ///
    /// Counts installs and retention. It identifies nothing about a person and
    /// is derived from nothing -- not the hardware, not the user, not the
    /// machine name -- so it cannot be correlated with anything outside our own
    /// records, and deleting the file below makes this install a new one.
    pub install: String,
    /// "pulpitry" or "castavox", and its version. Which releases are live.
    pub app: String,
    pub version: String,
    /// Platform mix: what to test on and what to support.
    pub os: String,
    pub os_version: String,
    /// The hardware questions we keep guessing at, and got wrong in both
    /// directions -- a thin-and-light handed a model it could not carry, an
    /// Apple laptop told it might struggle at twenty-five times real time.
    pub arch: String,
    pub cores: usize,
    /// Free, Azure, or a subscription. The split we have no way to see today.
    pub engine: String,
}

/// Where the install id lives. A plain file, so an operator who wants to be
/// counted as a new install can delete it.
fn id_path(data_dir: &Path) -> PathBuf {
    data_dir.join("install-id")
}

/// When we last checked in, so a machine opened five times in a morning is
/// counted once.
fn stamp_path(data_dir: &Path) -> PathBuf {
    data_dir.join("last-check-in")
}

/// Reads the install id, making one the first time.
///
/// Random rather than derived. A hash of the hardware would be stable across a
/// reinstall, which sounds useful and is exactly what makes it a fingerprint --
/// it would survive an operator deliberately starting fresh.
fn install_id(data_dir: &Path) -> String {
    let path = id_path(data_dir);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // 128 bits from the OS, formatted as a UUID so it reads as an identifier
    // rather than as something decodable.
    let mut bytes = [0u8; 16];
    getrandom(&mut bytes);
    let made = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    );
    let _ = std::fs::write(&path, &made);
    made
}

/// Fills a buffer with random bytes, falling back to the clock.
///
/// The fallback is weak and that is acceptable here: the worst case is two
/// installs sharing an id and being counted as one, which is a rounding error
/// in a population count. Nothing about this identifier guards anything.
fn getrandom(bytes: &mut [u8; 16]) {
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, bytes))
        .is_ok()
    {
        return;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let seed = now.as_nanos() as u64 ^ std::process::id() as u64;
    for (at, byte) in bytes.iter_mut().enumerate() {
        *byte = (seed >> ((at % 8) * 8)) as u8 ^ (at as u8).wrapping_mul(31);
    }
}

/// Whether a day has passed since the last check-in.
fn due(data_dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(stamp_path(data_dir)) else {
        return true;
    };
    let Ok(then) = raw.trim().parse::<u64>() else {
        return true;
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // A clock moved backwards reads as due, which costs one extra check-in.
    now.saturating_sub(then) >= EVERY.as_secs()
}

/// Sends the check-in, on a background thread, if one is due.
///
/// Returns immediately. Every failure -- no network, a blocked domain, a broker
/// that is down -- is silent by design: this must never be the reason an app is
/// slow to open or a service does not start.
///
/// `enabled` is the operator's switch, and a false here does nothing at all: no
/// file is written, no thread is spawned, no request is made.
pub fn send(enabled: bool, endpoint: &str, data_dir: &Path, app: &str, version: &str, engine: &str) {
    if !enabled || !due(data_dir) {
        return;
    }

    let body = CheckIn {
        install: install_id(data_dir),
        app: app.to_string(),
        version: version.to_string(),
        os: std::env::consts::OS.to_string(),
        os_version: os_version(),
        arch: std::env::consts::ARCH.to_string(),
        cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        engine: engine.to_string(),
    };

    let endpoint = endpoint.to_string();
    let data_dir = data_dir.to_path_buf();
    std::thread::Builder::new()
        .name("castavox-check-in".into())
        .spawn(move || {
            /*
             * Caught, not merely handled.
             *
             * Everything below is best-effort and the whole feature is a
             * courtesy, so nothing here may take a process down. It very nearly
             * did: reqwest is built with `rustls-no-provider` and building a
             * client before a provider is installed does not fail, it *panics*
             * -- and on the first launch with the switch on, that panic reached
             * the event loop thread and killed the window. A church would have
             * discovered that by agreeing to help us.
             *
             * `tls::client` below is the fix for that particular cause and the
             * module exists precisely so nobody has to remember it. This catch
             * is for the next cause, whatever it is.
             */
            let attempt = std::panic::catch_unwind(|| {
                let client = crate::tls::client().timeout(TIMEOUT).build().ok()?;
                let sent = client.post(&endpoint).json(&body).send().ok()?;
                sent.status().is_success().then_some(())
            });

            // Stamped only on success, so a fortnight offline does not read as a
            // fortnight of check-ins that never happened -- and a panic is not a
            // success however loudly it announces itself.
            if matches!(attempt, Ok(Some(()))) {
                let now =
                    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                let _ = std::fs::write(stamp_path(&data_dir), now.to_string());
            }
        })
        .ok();
}

/// The OS version, or empty when the platform will not say cheaply.
fn os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "macos"))]
    {
        String::new()
    }
}

/// What the broker stores, once the edge has turned an address into a country
/// and thrown the address away.
///
/// Here rather than only in the broker so that both halves of the contract are
/// visible in one place: what is sent, and what is kept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    pub install: String,
    pub app: String,
    pub version: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cores: usize,
    pub engine: String,
    /// Two letters, derived at the edge. There is no field for the address it
    /// came from, here or in the table.
    pub country: String,
    /// The day, not the moment. A per-launch timestamp is a usage log, and the
    /// questions this answers are counted by day.
    pub day: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_install_id_is_made_once_and_then_kept() {
        let dir = std::env::temp_dir().join(format!("checkin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let first = install_id(&dir);
        assert_eq!(first, install_id(&dir), "a second launch is the same install");
        assert_eq!(first.len(), 36, "shaped like a uuid");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_switch_that_is_off_does_nothing_at_all() {
        let dir = std::env::temp_dir().join(format!("checkin-off-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        send(false, "http://127.0.0.1:1/never", &dir, "pulpitry", "0.4.0", "local");

        // Not merely "sends nothing": writes nothing either. An install id
        // created for somebody who declined would be an identifier we made
        // without being allowed to.
        assert!(!id_path(&dir).exists(), "no install id for an operator who said no");
        assert!(!stamp_path(&dir).exists(), "no record of a check-in that never happened");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_payload_has_nowhere_to_put_content() {
        // A field-by-field guard: this is the list the privacy policy will
        // describe, and a field added here without a matching sentence there is
        // the failure this test exists to make loud.
        let json = serde_json::to_string(&CheckIn {
            install: "i".into(),
            app: "pulpitry".into(),
            version: "0.4.0".into(),
            os: "macos".into(),
            os_version: "15.0".into(),
            arch: "aarch64".into(),
            cores: 8,
            engine: "local".into(),
        })
        .expect("encode");

        let mut keys: Vec<&str> = serde_json::from_str::<serde_json::Value>(&json)
            .expect("decode")
            .as_object()
            .expect("object")
            .keys()
            .map(|k| k.as_str().to_owned().leak() as &str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["app", "arch", "cores", "engine", "install", "os", "osVersion", "version"],
            "the payload changed -- the privacy policy has to change with it"
        );
    }
}
