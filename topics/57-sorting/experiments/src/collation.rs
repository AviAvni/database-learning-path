//! A toy three-level collation in the shape of UTS #10 (the Unicode Collation
//! Algorithm), and the two ways to sort under it.
//!
//! **The collation.** Every supported character maps to one *collation
//! element* of three weights — primary (which letter), secondary (which
//! accent) and tertiary (which case) — modelled on the DUCET's `[.p.0020.0002]`
//! shape. Two strings compare by their primary weights first; only if those
//! are equal in every position and in length does the secondary level decide,
//! and only then the tertiary one (UTS #10 §1.1, §7.4). So `cafe < Cafe <
//! café < Café`: accents outrank case. It is a *toy*: Latin letters, digits
//! and space, no contractions, no expansions, no variable weighting, no
//! backwards French accents. The real tables are the DUCET (`allkeys.txt`)
//! and CLDR tailorings.
//!
//! **Provided:** [`ce`], [`collate_cmp`] — the comparison evaluated on the fly,
//! decoding characters and looking weights up on every call.
//!
//! **Exercise (lane 3):** [`sort_key`] and [`sort_by_sort_keys`] — evaluate the
//! collation once per string into a byte string whose `memcmp` order is the
//! collation order (UTS #10 §7.3), then sort bytes.

use crate::StrSet;
use std::cmp::Ordering;
use std::sync::OnceLock;

/// One collation element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ce {
    pub p: u16,
    pub s: u16,
    pub t: u16,
}

/// Secondary weight of an unaccented letter (DUCET's `0020`).
pub const SEC_BASE: u16 = 0x0020;
/// Tertiary weights: lowercase `0002`, uppercase `0008`, as in the DUCET.
pub const TER_LOWER: u16 = 0x0002;
pub const TER_UPPER: u16 = 0x0008;

/// `(accented lowercase letter, base letter, accent id)`. The accent id is
/// added to [`SEC_BASE`]: 1 acute, 2 grave, 3 circumflex, 4 diaeresis,
/// 5 tilde, 6 cedilla.
pub const ACCENTED: &[(char, char, u16)] = &[
    ('á', 'a', 1), ('à', 'a', 2), ('â', 'a', 3), ('ä', 'a', 4),
    ('é', 'e', 1), ('è', 'e', 2), ('ê', 'e', 3), ('ë', 'e', 4),
    ('í', 'i', 1), ('ì', 'i', 2), ('î', 'i', 3), ('ï', 'i', 4),
    ('ó', 'o', 1), ('ò', 'o', 2), ('ô', 'o', 3), ('ö', 'o', 4),
    ('ú', 'u', 1), ('ù', 'u', 2), ('û', 'u', 3), ('ü', 'u', 4),
    ('ñ', 'n', 5), ('ç', 'c', 6),
];

/// The accented lowercase forms of a base letter (empty for most letters).
pub fn accented_forms(base: char) -> Vec<char> {
    ACCENTED.iter().filter(|(_, b, _)| *b == base).map(|(c, _, _)| *c).collect()
}

fn primary_of_letter(l: char) -> u16 {
    0x0400 + 0x10 * (l as u16 - 'a' as u16)
}

fn table() -> &'static [Ce; 256] {
    static T: OnceLock<[Ce; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let unassigned = |c: u32| Ce { p: 0xF000 + (c as u16 & 0x0FFF), s: SEC_BASE, t: TER_LOWER };
        let mut t = [Ce { p: 0, s: 0, t: 0 }; 256];
        for (i, e) in t.iter_mut().enumerate() {
            *e = unassigned(i as u32);
        }
        t[b' ' as usize] = Ce { p: 0x0200, s: SEC_BASE, t: TER_LOWER };
        for d in 0..10u16 {
            t[(b'0' as u16 + d) as usize] = Ce { p: 0x0300 + 0x10 * d, s: SEC_BASE, t: TER_LOWER };
        }
        for l in 'a'..='z' {
            let p = primary_of_letter(l);
            t[l as usize] = Ce { p, s: SEC_BASE, t: TER_LOWER };
            t[l.to_ascii_uppercase() as usize] = Ce { p, s: SEC_BASE, t: TER_UPPER };
        }
        for &(c, base, acc) in ACCENTED {
            let p = primary_of_letter(base);
            t[c as usize] = Ce { p, s: SEC_BASE + acc, t: TER_LOWER };
            let up = c.to_uppercase().next().unwrap();
            t[up as usize] = Ce { p, s: SEC_BASE + acc, t: TER_UPPER };
        }
        t
    })
}

/// The collation element of one character. Characters outside the supported
/// Latin-1 repertoire get an "unassigned" element that sorts after everything
/// else, by code point (a toy stand-in for UTS #10 §10.1.3's implicit weights).
#[inline]
pub fn ce(c: char) -> Ce {
    let cp = c as u32;
    if cp < 256 {
        table()[cp as usize]
    } else {
        Ce { p: 0xF000 + (cp as u16 & 0x0FFF), s: SEC_BASE, t: TER_LOWER }
    }
}

/// Compare two strings under the collation, evaluated on the fly: three
/// passes over the characters, each decoding UTF-8 and looking up a weight.
/// This is what a comparison sort pays on *every* comparison when the
/// collation lives inside the comparator.
pub fn collate_cmp(a: &str, b: &str) -> Ordering {
    // Level 1: primary weights, position by position; a proper prefix is smaller.
    let (mut ia, mut ib) = (a.chars(), b.chars());
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = ce(x).p.cmp(&ce(y).p);
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
    // The strings now have the same number of characters. Level 2: accents.
    for (x, y) in a.chars().zip(b.chars()) {
        let o = ce(x).s.cmp(&ce(y).s);
        if o != Ordering::Equal {
            return o;
        }
    }
    // Level 3: case.
    for (x, y) in a.chars().zip(b.chars()) {
        let o = ce(x).t.cmp(&ce(y).t);
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

// ---------------------------------------------------------------------------
// Lane 3 — EXERCISE
// ---------------------------------------------------------------------------

/// Build the binary sort key of `s` (UTS #10 §7.3, Step 3):
///
/// ```text
/// all primary weights  |  0000  |  all secondary weights  |  0000  |  all tertiary weights
/// ```
///
/// every weight written as a **big-endian `u16`**, so that a byte-wise
/// `memcmp` of two keys equals [`collate_cmp`] of the two strings. The `0000`
/// level separator is lower than every real weight, which is what makes a
/// proper prefix sort first.
///
/// Worked example — `sort_key("ab")` is the 14 bytes
/// `04 00 04 10 | 00 00 | 00 20 00 20 | 00 00 | 00 02 00 02`.
pub fn sort_key(s: &str) -> Vec<u8> {
    let _ = s;
    todo!("lane 3: append p, 0000, s, 0000, t weights of every character, big-endian")
}

/// Sort the strings of `set` under the collation by **computing every sort key
/// once** and then comparing bytes. Returns the row indices (`0..set.len()`)
/// in collation order; rows that collate equal may appear in either order.
///
/// How you store the keys (one `Vec<u8>` each, one contiguous buffer plus
/// offsets, fixed-width prefixes plus a tie-break) is the exercise.
pub fn sort_by_sort_keys(set: &StrSet) -> Vec<u32> {
    let _ = set;
    todo!("lane 3: build all sort keys once, sort row indices by memcmp of the keys")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::gen_collation_strings;

    // ---- provided: the comparison itself ------------------------------

    #[test]
    fn accents_outrank_case() {
        let order = ["cafe", "Cafe", "café", "Café"];
        for w in order.windows(2) {
            assert_eq!(collate_cmp(w[0], w[1]), Ordering::Less, "{} < {}", w[0], w[1]);
        }
    }

    #[test]
    fn primary_beats_everything_and_prefix_is_smaller() {
        assert_eq!(collate_cmp("Zebra", "apple"), Ordering::Greater);
        assert_eq!(collate_cmp("ÉCOLE", "ecole"), Ordering::Greater);
        assert_eq!(collate_cmp("école", "ecolf"), Ordering::Less);
        assert_eq!(collate_cmp("ab", "abc"), Ordering::Less);
        assert_eq!(collate_cmp("ab", "ab"), Ordering::Equal);
    }

    #[test]
    fn comparison_is_a_total_preorder_on_generated_strings() {
        let s = gen_collation_strings(300, 1);
        let v: Vec<&str> = s.rows().iter().map(|r| s.get(*r)).collect();
        for a in &v {
            for b in &v {
                let ab = collate_cmp(a, b);
                assert_eq!(ab, collate_cmp(b, a).reverse());
            }
        }
    }

    // ---- exercise: the sort key --------------------------------------

    #[test]
    fn sort_key_layout_matches_the_worked_example() {
        assert_eq!(
            sort_key("ab"),
            vec![0x04, 0x00, 0x04, 0x10, 0, 0, 0x00, 0x20, 0x00, 0x20, 0, 0, 0x00, 0x02, 0x00, 0x02]
        );
    }

    #[test]
    fn sort_key_order_equals_collate_cmp() {
        let s = gen_collation_strings(2_000, 2);
        let v: Vec<&str> = s.rows().iter().map(|r| s.get(*r)).collect();
        let keys: Vec<Vec<u8>> = v.iter().map(|t| sort_key(t)).collect();
        for i in 0..v.len() {
            for j in (i % 7..v.len()).step_by(13) {
                assert_eq!(keys[i].cmp(&keys[j]), collate_cmp(v[i], v[j]), "{:?} vs {:?}", v[i], v[j]);
            }
        }
    }

    #[test]
    fn sort_key_ties_exactly_when_collation_ties() {
        // Identical strings tie; strings that differ only in case or accent do not.
        assert_eq!(sort_key("Hello"), sort_key("Hello"));
        assert_ne!(sort_key("hello"), sort_key("Hello"));
        assert_ne!(sort_key("hello"), sort_key("héllo"));
        assert!(sort_key("ab") < sort_key("abc"));
        assert!(sort_key("") < sort_key("a"));
    }

    #[test]
    fn sort_by_sort_keys_returns_a_sorted_permutation() {
        let s = gen_collation_strings(5_000, 3);
        let order = sort_by_sort_keys(&s);
        assert_eq!(order.len(), s.len());
        let mut seen = vec![false; order.len()];
        for &i in &order {
            assert!(!seen[i as usize], "row {i} appears twice");
            seen[i as usize] = true;
        }
        for w in order.windows(2) {
            let (a, b) = (s.get(s.rows()[w[0] as usize]), s.get(s.rows()[w[1] as usize]));
            assert_ne!(collate_cmp(a, b), Ordering::Greater, "{a:?} before {b:?}");
        }
    }

    #[test]
    fn sort_by_sort_keys_handles_empty_and_single() {
        let empty = StrSet::new(vec![], vec![]);
        assert!(sort_by_sort_keys(&empty).is_empty());
        let one = StrSet::new(b"x".to_vec(), vec![crate::StrRef { off: 0, len: 1 }]);
        assert_eq!(sort_by_sort_keys(&one), vec![0]);
    }
}
