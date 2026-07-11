//! Filter: count + compact under a threshold. The DB kernel
//! (SIGMOD '15 §4). Selectivity decides which shape wins.

/// Rung 1 (PROVIDED): branchy. Mispredicts near 50% selectivity.
pub fn compact_branchy(vals: &[f32], t: f32, out: &mut Vec<f32>) {
    out.clear();
    for &v in vals {
        if v < t {
            out.push(v);
        }
    }
}

/// Rung 2 (PROVIDED): branchless append — always store, advance the
/// cursor by the predicate. Data-independent control flow.
pub fn compact_branchless(vals: &[f32], t: f32, out: &mut Vec<f32>) {
    out.clear();
    out.resize(vals.len(), 0.0);
    let mut k = 0usize;
    for &v in vals {
        out[k] = v;
        k += (v < t) as usize;
    }
    out.truncate(k);
}

/// PROVIDED: branchy count (the oracle for count kernels).
pub fn count_branchy(vals: &[f32], t: f32) -> usize {
    vals.iter().filter(|&&v| v < t).count()
}

/// Rung 3 (YOURS): NEON count.
///
/// Per 4 lanes: `vcltq_f32(v, vdupq_n_f32(t))` → 0xFFFFFFFF/0 lanes.
/// Accumulate lane-wise into a `uint32x4_t` counter by SUBTRACTING
/// the mask (0xFFFFFFFF == -1, so `vsubq_u32(counts, mask)` adds 1
/// per set lane) — one instruction, no narrowing needed per iter.
/// Reduce with `vaddvq_u32` at the end; scalar remainder.
///
/// (The vshrn movemask idiom from memchr vector.rs:322-328 is the
/// alternative when you need bit POSITIONS, not just a count — you
/// will need it in compact_neon below.)
#[cfg(target_arch = "aarch64")]
pub fn count_neon(vals: &[f32], t: f32) -> usize {
    todo!("vcltq_f32 + vsubq_u32 mask accumulation + vaddvq_u32")
}

/// Rung 4 (YOURS): NEON LUT-compress compact — simdjson's missing-
/// vpcompress emulation (arm64/simd.h:267-276), f32 edition.
///
/// Per 4 lanes:
///   1. mask4 = 4-bit mask from `vcltq_f32` (narrow the 4×32-bit
///      lanes: `vmovn_u32` → u16x4, reinterpret u64, collect one bit
///      per lane — or the vshrn trick).
///   2. `TABLE[mask4]` = precomputed `[u8; 16]` byte-shuffle indices
///      that gather the selected 4-byte lanes to the front
///      (build all 16 entries in a `const fn` or `once` init).
///   3. `vqtbl1q_u8(bytes_of_v, shuffle)` compresses; `vst1q` stores
///      16 bytes UNCONDITIONALLY; advance out-ptr by popcount(mask4).
///      Over-write, under-advance — trailing garbage is overwritten
///      by the next store and truncated at the end (simdjson's
///      flatten_bits shape).
/// Output must EXACTLY match compact_branchy.
#[cfg(target_arch = "aarch64")]
pub fn compact_neon(vals: &[f32], t: f32, out: &mut Vec<f32>) {
    todo!("4-bit mask → shuffle LUT → vqtbl1q_u8 → advance by popcount")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gen_f32, threshold_for_selectivity};

    #[test]
    fn branchless_matches_branchy() {
        for pct in [1u32, 25, 50, 75, 99] {
            let vals = gen_f32(10_003, pct as u64);
            let t = threshold_for_selectivity(pct);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            compact_branchy(&vals, t, &mut a);
            compact_branchless(&vals, t, &mut b);
            assert_eq!(a, b);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_count_matches() {
        for pct in [1u32, 25, 50, 75, 99] {
            let vals = gen_f32(10_003, 100 + pct as u64);
            let t = threshold_for_selectivity(pct);
            assert_eq!(count_neon(&vals, t), count_branchy(&vals, t));
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_compact_matches() {
        for pct in [1u32, 25, 50, 75, 99] {
            let vals = gen_f32(10_003, 200 + pct as u64);
            let t = threshold_for_selectivity(pct);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            compact_branchy(&vals, t, &mut a);
            compact_neon(&vals, t, &mut b);
            assert_eq!(a, b, "pct={pct}");
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_compact_all_16_masks() {
        // 4 elements exercise every mask pattern deterministically
        for m in 0u32..16 {
            let vals: Vec<f32> =
                (0..4).map(|i| if m >> i & 1 == 1 { 0.25 } else { 0.75 }).collect();
            let (mut a, mut b) = (Vec::new(), Vec::new());
            compact_branchy(&vals, 0.5, &mut a);
            compact_neon(&vals, 0.5, &mut b);
            assert_eq!(a, b, "mask={m:04b}");
        }
    }
}
