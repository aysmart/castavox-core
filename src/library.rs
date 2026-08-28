//! Putting a translation into a church's Bible, and taking one out.
//!
//! # Why this is not in either application
//!
//! It was in both. `bible.rs` exists twice -- once in Pulpitry, once vendored
//! into the Castavox sidecar with its Tauri coupling stripped -- and the two
//! copies had already drifted by a couple of hundred lines. The import itself
//! never differed: the same schema, the same transaction, the same FTS rebuild,
//! byte for byte in both files.
//!
//! Writing the download half a second time into the second copy is what this
//! module exists to avoid. The project has paid for that mistake more than once
//! already -- a table that would not draw because one of three copies of a
//! writer was missing a field, a Fill switch that did nothing because a third
//! copy of the picture code dropped it -- and a Bible with the wrong verses in
//! it is a worse thing to get wrong than either.
//!
//! # What each application still owns
//!
//! Where the database lives, how it is locked, and where a bundled resource is
//! found. Those genuinely differ: one resolves paths through Tauri, the other
//! is handed a directory. Everything below takes a connection and a file.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use rusqlite::{params, Connection, OptionalExtension};

use crate::packs::Pack;

/// Bumped when the shape below changes in a way an existing database cannot be
/// migrated into. That is a drop and a reimport, which for a downloaded
/// translation means downloading it again -- so prefer `migrate` below, and
/// keep this for changes that genuinely cannot be expressed as an ALTER.
pub const SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS translations (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    year        INTEGER NOT NULL DEFAULT 0,
    ordinal     INTEGER NOT NULL DEFAULT 0,
    checksum    TEXT    NOT NULL DEFAULT '',
    verse_count INTEGER NOT NULL DEFAULT 0
);

/*
 * Translations this church has said no to.
 *
 * Without this, removing a bundled translation is a button that works until
 * the next launch and then quietly undoes itself, because the import that
 * fills an empty library on first run cannot tell "never had it" from "did not
 * want it". One row per refusal, cleared when somebody downloads it again.
 */
CREATE TABLE IF NOT EXISTS declined (
    id TEXT PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS verses (
    id          INTEGER PRIMARY KEY,
    translation TEXT    NOT NULL,
    book_number INTEGER NOT NULL,
    book        TEXT    NOT NULL,
    chapter     INTEGER NOT NULL,
    verse       INTEGER NOT NULL,
    text        TEXT    NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_verses_reference
    ON verses(translation, book_number, chapter, verse);

/*
 * The original languages, keyed by where a word sits rather than by
 * translation.
 *
 * Greek and Hebrew belong to no translation: a church reading the KJV and one
 * reading the Yoruba Bible are looking at the same underlying text. So this
 * joins on book, chapter and verse, and every translation gets it for free.
 *
 * No foreign key to `verses`. A verse a translation happens not to carry -- a
 * versification difference, a book still importing -- should not take its Greek
 * with it.
 */
CREATE TABLE IF NOT EXISTS interlinear (
    book_number     INTEGER NOT NULL,
    chapter         INTEGER NOT NULL,
    verse           INTEGER NOT NULL,
    position        INTEGER NOT NULL,
    original        TEXT    NOT NULL,
    transliteration TEXT    NOT NULL DEFAULT '',
    gloss           TEXT    NOT NULL DEFAULT '',
    strongs         TEXT    NOT NULL DEFAULT '',
    morphology      TEXT    NOT NULL DEFAULT '',
    lemma           TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (book_number, chapter, verse, position)
);

/*
 * What a Strong's number means, from STEPBible's brief lexicons.
 *
 * Keyed by the same extended numbers the interlinear carries, so a word on
 * screen reaches its entry with nothing to match on.
 */
CREATE TABLE IF NOT EXISTS lexicon (
    strongs     TEXT PRIMARY KEY,
    lemma       TEXT NOT NULL DEFAULT '',
    translit    TEXT NOT NULL DEFAULT '',
    part        TEXT NOT NULL DEFAULT '',
    gloss       TEXT NOT NULL DEFAULT '',
    definition  TEXT NOT NULL DEFAULT '',
    -- Thayer's, where there is one. Greek New Testament only.
    thayer      TEXT NOT NULL DEFAULT ''
);

/*
 * Which build of a bundled resource is in the tables above.
 *
 * The interlinear and the lexicon were guarded on row count alone, which
 * answers "has this ever been imported" and not "is this the current text".
 * They are the same count either way, so a correction to the words themselves
 * -- 2,218 Thayer's entries reaching the operator with a literal `&#x27;` in
 * the middle of a sentence -- would have landed on new installations only, and
 * never on anybody who already had the app.
 */
CREATE TABLE IF NOT EXISTS imports (
    name        TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS verse_embeddings (
    verse_id    INTEGER PRIMARY KEY,
    translation TEXT NOT NULL,
    vector      BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_embeddings_translation
    ON verse_embeddings(translation);

CREATE VIRTUAL TABLE IF NOT EXISTS verses_fts USING fts5(
    text,
    content='verses',
    content_rowid='id',
    tokenize='porter unicode61'
);
"#;

/// Changes an existing database can absorb without losing its contents.
///
/// A failure here is almost always "duplicate column name", which is success.
/// Anything else surfaces when the query that needs the column runs, with a
/// message naming the column rather than the migration.
fn migrate(connection: &Connection) {
    for statement in [
        "ALTER TABLE lexicon ADD COLUMN thayer TEXT NOT NULL DEFAULT ''",
        /*
         * Where a translation's licence obliges us to name somebody.
         *
         * Recorded against the installed row rather than read from the bundled
         * manifest, because the manifest only describes what ships. The moment
         * a translation is downloaded rather than bundled -- which is the whole
         * direction of travel here -- a manifest lookup returns nothing and the
         * attribution silently stops being shown. For CC BY-SA that is not a
         * cosmetic regression: naming the source is a condition of being
         * allowed to ship the text at all.
         *
         * By migration and not a schema bump. A bump drops the tables, and a
         * downloaded translation would then have to be downloaded again.
         */
        "ALTER TABLE translations ADD COLUMN licence TEXT NOT NULL DEFAULT ''",
        "ALTER TABLE translations ADD COLUMN attribution TEXT NOT NULL DEFAULT ''",
    ] {
        let _ = connection.execute(statement, []);
    }
}

/// Brings a freshly opened database up to the shape everything below expects.
///
/// Both applications did this identically and separately, which is how the two
/// copies of the schema were free to drift. A church's verses are the last
/// thing that should depend on two files being kept in step by hand.
///
/// A database at the wrong version has its translation tables dropped rather
/// than migrated. That costs a reimport of the bundled texts, which is seconds,
/// and it is why `migrate` exists for everything that can be done in place.
pub fn prepare(connection: &Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        connection.execute_batch(
            "DROP TABLE IF EXISTS verses_fts;
             DROP TABLE IF EXISTS verses;
             DROP TABLE IF EXISTS translations;",
        )?;
    }
    connection
        .execute_batch(SCHEMA)
        .context("could not prepare the verse library")?;
    migrate(connection);
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// The ids a church has removed and does not want back.
///
/// The first-run import cannot otherwise tell "never had it" from "did not want
/// it", so without this every removal quietly undoes itself overnight.
pub fn declined(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare("SELECT id FROM declined")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The texts whose licence obliges us to name them.
///
/// CC BY-SA permits the commercial use we make of these and asks for
/// attribution in return, so this is a condition of shipping rather than a
/// credit we chose to give. Read from what is installed, so it stays right when
/// a translation is downloaded rather than bundled.
///
/// Deduplicated: five of the translations are Biblica's and one line covers all
/// of them.
pub fn attributions(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT attribution, licence FROM translations
         WHERE licence <> '' AND attribution <> ''
         ORDER BY attribution",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(format!(
            "{} — {}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Records a licence against a translation already installed without one.
///
/// Every existing installation imported its texts before there were columns to
/// put this in, and an import only runs again when the text itself changes. So
/// the obligation would not appear on those machines until some unrelated
/// correction happened to trigger a reimport, which could be never.
pub fn backfill_licence(connection: &Connection, pack: &Pack) -> Result<()> {
    let (Some(licence), Some(attribution)) = (&pack.licence, &pack.attribution) else {
        return Ok(());
    };
    connection.execute(
        "UPDATE translations SET licence = ?2, attribution = ?3
         WHERE id = ?1 AND licence = ''",
        params![pack.id, licence, attribution],
    )?;
    Ok(())
}

/// Whether the installed copy matches the pack.
///
/// Two things have to line up. The checksum catches a corrected text shipped in
/// an application update, which a verse count alone would miss because fixing
/// wording changes no counts. The row count catches an import that was
/// interrupted partway and must not be mistaken for a finished one.
pub fn is_current(connection: &Connection, pack: &Pack) -> Result<bool> {
    let installed: Option<(String, i64)> = connection
        .query_row(
            "SELECT checksum, verse_count FROM translations WHERE id = ?1",
            params![pack.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let Some((checksum, expected)) = installed else { return Ok(false) };
    if checksum != pack.checksum || expected <= 0 {
        return Ok(false);
    }

    let actual: i64 = connection.query_row(
        "SELECT COUNT(*) FROM verses WHERE translation = ?1",
        params![pack.id],
        |row| row.get(0),
    )?;
    Ok(actual == expected)
}

/// Reads a pack into the library, replacing whatever was there under that id.
///
/// A transaction, so the two states a church can be left in are "as it was" and
/// "with the translation". There is no third state where a Bible has holes in
/// it: a truncated file is refused by the count check before the commit.
pub fn import(connection: &mut Connection, pack: &Pack, ordinal: i64, path: &Path) -> Result<i64> {
    let id = &pack.id;
    let file = std::fs::File::open(path)
        .with_context(|| format!("could not open {}", path.display()))?;
    let mut raw = String::new();
    GzDecoder::new(file)
        .read_to_string(&mut raw)
        .with_context(|| format!("could not decompress {}", path.display()))?;

    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM verses WHERE translation = ?1", params![id])?;

    let mut imported = 0i64;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO verses(translation, book_number, book, chapter, verse, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;

        for (index, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut fields = line.splitn(5, '\t');
            let (Some(book_number), Some(book), Some(chapter), Some(verse), Some(text)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                bail!("{}: line {} has too few fields", path.display(), index + 1);
            };

            insert.execute(params![
                id,
                book_number.parse::<i64>().context("bad book number")?,
                book,
                chapter.parse::<i64>().context("bad chapter number")?,
                verse.parse::<i64>().context("bad verse number")?,
                text,
            ])?;
            imported += 1;
        }
    }

    // The pack records what this file should contain. A mismatch means it is
    // truncated or does not match the entry it came with -- importing it part
    // way would leave a Bible with holes in it.
    if imported != pack.verse_count {
        bail!(
            "{} yielded {imported} verses but {} were expected",
            path.display(),
            pack.verse_count
        );
    }

    transaction.execute(
        "INSERT INTO translations(id, name, year, ordinal, checksum, verse_count,
                                  licence, attribution)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             year = excluded.year,
             ordinal = excluded.ordinal,
             checksum = excluded.checksum,
             verse_count = excluded.verse_count,
             licence = excluded.licence,
             attribution = excluded.attribution",
        params![
            id,
            pack.name,
            pack.year,
            ordinal,
            pack.checksum,
            imported,
            pack.licence.clone().unwrap_or_default(),
            pack.attribution.clone().unwrap_or_default(),
        ],
    )?;
    transaction.commit()?;

    // External-content FTS5: build the index in one pass from the table.
    connection.execute("INSERT INTO verses_fts(verses_fts) VALUES('rebuild')", [])?;

    Ok(imported)
}

/// Installs a downloaded pack, by exactly the route a bundled one takes.
///
/// The file has already been fetched and its SHA-256 checked by `packs`; what
/// happens here is the same transaction the first-run import runs. A download
/// that arrives corrupt never reaches this function.
///
/// The ordinal puts it after everything already installed. Nothing reads the
/// library in that order any more -- it is listed alphabetically -- but it
/// still records the order things arrived, which is worth keeping.
pub fn install(connection: &mut Connection, pack: &Pack, path: &Path) -> Result<i64> {
    let next: i64 = connection
        .query_row("SELECT COALESCE(MAX(ordinal), -1) + 1 FROM translations", [], |row| {
            row.get(0)
        })
        .unwrap_or(0);

    let imported =
        import(connection, pack, next, path).with_context(|| format!("could not install {}", pack.id))?;

    // Asked for again, so it is no longer refused: a bundled translation
    // downloaded back should behave exactly like one that was never removed,
    // including surviving a reinstall of the application.
    connection.execute("DELETE FROM declined WHERE id = ?1", params![pack.id])?;
    Ok(imported)
}

/// Removes a translation and everything derived from it.
///
/// Three tables, because a translation is not only its verses: the meaning
/// index is 65 MB of the reason somebody is removing it, and leaving those rows
/// behind would mean the disk barely moves and the operator concludes the
/// button does nothing.
///
/// It refuses the last one. An application whose purpose is putting scripture
/// on a screen should not offer a control that leaves it with no scripture.
pub fn remove(connection: &mut Connection, id: &str) -> Result<()> {
    let installed: i64 =
        connection.query_row("SELECT COUNT(*) FROM translations", [], |row| row.get(0))?;
    if installed <= 1 {
        bail!("this is the only translation installed, so it cannot be removed");
    }

    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM verse_embeddings WHERE translation = ?1", params![id])?;
    let removed = transaction.execute("DELETE FROM verses WHERE translation = ?1", params![id])?;
    transaction.execute("DELETE FROM translations WHERE id = ?1", params![id])?;
    transaction.execute(
        "INSERT INTO declined(id) VALUES (?1) ON CONFLICT(id) DO NOTHING",
        params![id],
    )?;
    transaction.commit()?;

    if removed == 0 {
        bail!("{id} is not installed");
    }

    // External-content FTS5 keeps its own copy of the text, so the delete above
    // does not shrink the index. Rebuilding is what actually returns the space,
    // and doing it now means the size reported afterwards is the size on disk.
    connection.execute("INSERT INTO verses_fts(verses_fts) VALUES('rebuild')", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn open() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        prepare(&connection).unwrap();
        connection
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A pack on disk, as a download would have left one.
    fn pack_file(directory: &Path, id: &str, verses: &[(i64, &str, i64, i64, &str)]) -> PathBuf {
        use std::io::Write;
        let path = directory.join(format!("{id}.tsv.gz"));
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        for (number, book, chapter, verse, text) in verses {
            writeln!(encoder, "{number}\t{book}\t{chapter}\t{verse}\t{text}").unwrap();
        }
        encoder.finish().unwrap();
        path
    }

    fn pack(id: &str, verse_count: i64) -> Pack {
        Pack {
            id: id.into(),
            name: format!("{id} Version"),
            year: 1611,
            file: format!("{id}.tsv.gz"),
            verse_count,
            checksum: format!("{id}-checksum"),
            sha256: String::new(),
            bytes: 0,
            language: "English".into(),
            bundled: true,
            note: None,
            missing_verses: 0,
            licence: None,
            attribution: None,
        }
    }

    fn installed(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT id FROM translations ORDER BY id")
            .unwrap();
        let rows = statement.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    /// The verses land, and the search index that was rebuilt can find them.
    ///
    /// Searched rather than counted: an import that fills `verses` and leaves
    /// the index empty is one where every search comes back empty and nothing
    /// says why.
    #[test]
    fn installs_a_pack_and_indexes_it() {
        let dir = scratch("install");
        let mut connection = open();
        let file = pack_file(&dir, "AAA", &[(43, "John", 3, 16, "For God so loved the world")]);

        assert_eq!(install(&mut connection, &pack("AAA", 1), &file).unwrap(), 1);
        assert_eq!(installed(&connection), ["AAA"]);

        let hits: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM verses_fts WHERE verses_fts MATCH '\"loved the world\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    /// A licence follows its translation into the library, and is read back
    /// from what is installed rather than from what happens to be bundled.
    ///
    /// That distinction is the whole point: the moment a CC BY-SA text is
    /// downloaded instead of shipped, a manifest lookup returns nothing and we
    /// quietly stop naming somebody we are obliged to name.
    #[test]
    fn an_attribution_survives_being_downloaded_rather_than_bundled() {
        let dir = scratch("licence");
        let mut connection = open();
        let file = pack_file(&dir, "FBV", &[(1, "Genesis", 1, 1, "In the beginning")]);

        let mut entry = pack("FBV", 1);
        entry.licence = Some("CC BY-SA 4.0".into());
        entry.attribution = Some("Free Bible Version © Dr. Jonathan Gallagher".into());
        install(&mut connection, &entry, &file).unwrap();

        assert_eq!(
            attributions(&connection).unwrap(),
            ["Free Bible Version © Dr. Jonathan Gallagher — CC BY-SA 4.0"]
        );
    }

    /// One line covers five Biblica translations, not five identical lines.
    #[test]
    fn attributions_are_deduplicated() {
        let dir = scratch("dedupe");
        let mut connection = open();
        for id in ["YOR", "IBO", "HAU"] {
            let file = pack_file(&dir, id, &[(1, "Genesis", 1, 1, "In the beginning")]);
            let mut entry = pack(id, 1);
            entry.licence = Some("CC BY-SA 4.0".into());
            entry.attribution = Some("Biblica® open edition © Biblica, Inc.".into());
            install(&mut connection, &entry, &file).unwrap();
        }
        assert_eq!(attributions(&connection).unwrap().len(), 1);
    }

    /// A translation installed before there were columns for this gets them
    /// filled in, and one that already has a licence is left alone.
    #[test]
    fn backfills_a_licence_recorded_before_there_was_anywhere_to_put_it() {
        let dir = scratch("backfill");
        let mut connection = open();
        let file = pack_file(&dir, "FBV", &[(1, "Genesis", 1, 1, "In the beginning")]);
        install(&mut connection, &pack("FBV", 1), &file).unwrap();
        assert!(attributions(&connection).unwrap().is_empty());

        let mut entry = pack("FBV", 1);
        entry.licence = Some("CC BY-SA 4.0".into());
        entry.attribution = Some("Free Bible Version © Dr. Jonathan Gallagher".into());
        backfill_licence(&connection, &entry).unwrap();

        assert_eq!(attributions(&connection).unwrap().len(), 1);
    }

    /// A truncated pack imports nothing rather than a Bible with holes in it.
    #[test]
    fn refuses_a_pack_that_does_not_match_its_count() {
        let dir = scratch("short");
        let mut connection = open();
        let file = pack_file(&dir, "BBB", &[(1, "Genesis", 1, 1, "In the beginning")]);

        assert!(install(&mut connection, &pack("BBB", 31102), &file).is_err());
        assert!(installed(&connection).is_empty());
    }

    /// Removing takes only its own translation, and its own index rows with it.
    #[test]
    fn removes_one_translation_and_leaves_the_rest() {
        let dir = scratch("remove");
        let mut connection = open();
        let first = pack_file(&dir, "AAA", &[(43, "John", 3, 16, "For God so loved the world")]);
        let second = pack_file(&dir, "BBB", &[(43, "John", 3, 16, "God loved the world so")]);
        install(&mut connection, &pack("AAA", 1), &first).unwrap();
        install(&mut connection, &pack("BBB", 1), &second).unwrap();

        remove(&mut connection, "AAA").unwrap();

        assert_eq!(installed(&connection), ["BBB"]);
        let left: i64 = connection
            .query_row("SELECT COUNT(*) FROM verses WHERE translation = 'AAA'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(left, 0);
    }

    /// The last one stays. An application for putting scripture on a screen
    /// should not offer a control that leaves it with none.
    #[test]
    fn will_not_remove_the_only_translation() {
        let dir = scratch("last");
        let mut connection = open();
        let file = pack_file(&dir, "AAA", &[(1, "Genesis", 1, 1, "In the beginning")]);
        install(&mut connection, &pack("AAA", 1), &file).unwrap();

        assert!(remove(&mut connection, "AAA").is_err());
        assert_eq!(installed(&connection), ["AAA"]);
    }

    /// A removal outlives the launch that made it, and a reinstall forgets it.
    #[test]
    fn a_removal_is_remembered() {
        let dir = scratch("declined");
        let mut connection = open();
        let first = pack_file(&dir, "AAA", &[(1, "Genesis", 1, 1, "In the beginning")]);
        let second = pack_file(&dir, "BBB", &[(1, "Genesis", 1, 1, "In the beginning")]);
        install(&mut connection, &pack("AAA", 1), &first).unwrap();
        install(&mut connection, &pack("BBB", 1), &second).unwrap();

        remove(&mut connection, "AAA").unwrap();
        assert_eq!(declined(&connection).unwrap(), ["AAA"]);

        install(&mut connection, &pack("AAA", 1), &first).unwrap();
        assert!(declined(&connection).unwrap().is_empty());
    }

    /// A reimport of the same text is recognised rather than repeated, and a
    /// corrected one is not.
    #[test]
    fn notices_whether_the_installed_copy_is_current() {
        let dir = scratch("current");
        let mut connection = open();
        let file = pack_file(&dir, "AAA", &[(1, "Genesis", 1, 1, "In the beginning")]);
        let entry = pack("AAA", 1);
        install(&mut connection, &entry, &file).unwrap();

        assert!(is_current(&connection, &entry).unwrap());

        let mut corrected = entry.clone();
        corrected.checksum = "different".into();
        assert!(!is_current(&connection, &corrected).unwrap());
    }
}
