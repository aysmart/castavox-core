//! Translations a church downloads, rather than ones it carries whether it
//! wants them or not.
//!
//! # Why this is not in `bible.rs`
//!
//! There are two copies of that file — one in Pulpitry and one vendored into
//! the Castavox sidecar with its Tauri coupling stripped — and they have
//! already drifted. Building the download half twice would make a third copy of
//! the thing most worth getting right, so the parts that are the same in both
//! live here: what is available, fetching it, and proving it is what we
//! published. The import itself stays in each application, because that is the
//! half that is genuinely different.
//!
//! # The integrity check is real, and the bundled one is not
//!
//! `bible.rs` fingerprints a bundled file with FNV-1a and says so in as many
//! words: "nothing here is defending against a tampered resource, only noticing
//! that the text changed". That is the right tool for a file that arrived
//! inside a signed application.
//!
//! A downloaded file did not. It came over a network, from a host we do not
//! own the last mile of, onto a church laptop that will then read scripture
//! from it in front of a congregation. So a pack carries a SHA-256, it is
//! checked before anything is imported, and a mismatch is a hard failure rather
//! than a warning — the failure mode this is guarding against is a Bible with
//! something else in it, and there is no version of that worth showing.
//!
//! # Nothing here writes to the library
//!
//! It downloads to a file and returns the path. Importing is a transaction the
//! application owns, and keeping the two apart means a failed download cannot
//! leave a church halfway through losing the Bible it already had.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long to wait for the catalogue. It is a small JSON document.
const CATALOGUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// A translation that can be had, whether or not this machine has it.
///
/// The shape of `translations.json` plus what a download needs: how big it is,
/// so a church on a metered connection can decide, and a hash, so we can tell
/// whether what arrived is what we published.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Pack {
    pub id: String,
    pub name: String,
    pub year: i64,
    /// The file, relative to the catalogue's own location.
    pub file: String,
    pub verse_count: i64,
    /// The fingerprint `bible.rs` stores, to tell a re-import from a no-op.
    pub checksum: String,
    /// Lower-case hex SHA-256 of the file as published.
    #[serde(default)]
    pub sha256: String,
    /// Compressed size, for a church deciding whether to spend it.
    #[serde(default)]
    pub bytes: u64,
    /// What language a church would look for this under.
    #[serde(default)]
    pub language: String,
    /**
     * What a church should know before downloading, where there is something.
     *
     * Not every pack is a whole Bible, and the ones that are not look like a
     * broken download unless something says otherwise. The Passion Translation
     * has four Old Testament books because that is all of it there exists; the
     * NIV and three others are short of the chapters that are lists, because
     * the text we were given renders those as tables and the verses are simply
     * not in it.
     *
     * Saying so costs a line in a list. Not saying so costs an operator the
     * discovery that Ezra 2 is empty, during a service, with nothing on the
     * screen explaining why.
     */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Verses absent against a complete Bible, counted at build time.
    #[serde(default)]
    pub missing_verses: i64,
    /// Where the text carries a licence, naming it is a condition of shipping
    /// it rather than a courtesy. See `bible.rs`.
    #[serde(default)]
    pub licence: Option<String>,
    #[serde(default)]
    pub attribution: Option<String>,
}

/// Everything on offer, read from the published catalogue.
///
/// Fetched rather than compiled in, so a translation added next month appears
/// in a church's list without an application update — which is the difference
/// between adding a translation and shipping a release to add one.
pub fn catalogue(url: &str) -> Result<Vec<Pack>> {
    let client = crate::tls::client()
        .timeout(CATALOGUE_TIMEOUT)
        .build()
        .context("could not prepare the download client")?;

    let response = client
        .get(url)
        .send()
        .with_context(|| format!("could not reach the translation catalogue at {url}"))?;

    if !response.status().is_success() {
        bail!("the translation catalogue answered {}", response.status());
    }

    response.json().context("the translation catalogue is malformed")
}

/// How far a download has got, for a screen that shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub received: u64,
    /// What the catalogue said to expect, which may be zero if it did not say.
    pub total: u64,
}

/// Fetches one pack and proves it is the one we published.
///
/// Written to `into` only after the hash matches, via a neighbouring temporary
/// file: a half-downloaded pack that kept the name of a real one is a file the
/// next launch would try to import.
///
/// The progress callback is called on the calling thread as bytes arrive, and
/// must not block — it is between the socket and the disk.
pub fn download(
    pack: &Pack,
    base_url: &str,
    into: &Path,
    progress: impl Fn(Progress),
) -> Result<PathBuf> {
    if pack.sha256.trim().is_empty() {
        // A pack with no hash cannot be checked, and an unchecked Bible is the
        // thing this module exists to refuse.
        bail!("{} has no published checksum and will not be downloaded", pack.id);
    }

    let url = format!("{}/{}", base_url.trim_end_matches('/'), pack.file);
    let client = crate::tls::client()
        // No overall timeout: a 5 MB file on a hall's connection is slow rather
        // than broken, and a church watching it arrive should not have it cut
        // off at an arbitrary minute.
        .build()
        .context("could not prepare the download client")?;

    let mut response = client
        .get(&url)
        .send()
        .with_context(|| format!("could not download {}", pack.name))?;

    if !response.status().is_success() {
        bail!("{} could not be downloaded ({})", pack.name, response.status());
    }

    let total = response.content_length().unwrap_or(pack.bytes);
    std::fs::create_dir_all(into)
        .with_context(|| format!("could not make {}", into.display()))?;

    let temporary = into.join(format!("{}.part", pack.id));
    let mut file = std::fs::File::create(&temporary)
        .with_context(|| format!("could not write to {}", temporary.display()))?;

    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let read = response.read(&mut buffer).context("the download was interrupted")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        std::io::Write::write_all(&mut file, &buffer[..read])
            .context("the download could not be written to disk")?;
        received += read as u64;
        progress(Progress { received, total });
    }

    drop(file);

    let got = format!("{:x}", hasher.finalize());
    if got != pack.sha256.trim().to_lowercase() {
        // Removed rather than kept for inspection. What is on the disk is a
        // file claiming to be scripture that is not the file we published, and
        // leaving it anywhere near the library is the whole risk.
        let _ = std::fs::remove_file(&temporary);
        bail!(
            "{} did not arrive intact and has been discarded. Try again, or check the connection.",
            pack.name
        );
    }

    let destination = into.join(&pack.file);
    std::fs::rename(&temporary, &destination)
        .with_context(|| format!("could not put {} in place", pack.name))?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("packs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pack() -> Pack {
        Pack {
            id: "TEST".into(),
            name: "A Test Translation".into(),
            year: 2026,
            file: "TEST.tsv.gz".into(),
            verse_count: 3,
            checksum: "abcd".into(),
            sha256: String::new(),
            bytes: 0,
            language: "English".into(),
            note: None,
            missing_verses: 0,
            licence: None,
            attribution: None,
        }
    }

    #[test]
    fn a_pack_with_no_published_hash_is_refused() {
        // The whole point of the module. An unchecked download is a Bible we
        // cannot vouch for, and there is no degraded version of that worth
        // offering a church.
        let dir = scratch("unhashed");
        let error = download(&pack(), "https://example.invalid", &dir, |_| {}).unwrap_err();
        assert!(
            error.to_string().contains("no published checksum"),
            "refused for the wrong reason: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_catalogue_reads_the_shape_that_ships_today() {
        // The bundled manifest's fields, plus the two a download needs. An
        // entry written before those existed must still parse, or adding the
        // catalogue would break reading the thing it was grown from.
        let packs: Vec<Pack> = serde_json::from_str(
            r#"[
                 {"id":"KJV","name":"King James Version","year":1611,
                  "file":"KJV.tsv.gz","verseCount":31102,"checksum":"a8f1dafb0f618365"},
                 {"id":"BSB","name":"Berean Standard Bible","year":2022,
                  "file":"BSB.tsv.gz","verseCount":31086,"checksum":"beef",
                  "sha256":"AA11","bytes":4194304,"language":"English"}
               ]"#,
        )
        .expect("the catalogue could not be read");

        assert_eq!(packs[0].id, "KJV");
        assert!(packs[0].sha256.is_empty(), "an old entry gained a hash from nowhere");
        assert_eq!(packs[1].bytes, 4_194_304);
        assert_eq!(packs[1].language, "English");
    }
}
