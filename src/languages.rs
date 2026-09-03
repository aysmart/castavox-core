//! What can be spoken, and which engine can hear it.
//!
//! One table, shared, because the two engines disagree and the disagreement
//! is the whole point. A picker that lists a language without saying which
//! engine can transcribe it sells a church something that produces nonsense: the
//! hosted engine answers a Swahili sermon with confident Spanish-shaped English,
//! and nothing in the interface admits why.
//!
//! # Where the two differ
//!
//! **Hosted** is Deepgram nova-3. English is its own model; everything else goes
//! through `multi`, its multilingual mode, which covers ten languages and no
//! African one. What is not in that set must not claim to be hosted-capable.
//!
//! **This machine** is whisper, which has much the wider list of the two and is
//! the only one that can hear **Yoruba or Hausa at all**. That is worth stating
//! plainly, because it inverts the usual advice: for those languages the free
//! engine is not the compromise, it is the only thing that works.
//!
//! A third column stood here until 0.8.4, for a church bringing its own Azure
//! Speech account. That option is gone, and with it the only engine that could
//! hear Zulu -- which is why Zulu is now in the table with nothing able to
//! transcribe it, rather than quietly dropped.
//!
//! Igbo is in none of them. The Igbo Bible is bundled and can be searched,
//! typed and displayed; it simply cannot be listened for, and saying so is
//! better than a setting that silently does nothing.

/// One language an operator can choose, and what will actually hear it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// A full BCP-47 locale, which is what settings hold everywhere.
    pub code: &'static str,
    pub label: &'static str,
    /// Deepgram, through a Castavox subscription.
    pub hosted: bool,
    /// whisper, on this machine.
    pub local: bool,
}

impl Language {
    /// Whether either engine can hear this. Zulu is the one entry where the
    /// answer is no; a locale typed by hand can be too.
    pub fn heard_by_anything(&self) -> bool {
        self.hosted || self.local
    }
}

/// The languages offered, most likely first.
///
/// English variants lead because that is what most services are in, including
/// most Nigerian ones. The languages whose Bibles this now bundles follow, then
/// the rest of what the engines can do.
///
/// Regional English tags are kept even though the hosted engine collapses them
/// to `en`: whisper reads the region, and an operator picking "English
/// (Nigeria)" is telling us something true about the room whether or not every
/// engine acts on it.
pub const LANGUAGES: &[Language] = &[
    Language { code: "en-US", label: "English (United States)", hosted: true, local: true },
    Language { code: "en-GB", label: "English (United Kingdom)", hosted: true, local: true },
    Language { code: "en-NG", label: "English (Nigeria)", hosted: true, local: true },
    Language { code: "en-GH", label: "English (Ghana)", hosted: true, local: true },
    Language { code: "en-KE", label: "English (Kenya)", hosted: true, local: true },
    Language { code: "en-ZA", label: "English (South Africa)", hosted: true, local: true },
    Language { code: "en-IN", label: "English (India)", hosted: true, local: true },
    Language { code: "en-AU", label: "English (Australia)", hosted: true, local: true },
    Language { code: "en-CA", label: "English (Canada)", hosted: true, local: true },

    /*
     * The languages whose Bibles are now bundled.
     *
     * Yoruba and Hausa are local-only, and that is not an oversight to be
     * fixed later -- no streaming provider we can buy transcribes them. A
     * Yoruba church on a subscription must be told to use its own machine.
     */
    Language { code: "yo-NG", label: "Yoruba", hosted: false, local: true },
    Language { code: "ha-NG", label: "Hausa", hosted: false, local: true },
    Language { code: "sw-KE", label: "Swahili (Kenya)", hosted: false, local: true },
    Language { code: "sw-TZ", label: "Swahili (Tanzania)", hosted: false, local: true },
    Language { code: "fr-FR", label: "French (France)", hosted: true, local: true },
    Language { code: "fr-CA", label: "French (Canada)", hosted: true, local: true },
    Language { code: "es-ES", label: "Spanish (Spain)", hosted: true, local: true },
    Language { code: "es-MX", label: "Spanish (Mexico)", hosted: true, local: true },
    Language { code: "pt-BR", label: "Portuguese (Brazil)", hosted: true, local: true },
    Language { code: "pt-PT", label: "Portuguese (Portugal)", hosted: true, local: true },

    // The rest of what the hosted engine's multilingual mode covers.
    Language { code: "de-DE", label: "German", hosted: true, local: true },
    Language { code: "it-IT", label: "Italian", hosted: true, local: true },
    Language { code: "nl-NL", label: "Dutch", hosted: true, local: true },
    Language { code: "hi-IN", label: "Hindi", hosted: true, local: true },
    Language { code: "ru-RU", label: "Russian", hosted: true, local: true },
    Language { code: "ja-JP", label: "Japanese", hosted: true, local: true },

    // This machine only. The subscription's multilingual mode has none of
    // these, and the account that used to is no longer an option.
    Language { code: "zh-CN", label: "Chinese (Mandarin)", hosted: false, local: true },
    Language { code: "ko-KR", label: "Korean", hosted: false, local: true },
    Language { code: "ar-EG", label: "Arabic (Egypt)", hosted: false, local: true },
    Language { code: "am-ET", label: "Amharic", hosted: false, local: true },
    Language { code: "af-ZA", label: "Afrikaans", hosted: false, local: true },
    Language { code: "zu-ZA", label: "Zulu", hosted: false, local: false },
    Language { code: "tl-PH", label: "Tagalog", hosted: false, local: true },
    Language { code: "id-ID", label: "Indonesian", hosted: false, local: true },
    Language { code: "pl-PL", label: "Polish", hosted: true, local: true },
    Language { code: "uk-UA", label: "Ukrainian", hosted: false, local: true },
    Language { code: "ro-RO", label: "Romanian", hosted: false, local: true },
    Language { code: "ta-IN", label: "Tamil", hosted: false, local: true },
];

/// One language by its code, for turning a stored setting back into a fact.
pub fn find(code: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|language| language.code.eq_ignore_ascii_case(code))
}

/// What to say when the chosen language and the chosen engine disagree.
///
/// Returns nothing when they agree, which is the ordinary case and should cost
/// no words. The sentence names the engine that *would* work rather than only
/// the one that will not: "this cannot do Yoruba" leaves an operator stuck,
/// "use this machine instead" does not.
pub fn warning(code: &str, engine_hosted: bool) -> Option<String> {
    let language = find(code)?;

    let (works, engine) = if engine_hosted {
        (language.hosted, "A Castavox subscription")
    } else {
        (language.local, "This machine")
    };
    if works {
        return None;
    }

    let mut instead = Vec::new();
    if language.local && engine_hosted {
        instead.push("this machine");
    }
    if language.hosted && !engine_hosted {
        instead.push("a Castavox subscription");
    }

    Some(match instead.as_slice() {
        [] => format!(
            "{} cannot transcribe {}, and nor can the others. The Bible can still be searched \
             and displayed in it; only listening is unavailable.",
            engine, language.label,
        ),
        [one] => format!(
            "{} cannot transcribe {}. Use {} instead.",
            engine, language.label, one,
        ),
        many => format!(
            "{} cannot transcribe {}. Use {} instead.",
            engine,
            language.label,
            many.join(" or "),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_unique_and_regioned() {
        let mut seen = Vec::new();
        for language in LANGUAGES {
            assert!(!seen.contains(&language.code), "{} listed twice", language.code);
            assert!(language.code.contains('-'), "{} has no region", language.code);
            seen.push(language.code);
        }
    }

    /// The whole reason this table exists.
    #[test]
    fn yoruba_is_local_only_and_says_so() {
        // No streaming provider we can buy transcribes Yoruba. A church on a
        // subscription has to be told to use its own machine, or it gets
        // confident nonsense and no explanation.
        let said = warning("yo-NG", true).expect("a subscription cannot do Yoruba");
        assert!(said.contains("Yoruba"), "{said}");
        assert!(said.contains("this machine"), "{said}");

        assert!(warning("yo-NG", false).is_none(), "locally it works");
    }

    #[test]
    fn swahili_is_local_only_now_that_azure_is_gone() {
        // It was Azure or local. Withdrawing the Azure engine did not change
        // what Deepgram can hear, so a Swahili church is now local-only in
        // fact as well as in this table -- and must be told so plainly.
        let said = warning("sw-KE", true).expect("Deepgram's multi has no Swahili");
        assert!(said.contains("this machine"), "{said}");
        assert!(!said.contains("Azure"), "the Azure engine is withdrawn: {said}");
    }

    /// Zulu is the one language left with no engine at all.
    #[test]
    fn zulu_says_nothing_can_hear_it() {
        let said = warning("zu-ZA", true).expect("nothing transcribes Zulu now");
        assert!(said.contains("nor can the others"), "{said}");
        assert!(warning("zu-ZA", false).is_some(), "not locally either");
    }

    #[test]
    fn the_ordinary_case_costs_no_words() {
        // English on any engine, and the languages the bundled Bibles cover
        // that the hosted engine does handle.
        for code in ["en-NG", "en-US", "fr-FR", "es-ES", "pt-BR"] {
            assert!(warning(code, true).is_none(), "{code} is hosted-capable");
        }
    }

    /// Igbo is bundled as a Bible and heard by nothing.
    #[test]
    fn a_language_no_engine_knows_is_not_offered_for_listening() {
        assert!(find("ig-NG").is_none(), "Igbo must not be offered as a spoken language");
    }

    #[test]
    fn an_unknown_locale_warns_about_nothing() {
        // A hand-typed locale is not this function's business to police; the
        // engines refuse it themselves and say so in their own words.
        assert!(warning("xx-YY", true).is_none());
    }
}
