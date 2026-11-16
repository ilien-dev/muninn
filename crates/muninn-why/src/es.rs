//! Spanish gender and number forms for `muninn why` (R-15). Used by `why` alone: no hook and
//! no `muninn recall` reaches this module.
//!
//! The index's tokenizer, `porter unicode61`, strips English suffixes only, so "abierto" and
//! "abiertas" are different tokens. A word of the question that matches nothing is replaced by
//! the forms of its stem the store actually holds. The guard against false friends is the
//! *same-stem check*: a stored word comes with the question's word only when its own stem is
//! the same stem, so "carta" does not reach "cartera" (stem "carter"), as a bare prefix query
//! "cart*" would. Its limit: two different words that happen to share a stem ("cuento" and
//! "cuenta") still reach each other, and a stem is a suffix list, not a dictionary.

use muninn_core::sanitize::fts_term;
use muninn_core::{Db, Result};

/// Inflectional suffixes, folded as `fts_term` folds them. The first one that leaves at least
/// `MIN_STEM` letters is stripped (the list and the floor are the analysis's, R-15).
const SUFFIXES: &[&str] = &[
    "aciones", "amientos", "imientos", "amiento", "imiento", "adores", "adoras", "acion", "ancias",
    "encias", "mente", "ables", "ibles", "ando", "iendo", "adas", "ados", "idas", "idos", "aron",
    "ieron", "aban", "aria", "eria", "iria", "ador", "ante", "able", "ible", "amos", "emos",
    "imos", "ada", "ado", "ida", "ido", "ar", "er", "ir", "es", "an", "en", "as", "os", "a", "o",
    "e", "s",
];

/// The fewest letters a stem keeps.
const MIN_STEM: usize = 4;

/// Forms taken for one word, at most: a stem with more forms than this is not one word.
const MAX_FORMS: usize = 6;

/// The stem of `term`: one suffix stripped, at least `MIN_STEM` letters kept, or the word itself.
/// `None` for a word shorter than `MIN_STEM`.
pub fn stem(term: &str) -> Option<&str> {
    let letters = term.chars().count();
    if letters < MIN_STEM {
        return None;
    }
    SUFFIXES
        .iter()
        .find_map(|suf| {
            let rest = term.strip_suffix(suf)?;
            (letters - suf.chars().count() >= MIN_STEM).then_some(rest)
        })
        .or(Some(term))
}

fn same_stem(token: &str, s: &str) -> bool {
    token.starts_with(s) && stem(token) == Some(s)
}

/// The forms of `word`'s stem that the index holds (active records), `word` excluded.
pub fn indexed_forms(db: &Db, word: &str) -> Result<Vec<String>> {
    let Some(s) = stem(word) else {
        return Ok(vec![]);
    };
    let mut st = db.conn.prepare(
        "SELECT DISTINCT term FROM record_vocab WHERE term >= ?1 AND term < ?2 ORDER BY term",
    )?;
    let upper = format!("{s}\u{10FFFF}");
    let forms = st
        .query_map([s, upper.as_str()], |r| r.get::<_, String>(0))?
        .flatten()
        .filter(|t| t != word && same_stem(t, s))
        .take(MAX_FORMS)
        .collect();
    Ok(forms)
}

/// The forms of `word`'s stem in the text of every record, retired ones included (`--all`: a
/// retired record is not in the index).
pub fn stored_forms(db: &Db, word: &str) -> Result<Vec<String>> {
    let Some(s) = stem(word) else {
        return Ok(vec![]);
    };
    // compared folded (`crate::folded_holds`), so an accented stem is found in accented text
    let mut st = db.conn.prepare(&format!(
        "SELECT r.subject || ' ' || r.object || ' ' || r.body FROM record r WHERE {} LIMIT 200",
        crate::folded_holds(1)
    ))?;
    let mut forms: Vec<String> = Vec::new();
    for text in st
        .query_map([crate::like_pattern(s)], |r| r.get::<_, String>(0))?
        .flatten()
    {
        for t in text
            .split(|c: char| !c.is_alphanumeric())
            .filter_map(fts_term)
        {
            if t != word && same_stem(&t, s) && !forms.contains(&t) {
                forms.push(t);
            }
        }
        if forms.len() >= MAX_FORMS {
            break;
        }
    }
    forms.truncate(MAX_FORMS);
    Ok(forms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stem_strips_one_inflection_and_keeps_four_letters() {
        assert_eq!(stem("abierto"), Some("abiert"));
        assert_eq!(stem("abiertas"), Some("abiert"));
        assert_eq!(stem("rapida"), Some("rapid"));
        assert_eq!(stem("rapidos"), Some("rapid"));
        assert_eq!(stem("carta"), Some("cart"));
        assert_eq!(stem("cartera"), Some("carter"));
        assert_eq!(stem("ojo"), None);
        assert!(!same_stem("cartera", "cart"));
        assert!(same_stem("cartas", "cart"));
    }
}
