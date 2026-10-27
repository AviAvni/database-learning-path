//! A fixed, seedless hash: every partition size and every table slot
//! reproduces exactly from run to run.
//!
//! The split of the 64 hash bits is part of the design, because radix
//! partitioning and the table inside a partition must not read the same bits:
//! partitions are chosen from the **low** bits, the table slot from the
//! **high** bits. Inside one partition all keys share their low bits, so a
//! table indexed by low bits would put the whole partition in one slot.

/// MurmurHash3's 64-bit finaliser applied to the key: every output bit
/// depends on every input bit.
#[inline(always)]
pub fn hash32(key: u32) -> u64 {
    let mut h = key as u64;
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

/// Partition number for `bits` radix bits starting at bit `shift` of the hash
/// (pass 1 uses `shift = 0`; pass 2 continues with `shift = bits_of_pass_1`).
#[inline(always)]
pub fn radix_of(h: u64, shift: u32, bits: u32) -> usize {
    ((h >> shift) & ((1u64 << bits) - 1)) as usize
}

/// Table slot in a power-of-two table of `1 << table_bits` slots: the top bits.
#[inline(always)]
pub fn slot_of(h: u64, table_bits: u32) -> usize {
    (h >> (64 - table_bits)) as usize
}
