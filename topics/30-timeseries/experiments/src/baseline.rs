//! Baselines — PROVIDED. What Gorilla must beat: raw 16 B/sample, and
//! the obvious first ideas, one per column of the sample:
//!
//!   timestamps  zigzag-varint deltas (`encode_timestamps`)
//!   values      either raw LE f64 (`delta_varint_encode`) or XOR with the
//!               previous value with the leading zero BYTES dropped
//!               (`delta_xor_encode`) — a byte-aligned cousin of Gorilla's
//!               XOR step with no bit-level window reuse, so Gorilla's
//!               bit-granular tricks stay the exercise.
//!
//! VictoriaMetrics's nearest_delta2.go:15 is the timestamp idea taken to
//! delta-of-delta + varint, batched; prometheus went bit-granular instead.

/// Zigzag: map signed to unsigned so small magnitudes stay small.
pub fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

pub fn write_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(b);
            return;
        }
        buf.push(b | 0x80);
    }
}

pub fn read_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

/// Timestamp stream: count, then first + zigzag-varint deltas.
pub fn encode_timestamps(buf: &mut Vec<u8>, ts: &[i64]) {
    write_varint(buf, ts.len() as u64);
    let mut prev = 0i64;
    for (i, &t) in ts.iter().enumerate() {
        let d = if i == 0 { t } else { t - prev };
        write_varint(buf, zigzag(d));
        prev = t;
    }
}

pub fn decode_timestamps(buf: &[u8], pos: &mut usize) -> Vec<i64> {
    let n = read_varint(buf, pos) as usize;
    let mut ts = Vec::with_capacity(n);
    let mut prev = 0i64;
    for i in 0..n {
        let d = unzigzag(read_varint(buf, pos));
        prev = if i == 0 { d } else { prev + d };
        ts.push(prev);
    }
    ts
}

/// Value stream, byte-aligned XOR: each value is XORed with its
/// predecessor (the first with 0), the leading zero BYTES of the result are
/// dropped, and the count of kept bytes (0..=8) goes in a 4-bit tag. Layout:
/// all tags first (two per byte), then the kept low bytes, little-endian.
/// A repeated value costs one tag nibble (0.5 B); a value that shares only
/// its sign/exponent bits with its predecessor costs 6-8 B. It sees leading
/// zeros only — trailing zeros (an integer-valued double has ~40 of them)
/// are kept as payload, which is exactly what Gorilla's window reuse fixes.
pub fn xor_bytes_encode(vs: &[f64]) -> Vec<u8> {
    let mut tags = vec![0u8; (vs.len() + 1) / 2];
    let mut payload = Vec::with_capacity(vs.len() * 2);
    let mut prev = 0u64;
    for (i, &v) in vs.iter().enumerate() {
        let x = v.to_bits() ^ prev;
        prev = v.to_bits();
        let keep = 8 - (x.leading_zeros() / 8) as usize; // 0 when x == 0
        tags[i / 2] |= (keep as u8) << ((i % 2) * 4);
        payload.extend_from_slice(&x.to_le_bytes()[..keep]);
    }
    tags.extend_from_slice(&payload);
    tags
}

pub fn xor_bytes_decode(buf: &[u8], n: usize) -> Vec<f64> {
    let mut pos = (n + 1) / 2;
    let mut out = Vec::with_capacity(n);
    let mut prev = 0u64;
    for i in 0..n {
        let keep = ((buf[i / 2] >> ((i % 2) * 4)) & 0xf) as usize;
        let mut b = [0u8; 8];
        b[..keep].copy_from_slice(&buf[pos..pos + keep]);
        pos += keep;
        prev ^= u64::from_le_bytes(b);
        out.push(f64::from_bits(prev));
    }
    out
}

/// Timestamps as first + zigzag-varint deltas; values as raw LE f64.
pub fn delta_varint_encode(ts: &[i64], vs: &[f64]) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_timestamps(&mut buf, ts);
    for &v in vs {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

pub fn delta_varint_decode(buf: &[u8]) -> (Vec<i64>, Vec<f64>) {
    let mut pos = 0;
    let ts = decode_timestamps(buf, &mut pos);
    let n = ts.len();
    let mut vs = Vec::with_capacity(n);
    for _ in 0..n {
        let mut b = [0u8; 8];
        b.copy_from_slice(&buf[pos..pos + 8]);
        vs.push(f64::from_le_bytes(b));
        pos += 8;
    }
    (ts, vs)
}

/// Timestamps as above; values through `xor_bytes_encode`.
pub fn delta_xor_encode(ts: &[i64], vs: &[f64]) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_timestamps(&mut buf, ts);
    buf.extend_from_slice(&xor_bytes_encode(vs));
    buf
}

pub fn delta_xor_decode(buf: &[u8]) -> (Vec<i64>, Vec<f64>) {
    let mut pos = 0;
    let ts = decode_timestamps(buf, &mut pos);
    let vs = xor_bytes_decode(&buf[pos..], ts.len());
    (ts, vs)
}

pub fn raw_size(n: usize) -> usize {
    n * 16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::*;

    #[test]
    fn zigzag_roundtrip() {
        for v in [0, 1, -1, 63, -64, i64::MAX, i64::MIN] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }

    #[test]
    fn xor_bytes_roundtrip_every_shape() {
        let n = 20_001; // odd on purpose: the last tag byte is half full
        let ts = scrape_timestamps(n, 1_700_000_000_000, 10_000, 50, 5);
        let mut odd = gauge_values(n, 8);
        odd[10] = f64::NAN;
        odd[11] = -0.0;
        odd[12] = f64::INFINITY;
        odd[13] = f64::from_bits(1); // subnormal
        for vs in [constant_values(n), gauge_values(n, 6), counter_values(n, 7), random_values(n, 9), odd] {
            let buf = delta_xor_encode(&ts, &vs);
            let (t2, v2) = delta_xor_decode(&buf);
            assert_eq!(ts, t2);
            assert!(vs.iter().zip(&v2).all(|(a, b)| a.to_bits() == b.to_bits()));
        }
    }

    #[test]
    fn xor_bytes_separates_the_shapes() {
        let n = 20_000;
        let per = |vs: &[f64]| xor_bytes_encode(vs).len() as f64 / n as f64;
        let constant = per(&constant_values(n));
        let counter = per(&counter_values(n, 2));
        let random = per(&random_values(n, 3));
        assert!(constant < 0.6, "constant {constant}");
        assert!(counter < random, "counter {counter} vs random {random}");
        assert!(random > 7.0, "full-entropy values must not compress: {random}");
    }

    #[test]
    fn delta_varint_roundtrip() {
        let ts = scrape_timestamps(10_000, 1_700_000_000_000, 10_000, 50, 5);
        let vs = gauge_values(10_000, 6);
        let buf = delta_varint_encode(&ts, &vs);
        let (t2, v2) = delta_varint_decode(&buf);
        assert_eq!(ts, t2);
        assert_eq!(vs, v2);
        // values dominate: ~8 B/sample of the ~11 B/sample total
        assert!(buf.len() < raw_size(10_000) * 3 / 4);
    }
}
