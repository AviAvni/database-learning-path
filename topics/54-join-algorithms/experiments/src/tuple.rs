//! The 8-byte tuple every algorithm joins, and the order-independent result.

/// A `<key, payload>` tuple: 4 + 4 bytes, the narrow shape the main-memory
/// join literature measures (Balkesen et al. §7.1, "4 / 4 bytes"). For R the
/// payload is the row id in R; for S it is the row id in S.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Tuple {
    pub key: u32,
    pub pay: u32,
}

/// What a join returns: how many pairs matched, and an order-independent
/// checksum over the `(R payload, S payload)` pairs so that every algorithm —
/// whatever order it emits in — can be checked against every other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct JoinOutput {
    pub matches: u64,
    pub checksum: u64,
}

impl JoinOutput {
    /// Fold one matched pair in. Addition is commutative, so the final
    /// checksum does not depend on emission order or on how work was split
    /// across threads or partitions.
    #[inline(always)]
    pub fn add(&mut self, r_pay: u32, s_pay: u32) {
        self.matches += 1;
        self.checksum = self.checksum.wrapping_add(pair_mix(r_pay, s_pay));
    }

    /// Fold one pair in if `m` is true, without a branch: the match flag
    /// masks the checksum term. Used where whether a slot matches is
    /// unpredictable (which of a bucket's four slots holds the key).
    #[inline(always)]
    pub fn add_if(&mut self, r_pay: u32, s_pay: u32, m: bool) {
        self.matches += m as u64;
        self.checksum = self.checksum.wrapping_add(pair_mix(r_pay, s_pay) & (m as u64).wrapping_neg());
    }

    /// Combine the outputs of two disjoint pieces of work.
    pub fn merge(&mut self, other: JoinOutput) {
        self.matches += other.matches;
        self.checksum = self.checksum.wrapping_add(other.checksum);
    }
}

#[inline(always)]
fn pair_mix(r_pay: u32, s_pay: u32) -> u64 {
    // one multiply and one fold: cheap enough to run on every slot of a bucket
    let x = (((r_pay as u64) << 32) | s_pay as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^ (x >> 32)
}
