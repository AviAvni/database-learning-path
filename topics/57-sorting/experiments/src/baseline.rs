//! Lane 1 (provided) — comparison sorts over row structs, and the checks the
//! bench uses to keep every lane honest.

use crate::collation::collate_cmp;
use crate::{mix, IntStr, Row64, StrSet};
use std::cmp::Ordering;

/// `sort_unstable_by` with a comparator over the row struct. Rust's
/// `sort_unstable` is ipnsort (see `reading-rust-sort.md`).
pub fn sort_u64_comparator(rows: &mut [Row64]) {
    rows.sort_unstable_by(|a, b| a.key.cmp(&b.key));
}

/// The same with the stable `sort_by` (driftsort).
pub fn sort_u64_stable_comparator(rows: &mut [Row64]) {
    rows.sort_by(|a, b| a.key.cmp(&b.key));
}

/// `(a, string)`: compare the integer, and only on a tie chase the string
/// pointer into the arena.
pub fn sort_int_str_comparator(rows: &mut [IntStr], arena: &[u8]) {
    rows.sort_unstable_by(|x, y| x.a.cmp(&y.a).then_with(|| x.string(arena).cmp(y.string(arena))));
}

/// Collation evaluated inside the comparator: every comparison re-decodes both
/// strings and re-looks-up their weights. Returns the row indices in order.
pub fn sort_collating_in_comparator(set: &StrSet) -> Vec<u32> {
    let mut idx: Vec<u32> = (0..set.len() as u32).collect();
    idx.sort_unstable_by(|&i, &j| collate_cmp(set.get(set.rows()[i as usize]), set.get(set.rows()[j as usize])));
    idx
}

// ---- verification helpers ----------------------------------------------

pub fn is_sorted_u64(v: &[Row64]) -> bool {
    v.windows(2).all(|w| w[0].key <= w[1].key)
}

/// Order-independent checksum of the multiset of rows.
pub fn checksum_u64(v: &[Row64]) -> u64 {
    v.iter().fold(0u64, |s, r| s.wrapping_add(mix(r.key, r.payload)))
}

pub fn is_sorted_int_str(v: &[IntStr], arena: &[u8]) -> bool {
    v.windows(2).all(|w| {
        w[0].a.cmp(&w[1].a).then_with(|| w[0].string(arena).cmp(w[1].string(arena))) != Ordering::Greater
    })
}

pub fn checksum_int_str(v: &[IntStr]) -> u64 {
    v.iter().fold(0u64, |s, r| s.wrapping_add(mix(r.a as u32 as u64, r.off as u64)))
}

/// `order` is a permutation of `0..n` and the strings it lists are in
/// collation order.
pub fn is_collation_sorted(set: &StrSet, order: &[u32]) -> bool {
    if order.len() != set.len() {
        return false;
    }
    let mut seen = vec![false; order.len()];
    for &i in order {
        if seen[i as usize] {
            return false;
        }
        seen[i as usize] = true;
    }
    order.windows(2).all(|w| {
        collate_cmp(set.get(set.rows()[w[0] as usize]), set.get(set.rows()[w[1] as usize])) != Ordering::Greater
    })
}

/// Number of distinct collation classes (strings that collate equal) in a
/// sorted order — a seeded, machine-independent count.
pub fn collation_classes(set: &StrSet, order: &[u32]) -> usize {
    if order.is_empty() {
        return 0;
    }
    1 + order
        .windows(2)
        .filter(|w| {
            collate_cmp(set.get(set.rows()[w[0] as usize]), set.get(set.rows()[w[1] as usize])) != Ordering::Equal
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::*;

    #[test]
    fn comparator_sorts_u64() {
        let mut v = gen_u64(50_000, 5);
        let c = checksum_u64(&v);
        sort_u64_comparator(&mut v);
        assert!(is_sorted_u64(&v));
        assert_eq!(checksum_u64(&v), c);
        let mut w = gen_u64(50_000, 5);
        sort_u64_stable_comparator(&mut w);
        assert!(is_sorted_u64(&w));
    }

    #[test]
    fn comparator_sorts_int_str() {
        for shape in [StrShape::Random, StrShape::SharedPrefix20] {
            let mut d = gen_int_str(20_000, 100, shape, 6);
            let c = checksum_int_str(&d.rows);
            sort_int_str_comparator(&mut d.rows, &d.arena);
            assert!(is_sorted_int_str(&d.rows, &d.arena));
            assert_eq!(checksum_int_str(&d.rows), c);
        }
    }

    #[test]
    fn collating_comparator_sorts_strings() {
        let s = gen_collation_strings(20_000, 7);
        let order = sort_collating_in_comparator(&s);
        assert!(is_collation_sorted(&s, &order));
        assert!(collation_classes(&s, &order) < s.len());
    }
}
