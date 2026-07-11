//! Dot product: the reduction kernel. Four rungs.
//!
//! The lesson (README §1): lanes don't make you fast, independent
//! dependency chains do. M-series wants ~12 FMA chains in flight;
//! one accumulator uses 1/12 of the machine.

/// Rung 1 (PROVIDED): one accumulator = one serial dependency chain.
/// LLVM cannot vectorize this without -ffast-math (float
/// reassociation changes the answer).
pub fn dot_naive(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        sum += a[i] * b[i];
    }
    sum
}

/// Rung 2 (PROVIDED): 8 explicit accumulators. The reassociation is
/// now in the SOURCE, so LLVM is free to keep 8 chains and often
/// autovectorizes the inner ops too.
pub fn dot_unrolled8(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut acc = [0.0f32; 8];
    let chunks = a.len() / 8;
    for c in 0..chunks {
        let base = c * 8;
        for l in 0..8 {
            acc[l] += a[base + l] * b[base + l];
        }
    }
    let mut sum: f32 = acc.iter().sum();
    for i in chunks * 8..a.len() {
        sum += a[i] * b[i];
    }
    sum
}

/// Rung 3 (YOURS): portable SIMD via the `wide` crate.
///
/// Use `wide::f32x4` with FOUR independent accumulator vectors
/// (16 partial sums total). Process 16 elements per iteration via
/// `chunks_exact(16)`, `f32x4::from(&chunk[0..4])` loads, and
/// `mul_add`. Horizontal-reduce the four vectors only at the end
/// (`reduce_add`), then fold the remainder scalar.
pub fn dot_wide(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    todo!("wide::f32x4, 4 accumulator vectors, reduce at the end")
}

/// Rung 4 (YOURS): NEON intrinsics.
///
/// `core::arch::aarch64`: four `float32x4_t` accumulators
/// (`vdupq_n_f32(0.0)`), loop 16 elements per iteration with
/// `vld1q_f32` + `vfmaq_f32`, combine with `vaddq_f32`, reduce with
/// `vaddvq_f32`, scalar remainder. Mark the inner fn
/// `#[target_feature(enable = "neon")]` or rely on aarch64 baseline.
///
/// SimSIMD's version upcasts to f64 for accuracy (dot/neon.h:126);
/// here stay in f32 and OBSERVE the error vs dot_naive instead.
#[cfg(target_arch = "aarch64")]
pub fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    todo!("vfmaq_f32 with 4 accumulators, vaddvq_f32 reduce")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen_f32;

    fn rel_err(x: f32, y: f32) -> f32 {
        (x - y).abs() / y.abs().max(1e-6)
    }

    #[test]
    fn provided_rungs_agree() {
        let a = gen_f32(10_001, 1);
        let b = gen_f32(10_001, 2);
        // different summation orders → small float divergence allowed
        assert!(rel_err(dot_unrolled8(&a, &b), dot_naive(&a, &b)) < 1e-2);
    }

    #[test]
    fn wide_matches() {
        let a = gen_f32(10_001, 3);
        let b = gen_f32(10_001, 4);
        assert!(rel_err(dot_wide(&a, &b), dot_naive(&a, &b)) < 1e-2);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_matches() {
        let a = gen_f32(10_001, 5);
        let b = gen_f32(10_001, 6);
        assert!(rel_err(dot_neon(&a, &b), dot_naive(&a, &b)) < 1e-2);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_handles_short_and_remainder() {
        for n in [0usize, 1, 3, 15, 16, 17, 31] {
            let a = gen_f32(n, 7);
            let b = gen_f32(n, 8);
            let expect = dot_naive(&a, &b);
            let got = dot_neon(&a, &b);
            assert!((got - expect).abs() < 1e-3, "n={n}: {got} vs {expect}");
        }
    }
}
