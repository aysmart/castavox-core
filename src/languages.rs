//! What can be spoken, and which engine can hear it.
//!
//! One table, shared, because the three engines disagree and the disagreement
//! is the whole point. A picker that lists a language without saying which
//! engine can transcribe it sells a church something that produces nonsense: the
//! hosted engine answers a Swahili sermon with confident Spanish-shaped English,
//! and nothing in the interface admits why.
//!
//! # Where the three differ
//!
//! **Hosted** is Deepgram nova-3. English is its own model; everything else goes
//! through `multi`, its multilingual mode, which covers ten languages and no
//! African one. What is not in that set must not claim to be hosted-capable.
//!
//! **Azure** takes a full BCP-47 locale and covers considerably more, including
//! Swahili, Amharic, Zulu and Afrikaans -- but a church has to hold its own
//! subscription for it.
//!
//! **This machine** is whisper, which has the widest list of the three and is
//! the only one that can hear **Yoruba or Hausa at all**. That is worth stating
//! plainly, because it inverts the usual advice: for those languages the free
//! engine is not the compromise, it is the only thing that works.
//!
//! Igbo is in none of them. The Igbo Bible is bundled and can be searched,
//! typed and displayed; it simply cannot be listened for, and saying so is
//! better than a setting that silently does nothing.

/// One language an operator can choose, and what will actually hear it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// Azure's spelling, which is what settings hold everywhere.
    pub code: &'static str,
    pub label: &'static str,
    /// Deepgram, through a Castavox subscription.
    pub hosted: bool,
    /// Azure, on the church's own account.
    pub azure: bool,
    /// whisper, on this machine.
    pub local: bool,
}

impl Language {
    /// Whether any engine at all can hear this. Nothing in the table is false
    /// on all three, but a locale typed by hand can be.
    pub fn heard_by_anything(&self) -> bool {
        self.hosted || self.azure || self.local
    }
}

/// The languages offered, most likely first.
///
/// English variants lead because that is what most services are in, including
/// most Nigerian ones. The languages whose Bibles this now bundles follow, then
/// the rest of what the engines can do.
///
/// Regional English tags are kept even though the hosted engine collapses them
/// to `en`: Azure uses them properly, and an operator picking "English
/// (Nigeria)" is telling us something true about the room whether or not every
/// engine acts on it.
pub const LANGUAGES: &[Language] = &[
    Language { code: "en-US", label: "English (United States)", hosted: true, azure: true, local: true },
    Language { code: "en-GB", label: "English (United Kingdom)", hosted: true, azure: true, local: true },
    Language { code: "en-NG", label: "English (Nigeria)", hosted: true, azure: true, local: true },
    Language { code: "en-GH", label: "English (Ghana)", hosted: true, azure: true, local: true },
    Language { code: "en-KE", label: "English (Kenya)", hosted: true, azure: true, local: true },
    Language { code: "en-ZA", label: "English (South Africa)", hosted: true, azure: true, local: true },
    Language { code: "en-IN", label: "English (India)", hosted: true, azure: true, local: true },
    Language { code: "en-AU", label: "English (Australia)", hosted: true, azure: true, local: true },
    Language { code: "en-CA", label: "English (Canada)", hosted: true, azure: true, local: true },

    /*
     * The languages whose Bibles are now bundled.
     *
     * Yoruba and Hausa are local-only, and that is not an oversight to be
     * fixed later -- no streaming provider we can buy transcribes them. A
     * Yoruba church on a subscription must be told to use its own machine.
     */
    Language { code: "yo-NG", label: "Yoruba", hosted: false, azure: false, local: true },
    Language { code: "ha-NG", label: "Hausa", hosted: false, azure: false, local: true },
    Language { code: "sw-KE", label: "Swahili (Kenya)", hosted: false, azure: true, local: true },
    Language { code: "sw-TZ", label: "Swahili (Tanzania)", hosted: false, azure: true, local: true },
    Language { code: "fr-FR", label: "French (France)", hosted: true, azure: true, local: true },
    Language { code: "fr-CA", label: "French (Canada)", hosted: true, azure: true, local: true },
    Language { code: "es-ES", label: "Spanish (Spain)", hosted: true, azure: true, local: true },
    Language { code: "es-MX", label: "Spanish (Mexico)", hosted: true, azure: true, local: true },
    Language { code: "pt-BR", label: "Portuguese (Brazil)", hosted: true, azure: true, local: true },
    Language { code: "pt-PT", label: "Portuguese (Portugal)", hosted: true, azure: true, local: true },

    // The rest of what the hosted engine's multilingual mode covers.
    Language { code: "de-DE", label: "German", hosted: true, azure: true, local: true },
    Language { code: "it-IT", label: "Italian", hosted: true, azure: true, local: true },
    Language { code: "nl-NL", label: "Dutch", hosted: true, azure: true, local: true },
    Language { code: "hi-IN", label: "Hindi", hosted: true, azure: true, local: true },
    Language { code: "ru-RU", label: "Russian", hosted: true, azure: true, local: true },
    Language { code: "ja-JP", label: "Japanese", hosted: true, azure: true, local: true },

    // Azure and this machine, but not the subscription.
    Language { code: "zh-CN", label: "Chinese (Mandarin)", hosted: false, azure: true, local: true },
    Language { code: "ko-KR", label: "Korean", hosted: false, azure: true, local: true },
    Language { code: "ar-EG", label: "Arabic (Egypt)", hosted: false, azure: true, local: true },
    Language { code: "am-ET", label: "Amharic", hosted: false, azure: true, local: true },
    Language { code: "af-ZA", label: "Afrikaans", hosted: false, azure: true, local: true },
    Language { code: "zu-ZA", label: "Zulu", hosted: false, azure: true, local: false },
    Language { code: "tl-PH", label: "Tagalog", hosted: false, azure: true, local: true },
    Language { code: "id-ID", label: "Indonesian", hosted: false, azure: true, local: true },
    Language { code: "pl-PL", label: "Polish", hosted: true, azure: true, local: true },
    Language { code: "uk-UA", label: "Ukrainian", hosted: false, azure: true, local: true },
    Language { code: "ro-RO", label: "Romanian", hosted: false, azure: true, local: true },
    Language { code: "ta-IN", label: "Tamil", hosted: false, azure: true, local: true },
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
pub fn warning(code: &str, engine_hosted: bool, engine_azure: bool) -> Option<String> {
    let language = find(code)?;

    let (works, engine) = if engine_hosted {
        (language.hosted, "A Castavox subscription")
    } else if engine_azure {
        (language.azure, "Azure")
    } else {
        (language.local, "This machine")
    };
    if works {
        return None;
    }

    let mut instead = Vec::new();
    if language.local {
        instead.push("this machine");
    }
    if language.azure && !engine_azure {
        instead.push("your own Azure account");
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
        let said = warning("yo-NG", true, false).expect("a subscription cannot do Yoruba");
        assert!(said.contains("Yoruba"), "{said}");
        assert!(said.contains("this machine"), "{said}");

        assert!(warning("yo-NG", false, false).is_none(), "locally it works");
    }

    #[test]
    fn swahili_is_azure_or_local_but_not_hosted() {
        let said = warning("sw-KE", true, false).expect("Deepgram's multi has no Swahili");
        assert!(said.contains("your own Azure account"), "{said}");
        assert!(said.contains("this machine"), "{said}");
    }

    #[test]
    fn the_ordinary_case_costs_no_words() {
        // English on any engine, and the languages the bundled Bibles cover
        // that the hosted engine does handle.
        for code in ["en-NG", "en-US", "fr-FR", "es-ES", "pt-BR"] {
            assert!(warning(code, true, false).is_none(), "{code} is hosted-capable");
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
        assert!(warning("xx-YY", true, false).is_none());
    }
}
