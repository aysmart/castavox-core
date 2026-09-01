//! Numbers as people say them out loud.
//!
//! A recogniser writes "twenty eight" or "28" depending on the engine, the
//! phrasing and its mood, so anything reading speech for a chapter or a verse
//! has to accept both. This was written for spoken scripture references and is
//! now also what voice commands read a chapter number with.
//!
//! # Why it is here rather than in each product
//!
//! It existed twice already -- once in each product's `spoken.rs` -- and voice
//! commands would have made a third. The rules are subtle enough to be worth
//! having once: "three sixteen" is two numbers and "twenty eight" is one, and
//! getting that backwards turns John 3:16 into John 316.

const UNITS: &[(&str, i64)] = &[
    ("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6),
    ("seven", 7), ("eight", 8), ("nine", 9), ("ten", 10), ("eleven", 11),
    ("twelve", 12), ("thirteen", 13), ("fourteen", 14), ("fifteen", 15),
    ("sixteen", 16), ("seventeen", 17), ("eighteen", 18), ("nineteen", 19),
];

const TENS: &[(&str, i64)] = &[
    ("twenty", 20), ("thirty", 30), ("forty", 40), ("fifty", 50), ("sixty", 60),
    ("seventy", 70), ("eighty", 80), ("ninety", 90),
];


pub fn word_value(token: &str) -> Option<i64> {
    if let Ok(number) = token.parse::<i64>() {
        return Some(number);
    }
    UNITS
        .iter()
        .chain(TENS.iter())
        .find(|(word, _)| *word == token)
        .map(|(_, value)| *value)
}

/// The first number in `tokens`, and how many words it took.
///
/// `read_numbers` says what the numbers are; this says where the first one
/// ends, which is what anything reading "John nine eight" needs -- the words
/// are two numbers, and consuming every number word in a row would swallow the
/// verse into the chapter.
pub fn read_first(tokens: &[String]) -> Option<(i64, usize)> {
    let first = *read_numbers(tokens).first()?;

    // The shortest run of words that reads as the whole first number. Asking
    // `read_numbers` itself rather than reimplementing when a ten absorbs a
    // unit, so the two cannot disagree -- and disagreeing is the failure that
    // matters, because it would silently swallow a verse into its chapter.
    for used in 1..=tokens.len() {
        if read_numbers(&tokens[..used]) == [first] {
            return Some((first, used));
        }
    }
    None
}

/// Collapses a run of number words into the numbers they name.
///
/// "twenty eight" is one number; "three sixteen" is two. The difference is
/// whether the words compose — tens absorb a following unit, and "hundred"
/// multiplies what came before.
pub fn read_numbers(tokens: &[String]) -> Vec<i64> {
    let mut numbers = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        let token = tokens[index].as_str();
        if token == "and" && !numbers.is_empty() {
            index += 1;
            continue;
        }
        let Some(value) = word_value(token) else { break };
        index += 1;

        let mut total = value;

        // "one hundred", "a hundred"
        if tokens.get(index).is_some_and(|next| next == "hundred") {
            total *= 100;
            index += 1;
            if tokens.get(index).is_some_and(|next| next == "and") {
                index += 1;
            }
            // "one hundred nineteen", "one hundred and twenty three"
            if let Some(rest) = tokens.get(index).and_then(|token| word_value(token)) {
                if rest < 100 {
                    total += rest;
                    index += 1;
                    if TENS.iter().any(|(_, value)| *value == rest) {
                        if let Some(unit) = tokens.get(index).and_then(|t| word_value(t)) {
                            if unit < 10 {
                                total += unit;
                                index += 1;
                            }
                        }
                    }
                }
            }
        } else if TENS.iter().any(|(_, tens)| *tens == value) {
            // "twenty eight"
            if let Some(unit) = tokens.get(index).and_then(|token| word_value(token)) {
                if unit < 10 {
                    total += unit;
                    index += 1;
                }
            }
        }

        numbers.push(total);
        if numbers.len() == 3 {
            break;
        }
    }

    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Vec<i64> {
        read_numbers(&text.split(' ').map(str::to_string).collect::<Vec<_>>())
    }

    /// The distinction the whole thing exists for: tens absorb a unit, and two
    /// separate numbers stay separate. Getting it backwards turns John 3:16
    /// into John 316.
    #[test]
    fn tens_compose_and_separate_numbers_do_not() {
        assert_eq!(read("twenty eight"), [28]);
        assert_eq!(read("three sixteen"), [3, 16]);
    }

    /// Where one number ends and the next begins, which is the whole reason
    /// "John nine eight" can be read as a chapter and a verse.
    #[test]
    fn the_first_number_knows_where_it_ends() {
        let words = |t: &str| t.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(read_first(&words("nine eight")), Some((9, 1)));
        assert_eq!(read_first(&words("twenty eight")), Some((28, 2)));
        assert_eq!(read_first(&words("one hundred nineteen")), Some((119, 3)));
        assert_eq!(read_first(&words("nine")), Some((9, 1)));
        assert_eq!(read_first(&words("chapter")), None);
        // The words after a number are not part of it, however many there are.
        assert_eq!(read_first(&words("eight is where we finished")), Some((8, 1)));
    }

    #[test]
    fn digits_and_words_read_the_same() {
        assert_eq!(read("3 16"), [3, 16]);
        assert_eq!(read("one hundred nineteen"), [119]);
    }
}
