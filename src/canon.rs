//! Which book is which, when not every Bible agrees on the list.
//!
//! # The problem this exists for
//!
//! A translation's `book_number` orders that translation's own books, and until
//! now every translation we shipped had the same sixty-six in the same order,
//! so the number was effectively universal. A Catholic Bible is not that: it
//! has seventy-three books, and seven of them sit *between* the others -- Tobit
//! and Judith after Nehemiah, Wisdom and Sirach after the Song of Solomon,
//! Baruch after Lamentations, the two books of Maccabees after Malachi. Putting
//! them at the end instead would be a Protestant Bible with an appendix, which
//! is not what a parish is reading from.
//!
//! So numbering has to be per translation. The catch is that the interlinear --
//! the Greek and Hebrew behind a verse -- is keyed by book number and has no
//! translation of its own: it is one text, numbered in the sixty-six-book
//! scheme. Look up John by a Catholic Bible's number and you get Philippians'
//! Greek, presented as the original of what is on the screen.
//!
//! Hence this: the interlinear is addressed by name, and the name is resolved
//! here. A book with no entry -- every deuterocanonical one -- has no Hebrew or
//! Greek in the data we ship, and correctly returns nothing.

/// The sixty-six, in the order the interlinear numbers them.
const PROTESTANT: [&str; 66] = [
    "Genesis", "Exodus", "Leviticus", "Numbers", "Deuteronomy", "Joshua", "Judges", "Ruth",
    "1 Samuel", "2 Samuel", "1 Kings", "2 Kings", "1 Chronicles", "2 Chronicles", "Ezra",
    "Nehemiah", "Esther", "Job", "Psalms", "Proverbs", "Ecclesiastes", "Song of Solomon",
    "Isaiah", "Jeremiah", "Lamentations", "Ezekiel", "Daniel", "Hosea", "Joel", "Amos",
    "Obadiah", "Jonah", "Micah", "Nahum", "Habakkuk", "Zephaniah", "Haggai", "Zechariah",
    "Malachi", "Matthew", "Mark", "Luke", "John", "Acts", "Romans", "1 Corinthians",
    "2 Corinthians", "Galatians", "Ephesians", "Philippians", "Colossians", "1 Thessalonians",
    "2 Thessalonians", "1 Timothy", "2 Timothy", "Titus", "Philemon", "Hebrews", "James",
    "1 Peter", "2 Peter", "1 John", "2 John", "3 John", "Jude", "Revelation",
];

/// The number the interlinear knows a book by, if it knows it at all.
///
/// `None` for the deuterocanon, which is the honest answer: there is no Hebrew
/// or Greek for Tobit in the data we ship, and inventing a number for it would
/// return somebody else's.
pub fn interlinear_book(name: &str) -> Option<i64> {
    PROTESTANT
        .iter()
        .position(|book| book.eq_ignore_ascii_case(name.trim()))
        .map(|index| index as i64 + 1)
}

/// Whether a book is one the deuterocanon adds.
///
/// Used to say so on screen rather than to gate anything: a parish knows what
/// these are, and a reader from another tradition is better told than left to
/// wonder why their Bible does not have it.
pub fn is_deuterocanonical(name: &str) -> bool {
    interlinear_book(name).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_sixty_six() {
        assert_eq!(interlinear_book("Genesis"), Some(1));
        assert_eq!(interlinear_book("John"), Some(43));
        assert_eq!(interlinear_book("Revelation"), Some(66));
    }

    /// The whole point: John is book 43 to the interlinear however a Catholic
    /// Bible numbers it. Numbered by position in its own canon, John is the
    /// fiftieth book there, and looking that up would return Philippians.
    #[test]
    fn a_catholic_numbering_does_not_shift_the_interlinear() {
        assert_eq!(interlinear_book("John"), Some(43));
        assert_ne!(interlinear_book("John"), Some(50));
    }

    /// No Greek for Tobit, and saying so beats returning somebody else's.
    #[test]
    fn the_deuterocanon_has_no_interlinear() {
        for book in ["Tobit", "Judith", "Wisdom", "Sirach", "Baruch", "1 Maccabees"] {
            assert_eq!(interlinear_book(book), None, "{book}");
            assert!(is_deuterocanonical(book));
        }
    }

    #[test]
    fn matching_is_forgiving_about_case_and_spaces() {
        assert_eq!(interlinear_book("  song of solomon "), Some(22));
    }
}
