//! What was said, kept.
//!
//! Until now the transcript lived in the window and died with it. An operator
//! who closed Pulpitry after a service had nothing: no record of what was
//! preached, no way to find the week someone asked about, nothing to hand the
//! person who missed it.
//!
//! So every listening session is written down as it happens. Not on request --
//! on request is a thing people remember afterwards, which is exactly when it
//! is too late.
//!
//! # What a session is
//!
//! One press of Start Listening to the next Stop. That is usually a service,
//! sometimes a rehearsal, and occasionally two minutes of testing the
//! microphone. Short ones are still kept: deciding for the operator which of
//! their sessions mattered is not this module's business, and a list is easier
//! to delete from than to reconstruct.
//!
//! # Where it lives
//!
//! The same database as the library, in its own tables. Full-text indexed,
//! because the question is almost always "which week was that?" rather than
//! "what did we do on the 14th".

use std::path::Path;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS transcript_sessions (
    id          INTEGER PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    -- Written by the operator, or by the model when the speaker never gave one.
    title       TEXT    NOT NULL DEFAULT '',
    -- Markdown. It is what the model writes, what the app renders, and the one
    -- form every export is converted from -- so there is a single thing to get
    -- right rather than one per output format.
    summary     TEXT    NOT NULL DEFAULT '',
    -- A JSON array. Kept as text because it is only ever read whole.
    topics      TEXT    NOT NULL DEFAULT '[]',
    word_count  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS transcript_lines (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL,
    -- Milliseconds from the start of the session, so a line can be found again
    -- in a recording of the same service.
    at_ms      INTEGER NOT NULL,
    text       TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_transcript_lines
    ON transcript_lines(session_id, at_ms);

CREATE INDEX IF NOT EXISTS idx_transcript_sessions
    ON transcript_sessions(started_at DESC);

-- "Which week was that?" is the question people actually ask, and they ask it
-- with half a remembered sentence rather than a date.
CREATE VIRTUAL TABLE IF NOT EXISTS transcript_fts USING fts5(
    text,
    tokenize='porter unicode61'
);
"#;

/// One listening session, without its lines.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: i64,
    pub started_at: i64,
    /// Absent while it is still being recorded.
    pub ended_at: Option<i64>,
    pub title: String,
    /// Markdown.
    pub summary: String,
    pub topics: Vec<String>,
    pub word_count: i64,
}

impl Session {
    /// A placeholder carrying nothing but an id.
    ///
    /// For the reply to a summary that failed: the host has to be told which
    /// service stopped, and inventing a title or a word count to fill the shape
    /// would put numbers on screen that were never true.
    pub fn empty(id: i64) -> Self {
        Self {
            id,
            started_at: 0,
            ended_at: None,
            title: String::new(),
            summary: String::new(),
            topics: Vec::new(),
            word_count: 0,
        }
    }
}

/// One settled utterance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub at_ms: i64,
    pub text: String,
}

pub struct Transcripts {
    connection: Mutex<Connection>,
    /// The session being written to, if any.
    open: Mutex<Option<Open>>,
}

struct Open {
    id: i64,
    started_at: i64,
}

impl Transcripts {
    pub fn open(directory: &Path) -> Result<Self> {
        let connection = Connection::open(directory.join("pulpitry.db"))
            .context("could not open the transcript store")?;
        connection.execute_batch(SCHEMA).context("could not prepare the transcript store")?;
        Ok(Self { connection: Mutex::new(connection), open: Mutex::new(None) })
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection: Mutex::new(connection), open: Mutex::new(None) })
    }

    /// Opens a session. Returns its id.
    ///
    /// An already-open session is closed first: whatever left it open -- a
    /// crash, a session that ended without being told -- its words are still
    /// worth keeping, and stitching them onto the next service would be worse.
    pub fn begin(&self) -> Result<i64> {
        self.end()?;

        let started_at = now_millis();
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO transcript_sessions(started_at) VALUES (?1)",
            params![started_at],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);

        *self.open.lock() = Some(Open { id, started_at });
        Ok(id)
    }

    /// Appends one settled utterance to the open session.
    ///
    /// Silent when nothing is open, because speech can settle a moment after
    /// the operator pressed Stop and that is not an error worth reporting.
    pub fn append(&self, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }

        let (id, at_ms) = {
            let open = self.open.lock();
            let Some(open) = open.as_ref() else { return Ok(()) };
            (open.id, now_millis() - open.started_at)
        };

        let words = text.split_whitespace().count() as i64;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO transcript_lines(session_id, at_ms, text) VALUES (?1, ?2, ?3)",
            params![id, at_ms, text],
        )?;
        let row = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO transcript_fts(rowid, text) VALUES (?1, ?2)",
            params![row, text],
        )?;
        // Kept as a running total rather than counted on read: a two-hour
        // service is thousands of rows, and the list shows this for every one.
        transaction.execute(
            "UPDATE transcript_sessions SET word_count = word_count + ?2 WHERE id = ?1",
            params![id, words],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Adds a service from text that was written somewhere else.
    ///
    /// For a transcript that exists as a file: one recorded before this was
    /// installed, one taken down by hand, or -- the reason it was written -- a
    /// service whose summary needs testing without waiting four hours to speak
    /// one. It becomes an ordinary past service, and everything that works on
    /// those works on it.
    ///
    /// Deliberately not `begin`/`append`/`end`: those drive the *open* session,
    /// and importing a file while a service is being recorded would close the
    /// live one and take the operator's words with it. This touches the tables
    /// and never the open handle, so it is safe mid-service.
    ///
    /// Timings are made up, and made up transparently. A file does not say when
    /// its words were spoken, so lines are spaced at a steady reading pace --
    /// enough for the transcript to scroll sensibly and for the service to have
    /// a plausible length, and no more truthful than that.
    pub fn import(&self, title: &str, text: &str) -> Result<i64> {
        let lines = split(text);
        if lines.is_empty() {
            anyhow::bail!("there are no words in that file");
        }

        let words: usize = lines.iter().map(|line| line.split_whitespace().count()).sum();
        let started_at = now_millis();

        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO transcript_sessions(started_at, ended_at, title, word_count) \
             VALUES (?1, ?2, ?3, ?4)",
            params![started_at, started_at + duration_ms(words), title.trim(), words as i64],
        )?;
        let id = transaction.last_insert_rowid();

        let mut so_far = 0usize;
        for line in &lines {
            transaction.execute(
                "INSERT INTO transcript_lines(session_id, at_ms, text) VALUES (?1, ?2, ?3)",
                params![id, duration_ms(so_far), line],
            )?;
            let row = transaction.last_insert_rowid();
            transaction.execute(
                "INSERT INTO transcript_fts(rowid, text) VALUES (?1, ?2)",
                params![row, line],
            )?;
            so_far += line.split_whitespace().count();
        }
        transaction.commit()?;
        Ok(id)
    }

    /// Closes the open session, if there is one.
    pub fn end(&self) -> Result<()> {
        let Some(open) = self.open.lock().take() else { return Ok(()) };
        let connection = self.connection.lock();
        connection.execute(
            "UPDATE transcript_sessions SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
            params![open.id, now_millis()],
        )?;
        Ok(())
    }

    /// Sessions, most recent first.
    pub fn list(&self, limit: i64) -> Result<Vec<Session>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, started_at, ended_at, title, summary, topics, word_count
             FROM transcript_sessions
             -- id breaks the tie: two sessions can begin in the same
             -- millisecond, and without it their order is undefined, so the
             -- same list comes back differently on consecutive reads.
             ORDER BY started_at DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| Ok(read_session(row)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Session>> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT id, started_at, ended_at, title, summary, topics, word_count
                 FROM transcript_sessions WHERE id = ?1",
                params![id],
                |row| Ok(read_session(row)),
            )
            .optional()?)
    }

    pub fn lines(&self, id: i64) -> Result<Vec<Line>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT at_ms, text FROM transcript_lines WHERE session_id = ?1 ORDER BY at_ms",
        )?;
        let rows = statement
            .query_map(params![id], |row| Ok(Line { at_ms: row.get(0)?, text: row.get(1)? }))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sessions containing a phrase, most recent first.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<Session>> {
        let query = query.trim();
        if query.is_empty() {
            return self.list(limit);
        }

        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT DISTINCT s.id, s.started_at, s.ended_at, s.title, s.summary, s.topics,
                    s.word_count
             FROM transcript_fts f
             JOIN transcript_lines l ON l.id = f.rowid
             JOIN transcript_sessions s ON s.id = l.session_id
             WHERE transcript_fts MATCH ?1
             ORDER BY s.started_at DESC, s.id DESC
             LIMIT ?2",
        )?;
        // Quoted, so an operator typing an apostrophe or a colon gets a search
        // rather than a syntax error out of FTS5.
        let phrase = format!("\"{}\"", query.replace('"', ""));
        let rows = statement.query_map(params![phrase, limit], |row| Ok(read_session(row)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Stores what the model made of a session.
    pub fn describe(&self, id: i64, title: &str, summary: &str, topics: &[String]) -> Result<()> {
        let connection = self.connection.lock();
        connection.execute(
            "UPDATE transcript_sessions SET title = ?2, summary = ?3, topics = ?4 WHERE id = ?1",
            params![id, title, summary, serde_json::to_string(topics)?],
        )?;
        Ok(())
    }

    /// Renames a session. The operator's own words win over the model's.
    pub fn retitle(&self, id: i64, title: &str) -> Result<()> {
        let connection = self.connection.lock();
        connection.execute(
            "UPDATE transcript_sessions SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        Ok(())
    }

    pub fn remove(&self, id: i64) -> Result<()> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        // The index is keyed on line ids, so it has to go first or it keeps
        // matching rows that no longer exist.
        transaction.execute(
            "DELETE FROM transcript_fts WHERE rowid IN
                 (SELECT id FROM transcript_lines WHERE session_id = ?1)",
            params![id],
        )?;
        transaction.execute("DELETE FROM transcript_lines WHERE session_id = ?1", params![id])?;
        transaction.execute("DELETE FROM transcript_sessions WHERE id = ?1", params![id])?;
        transaction.commit()?;
        Ok(())
    }
}

fn read_session(row: &rusqlite::Row<'_>) -> Session {
    let topics: String = row.get(5).unwrap_or_else(|_| "[]".into());
    Session {
        id: row.get(0).unwrap_or_default(),
        started_at: row.get(1).unwrap_or_default(),
        ended_at: row.get(2).ok().flatten(),
        title: row.get(3).unwrap_or_default(),
        summary: row.get(4).unwrap_or_default(),
        topics: serde_json::from_str(&topics).unwrap_or_default(),
        word_count: row.get(6).unwrap_or_default(),
    }
}

/// Words at a steady pace, as milliseconds.
///
/// 150 a minute is unhurried speech. It is a guess and only ever a guess: an
/// imported file carries no timings at all, and the alternative -- every line
/// at zero -- makes a four-hour service look instantaneous in the list.
fn duration_ms(words: usize) -> i64 {
    (words as i64) * 60_000 / 150
}

/// A blob of text as utterances.
///
/// Blank-line-separated paragraphs first, because that is how a transcript
/// people have edited tends to be laid out. A paragraph longer than a breath is
/// then broken at sentence ends, so that one unbroken wall of text does not
/// become a single line thousands of words long that no view can render and no
/// search can usefully match.
fn split(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in text.split(|c| c == '\n' || c == '\r') {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if block.split_whitespace().count() <= LINE_WORDS {
            out.push(block.to_string());
            continue;
        }

        let mut current = String::new();
        for word in block.split_whitespace() {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
            let ends_sentence = word.ends_with('.') || word.ends_with('?') || word.ends_with('!');
            if ends_sentence && current.split_whitespace().count() >= LINE_WORDS / 2 {
                out.push(std::mem::take(&mut current));
            } else if current.split_whitespace().count() >= LINE_WORDS * 2 {
                // No sentence end in sight; break anyway rather than grow
                // without limit.
                out.push(std::mem::take(&mut current));
            }
        }
        if !current.trim().is_empty() {
            out.push(current);
        }
    }
    out
}

/// About the length of a settled utterance from the recogniser.
const LINE_WORDS: usize = 40;

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_was_said() {
        let store = Transcripts::in_memory().unwrap();
        let id = store.begin().unwrap();
        store.append("For God so loved the world").unwrap();
        store.append("that he gave his only begotten Son").unwrap();
        store.end().unwrap();

        let lines = store.lines(id).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "For God so loved the world");

        let session = store.get(id).unwrap().expect("session");
        // Six words then seven.
        assert_eq!(session.word_count, 13);
        assert!(session.ended_at.is_some(), "a closed session records when it ended");
    }

    #[test]
    fn words_spoken_after_stop_are_not_kept() {
        // Speech settles a moment after the operator presses Stop. Those words
        // belong to no session and must not open one.
        let store = Transcripts::in_memory().unwrap();
        let id = store.begin().unwrap();
        store.end().unwrap();
        store.append("a stray half sentence").unwrap();
        assert_eq!(store.lines(id).unwrap().len(), 0);
        assert_eq!(store.list(10).unwrap().len(), 1, "and no second session appeared");
    }

    #[test]
    fn a_session_left_open_is_closed_by_the_next_one() {
        // What a crash leaves behind. Its words are still worth keeping, and
        // stitching them onto next week's service would be worse than either.
        let store = Transcripts::in_memory().unwrap();
        let first = store.begin().unwrap();
        store.append("last week").unwrap();

        let second = store.begin().unwrap();
        store.append("this week").unwrap();

        assert_ne!(first, second);
        assert_eq!(store.lines(first).unwrap().len(), 1);
        assert_eq!(store.lines(second).unwrap().len(), 1);
        assert!(store.get(first).unwrap().unwrap().ended_at.is_some());
    }

    #[test]
    fn finds_the_week_by_half_a_sentence() {
        let store = Transcripts::in_memory().unwrap();
        let a = store.begin().unwrap();
        store.append("the prodigal son came to himself").unwrap();
        store.end().unwrap();
        let b = store.begin().unwrap();
        store.append("a certain man had two sons").unwrap();
        store.end().unwrap();

        let found = store.search("prodigal", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, a);

        // Stemming, so "sons" finds "son" and the operator is not made to
        // remember which of the two the speaker used. Both sessions match here
        // -- the newest comes first.
        assert_eq!(store.search("sons", 10).unwrap()[0].id, b);
    }

    #[test]
    fn punctuation_in_a_search_does_not_break_it() {
        let store = Transcripts::in_memory().unwrap();
        store.begin().unwrap();
        store.append("he said: it is finished").unwrap();
        store.end().unwrap();
        // Bare, this is FTS5 syntax and would be an error rather than a search.
        assert_eq!(store.search("said: it is", 10).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_session_takes_its_words_out_of_the_index() {
        let store = Transcripts::in_memory().unwrap();
        let id = store.begin().unwrap();
        store.append("something regrettable").unwrap();
        store.end().unwrap();

        store.remove(id).unwrap();
        assert!(store.search("regrettable", 10).unwrap().is_empty());
        assert!(store.get(id).unwrap().is_none());
    }

    #[test]
    fn a_title_the_operator_typed_survives() {
        let store = Transcripts::in_memory().unwrap();
        let id = store.begin().unwrap();
        store.end().unwrap();

        store.describe(id, "The Prodigal Son", "A summary.", &["grace".into(), "return".into()]).unwrap();
        store.retitle(id, "Coming Home").unwrap();

        let session = store.get(id).unwrap().unwrap();
        assert_eq!(session.title, "Coming Home");
        assert_eq!(session.summary, "A summary.", "retitling must not clear the summary");
        assert_eq!(session.topics, vec!["grace", "return"]);
    }

    #[test]
    fn an_imported_file_becomes_a_service_with_every_word_kept() {
        let store = Transcripts::in_memory().unwrap();
        let text = "First line of the service.\n\nSecond line, after a blank.\nThird line.";
        let id = store.import("Sunday teaching", text).unwrap();

        let session = store.get(id).unwrap().expect("the service is in the list");
        assert_eq!(session.title, "Sunday teaching");
        assert_eq!(session.word_count, 12, "the count must match what was stored");
        assert!(session.ended_at.is_some(), "an imported service is not still recording");

        let lines = store.lines(id).unwrap();
        assert_eq!(lines.len(), 3, "blank lines are separators, not utterances");
        let joined = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
        for word in ["First", "Second", "Third", "blank."] {
            assert!(joined.contains(word), "{word} was lost on the way in");
        }
    }

    #[test]
    fn importing_does_not_disturb_a_service_being_recorded() {
        let store = Transcripts::in_memory().unwrap();
        let live = store.begin().unwrap();
        store.append("spoken while the file was imported").unwrap();

        let imported = store.import("From a file", "some words in a file").unwrap();
        assert_ne!(imported, live);

        // The live one is still the one being written to.
        store.append("still recording").unwrap();
        let lines = store.lines(live).unwrap();
        assert_eq!(lines.len(), 2, "the import stole the open session");
    }

    #[test]
    fn one_unbroken_wall_of_text_is_broken_into_utterances() {
        let store = Transcripts::in_memory().unwrap();
        let wall = (0..500).map(|i| format!("word{i}.")).collect::<Vec<_>>().join(" ");
        let id = store.import("Wall", &wall).unwrap();

        let lines = store.lines(id).unwrap();
        assert!(lines.len() > 1, "a 500-word paragraph became a single line");
        assert!(
            lines.iter().all(|l| l.text.split_whitespace().count() <= LINE_WORDS * 2),
            "a line grew without limit"
        );
        let total: usize = lines.iter().map(|l| l.text.split_whitespace().count()).sum();
        assert_eq!(total, 500, "words were lost in the breaking");
    }

    #[test]
    fn a_file_with_no_words_is_refused_rather_than_stored() {
        let store = Transcripts::in_memory().unwrap();
        assert!(store.import("Empty", "   \n\n  ").is_err());
    }

    #[test]
    fn imported_lines_advance_in_time_so_the_service_has_a_length() {
        let store = Transcripts::in_memory().unwrap();
        let id = store.import("Paced", "one two three four five.\nsix seven eight nine ten.").unwrap();
        let lines = store.lines(id).unwrap();
        assert_eq!(lines[0].at_ms, 0);
        assert!(lines[1].at_ms > 0, "every line claimed to be spoken at once");
    }
}
