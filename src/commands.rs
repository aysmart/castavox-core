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
//!    per arrival advances three verses on one instruction.
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
    pub fn new(wake: &str, translations: &[(String, String)], custom: &[Phrase]) -> Self {
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

        Self {
            wake: words(wake).join(" "),
            named,
            custom: custom.to_vec(),
            last: None,
        }
    }

    /// Reads the tail of the transcript for an instruction.
    ///
    /// `None` is the answer for almost everything ever said in a church, which
    /// is the design working rather than failing.
    pub fn hear(&mut self, transcript: &str) -> Option<Command> {
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
            return None;
        }
        self.last = Some(phrase);
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

        for (name, id) in &self.named {
            if name.len() >= 3 && said == strip(&words(name)) {
                return Some((Command::Switch { translation: id.clone() }, name.clone()));
            }
        }
        None
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

    fn listener() -> Listener {
        Listener::new("Castavox", &translations(), &[])
    }

    #[test]
    fn hears_the_plain_instructions() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next verse"), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("Castavox, previous verse."), Some(Command::PreviousVerse));
        ear.forget();
        assert_eq!(ear.hear("castavox clear the screen"), Some(Command::Clear));
    }

    /// The whole reason for the wake word. A preacher saying this is preaching.
    #[test]
    fn a_sermon_is_not_a_command() {
        let mut ear = listener();
        assert_eq!(ear.hear("and the next verse says something remarkable"), None);
        assert_eq!(ear.hear("let us go back to what Paul wrote"), None);
        assert_eq!(ear.hear("I want to clear the screen of my mind"), None);
        assert_eq!(ear.hear("switch to the King James for a moment"), None);
    }

    /// One instruction is obeyed once, however many times the transcript is
    /// revised. Without this a single "next verse" advances three.
    #[test]
    fn a_settling_transcript_does_not_repeat_the_command() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next"), None);
        assert_eq!(ear.hear("castavox next verse"), Some(Command::NextVerse));
        assert_eq!(ear.hear("castavox next verse"), None);
        assert_eq!(ear.hear("castavox next verse."), None);
    }

    /// The transcript keeps growing while a sentence settles, so the words
    /// after the command are different every time it arrives. Keyed on all of
    /// them, one "next verse" would advance three or four.
    #[test]
    fn a_command_does_not_fire_again_as_more_is_said_after_it() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next verse"), Some(Command::NextVerse));
        assert_eq!(ear.hear("castavox next verse and"), None);
        assert_eq!(ear.hear("castavox next verse and then"), None);
        assert_eq!(ear.hear("castavox next verse and then he said"), None);
    }

    /// A wake word is a whole word. A church that chooses a short one should
    /// not be woken by every word that happens to start with it.
    #[test]
    fn the_wake_word_is_not_matched_inside_another_word() {
        let mut ear = Listener::new("bee", &translations(), &[]);
        assert_eq!(ear.hear("the beef next verse"), None);
        assert_eq!(ear.hear("bee next verse"), Some(Command::NextVerse));
    }

    /// Same for a translation name, which is matched anywhere after the wake
    /// word rather than only at the start.
    #[test]
    fn a_translation_is_not_matched_inside_another_word() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox tell the university about it"), None);
    }

    /// Said again on purpose, after something else, is a second instruction.
    #[test]
    fn the_same_words_later_are_heard_again() {
        let mut ear = listener();
        assert_eq!(ear.hear("castavox next verse"), Some(Command::NextVerse));
        assert_eq!(ear.hear("castavox clear the screen"), Some(Command::Clear));
        assert_eq!(ear.hear("castavox next verse"), Some(Command::NextVerse));
    }

    /// A phrase that has scrolled away is not an instruction any more.
    #[test]
    fn only_the_tail_counts() {
        let mut ear = listener();
        let long = "castavox next verse ".to_string()
            + &"and then he said to them all of this and much more besides ".repeat(2);
        assert_eq!(ear.hear(&long), None);
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
                ear.hear(said),
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
            ear.hear("castavox switch to the new king james"),
            Some(Command::Switch { translation: "NKJV".into() })
        );
    }

    #[test]
    fn a_church_can_teach_it_their_own_words() {
        let custom = vec![Phrase { said: "shema".into(), does: Command::NextVerse }];
        let mut ear = Listener::new("Castavox", &translations(), &custom);
        assert_eq!(ear.hear("castavox shema"), Some(Command::NextVerse));
    }

    /// Their words beat ours where both would match, because they added theirs
    /// precisely because ours did not fit.
    #[test]
    fn a_churchs_own_phrase_wins() {
        let custom = vec![Phrase { said: "next verse".into(), does: Command::Clear }];
        let mut ear = Listener::new("Castavox", &translations(), &custom);
        assert_eq!(ear.hear("castavox next verse"), Some(Command::Clear));
    }

    /// The second instruction, not the first: an operator who corrects
    /// themselves mid-sentence meant the correction.
    #[test]
    fn the_last_wake_word_is_the_one_that_counts() {
        let mut ear = listener();
        assert_eq!(
            ear.hear("castavox next verse castavox clear the screen"),
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
        let mut ear = Listener::new("", &translations(), &[]);
        assert_eq!(ear.hear("next verse"), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("previous verse"), Some(Command::PreviousVerse));
        ear.forget();
        assert_eq!(ear.hear("clear the screen"), Some(Command::Clear));
        ear.forget();
        assert_eq!(
            ear.hear("give me the King James"),
            Some(Command::Switch { translation: "KJV".into() })
        );
    }

    /// And the sermon still does not move the screen, which is the whole
    /// reason the wake word was there. Buried in a sentence is not an
    /// instruction, however exactly the words match.
    #[test]
    fn without_a_wake_word_a_sermon_is_still_not_a_command() {
        let mut ear = Listener::new("", &translations(), &[]);
        for said in [
            "and the next verse says something remarkable",
            "let us go back to what Paul wrote",
            "I want to clear the screen of my mind",
            "switch to the King James for a moment",
            "the King James is clearer here",
            "he read the next verse and then stopped",
            "we will come back to that",
        ] {
            assert_eq!(ear.hear(said), None, "{said}");
        }
    }

    /// A church's own word works bare too, which is the point of teaching it.
    /// The point of "complementary": a church that sets a wake word can still
    /// just say the instruction. Setting one is not a promise to use it.
    #[test]
    fn a_wake_word_does_not_stop_a_bare_instruction() {
        let mut ear = listener();
        assert_eq!(ear.hear("next verse"), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("castavox next verse"), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(
            ear.hear("give me the King James"),
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
            assert_eq!(ear.hear(said), None, "{said}");
        }
    }

    #[test]
    fn a_bare_custom_phrase_works() {
        let custom = vec![Phrase { said: "shema".into(), does: Command::NextVerse }];
        let mut ear = Listener::new("", &translations(), &custom);
        assert_eq!(ear.hear("shema"), Some(Command::NextVerse));
        ear.forget();
        assert_eq!(ear.hear("and then he said shema to them"), None);
    }

    /// The wake word alone is somebody saying the product's name.
    #[test]
    fn the_wake_word_on_its_own_asks_for_nothing() {
        let mut ear = listener();
        assert_eq!(ear.hear("we run this on castavox"), None);
    }
}
