//! Lane 2, part 1 (EXERCISE) — normalized keys.
//!
//! A **normalized key** is a byte string whose `memcmp` order equals the order
//! of the original multi-column key. Build it column by column:
//!
//! * a signed integer: write it big-endian (most significant byte first, so
//!   `memcmp` sees the high bytes first) and flip the sign bit (so negatives,
//!   whose two's-complement top bit is 1, sort before positives);
//! * an unsigned integer: big-endian;
//! * a string: its first [`PREFIX`] bytes, zero-padded when shorter. A prefix
//!   decides the order only when the prefixes differ — when they are equal and
//!   a string is longer than the prefix, the full strings must be compared.
//!   Strings in this lane never contain a `0x00` byte, so the zero padding
//!   cannot collide with real data (a real engine must escape or length-tag
//!   it; DuckDB's variable-size sort keys do).
//!
//! This lane's key is `(a: i32 ASC, s: string ASC)`, no NULLs, no DESC.
//! Extensions you will want afterwards: DESC (invert every byte of the
//! column), NULLS FIRST/LAST (one marker byte per column), floats (flip the
//! sign bit of positives, all bits of negatives).

/// Bytes of the string that go into the key.
pub const PREFIX: usize = 12;
/// 4 bytes of `a` + [`PREFIX`] bytes of string.
pub const KEY_LEN: usize = 4 + PREFIX;

/// Big-endian with the sign bit flipped: `encode_i32(a) < encode_i32(b)` (as
/// byte slices) exactly when `a < b`.
pub fn encode_i32(x: i32) -> [u8; 4] {
    let _ = x;
    todo!("lane 2: x as u32 ^ 0x8000_0000, to big-endian bytes")
}

/// Big-endian.
pub fn encode_u64(x: u64) -> [u8; 8] {
    let _ = x;
    todo!("lane 2: x.to_be_bytes()")
}

/// First [`PREFIX`] bytes of `s`, zero-padded.
pub fn encode_str_prefix(s: &[u8]) -> [u8; PREFIX] {
    let _ = s;
    todo!("lane 2: copy min(len, PREFIX) bytes, leave the rest zero")
}

/// The whole key for one `(a, string)` row.
pub fn encode_int_str(a: i32, s: &[u8]) -> [u8; KEY_LEN] {
    let _ = (a, s);
    todo!("lane 2: encode_i32(a) followed by encode_str_prefix(s)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_integers_order_like_their_values() {
        let v = [i32::MIN, -1_000_000, -2, -1, 0, 1, 2, 255, 256, 65_536, i32::MAX];
        for w in v.windows(2) {
            assert!(encode_i32(w[0]) < encode_i32(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(encode_i32(0), [0x80, 0, 0, 0]);
        assert_eq!(encode_i32(-1), [0x7F, 0xFF, 0xFF, 0xFF]);
        assert_eq!(encode_i32(1), [0x80, 0, 0, 1]);
    }

    #[test]
    fn unsigned_integers_are_big_endian() {
        assert_eq!(encode_u64(1), [0, 0, 0, 0, 0, 0, 0, 1]);
        assert!(encode_u64(255) < encode_u64(256));
        assert!(encode_u64(u64::MAX - 1) < encode_u64(u64::MAX));
    }

    #[test]
    fn string_prefix_pads_and_truncates() {
        assert_eq!(encode_str_prefix(b"ab"), *b"ab\0\0\0\0\0\0\0\0\0\0");
        assert_eq!(encode_str_prefix(b"abcdefghijklmnop"), *b"abcdefghijkl");
        assert_eq!(encode_str_prefix(b""), [0u8; PREFIX]);
    }

    #[test]
    fn whole_key_orders_by_a_then_string_prefix() {
        let k = |a, s: &[u8]| encode_int_str(a, s);
        assert!(k(-5, b"zzz") < k(3, b"aaa"), "a decides first");
        assert!(k(3, b"aaa") < k(3, b"aab"));
        assert!(k(3, b"ab") < k(3, b"abc"), "a proper prefix sorts first");
        // Strings longer than the prefix tie in the key when their first 12 bytes agree.
        assert_eq!(k(3, b"abcdefghijklX"), k(3, b"abcdefghijklY"));
    }
}
