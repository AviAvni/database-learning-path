//! Lane 2, part 2 (EXERCISE) — radix sort over normalized keys.
//!
//! A **radix sort** sorts by *digits* instead of by comparisons: it distributes
//! the rows into 256 buckets by one byte, then handles the next byte. Two
//! orders of visiting the bytes:
//!
//! * **LSD** (least significant digit first): one stable counting-sort pass per
//!   byte from the last to the first; always `bytes × n` work, no recursion,
//!   needs a second buffer.
//! * **MSD** (most significant digit first): distribute by the first byte, then
//!   recurse into each bucket on the next byte; stops early on small buckets
//!   (fall back to insertion sort) and wastes nothing on bytes that only
//!   distinguish already-separated rows.
//!
//! Both functions sort **in place** from the caller's point of view and must
//! be correct for every input, including `n = 0`, `n = 1`, all-equal keys and
//! already-sorted input. Which algorithm, which fallback threshold and which
//! skip optimisations you use is the exercise. A good first measurement:
//! count how many of the passes over the data do no work because every row
//! fell in one bucket (`u64` keys below 2^20 have five such bytes).

use crate::{IntStr, Row64};

/// Sort `rows` by `key` ascending. Rows with equal keys may end in any order.
pub fn sort_u64(rows: &mut [Row64]) {
    let _ = rows;
    todo!("lane 2: LSD or MSD radix sort of the 8 key bytes (big-endian digit order)")
}

/// Sort `rows` by `(a ascending, string ascending)`, where the string order
/// is plain byte-wise lexicographic with a proper prefix sorting first —
/// exactly `a.cmp(&b.a).then(sa.cmp(sb))` on `&[u8]`.
///
/// Intended design: build the 16-byte normalized key of every row
/// ([`crate::normalize::encode_int_str`]) next to the row's position, radix
/// sort the keys, then finish the groups whose 16-byte keys are equal but
/// whose strings are longer than [`crate::normalize::PREFIX`] with a
/// comparison on the full strings, and finally permute `rows`.
pub fn sort_int_str(rows: &mut [IntStr], arena: &[u8]) {
    let _ = (rows, arena);
    todo!("lane 2: radix sort the normalized keys, tie-break on the full strings")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{gen_int_str, gen_u64, StrShape};

    fn u64_sorted(v: &[Row64]) -> bool {
        v.windows(2).all(|w| w[0].key <= w[1].key)
    }

    fn checksum64(v: &[Row64]) -> u64 {
        v.iter().fold(0u64, |s, r| s.wrapping_add(crate::mix(r.key, r.payload)))
    }

    #[test]
    fn u64_random_sorted_and_a_permutation() {
        for n in [0usize, 1, 2, 3, 17, 255, 256, 257, 1_000, 100_000] {
            let mut v = gen_u64(n, 11);
            let before = checksum64(&v);
            sort_u64(&mut v);
            assert!(u64_sorted(&v), "n = {n}");
            assert_eq!(checksum64(&v), before, "rows were lost or altered, n = {n}");
        }
    }

    #[test]
    fn u64_degenerate_inputs() {
        // all equal
        let mut v: Vec<Row64> = (0..5_000).map(|i| Row64 { key: 42, payload: i }).collect();
        sort_u64(&mut v);
        assert!(v.iter().all(|r| r.key == 42));
        // already sorted, and reverse sorted
        let mut v: Vec<Row64> = (0..5_000).map(|i| Row64 { key: i * 3, payload: i }).collect();
        sort_u64(&mut v);
        assert!(u64_sorted(&v));
        v.reverse();
        sort_u64(&mut v);
        assert!(u64_sorted(&v));
        // keys that differ only in the top byte, and only in the bottom byte
        let mut v: Vec<Row64> = (0..4_000).map(|i| Row64 { key: (i as u64 % 251) << 56, payload: i }).collect();
        sort_u64(&mut v);
        assert!(u64_sorted(&v));
        let mut v: Vec<Row64> = (0..4_000).map(|i| Row64 { key: (i as u64 * 7) % 251, payload: i }).collect();
        sort_u64(&mut v);
        assert!(u64_sorted(&v));
    }

    fn int_str_sorted(v: &[IntStr], arena: &[u8]) -> bool {
        v.windows(2).all(|w| {
            let o = w[0].a.cmp(&w[1].a).then_with(|| w[0].string(arena).cmp(w[1].string(arena)));
            o != std::cmp::Ordering::Greater
        })
    }

    fn checksum_is(v: &[IntStr]) -> u64 {
        v.iter().fold(0u64, |s, r| s.wrapping_add(crate::mix(r.a as u32 as u64, r.off as u64)))
    }

    #[test]
    fn int_str_sorted_for_every_shape() {
        for (distinct, shape) in [
            (1_000u32, StrShape::Random),
            (0, StrShape::Random),
            (1_000, StrShape::SharedPrefix20),
            (1, StrShape::Random),
        ] {
            for n in [0usize, 1, 5, 100, 30_000] {
                let mut d = gen_int_str(n, distinct, shape, 21);
                let before = checksum_is(&d.rows);
                sort_int_str(&mut d.rows, &d.arena);
                assert!(int_str_sorted(&d.rows, &d.arena), "n = {n}, distinct = {distinct}, {shape:?}");
                assert_eq!(checksum_is(&d.rows), before, "rows lost, n = {n}");
            }
        }
    }

    #[test]
    fn int_str_prefix_and_negative_cases() {
        // hand-built: a proper prefix sorts first; strings equal in the first
        // 12 bytes but different after must still be ordered; negatives first.
        let strs: [&[u8]; 6] = [b"abc", b"ab", b"abcdefghijklZ", b"abcdefghijklA", b"abcdefghijkl", b""];
        let mut arena = Vec::new();
        let mut rows = Vec::new();
        for (i, s) in strs.iter().enumerate() {
            for a in [5, -5] {
                let off = arena.len() as u32;
                arena.extend_from_slice(s);
                rows.push(IntStr { a: a + (i as i32 % 2) * 0, off, len: s.len() as u32 });
            }
        }
        sort_int_str(&mut rows, &arena);
        assert!(int_str_sorted(&rows, &arena));
        assert_eq!(rows[0].a, -5);
        assert_eq!(rows[0].string(&arena), b"");
        assert_eq!(rows.last().unwrap().string(&arena), b"abcdefghijklZ");
    }
}
