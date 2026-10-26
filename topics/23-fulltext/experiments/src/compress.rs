//! Posting-list compression, two designs, both PROVIDED and round-trip tested.
//!
//! * `varint_*` — RediSearch's shape: each doc id is stored as the delta from
//!   the previous one, 7 bits per byte with a continuation bit
//!   (redisearch_rs `varint`). Random access needs a skip structure on top.
//! * `bitpack_*` — tantivy's shape (postings/compression/mod.rs at 7152d53):
//!   128 postings per block; within a block every delta is stored in the
//!   same number of bits, the width of the block's largest delta; the doc ids
//!   are *strictly* sorted so the stored value is `delta - 1`
//!   (`compress_block_sorted`, `num_bits_strictly_sorted`); the term
//!   frequencies are a second bit-packed array with `tf - 1`
//!   (`compress_block_unsorted(.., minus_one_encoded = true)`). The final
//!   partial block (fewer than 128 postings) is varint-encoded, as tantivy
//!   does. One width byte per block, and a block's size is
//!   `width * 128 / 8` bytes (`compressed_block_size`) — so the n-th block is
//!   found without decoding the ones before it.
//!
//! This is a scalar bit packer: tantivy's `bitpacking` crate uses SIMD, so the
//! decode *speed* here is the portable floor, not tantivy's. The byte counts
//! are exact for the layout described.

use crate::index::{Posting, BLOCK};

/// Append `v` as a little-endian base-128 varint.
pub fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn get_varint(buf: &[u8], pos: &mut usize) -> u32 {
    let mut v = 0u32;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= ((b & 0x7F) as u32) << shift;
        if b < 0x80 {
            return v;
        }
        shift += 7;
    }
}

/// Doc-id deltas and tfs, both varint, interleaved (doc delta, tf) per posting.
pub fn varint_encode(postings: &[Posting]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev = 0u32;
    for (i, p) in postings.iter().enumerate() {
        put_varint(&mut out, if i == 0 { p.doc } else { p.doc - prev });
        put_varint(&mut out, p.tf);
        prev = p.doc;
    }
    out
}

pub fn varint_decode(buf: &[u8], n: usize) -> Vec<Posting> {
    let (mut pos, mut prev) = (0usize, 0u32);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let d = get_varint(buf, &mut pos);
        let tf = get_varint(buf, &mut pos);
        let doc = if i == 0 { d } else { prev + d };
        out.push(Posting { doc, tf });
        prev = doc;
    }
    out
}

fn width(max: u32) -> u8 {
    (32 - max.leading_zeros()) as u8
}

fn pack(vals: &[u32], w: u8, out: &mut Vec<u8>) {
    let mut acc = 0u64;
    let mut bits = 0u32;
    for &v in vals {
        acc |= (v as u64) << bits;
        bits += w as u32;
        while bits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            bits -= 8;
        }
    }
    if bits > 0 {
        out.push(acc as u8);
    }
}

fn unpack(buf: &[u8], w: u8, n: usize, out: &mut Vec<u32>) -> usize {
    let bytes = (w as usize * n).div_ceil(8);
    let (mut acc, mut bits, mut pos) = (0u64, 0u32, 0usize);
    let mask = if w == 0 { 0 } else { (1u64 << w) - 1 };
    for _ in 0..n {
        while bits < w as u32 {
            acc |= (buf[pos] as u64) << bits;
            pos += 1;
            bits += 8;
        }
        out.push((acc & mask) as u32);
        acc >>= w;
        bits -= w as u32;
    }
    bytes
}

/// tantivy's layout: full 128-posting blocks bit-packed (width byte, packed
/// doc deltas minus one, width byte, packed tfs minus one), then the tail as
/// varints.
pub fn bitpack_encode(postings: &[Posting]) -> Vec<u8> {
    let mut out = Vec::new();
    let full = postings.len() / BLOCK * BLOCK;
    let mut prev = 0u32; // block offset: the previous block's last doc
    let mut first = true;
    for chunk in postings[..full].chunks(BLOCK) {
        let mut d = Vec::with_capacity(BLOCK);
        let mut p = prev;
        for (i, x) in chunk.iter().enumerate() {
            // strictly sorted: stored value is delta - 1; the very first doc of
            // the list is stored relative to "nothing" (offset 0 = None)
            d.push(if first && i == 0 { x.doc } else { x.doc - p - 1 });
            p = x.doc;
        }
        first = false;
        let w = width(*d.iter().max().unwrap());
        out.push(w);
        pack(&d, w, &mut out);
        let t: Vec<u32> = chunk.iter().map(|x| x.tf - 1).collect();
        let wt = width(*t.iter().max().unwrap());
        out.push(wt);
        pack(&t, wt, &mut out);
        prev = chunk.last().unwrap().doc;
    }
    // tail: varint, deltas continue from the last full block
    let mut p = if full == 0 { 0 } else { prev };
    for (i, x) in postings[full..].iter().enumerate() {
        put_varint(&mut out, if full == 0 && i == 0 { x.doc } else { x.doc - p });
        put_varint(&mut out, x.tf);
        p = x.doc;
    }
    out
}

pub fn bitpack_decode(buf: &[u8], n: usize) -> Vec<Posting> {
    let full = n / BLOCK * BLOCK;
    let mut out = Vec::with_capacity(n);
    let (mut pos, mut prev, mut first) = (0usize, 0u32, true);
    let mut d = Vec::with_capacity(BLOCK);
    let mut t = Vec::with_capacity(BLOCK);
    for _ in 0..full / BLOCK {
        let w = buf[pos];
        pos += 1;
        d.clear();
        pos += unpack(&buf[pos..], w, BLOCK, &mut d);
        let wt = buf[pos];
        pos += 1;
        t.clear();
        pos += unpack(&buf[pos..], wt, BLOCK, &mut t);
        let mut p = prev;
        for i in 0..BLOCK {
            let doc = if first && i == 0 { d[i] } else { p + d[i] + 1 };
            out.push(Posting { doc, tf: t[i] + 1 });
            p = doc;
        }
        first = false;
        prev = p;
    }
    let mut p = if full == 0 { 0 } else { prev };
    for i in full..n {
        let dd = get_varint(buf, &mut pos);
        let tf = get_varint(buf, &mut pos);
        let doc = if full == 0 && i == 0 { dd } else { p + dd };
        out.push(Posting { doc, tf });
        p = doc;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{corpus::gen_corpus, index::build_index};

    #[test]
    fn both_codecs_round_trip_every_list() {
        let idx = build_index(&gen_corpus(3000, 4000, 1.0, 5));
        for (t, l) in idx.lists.iter().enumerate() {
            if l.postings.is_empty() {
                continue;
            }
            let v = varint_encode(&l.postings);
            assert_eq!(varint_decode(&v, l.postings.len()), l.postings, "varint t{t}");
            let b = bitpack_encode(&l.postings);
            assert_eq!(bitpack_decode(&b, l.postings.len()), l.postings, "bitpack t{t}");
        }
    }

    #[test]
    fn dense_list_packs_to_about_one_bit_per_doc_delta() {
        // every doc id present: strictly-sorted delta-1 is always 0, width 0
        let ps: Vec<Posting> = (0..256).map(|d| Posting { doc: d, tf: 1 }).collect();
        let b = bitpack_encode(&ps);
        // two blocks x (width byte + 0 bytes + width byte + 0 bytes), first
        // delta of the list is the doc id 0 itself
        assert_eq!(b.len(), 4);
        assert_eq!(bitpack_decode(&b, 256), ps);
    }
}
