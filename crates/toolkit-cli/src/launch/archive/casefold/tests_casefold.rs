//! #5132: the generated simple case folding table and its lookup.

use super::table::{SIMPLE_CASE_FOLDING, UNICODE_VERSION};
use super::*;

#[test]
fn table_is_unicode_17_status_c_and_s_sorted_and_closed() {
    assert_eq!(UNICODE_VERSION, "17.0.0");
    assert_eq!(SIMPLE_CASE_FOLDING.len(), 1512);
    for pair in SIMPLE_CASE_FOLDING.windows(2) {
        assert!(pair[0].0 < pair[1].0, "sorted and unique: {pair:x?}");
    }
    for (from, to) in SIMPLE_CASE_FOLDING {
        assert_ne!(from, to);
        let from = char::from_u32(from).expect("scalar");
        let to = char::from_u32(to).expect("scalar");
        assert_eq!(simple_fold(from), to, "{from:?}");
        // A folded code point is its own folding.
        assert_eq!(simple_fold(to), to, "{to:?}");
    }
}

#[test]
fn simple_fold_known_values() {
    for (from, to) in [
        ('A', 'a'),
        ('z', 'z'),
        ('/', '/'),
        ('\u{1e9e}', '\u{df}'), // ẞ → ß (status S)
        ('\u{df}', '\u{df}'),   // ß has only a full folding (ss): unchanged
        ('\u{212a}', 'k'),      // Kelvin sign (C)
        ('\u{17f}', 's'),       // long s (C)
        ('\u{3a3}', '\u{3c3}'), // Σ → σ
        ('\u{3c2}', '\u{3c3}'), // final ς → σ
        ('I', 'i'),
        ('\u{131}', '\u{131}'),     // dotless ı: no C or S mapping
        ('\u{130}', '\u{130}'),     // İ: only F and T mappings
        ('\u{1c5}', '\u{1c6}'),     // ǅ → ǆ
        ('\u{10400}', '\u{10428}'), // Deseret, outside the BMP
        ('\u{1e921}', '\u{1e943}'), // the last row of the table
        ('\u{10ffff}', '\u{10ffff}'),
    ] {
        assert_eq!(simple_fold(from), to, "{from:?}");
    }
}

#[test]
fn simple_fold_str_maps_each_character_once() {
    assert_eq!(
        simple_fold_str("Stra\u{1e9e}E/\u{df}"),
        "stra\u{df}e/\u{df}"
    );
    assert_eq!(simple_fold_str("\u{131}I\u{130}"), "\u{131}i\u{130}");
    assert_eq!(simple_fold_str(""), "");
    // One code point to one code point: never longer in characters.
    let text = "\u{df}\u{1e9e}\u{130}\u{149}\u{1f80}";
    assert_eq!(simple_fold_str(text).chars().count(), text.chars().count());
}
