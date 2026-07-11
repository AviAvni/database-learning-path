//! HyperLogLog, dense encoding — redis hyperloglog.c's shape with P=14:
//! 16384 six-bit registers (hyperloglog.c:196-198), standard error
//! 1.04/sqrt(16384) = 0.81%. We skip redis's sparse encoding (the
//! byte-level ZERO/XZERO/VAL opcode dance at :380-383) but the reading
//! guide walks it — it's why a PFCOUNT key starts at ~30 bytes, not 12 KB.

pub const P: u32 = 14;
pub const M: usize = 1 << P; // 16384 registers

pub struct Hll {
    /// One byte per register for simplicity (redis packs 6 bits — :354's
    /// shift dance — trading 4 KB for byte-addressability here is fine).
    pub regs: Vec<u8>,
}

impl Hll {
    pub fn new() -> Hll {
        Hll { regs: vec![0; M] }
    }

    /// STUB — hash the key (crate::hash::splitmix64), then:
    ///   index = low P bits;  rest = remaining 64-P bits
    ///   rank  = leading-zeros-of-rest + 1, computed over the 64-P bit
    ///           window (i.e. (rest << P) then leading_zeros, +1; cap at
    ///           64-P+1)
    ///   regs[index] = max(regs[index], rank)
    pub fn add(&mut self, _key: u64) {
        todo!("index = low P bits, rank = lzcnt of the rest + 1, keep max")
    }

    /// STUB — the Ertl/redis estimator (hllCount, hyperloglog.c:1058):
    /// build reghisto[rank] counts, then
    ///   z = m * tau((m - reghisto[q+1]) / m)
    ///   for j = q..1: z = 0.5*(z + reghisto[j])   (0.5 via ldexp)
    ///   z += m * sigma(reghisto[0] / m)
    ///   E = alpha_inf * m * m / z,  alpha_inf = 0.5 / ln(2)
    /// with tau/sigma as in hyperloglog.c:1016-1052:
    ///   sigma(x): x=1 -> +inf; iterate y=1: x=x², z' = z + x*y, y*=2 until
    ///             z converges (start z=x)
    ///   tau(x): x=0 or 1 -> 0; iterate y=1: x=sqrt(x), y*=0.5,
    ///           z' = z - (1-x)²*y until converges (start z = 1-x); z/3
    /// No empirical bias tables, no linear-counting switch — this
    /// estimator is uniformly good across the whole range (that's the
    /// point of Ertl's derivation; contrast HLL++'s piecewise fixups).
    pub fn count(&self) -> f64 {
        todo!("tau/sigma estimator over the register histogram")
    }

    /// STUB — register-wise max. Union semantics: merge(A,B).count()
    /// estimates |A ∪ B|; this is why HLLs shard perfectly (PFMERGE, and
    /// hllMergeDenseAVX2 at hyperloglog.c:1116 shows redis vectorizing it).
    pub fn merge(&mut self, _other: &Hll) {
        todo!("regs[i] = max(regs[i], other.regs[i])")
    }
}

impl Default for Hll {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(range: std::ops::Range<u64>, stride: u64) -> Hll {
        let mut h = Hll::new();
        for k in range {
            h.add(k * stride + 12345);
        }
        h
    }

    #[test]
    fn error_within_3_percent_across_scales() {
        for &n in &[1_000u64, 100_000, 5_000_000] {
            let h = filled(0..n, 7);
            let est = h.count();
            let err = (est - n as f64).abs() / n as f64;
            assert!(err < 0.03, "n={} est={:.0} err={:.3}", n, est, err);
        }
    }

    #[test]
    fn duplicates_do_not_count() {
        let mut h = Hll::new();
        for _ in 0..100 {
            for k in 0..1000u64 {
                h.add(k);
            }
        }
        let est = h.count();
        assert!((est - 1000.0).abs() / 1000.0 < 0.05, "est {:.0}", est);
    }

    #[test]
    fn merge_equals_union() {
        let a = filled(0..50_000, 3);
        let b = filled(25_000..75_000, 3); // 50% overlap with a
        let mut merged = Hll::new();
        merged.merge(&a);
        merged.merge(&b);
        let mut union = Hll::new();
        for k in 0..75_000u64 {
            union.add(k * 3 + 12345);
        }
        // register-wise max must give IDENTICAL registers, not just close
        assert_eq!(merged.regs, union.regs);
        let err = (merged.count() - 75_000.0).abs() / 75_000.0;
        assert!(err < 0.03, "union est err {:.3}", err);
    }
}
