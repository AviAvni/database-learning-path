//! 4-bit unpack to u32 — topic 12's decoder, SIMD edition.
//! FastLanes' baby case: at w=4 no value straddles a byte, so even
//! the sequential layout vectorizes with two ops.

/// PROVIDED: scalar unpack (topic 12's shape). Byte b yields
/// [b & 0x0F, b >> 4] — low nibble first.
pub fn unpack4_scalar(packed: &[u8], out: &mut Vec<u32>) {
    out.clear();
    out.reserve(packed.len() * 2);
    for &b in packed {
        out.push((b & 0x0F) as u32);
        out.push((b >> 4) as u32);
    }
}

/// PROVIDED: the round-trip oracle.
pub fn pack4(vals: &[u32]) -> Vec<u8> {
    assert!(vals.len() % 2 == 0);
    vals.chunks_exact(2)
        .map(|p| {
            debug_assert!(p[0] < 16 && p[1] < 16);
            (p[0] as u8) | ((p[1] as u8) << 4)
        })
        .collect()
}

/// YOURS: NEON unpack.
///
/// Per 16 bytes (`vld1q_u8`) = 32 nibbles:
///   lo = `vandq_u8(bytes, vdupq_n_u8(0x0F))`
///   hi = `vshrq_n_u8(bytes, 4)`
/// Then INTERLEAVE (lo0, hi0, lo1, hi1, ...) — `vzipq_u8(lo, hi)`
/// gives the byte order matching unpack4_scalar — and widen
/// u8 → u16 → u32 (`vmovl_u8`, `vmovl_u16`) before `vst1q_u32`.
/// Scalar remainder for len % 16. Output must equal unpack4_scalar.
#[cfg(target_arch = "aarch64")]
pub fn unpack4_neon(packed: &[u8], out: &mut Vec<u32>) {
    todo!("vandq/vshrq nibble split + vzipq interleave + vmovl widen")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen_bytes;

    #[test]
    fn scalar_round_trips() {
        let packed = gen_bytes(1024, 1);
        let mut vals = Vec::new();
        unpack4_scalar(&packed, &mut vals);
        assert_eq!(pack4(&vals), packed);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_matches_scalar() {
        for n in [16usize, 17, 160, 1000, 1024] {
            let packed = gen_bytes(n, n as u64);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            unpack4_scalar(&packed, &mut a);
            unpack4_neon(&packed, &mut b);
            assert_eq!(a, b, "n={n}");
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_round_trips() {
        let packed = gen_bytes(4096, 42);
        let mut vals = Vec::new();
        unpack4_neon(&packed, &mut vals);
        assert_eq!(pack4(&vals), packed);
    }
}
