//! The natural language of a document (ISO 32000-1, 14.9.2), for the catalog's `/Lang` at level
//! a. A document that declares one keeps it; else the language the caller gives;
//! else French or English when the words of the document leave no doubt, without guessing the
//! region; else none, which PDF/A allows. A wrong language makes a screen reader read with the
//! wrong voice, where without one it reads in its user's.

use std::collections::BTreeMap;

use crate::edit::Location;
use crate::fonts::unicode::Meaning;
use crate::scan::{Break, Scan};

/// Fewer words than this of either list tell nothing.
const MIN_WORDS: usize = 20;
/// The share of the words found, in tenths, one language must have.
const MIN_SHARE_TENTHS: usize = 8;
/// A text drawing its spaces has at least this many in a hundred characters: some fifteen in
/// French or English prose.
const MIN_SPACE_PERCENT: usize = 5;

/// Frequent French words that are not English words.
const FRENCH: &[&str] = &[
    "le", "la", "les", "des", "du", "de", "et", "est", "une", "un", "pour", "dans", "que", "qui",
    "sur", "par", "au", "aux", "avec", "ce", "cette", "ces", "sont", "pas", "vous", "nous", "il",
    "elle", "ils", "à", "été", "être", "leur", "ses", "sa", "votre", "vos", "notre", "nos", "mais",
    "comme", "plus", "tout", "entre", "lors", "selon", "afin", "dont", "où", "en", "ou", "si",
];

/// Frequent English words that are not French words.
const ENGLISH: &[&str] = &[
    "the", "and", "of", "to", "in", "is", "for", "that", "with", "by", "this", "are", "be", "at",
    "from", "it", "you", "your", "will", "not", "which", "have", "has", "shall", "any", "all",
    "its", "was", "were", "been", "their", "there", "these", "those", "such", "may", "must",
    "would", "should", "other", "than", "into", "upon", "each", "our", "we", "they", "he", "she",
];

/// Whether `tag` is a language identifier PDF/A accepts (ISO 19005-3, 6.7.4): empty (unknown),
/// or a tag of RFC 3066 as veraPDF reads it.
pub(crate) fn valid(tag: &str) -> bool {
    if tag.is_empty() {
        return true;
    }
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or_default();
    let fits = |part: &str, alphabetic: bool| {
        (1..=8).contains(&part.len())
            && part.bytes().all(|b| {
                if alphabetic {
                    b.is_ascii_alphabetic()
                } else {
                    b.is_ascii_alphanumeric()
                }
            })
    };
    fits(primary, true) && parts.all(|part| fits(part, false))
}

/// The text of the strings the scan kept, as far as the conversion knows what their codes mean.
///
/// Where one operator ends and the next begins, a word ends only if the producer does not draw
/// its spaces: some draw a word per operator, others two glyphs at a time (measured
/// 15/11/2026).
pub(crate) fn sample(scan: &Scan, meanings: &BTreeMap<Location, Meaning>) -> String {
    let mut strings: Vec<(Break, String)> = Vec::new();
    for shown in &scan.sample {
        let Some(meaning) = meanings.get(&shown.font) else {
            continue;
        };
        let mut text = String::new();
        let codes: Box<dyn Iterator<Item = u32>> = match meaning.width {
            1 => Box::new(shown.codes.iter().map(|b| u32::from(*b))),
            _ => Box::new(
                shown
                    .codes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u32::from(u16::from_be_bytes(*pair))),
            ),
        };
        for code in codes {
            if let Some(value) = meaning.values.get(&code) {
                text.push_str(value);
            }
        }
        strings.push((shown.after, text));
    }
    let (spaces, characters) = strings
        .iter()
        .flat_map(|(_, text)| text.chars())
        .fold((0usize, 0usize), |(spaces, all), c| {
            (spaces + usize::from(c.is_whitespace()), all + 1)
        });
    let drawn_spaces = spaces * 100 >= characters * MIN_SPACE_PERCENT;
    let mut text = String::new();
    for (after, string) in strings {
        if after == Break::Gap || (after == Break::Operator && !drawn_spaces) {
            text.push(' ');
        }
        text.push_str(&string);
    }
    text
}

/// French (`fr`) or English (`en`), when the frequent words of `text` leave no doubt.
pub(crate) fn detect(text: &str) -> Option<&'static str> {
    let (mut french, mut english) = (0usize, 0usize);
    for word in text.split(|c: char| !c.is_alphabetic()) {
        if word.is_empty() || word.chars().count() > 6 {
            continue;
        }
        let word = word.to_lowercase();
        french += usize::from(FRENCH.contains(&word.as_str()));
        english += usize::from(ENGLISH.contains(&word.as_str()));
    }
    let total = french + english;
    if total < MIN_WORDS {
        return None;
    }
    if french * 10 >= total * MIN_SHARE_TENTHS {
        Some("fr")
    } else if english * 10 >= total * MIN_SHARE_TENTHS {
        Some("en")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sealwax_core::ObjId;

    use super::{detect, sample, valid};
    use crate::edit::Location;
    use crate::fonts::unicode::Meaning;
    use crate::scan::{Break, Scan, Shown};

    /// What a scan keeps of `strings` shown with one font whose codes are ASCII.
    fn text_of(strings: &[(&str, Break)]) -> String {
        let font = Location::of(ObjId {
            number: 5,
            generation: 0,
        });
        let mut scan = Scan::default();
        let mut values = BTreeMap::new();
        for (string, after) in strings {
            for b in string.bytes() {
                values.insert(u32::from(b), char::from(b).to_string());
            }
            scan.sample.push(Shown {
                font: font.clone(),
                codes: string.as_bytes().to_vec(),
                after: *after,
            });
        }
        let meanings = BTreeMap::from([(font, Meaning { width: 1, values })]);
        sample(&scan, &meanings)
    }

    #[test]
    fn words_cut_across_operators_or_drawn_one_by_one() {
        // Two glyphs per operator, spaces drawn: operators do not end words.
        let pairs = [
            ("Le", Break::Operator),
            (" c", Break::Operator),
            ("on", Break::Operator),
        ];
        assert_eq!(text_of(&pairs), "Le con");
        // A word per operator, spaces not drawn; a wide adjustment inside a `TJ`.
        let words = [
            ("Le", Break::Operator),
            ("contrat", Break::Operator),
            ("est", Break::Gap),
        ];
        assert_eq!(text_of(&words), " Le contrat est");
    }

    #[test]
    fn language_tags_as_verapdf_reads_them() {
        for tag in ["", "fr", "fr-CA", "en-US", "x-klingon", "zh-Hant-TW"] {
            assert!(valid(tag), "{tag}");
        }
        for tag in [
            "fr_CA",
            "-fr",
            "fr-",
            "français",
            "toolongtag",
            "fr-CA-",
            "1a",
        ] {
            assert!(!valid(tag), "{tag}");
        }
    }

    #[test]
    fn french_english_or_nothing() {
        let french = "Le présent contrat est conclu entre les parties pour une durée de un an, \
                      et il prend effet à la date de la signature par le client et la société. "
            .repeat(2);
        let english = "This agreement is made between the parties for a term of one year, and \
                       it takes effect on the date it is signed by the client and the company. "
            .repeat(3);
        assert_eq!(detect(&french), Some("fr"));
        assert_eq!(detect(&english), Some("en"));
        // Bilingual forms, frequent in Canada, and short texts: no language.
        assert_eq!(detect(&(french.clone() + &english)), None);
        assert_eq!(detect("Signature du client"), None);
    }
}
