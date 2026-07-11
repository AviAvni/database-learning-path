//! Baselines — PROVIDED. What Gorilla must beat: raw 16 B/sample, and
//! the obvious first idea (delta + zigzag varint timestamps, raw f64
//! values). VictoriaMetrics's nearest_delta2.go:15 is this idea taken to
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

/// Timestamps as first + zigzag-varint deltas; values as raw LE f64.
pub fn delta_varint_encode(ts: &[i64], vs: &[f64]) -> Vec<u8> {
    let mut buf = Vec::new();
    write_varint(&mut buf, ts.len() as u64);
    let mut prev = 0i64;
    for (i, &t) in ts.iter().enumerate() {
        let d = if i == 0 { t } else { t - prev };
        write_varint(&mut buf, zigzag(d));
        prev = t;
    }
    for &v in vs {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

pub fn delta_varint_decode(buf: &[u8]) -> (Vec<i64>, Vec<f64>) {
    let mut pos = 0;
    let n = read_varint(buf, &mut pos) as usize;
    let mut ts = Vec::with_capacity(n);
    let mut prev = 0i64;
    for i in 0..n {
        let d = unzigzag(read_varint(buf, &mut pos));
        prev = if i == 0 { d } else { prev + d };
        ts.push(prev);
    }
    let mut vs = Vec::with_capacity(n);
    for _ in 0..n {
        let mut b = [0u8; 8];
        b.copy_from_slice(&buf[pos..pos + 8]);
        vs.push(f64::from_le_bytes(b));
        pos += 8;
    }
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
