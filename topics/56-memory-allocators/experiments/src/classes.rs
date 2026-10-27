//! Size classes and page geometry shared by the two stub allocators (PROVIDED).
//!
//! Eight-byte steps up to 64, then four classes per doubling up to 1024 —
//! the same family jemalloc uses (`SC_LG_NGROUP = 2`, four classes per power of
//! two) and close to mimalloc's word-sized bins. Requests above `MAX_SMALL` go
//! to the global allocator in the stubs.

/// Pages are 64 KiB, aligned to 64 KiB, so `p & !(PAGE_SIZE - 1)` is the page
/// header — the same trick mimalloc's `free` plays with 4 MiB segments
/// (tech report §3.2), at a smaller scale.
pub const PAGE_SIZE: usize = 64 * 1024;
pub const PAGE_MASK: usize = !(PAGE_SIZE - 1);
/// Bytes at the start of each page reserved for the page's own bookkeeping.
pub const HEADER: usize = 64;
pub const MAX_SMALL: usize = 1024;

pub const CLASSES: [usize; 24] = [
    8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640,
    768, 896, 1024,
];
pub const NCLASSES: usize = CLASSES.len();

/// Index of the smallest class that fits `size` (1 ..= MAX_SMALL).
pub fn class_of(size: usize) -> usize {
    debug_assert!((1..=MAX_SMALL).contains(&size));
    CLASSES.partition_point(|&c| c < size)
}

pub fn class_size(idx: usize) -> usize {
    CLASSES[idx]
}

/// Blocks of class `idx` that fit in one page after the header.
pub fn blocks_per_page(idx: usize) -> usize {
    (PAGE_SIZE - HEADER) / CLASSES[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_covers_request_with_bounded_waste() {
        for n in 1..=MAX_SMALL {
            let c = class_size(class_of(n));
            assert!(c >= n);
            if n >= 64 {
                assert!(c - n <= n / 4, "n={n} class={c}");
            } else {
                assert_eq!(c % 8, 0);
            }
        }
    }

    #[test]
    fn page_geometry() {
        assert_eq!(PAGE_SIZE.count_ones(), 1);
        assert_eq!(0x1234_5678_9abc_usize & PAGE_MASK, 0x1234_5678_0000);
        assert_eq!(blocks_per_page(class_of(48)), (65536 - 64) / 48);
    }
}
