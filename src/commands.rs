//! Things an operator can say instead of reaching for the laptop.
//!
//! # Why this is careful rather than clever
//!
//! An operator during a service is standing at the back of a hall with a
//! running order in their hands, and every action costs them looking down. The
//! arrow keys were the cheap answer. Speaking is the other one — and it is the
//! one that can go wrong in front of a congregation.
//!
//! The transcript is already arriving word by word and detection already reads
//! it. A command is the same stream read for a different thing. That makes the
//! feature nearly free to add and dangerous in exactly one way: **a preacher
//! saying "and the next verse says" is not asking for anything**, and a screen
//! that moves because of it is worse than one that never listened.
//!
//! Three guards, and none of them is sufficient alone:
//!
//! 1. **It has to be addressed to the machine.** A wake word does that, and is
//!    the strongest form of it. Without one, the instruction has to be the
//!    whole thing said rather than words inside a sentence — see below.
//! 2. **Only the tail.** A command is acted on within a breath of being said or
//!    not at all. The same phrase found in a paragraph that settled thirty
//!    seconds ago is not a command, it is a coincidence.
//! 3. **Never twice for one utterance.** A live transcript is revised as it
//!    settles, so the same sentence arrives several times. A command that fires
//!    per arrival advances three verses on one instruction — and one that
//!    cannot tell a revision from a repeat refuses the second of two verses an
//!    operator genuinely asked for, which is the more common failure of the
//!    two. The engine already knows the difference: a *final* segment is an
//!    utterance that has ended, so that is where the memory is cleared.
//!
//! # Two ways in, not one
//!
//! Saying the product's name before every instruction is a real cost during a
//! service, so it is a way in rather than a toll. Both work, always:
//!
//! - **After the wake word**, anything may follow. Nobody says the wake word by
//!   accident, so having been addressed the machine can afford to be generous.
//! - **Standing alone**, the words have to be the *entire* utterance. An
//!   instruction buried in a sentence is not one.
//!
//! A church that sets a wake word keeps both. Setting one is not a promise to
//! use it every time; it is a second, stronger way to be heard over a room.
//!
//! That distinction does most of the work. "Next verse" said on its own is an
//! operator; "and the next verse says something remarkable" is a preacher, and
//! so is "let us go back to what Paul wrote" and "switch to the King James for
//! a moment". A small number of ordinary words are ignored either side — "the",
//! "please", "give me" — so "give me the King James" still works while "the
//! King James is clearer here" does not.
//!
//! Standing alone is weaker than being addressed, and honestly so: a preacher
//! who pauses and says only "next verse" will move the screen. The wake word is
//! there for a church that wants the stronger guarantee on a phrase they expect
//! to hear from the pulpit.
//!
//! # What it deliberately cannot do
//!
//! Only the actions that already exist, chosen from a list. A church saying
//! "shema" for *next verse* is the whole value; a scripting language is not.

use serde::{Deserialize, Serialize};

/// How far back a command may be found, in words.
///
/// Long enough for "Castavox, give me the King James Version" and no longer.
/// The point is that a phrase which has scrolled past is not an instruction.
const TAIL: usize = 12;

/// What an operator asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum Command {
    NextVerse,
    PreviousVerse,
    /// Blank the screen. Never the reverse: nothing said in a room should be
    /// able to *put* something in front of a congregation unprompted.
    Clear,
    /// Read from another translation, named however it was said.
    Switch { translation: String },

    /*
     * Moving about the book being read.
     *
     * A verse at a time is what an operator does most, and a chapter at a time
     * is what they do when the reading moves on -- "turn with me to chapter
     * four" is said aloud in most services, and until now the only answer was
     * to reach for the laptop.
     *
     * The book is never part of these. Saying a book name aloud is what
     * scripture *detection* already listens for, and it puts the passage in the
     * detected list; a command that also jumped the browser would be two things
     * happening for one sentence. These move within what is already open.
     */
    /**
     * Somewhere else in the scripture: a chapter, a verse, or a whole
     * reference.
     *
     * One shape rather than four, because every way of saying it is the same
     * request with different parts left out. "Next chapter" is a relative move
     * with no verse; "chapter four verse nine" is an absolute one with both;
     * "John nine eight" adds the book. Four commands meant four dispatches in
     * each product and four chances for one of them to be wrong.
     *
     * `book` is the library's own spelling, resolved here, so the products
     * never have to match a spoken name themselves.
     */
    Passage {
        /// Named only when the operator said one; otherwise the book being read.
        book: Option<String>,
        /// An absolute chapter, when one was named.
        chapter: Option<i64>,
        /// A relative move: 1 for "next chapter", -1 for "previous".
        by: i64,
        /// Absent means the top of the chapter.
        verse: Option<i64>,
    }
}

/// A phrase a church has taught the application, pointed at an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phrase {
    /// What somebody says, in their own words.
    pub said: String,
    pub does: Command,
}

/// The phrases we ship, before a church adds its own.
///
/// Ordered longest first where two overlap, because "previous verse" contains
/// "verse" and the longer reading is the one that was meant.
const BUILT_IN: &[(&str, Command)] = &[
    ("previous verse", Command::PreviousVerse),
    ("verse back", Command::PreviousVerse),
    ("go back", Command::PreviousVerse),
    ("next verse", Command::NextVerse),
    ("next one", Command::NextVerse),
    ("clear the screen", Command::Clear),
    ("clear the display", Command::Clear),
    ("blank the screen", Command::Clear),
    ("clear screen", Command::Clear),
];

/// Everything needed to read the stream for commands.
#[derive(Debug, Clone)]
pub struct Listener {
    /// Nothing is a command unless the phrase begins with this.
    wake: String,
    /// Translations by every name a person might say them by.
    named: Vec<(String, String)>,
    /// Book names as spoken, paired with the library's own spelling. Longest
    /// first, so "song of solomon" is not read as a song and "1 john" beats
    /// "john".
    books: Vec<(String, String)>,
    custom: Vec<Phrase>,
    /// The last thing acted on, so one instruction is obeyed once.
    last: Option<String>,
}

/// Folded to lower-case words with everything else dropped, as the reference
/// matcher does it. "Castavox, next verse!" and "castavox next verse" are the
/// same instruction and a speech engine will punctuate them differently.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|token| {
            token
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/*
 * Words that carry no instruction, dropped when there is no wake word.
 *
 * Small on purpose. Every word here is one that can no longer distinguish a
 * command from a sentence containing one, so the list is the ordinary
 * scaffolding of asking -- articles, politeness, and the verbs people put in
 * front of a translation's name.
 *
 * "and" is deliberately absent, and it is the most important absence: it is the
 * word that marks speech carrying on around a phrase. Keeping it is what stops
 * "and the next verse says" from being heard as "next verse".
 */
const IGNORABLE: &[&str] = &[
    "a", "an", "the", "please", "ok", "okay", "now", "just", "to", "me", "us", "give", "switch",
    "read", "from", "let", "lets", "put", "up",
];

/// The words that actually ask for something, both ends trimmed.
fn strip(tail: &[String]) -> String {
    tail.iter()
        .filter(|word| !IGNORABLE.contains(&word.as_str()))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The ways a book's name is actually said.
///
/// "1 John" is "first john" as often as "one john", and a recogniser writes
/// whichever it heard. Everything else is itself.
fn spoken_forms(book: &str) -> Vec<String> {
    let folded = words(book).join(" ");
    let mut forms = vec![folded.clone()];

    for (digit, spoken) in [("1", ["first", "one"]), ("2", ["second", "two"]), ("3", ["third", "three"])]
    {
        if let Some(rest) = folded.strip_prefix(&format!("{digit} ")) {
            for word in spoken {
                forms.push(format!("{word} {rest}"));
            }
        }
    }
    forms
}

/// Whether `haystack` contains `needle` as whole words.
///
/// Both are already folded to single-spaced lower-case words, so this is a
/// substring test with the edges checked. Without the edges, a church whose
/// wake word is "bee" is woken by "beef", and the NIV is selected by somebody
/// saying "univ".
fn contains_words(haystack: &str, needle: &str) -> bool {
    haystack
        .match_indices(needle)
        .any(|(at, _)| {
            let before = at == 0 || haystack.as_bytes()[at - 1] == b' ';
            let end = at + needle.len();
            let after = end == haystack.len() || haystack.as_bytes()[end] == b' ';
            before && after
        })
}

impl Listener {
    /// `translations` is `(id, name)` — "ESV", "English Standard Version".
    pub fn new(
        wake: &str,
        translations: &[(String, String)],
        books: &[String],
        custom: &[Phrase],
    ) -> Self {
        let mut named = Vec::new();
        for (id, name) in translations {
            // The code, said as a word: "switch to ESV".
            named.push((words(id).join(" "), id.clone()));
            // The full name: "the English Standard Version".
            named.push((words(name).join(" "), id.clone()));
            // And without the trailing "version", because almost nobody says
            // it: "give me the King James".
            let short = words(name);
            if short.len() > 1 && short.last().is_some_and(|w| w == "version") {
                named.push((short[..short.len() - 1].join(" "), id.clone()));
            }
        }
        // Longest first, so "new king james" is not read as "king james".
        named.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        named.dedup_by(|a, b| a.0 == b.0);

        // Spoken form against the library's spelling: "1 john" is said as
        // "first john" as often as not, and the ordinal readings are how the
        // reference matcher already copes with that.
        let mut spoken: Vec<(String, String)> = Vec::new();
        for book in books {
            for form in spoken_forms(book) {
                spoken.push((form, book.clone()));
            }
        }
        spoken.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        spoken.dedup_by(|a, b| a.0 == b.0);

        Self {
            wake: words(wake).join(" "),
            named,
            books: spoken,
            custom: custom.to_vec(),
            last: None,
        }
    }

    /// Reads the tail of the transcript for an instruction.
    ///
    /// `ended` says whether this is a completed utterance rather than words the
    /// engine is still revising. It is what separates "the same sentence
    /// arriving again as it settles" from "the operator asked for a second
    /// verse" — which are identical as text and opposite as intentions.
    ///
    /// `None` is the answer for almost everything ever said in a church, which
    /// is the design working rather than failing.
    pub fn hear(&mut self, transcript: &str, ended: bool) -> Option<Command> {
        let spoken = words(transcript);
        let tail = &spoken[spoken.len().saturating_sub(TAIL)..];

        // Two ways in, and a church that sets a wake word keeps both. Naming
        // the machine is the stronger one -- anything may follow it, because
        // nobody says the wake word by accident -- and standing alone is the
        // one that costs nothing to use.
        let Some((command, phrase)) = self.after_wake(tail).or_else(|| self.standing_alone(tail))
        else {
            // The moment has passed. Forgetting here is what lets the same
            // instruction be given again later in the service.
            self.last = None;
            return None;
        };

        // Keyed on the phrase that matched, not on everything said after the
        // wake word. A live transcript keeps growing while it settles, so the
        // text after "castavox next verse" is different every time it arrives
        // and a key made from all of it would obey one instruction three or
        // four times -- which on stage is three or four verses.
        if self.last.as_deref() == Some(phrase.as_str()) {
            // The same utterance arriving again. If it has now ended, forget it
            // so the operator can ask for the same thing a second time -- which
            // for "next verse" is the most ordinary thing they will do.
            if ended {
                self.last = None;
            }
            return None;
        }

        // Remembered only while the utterance is still being revised. Once it
        // has ended there is nothing left to suppress.
        self.last = if ended { None } else { Some(phrase) };
        Some(command)
    }

    /// Forgets what was last obeyed, so the same words said again are heard.
    ///
    /// Called when a service starts or the transcript is cleared. Without it an
    /// operator who says "next verse" twice in a row, deliberately, gets one.
    pub fn forget(&mut self) {
        self.last = None;
    }

    /// An instruction following the wake word, if one was set and said.
    ///
    /// Anything may follow it, because nobody says the wake word by accident:
    /// having been addressed, the machine can afford to be generous about the
    /// words after it.
    fn after_wake(&self, tail: &[String]) -> Option<(Command, String)> {
        if self.wake.is_empty() {
            return None;
        }
        let wake: Vec<&str> = self.wake.split(' ').collect();
        // The *last* one: an operator who corrects themselves mid-sentence
        // meant the correction.
        let at = tail
            .windows(wake.len())
            .rposition(|window| window.iter().zip(&wake).all(|(a, b)| a == b))?;

        let rest = tail[at + wake.len()..].join(" ");
        if rest.is_empty() {
            return None;
        }
        self.match_phrase(&rest)
    }

    /// An instruction that is the whole of what was said.
    ///
    /// Available whether or not a wake word is set, so saying the machine's
    /// name is a way in rather than a toll. Ordinary words are dropped from
    /// both ends -- "give me the King James" is somebody asking; "the King
    /// James is clearer here" is somebody preaching, and only one of them is
    /// left as a bare translation name once those words are gone.
    fn standing_alone(&self, tail: &[String]) -> Option<(Command, String)> {
        let bare = strip(tail);
        if bare.is_empty() {
            return None;
        }
        self.match_exactly(&bare)
    }

    /// Matches only when the words are the whole instruction and nothing else.
    ///
    /// The test that replaces the wake word. `starts_with` would be no test at
    /// all here: every sentence a preacher says beginning "next verse..." would
    /// move the screen, which is exactly what the wake word existed to prevent.
    fn match_exactly(&self, said: &str) -> Option<(Command, String)> {
        for phrase in &self.custom {
            let want = strip(&words(&phrase.said));
            if !want.is_empty() && said == want {
                return Some((phrase.does.clone(), want));
            }
        }

        for (phrase, command) in BUILT_IN {
            if said == strip(&words(phrase)) {
                return Some((command.clone(), (*phrase).to_string()));
            }
        }

        // Standing alone, so the whole utterance has to be the instruction:
        // "chapter four" is an operator, "in chapter four Paul writes" is not.
        // Measured by words consumed, because the words asking and the words
        // returned as a key are deliberately not the same thing.
        // The whole utterance has to be the instruction. Nothing else is
        // needed to tell it from speech: "in chapter four Paul writes" has
        // words left over, and so does every sentence that merely mentions a
        // reference.
        let spoken: Vec<String> = said.split(' ').map(str::to_string).collect();
        if let Some((command, key, consumed)) = self.match_passage(&spoken) {
            if consumed == spoken.len() {
                return Some((command, key));
            }
        }

        for (name, id) in &self.named {
            if name.len() >= 3 && said == strip(&words(name)) {
                return Some((Command::Switch { translation: id.clone() }, name.clone()));
            }
        }
        None
    }

    /**
     * Everywhere in the scripture somebody might ask to be taken.
     *
     * "Next chapter", "next chapter verse nine", "chapter four", "chapter four
     * verse nine", "verse nine", "John nine eight", "John nine verse eight",
     * "John chapter nine verse eight".
     *
     * All one matcher because they are all the same request with different
     * parts named. Read in the order a person says them: the book, then the
     * chapter, then the verse.
     *
     * Returns how many words the instruction used, which is what tells
     * "chapter four" said on its own from the same words inside a sentence. A
     * number can be several words long, so it is counted rather than assumed.
     */
    fn match_passage(&self, words: &[String]) -> Option<(Command, String, usize)> {
        let mut at = 0;

        // The book, if one was named. Longest first, so "song of solomon" is
        // not read as a song, and "1 john" beats "john".
        let mut book = None;
        for name in &self.books {
            let spelling: Vec<&str> = name.0.split(' ').collect();
            if words.len() >= spelling.len()
                && words[..spelling.len()].iter().zip(&spelling).all(|(a, b)| a == b)
            {
                book = Some(name.1.clone());
                at = spelling.len();
                break;
            }
        }

        // A relative move. "Next chapter" and, after a book, nothing: "John
        // next chapter" is not something anybody says.
        let mut by = 0;
        if book.is_none() && at + 1 < words.len() && words[at + 1] == "chapter" {
            by = match words[at].as_str() {
                "next" => 1,
                "previous" | "last" => -1,
                _ => 0,
            };
            if by != 0 {
                at += 2;
            }
        }

        // "chapter" is optional once a book has been named: "John nine eight"
        // and "John chapter nine verse eight" are the same request.
        let mut chapter = None;
        if by == 0 {
            if words.get(at).is_some_and(|word| word == "chapter") {
                at += 1;
                let (value, used) = Self::number(words, at)?;
                chapter = Some(value);
                at = used;
            } else if book.is_some() {
                let (value, used) = Self::number(words, at)?;
                chapter = Some(value);
                at = used;
            }
        }

        // The verse, spoken with or without the word.
        let mut verse = None;
        if words.get(at).is_some_and(|word| word == "verse") {
            at += 1;
            let (value, used) = Self::number(words, at)?;
            verse = Some(value);
            at = used;
        } else if book.is_some() && at < words.len() {
            // "John nine eight": the second number is the verse.
            if let Some((value, used)) = Self::number(words, at) {
                verse = Some(value);
                at = used;
            }
        }

        // A book with no numbers is somebody saying a name, and "chapter" with
        // no number is somebody talking about one.
        if chapter.is_none() && verse.is_none() && by == 0 {
            return None;
        }

        // Keyed on what was asked for rather than on the words that asked, so
        // the same request phrased twice is one instruction and a different
        // chapter is a different one.
        let key = format!("passage {book:?} {chapter:?} {by} {verse:?}");
        Some((Command::Passage { book, chapter, by, verse }, key, at))
    }

    /// One number starting at `at`, and where its words ended.
    ///
    /// A number can be several words long -- "one hundred nineteen" -- and
    /// knowing where it stopped is what lets the caller tell a complete
    /// instruction from words inside a sentence.
    fn number(words: &[String], at: usize) -> Option<(i64, usize)> {
        let (value, used) = crate::numbers::read_first(words.get(at..)?)?;
        Some((value, at + used))
    }

    /// The command, and the phrase that matched — which is what identifies the
    /// utterance for as long as it is still being revised.
    fn match_phrase(&self, said: &str) -> Option<(Command, String)> {
        // A church's own words first. They added them because ours did not fit.
        for phrase in &self.custom {
            let want = words(&phrase.said).join(" ");
            if !want.is_empty() && said.starts_with(&want) {
                return Some((phrase.does.clone(), want));
            }
        }

        for (phrase, command) in BUILT_IN {
            if said.starts_with(phrase) {
                return Some((command.clone(), (*phrase).to_string()));
            }
        }

        let spoken: Vec<String> = said.split(' ').map(str::to_string).collect();
        if let Some((command, key, _)) = self.match_passage(&spoken) {
            return Some((command, key));
        }

        // "switch to the ESV", "give me the King James", "read from the NIV".
        // The translation has to be named; the verb around it does not matter,
        // because the name is the part nobody says by accident after having
        // just said the wake word.
        for (name, id) in &self.named {
            if name.len() >= 3 && contains_words(said, name) {
                return Some((
                    Command::Switch { translation: id.clone() },
                    name.clone(),
                ));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translations() -> Vec<(String, String)> {
        [
            ("KJV", "King James Version"),
            ("ESV", "English Standard Version"),
            ("NKJV", "New King James Version"),
            ("NIV", "New International Version"),
        ]
        .iter()
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect()
    }

    fn books() -> Vec<String> {
        ["Genesis", "John", "1 John", "Psalms", "Song of Solomon", "Romans", "Jude"]
            .iter()
            .map(|book| book.to_string())
            .collect()
    }

    fn listener() -> Listener {
        Listener::new("Castavox", &translations(), &books(), &[])
    }

    #[test]
    fn hears_the_plain_instructions() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next verse", false), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("Castavox, previous verse.", false), Some(Command::PreviousVerse));
        ear.forget();
        assert_eq!(ear.hear("castavox clear the screen", false), Some(Command::Clear));
    }

    /// The whole reason for the wake word. A preacher saying this is preaching.
    #[test]
    fn a_sermon_is_not_a_command() {
        let mut ear = listener();
        assert_eq!(ear.hear("and the next verse says something remarkable", false), None);
        assert_eq!(ear.hear("let us go back to what Paul wrote", false), None);
        assert_eq!(ear.hear("I want to clear the screen of my mind", false), None);
        assert_eq!(ear.hear("switch to the King James for a moment", false), None);
    }

    /// One instruction is obeyed once, however many times the transcript is
    /// revised. Without this a single "next verse" advances three.
    #[test]
    fn a_settling_transcript_does_not_repeat_the_command() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next", false), None);
        assert_eq!(ear.hear("castavox next verse", false), Some(Command::NextVerse));
        assert_eq!(ear.hear("castavox next verse", false), None);
        assert_eq!(ear.hear("castavox next verse.", false), None);
    }

    /// The transcript keeps growing while a sentence settles, so the words
    /// after the command are different every time it arrives. Keyed on all of
    /// them, one "next verse" would advance three or four.
    #[test]
    fn a_command_does_not_fire_again_as_more_is_said_after_it() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next verse", false), Some(Command::NextVerse));
        assert_eq!(ear.hear("castavox next verse and", false), None);
        assert_eq!(ear.hear("castavox next verse and then", false), None);
        assert_eq!(ear.hear("castavox next verse and then he said", false), None);
    }

    /// A wake word is a whole word. A church that chooses a short one should
    /// not be woken by every word that happens to start with it.
    #[test]
    fn the_wake_word_is_not_matched_inside_another_word() {
        let mut ear = Listener::new("bee", &translations(), &books(), &[]);
        assert_eq!(ear.hear("the beef next verse", false), None);
        assert_eq!(ear.hear("bee next verse", false), Some(Command::NextVerse));
    }

    /// Same for a translation name, which is matched anywhere after the wake
    /// word rather than only at the start.
    #[test]
    fn a_translation_is_not_matched_inside_another_word() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox tell the university about it", false), None);
    }

    /// Two verses in a row, which is the most ordinary thing an operator does.
    ///
    /// The words are identical to a transcript settling, and the difference is
    /// that the engine has told us the first utterance ended. Without that,
    /// the guard against one instruction advancing three verses becomes a
    /// guard against advancing two on purpose -- and refusing the second is
    /// the more common failure of the two.
    #[test]
    fn the_same_instruction_twice_is_two_instructions() {
        let mut ear = listener();
        assert_eq!(ear.hear("next verse", true), Some(Command::NextVerse));
        assert_eq!(ear.hear("next verse", true), Some(Command::NextVerse));
        assert_eq!(ear.hear("next verse", true), Some(Command::NextVerse));
    }

    /// And the same words while the engine is still revising them are still
    /// one instruction, which is what this whole guard exists for.
    #[test]
    fn a_settling_utterance_is_still_only_obeyed_once() {
        let mut ear = listener();
        assert_eq!(ear.hear("next", false), None);
        assert_eq!(ear.hear("next verse", false), Some(Command::NextVerse));
        assert_eq!(ear.hear("next verse", false), None);
        assert_eq!(ear.hear("next verse and", false), None);
        // The final of that same utterance: still nothing, and the memory of
        // it is dropped so the next one is heard.
        assert_eq!(ear.hear("next verse and then he said", true), None);
        assert_eq!(ear.hear("next verse", true), Some(Command::NextVerse));
    }

    #[test]
    fn hears_a_chapter_moved_by_one() {
        let mut ear = listener();
        assert_eq!(ear.hear("next chapter", false), Some(Command::Passage { book: None, chapter: None, by: 1, verse: None }));
        ear.forget();
        assert_eq!(ear.hear("previous chapter", false), Some(Command::Passage { book: None, chapter: None, by: -1, verse: None }));
        ear.forget();
        assert_eq!(ear.hear("castavox last chapter", false), Some(Command::Passage { book: None, chapter: None, by: -1, verse: None }));
    }

    /// "Chapter four", however the recogniser wrote the number.
    #[test]
    fn hears_a_chapter_by_number() {
        for said in ["chapter four", "chapter 4"] {
            let mut ear = listener();
            assert_eq!(ear.hear(said, false), Some(Command::Passage { book: None, chapter: Some(4), by: 0, verse: None }), "{said}");
        }
    }

    /// The number is read as it is spoken. "Chapter twenty eight" is one
    /// chapter, not chapter twenty followed by an eight.
    #[test]
    fn a_spoken_number_is_one_number() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("chapter twenty eight", false),
            Some(Command::Passage { book: None, chapter: Some(28), by: 0, verse: None })
        );
    }

    #[test]
    fn hears_a_chapter_and_a_verse() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("chapter four verse nine", false),
            Some(Command::Passage { book: None, chapter: Some(4), by: 0, verse: Some(9) })
        );
    }

    /// A verse on its own keeps the chapter that is open, which is what
    /// somebody reading through a passage means by it.
    #[test]
    fn hears_a_verse_on_its_own() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("verse sixteen", false),
            Some(Command::Passage { book: None, chapter: None, by: 0, verse: Some(16) })
        );
    }

    /// The wake word buys the usual latitude: anything may follow it.
    #[test]
    fn a_wake_word_still_takes_a_passage_mid_sentence() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("castavox chapter four verse nine please", false),
            Some(Command::Passage { book: None, chapter: Some(4), by: 0, verse: Some(9) })
        );
    }

    /// And without one, the sentence a preacher actually says moves nothing.
    /// This is the case the whole design exists for, and chapters are said
    /// aloud far more often than "next verse" ever is.
    #[test]
    fn a_chapter_mentioned_in_preaching_is_not_a_command() {
        let mut ear = listener();
        for said in [
            "in chapter four Paul writes about this",
            "if you look at the next chapter you will see",
            "we read chapter twenty eight last week",
            "the previous chapter ends with a question",
            "verse sixteen is the one everybody knows",
        ] {
            assert_eq!(ear.hear(said, false), None, "{said}");
        }
    }

    /// "Chapter" with no number is somebody talking about one.
    #[test]
    fn a_chapter_with_no_number_asks_for_nothing() {
        let mut ear = listener();
        assert_eq!(ear.hear("chapter", false), None);
        assert_eq!(ear.hear("castavox chapter", false), None);
    }

    /// Two different chapters asked for in a row are two instructions; the
    /// same one twice while the sentence settles is one.
    #[test]
    fn a_different_chapter_is_a_different_instruction() {
        let mut ear = listener();
        assert_eq!(ear.hear("chapter four", false), Some(Command::Passage { book: None, chapter: Some(4), by: 0, verse: None }));
        assert_eq!(ear.hear("chapter four", false), None);
        assert_eq!(ear.hear("chapter five", false), Some(Command::Passage { book: None, chapter: Some(5), by: 0, verse: None }));
    }

    /// The trailing words of a longer number belong to it. Without counting
    /// them, "chapter twenty eight" read as "chapter twenty" plus a stray word
    /// and stopped being a whole utterance.
    #[test]
    fn a_long_number_is_still_the_whole_instruction() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("chapter one hundred nineteen", false),
            Some(Command::Passage { book: None, chapter: Some(119), by: 0, verse: None })
        );
    }

    /// A chapter step that also names a verse.
    #[test]
    fn hears_a_chapter_step_with_a_verse() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("next chapter verse nine", false),
            Some(Command::Passage { book: None, chapter: None, by: 1, verse: Some(9) })
        );
        ear.forget();
        assert_eq!(
            ear.hear("previous chapter verse two", false),
            Some(Command::Passage { book: None, chapter: None, by: -1, verse: Some(2) })
        );
    }

    /// A reference as it is actually said from a pulpit, in each of its
    /// shapes. All three mean John 9:8.
    #[test]
    fn hears_a_reference_however_it_is_said() {
        for said in ["John nine eight", "John nine verse eight", "John chapter nine verse eight"] {
            let mut ear = listener();
            assert_eq!(
                ear.hear(said, false),
                Some(Command::Passage {
                    book: Some("John".into()),
                    chapter: Some(9),
                    by: 0,
                    verse: Some(8),
                }),
                "{said}"
            );
        }
    }

    /// A book and a chapter with no verse is the top of that chapter.
    #[test]
    fn hears_a_book_and_chapter() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("Romans eight", false),
            Some(Command::Passage {
                book: Some("Romans".into()),
                chapter: Some(8),
                by: 0,
                verse: None,
            })
        );
    }

    /// Numbered books, said as people say them. "First John" and "one john"
    /// are both the book a recogniser may have written either way.
    #[test]
    fn hears_a_numbered_book_spoken() {
        for said in ["first John four eight", "one john four eight"] {
            let mut ear = listener();
            assert_eq!(
                ear.hear(said, false),
                Some(Command::Passage {
                    book: Some("1 John".into()),
                    chapter: Some(4),
                    by: 0,
                    verse: Some(8),
                }),
                "{said}"
            );
        }
    }

    /// The longest book name wins, so "1 John" is not read as "John".
    #[test]
    fn a_longer_book_name_wins() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("song of solomon two one", false),
            Some(Command::Passage {
                book: Some("Song of Solomon".into()),
                chapter: Some(2),
                by: 0,
                verse: Some(1),
            })
        );
    }

    /// A book named with no number is a preacher saying a name.
    #[test]
    fn a_book_on_its_own_asks_for_nothing() {
        let mut ear = listener();
        assert_eq!(ear.hear("John", false), None);
        assert_eq!(ear.hear("the book of Romans", false), None);
    }

    /// And a reference inside a sentence is preaching, not an instruction.
    /// This is the case that matters most: references are said aloud in every
    /// service, far more often than any other phrase here.
    #[test]
    fn a_reference_in_a_sentence_is_not_a_command() {
        let mut ear = listener();
        for said in [
            "turn with me to John nine eight",
            "as we read in Romans eight this morning",
            "John nine eight is where we finished last week",
            "look at first John four eight with me",
        ] {
            assert_eq!(ear.hear(said, false), None, "{said}");
        }
    }

    /// The wake word buys the usual latitude, because nobody says it by
    /// accident.
    #[test]
    fn a_wake_word_takes_a_reference_mid_sentence() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("castavox John nine eight please", false),
            Some(Command::Passage {
                book: Some("John".into()),
                chapter: Some(9),
                by: 0,
                verse: Some(8),
            })
        );
    }

    /// Said again on purpose, after something else, is a second instruction.
    #[test]
    fn the_same_words_later_are_heard_again() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next verse", false), Some(Command::NextVerse));
        assert_eq!(ear.hear("castavox clear the screen", false), Some(Command::Clear));
        assert_eq!(ear.hear("castavox next verse", false), Some(Command::NextVerse));
    }

    /// A phrase that has scrolled away is not an instruction any more.
    #[test]
    fn only_the_tail_counts() {
        let mut ear = listener();
        let long = "castavox next verse ".to_string()
            + &"and then he said to them all of this and much more besides ".repeat(2);
        assert_eq!(ear.hear(&long, false), None);
    }

    #[test]
    fn names_a_translation_however_it_is_said() {
        for said in [
            "castavox switch to the ESV",
            "castavox give me the English Standard Version",
            "castavox read from english standard",
        ] {
            let mut ear = listener();
            assert_eq!(
                ear.hear(said, false),
                Some(Command::Switch { translation: "ESV".into() }),
                "{said}"
            );
        }
    }

    /// "New King James" contains "King James", and the longer one was meant.
    /// Switching a congregation's Bible to something nobody asked for is a
    /// worse failure than not switching it.
    #[test]
    fn the_longer_name_wins() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("castavox switch to the new king james", false),
            Some(Command::Switch { translation: "NKJV".into() })
        );
    }

    #[test]
    fn a_church_can_teach_it_their_own_words() {
        let custom = vec![Phrase { said: "shema".into(), does: Command::NextVerse }];
        let mut ear = Listener::new("Castavox", &translations(), &books(), &custom);
        assert_eq!(ear.hear("castavox shema", false), Some(Command::NextVerse));
    }

    /// Their words beat ours where both would match, because they added theirs
    /// precisely because ours did not fit.
    #[test]
    fn a_churchs_own_phrase_wins() {
        let custom = vec![Phrase { said: "next verse".into(), does: Command::Clear }];
        let mut ear = Listener::new("Castavox", &translations(), &books(), &custom);
        assert_eq!(ear.hear("castavox next verse", false), Some(Command::Clear));
    }

    /// The second instruction, not the first: an operator who corrects
    /// themselves mid-sentence meant the correction.
    #[test]
    fn the_last_wake_word_is_the_one_that_counts() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("castavox next verse castavox clear the screen", false),
            Some(Command::Clear)
        );
    }

    /// Without a wake word an instruction still works — said on its own.
    ///
    /// Saying the product's name before every instruction is a real cost during
    /// a service, so a church can go without. What replaces the wake word is
    /// standing alone.
    #[test]
    fn without_a_wake_word_a_bare_instruction_still_works() {
        let mut ear = Listener::new("", &translations(), &books(), &[]);
        assert_eq!(ear.hear("next verse", false), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("previous verse", false), Some(Command::PreviousVerse));
        ear.forget();
        assert_eq!(ear.hear("clear the screen", false), Some(Command::Clear));
        ear.forget();
        assert_eq!(
            ear.hear("give me the King James", false),
            Some(Command::Switch { translation: "KJV".into() })
        );
    }

    /// And the sermon still does not move the screen, which is the whole
    /// reason the wake word was there. Buried in a sentence is not an
    /// instruction, however exactly the words match.
    #[test]
    fn without_a_wake_word_a_sermon_is_still_not_a_command() {
        let mut ear = Listener::new("", &translations(), &books(), &[]);
        for said in [
            "and the next verse says something remarkable",
            "let us go back to what Paul wrote",
            "I want to clear the screen of my mind",
            "switch to the King James for a moment",
            "the King James is clearer here",
            "he read the next verse and then stopped",
            "we will come back to that",
        ] {
            assert_eq!(ear.hear(said, false), None, "{said}");
        }
    }

    /// A church's own word works bare too, which is the point of teaching it.
    /// The point of "complementary": a church that sets a wake word can still
    /// just say the instruction. Setting one is not a promise to use it.
    #[test]
    fn a_wake_word_does_not_stop_a_bare_instruction() {
        let mut ear = listener();
        assert_eq!(ear.hear("next verse", false), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("castavox next verse", false), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(
            ear.hear("give me the King James", false),
            Some(Command::Switch { translation: "KJV".into() })
        );
    }

    /// And with a wake word set, a sermon is still safe by the same rule.
    #[test]
    fn a_wake_word_does_not_make_a_sermon_dangerous() {
        let mut ear = listener();
        for said in [
            "and the next verse says something remarkable",
            "the King James is clearer here",
            "we will come back to that",
        ] {
            assert_eq!(ear.hear(said, false), None, "{said}");
        }
    }

    #[test]
    fn a_bare_custom_phrase_works() {
        let custom = vec![Phrase { said: "shema".into(), does: Command::NextVerse }];
        let mut ear = Listener::new("", &translations(), &books(), &custom);
        assert_eq!(ear.hear("shema", false), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("and then he said shema to them", false), None);
    }

    /// The wake word alone is somebody saying the product's name.
    #[test]
    fn the_wake_word_on_its_own_asks_for_nothing() {
        let mut ear = listener();
        assert_eq!(ear.hear("we run this on castavox", false), None);
    }
}
