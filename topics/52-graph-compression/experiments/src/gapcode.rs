//! Gap coding (PROVIDED, lane 1): how many bits an ordering costs.
//!
//! Each adjacency list is sorted, so it is stored as the first target's
//! signed distance from the source (zig-zag mapped) followed by the gaps
//! `t[i] - t[i-1] - 1` (never negative, because lists have no duplicates).
//! Three prices of the same gaps:
//!
//! * `varint` — the LEB128-style byte code of Ligra+'s "byte" scheme: 7 data
//!   bits plus a continue bit per byte. Real bytes, real decoder.
//! * `gamma_bits` — Elias-gamma length of each value+1, an entropy-ish
//!   bit-level price (what a bit-aligned coder like WebGraph's would pay for
//!   gaps with no better code).
//! * `log_gap` — Dhulipala et al.'s LogGap objective: mean of
//!   `1 + floor(log2(delta))` over consecutive-neighbour deltas, the number of
//!   bits needed to write each delta in binary.

use crate::csr::Csr;

#[inline]
pub fn zigzag(x: i64) -> u64 {
    ((x << 1) ^ (x >> 63)) as u64
}

#[inline]
pub fn unzigzag(z: u64) -> i64 {
    ((z >> 1) as i64) ^ -((z & 1) as i64)
}

#[inline]
pub fn put_varint(out: &mut Vec<u8>, mut x: u64) {
    while x >= 0x80 {
        out.push((x as u8 & 0x7f) | 0x80);
        x >>= 7;
    }
    out.push(x as u8);
}

#[inline]
pub fn get_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut x = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        x |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return x;
        }
        shift += 7;
    }
}

/// Gap + varint form of a graph. `off[v]` is a byte offset; degrees are kept
/// beside it (as Ligra+ does) so a list is self-delimiting without a sentinel.
pub struct VarintGraph {
    pub n: usize,
    pub bytes: Vec<u8>,
    pub off: Vec<u32>,
    pub deg: Vec<u32>,
}

impl VarintGraph {
    pub fn encode(g: &Csr) -> VarintGraph {
        let mut bytes = Vec::with_capacity(g.m() * 2);
        let mut off = Vec::with_capacity(g.n);
        let mut deg = Vec::with_capacity(g.n);
        for v in 0..g.n {
            off.push(bytes.len() as u32);
            let l = g.neighbors(v);
            deg.push(l.len() as u32);
            if let Some(&f) = l.first() {
                put_varint(&mut bytes, zigzag(f as i64 - v as i64));
                for w in l.windows(2) {
                    put_varint(&mut bytes, (w[1] - w[0] - 1) as u64);
                }
            }
        }
        VarintGraph { n: g.n, bytes, off, deg }
    }

    pub fn decode_list(&self, v: usize, out: &mut Vec<u32>) {
        out.clear();
        let d = self.deg[v] as usize;
        if d == 0 {
            return;
        }
        let mut pos = self.off[v] as usize;
        let mut cur = (v as i64 + unzigzag(get_varint(&self.bytes, &mut pos))) as u32;
        out.push(cur);
        for _ in 1..d {
            cur += get_varint(&self.bytes, &mut pos) as u32 + 1;
            out.push(cur);
        }
    }

    /// Sum of `vals[target]` over every edge — the gather scan, decoding as
    /// it goes.
    pub fn scan(&self, vals: &[u64]) -> u64 {
        let mut total = 0u64;
        for v in 0..self.n {
            let d = self.deg[v] as usize;
            if d == 0 {
                continue;
            }
            let mut pos = self.off[v] as usize;
            let mut cur = (v as i64 + unzigzag(get_varint(&self.bytes, &mut pos))) as u32;
            let mut s = vals[cur as usize];
            for _ in 1..d {
                cur += get_varint(&self.bytes, &mut pos) as u32 + 1;
                s = s.wrapping_add(vals[cur as usize]);
            }
            total = total.wrapping_add(s);
        }
        total
    }

    /// Bits per edge of the edge bytes alone, and with the two 32-bit vertex
    /// arrays (offset + degree) charged to the edges.
    pub fn bits_per_edge(&self, m: usize) -> (f64, f64) {
        let e = self.bytes.len() as f64 * 8.0 / m as f64;
        (e, e + 64.0 * self.n as f64 / m as f64)
    }
}

#[inline]
fn gamma_len(x: u64) -> u64 {
    // Elias gamma of x >= 1: 2*floor(log2 x) + 1 bits
    2 * (63 - x.leading_zeros() as u64) + 1
}

/// Total Elias-gamma bits of the same values `varint` stores (each + 1).
pub fn gamma_bits(g: &Csr) -> u64 {
    let mut bits = 0;
    for v in 0..g.n {
        let l = g.neighbors(v);
        if let Some(&f) = l.first() {
            bits += gamma_len(zigzag(f as i64 - v as i64) + 1);
            for w in l.windows(2) {
                bits += gamma_len((w[1] - w[0] - 1) as u64 + 1);
            }
        }
    }
    bits
}

/// LogGap: mean over consecutive-neighbour deltas of `1 + floor(log2 delta)`.
pub fn log_gap(g: &Csr) -> f64 {
    let mut sum = 0u64;
    let mut cnt = 0u64;
    for v in 0..g.n {
        for w in g.neighbors(v).windows(2) {
            sum += 32 - (w[1] - w[0]).leading_zeros() as u64;
            cnt += 1;
        }
    }
    sum as f64 / cnt.max(1) as f64
}

/// An exact simulation of an 8-way set-associative LRU cache of 64-byte lines,
/// fed the addresses of `vals[target]` (8-byte values) in CSR scan order.
/// Returns (accesses, misses). It is a model, not the machine's cache: its
/// only job is to give a deterministic count that moves when locality does.
pub fn simulate_gather_misses(g: &Csr, cache_bytes: usize) -> (u64, u64) {
    const WAYS: usize = 8;
    let lines = cache_bytes / 64;
    let sets = lines / WAYS;
    assert!(sets.is_power_of_two());
    let mut tag = vec![u32::MAX; sets * WAYS];
    // age[i] = recency stamp; smaller is older
    let mut age = vec![0u64; sets * WAYS];
    let mut clock = 0u64;
    let mut misses = 0u64;
    for &t in &g.tgt {
        let line = t >> 3; // 8 u64 values per 64-byte line
        let s = (line as usize) & (sets - 1);
        let base = s * WAYS;
        clock += 1;
        let mut hit = false;
        for w in 0..WAYS {
            if tag[base + w] == line {
                age[base + w] = clock;
                hit = true;
                break;
            }
        }
        if !hit {
            misses += 1;
            let mut victim = 0;
            for w in 1..WAYS {
                if age[base + w] < age[base + victim] {
                    victim = w;
                }
            }
            tag[base + victim] = line;
            age[base + victim] = clock;
        }
    }
    (g.m() as u64, misses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{community_graph, GenParams};

    #[test]
    fn varint_round_trips_every_list() {
        let p = community_graph(GenParams::SMALL);
        let vg = VarintGraph::encode(&p.g);
        let mut buf = Vec::new();
        for v in 0..p.g.n {
            vg.decode_list(v, &mut buf);
            assert_eq!(buf, p.g.neighbors(v), "vertex {v}");
        }
    }

    #[test]
    fn varint_scan_equals_csr_scan() {
        let p = community_graph(GenParams::SMALL);
        let vals: Vec<u64> = (0..p.g.n as u64).map(|x| x * 2654435761 % 1009).collect();
        let vg = VarintGraph::encode(&p.g);
        assert_eq!(vg.scan(&vals), crate::measure::scan_csr(&p.g, &vals));
    }

    #[test]
    fn zigzag_round_trips() {
        for x in [-5i64, -1, 0, 1, 7, 1 << 40, -(1 << 40)] {
            assert_eq!(unzigzag(zigzag(x)), x);
        }
    }

    #[test]
    fn locality_lowers_every_price() {
        let p = community_graph(GenParams::SMALL); // planted order
        let shuffled = p.g.permute(&crate::gen::shuffle_perm(p.g.n, 3));
        assert!(log_gap(&p.g) < log_gap(&shuffled));
        assert!(gamma_bits(&p.g) < gamma_bits(&shuffled));
        let a = simulate_gather_misses(&p.g, 1 << 16).1;
        let b = simulate_gather_misses(&shuffled, 1 << 16).1;
        assert!(a < b, "planted {a} vs shuffled {b}");
    }

    #[test]
    fn cache_sim_counts_cold_misses_exactly() {
        // one vertex gathering 16 consecutive ids = 2 lines of 8 values
        let g = Csr::from_edges(32, (1..17).map(|t| (0u32, t as u32)).collect());
        assert_eq!(simulate_gather_misses(&g, 64 * 8 * 4), (16, 3)); // ids 1..=16 span lines 0,1,2
    }
}
