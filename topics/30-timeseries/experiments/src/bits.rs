//! Bit-level IO — PROVIDED so the Gorilla stub is about the *algorithm*,
//! not bit plumbing. MSB-first within each byte, like the paper and
//! prometheus's chunkenc bstream.

pub struct BitWriter {
    buf: Vec<u8>,
    /// Bits used in the last byte (0..=8; 8 or empty buf means "start a new byte").
    used: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self { buf: Vec::new(), used: 8 }
    }

    pub fn write_bit(&mut self, bit: bool) {
        if self.used == 8 {
            self.buf.push(0);
            self.used = 0;
        }
        if bit {
            let last = self.buf.len() - 1;
            self.buf[last] |= 1 << (7 - self.used);
        }
        self.used += 1;
    }

    /// Write the low `n` bits of `v`, most significant first. n <= 64.
    pub fn write_bits(&mut self, v: u64, n: u8) {
        for i in (0..n).rev() {
            self.write_bit((v >> i) & 1 == 1);
        }
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    pub fn bit_len(&self) -> usize {
        if self.buf.is_empty() {
            0
        } else {
            (self.buf.len() - 1) * 8 + self.used as usize
        }
    }
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct BitReader<'a> {
    buf: &'a [u8],
    pos: usize, // bit position
}

impl<'a> BitReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn read_bit(&mut self) -> bool {
        let byte = self.buf[self.pos / 8];
        let bit = (byte >> (7 - (self.pos % 8))) & 1 == 1;
        self.pos += 1;
        bit
    }

    /// Read `n` bits, most significant first. n <= 64.
    pub fn read_bits(&mut self, n: u8) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | self.read_bit() as u64;
        }
        v
    }
}

/// Sign-extend the low `n` bits of `v` to i64 (for dod buckets).
pub fn sign_extend(v: u64, n: u8) -> i64 {
    let shift = 64 - n;
    ((v << shift) as i64) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_roundtrip() {
        let mut w = BitWriter::new();
        w.write_bit(true);
        w.write_bits(0b1011, 4);
        w.write_bits(0xDEAD_BEEF_CAFE_F00D, 64);
        w.write_bits(7, 3);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(r.read_bit());
        assert_eq!(r.read_bits(4), 0b1011);
        assert_eq!(r.read_bits(64), 0xDEAD_BEEF_CAFE_F00D);
        assert_eq!(r.read_bits(3), 7);
    }

    #[test]
    fn sign_extend_works() {
        assert_eq!(sign_extend(0b1111111, 7), -1);
        assert_eq!(sign_extend(0b0111111, 7), 63);
        assert_eq!(sign_extend(0b1000000, 7), -64);
        assert_eq!(sign_extend(5, 32), 5);
    }
}
