//! Unicode simple case folding (#5132): the CaseFolding.txt mappings of
//! status C and S, from the generated [`table`]. R-AR-READ's Snapshot
//! duplicate and file/directory rules compare names after NFC and this
//! folding; it maps one code point to one code point, so `ß` and `ẞ` fold
//! together while `ß` and `ss` do not, and the Turkic `ı` stays apart from
//! `i`.

pub mod table;

/// The simple case folding of `c` (itself when CaseFolding.txt has no C or
/// S mapping for it).
#[must_use]
pub fn simple_fold(c: char) -> char {
    let _ = c;
    todo!("#5132 simple_fold")
}

/// [`simple_fold`] of every character of `text`.
#[must_use]
pub fn simple_fold_str(text: &str) -> String {
    let _ = text;
    todo!("#5132 simple_fold_str")
}

#[cfg(test)]
mod tests_casefold;
