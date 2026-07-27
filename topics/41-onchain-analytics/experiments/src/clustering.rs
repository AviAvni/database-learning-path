//! Address clustering: collapsing pseudonyms back into entities.
//!
//! Meiklejohn et al. (IMC'13) gave the field its two heuristics, and
//! they have very different risk profiles.
//!
//! **Heuristic 1 — multi-input / co-spend.** If two addresses are inputs
//! to the same transaction, they are controlled by the same user,
//! because whoever signed the transaction held both private keys. The
//! relation is transitive, so this is a union-find over the co-spend
//! hypergraph. On the 2013 chain it took **12,056,684 public keys down
//! to 5,579,176 clusters**. It is *safe*: an entity cannot fake it
//! without actually holding the keys.
//!
//! **Heuristic 2 — one-time change address.** A payment rarely matches a
//! UTXO exactly, so wallets send the remainder to a fresh address they
//! control. Meiklejohn's Definition 4.3 calls an output `pk` a one-time
//! change address when all four hold:
//!
//! ```text
//!   1. this is the first appearance of pk as an output
//!   2. the transaction is not a coin generation
//!   3. no output address is also an input address  (no self-change)
//!   4. exactly ONE output satisfies condition 1
//! ```
//!
//! This is *not* safe: it keys on an idiom of use rather than a property
//! of the protocol, so it can be wrong, and being wrong once is
//! catastrophic. A single false change label welds two entities together
//! permanently, and because union-find is transitive the damage compounds.
//! Meiklejohn's own refined run still produced a **super-cluster of 1.6
//! million public keys containing Mt. Gox, Instawallet, BitPay and Silk
//! Road simultaneously**; BlockSci, on the 2019 chain, reports **809
//! clusters over 20,000 addresses, one of them with over 17 million** —
//! and says plainly it is "likely a result of such a collapse".
//!
//! Their measured false-positive ladder for Heuristic 2 is worth
//! keeping: **13%** naively, **1%** after excluding the Satoshi Dice
//! payout pattern, **0.28%** if you wait a day before labelling, and
//! **0.17%** (7,382 addresses) if you wait a week. Precision here is
//! bought with latency, which should feel familiar from topic 39.

use crate::chain::Chain;
use std::collections::{HashMap, HashSet};

/// Union-find over addresses (PROVIDED). Path compression + union by
/// size, so the clustering is order-independent: the same set of merges
/// in any order yields the same partition.
pub struct UnionFind {
    parent: Vec<u32>,
    size: Vec<u32>,
}

impl UnionFind {
    pub fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n as u32).collect(),
            size: vec![1; n],
        }
    }
    pub fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let g = self.parent[self.parent[x as usize] as usize];
            self.parent[x as usize] = g;
            x = g;
        }
        x
    }
    pub fn union(&mut self, a: u32, b: u32) {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if self.size[ra as usize] < self.size[rb as usize] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb as usize] = ra;
        self.size[ra as usize] += self.size[rb as usize];
    }
    /// Address → cluster id, normalized so ids are contiguous.
    pub fn labels(&mut self) -> Vec<u32> {
        let n = self.parent.len();
        let mut remap: HashMap<u32, u32> = HashMap::new();
        (0..n as u32)
            .map(|a| {
                let r = self.find(a);
                let next = remap.len() as u32;
                *remap.entry(r).or_insert(next)
            })
            .collect()
    }
}

/// Quality of a clustering against the planted ground truth, scored on
/// *pairs* of addresses — the same metric topic 39 uses for record
/// linkage, because it is the same problem: deciding which pseudonyms
/// are one person.
#[derive(Debug, Clone, Copy)]
pub struct ClusterQuality {
    pub n_clusters: usize,
    pub largest: usize,
    pub precision: f64,
    pub recall: f64,
}

/// PROVIDED. Exact pair precision/recall via per-cluster × per-entity
/// contingency counts, so it stays O(addresses) rather than O(pairs).
pub fn quality(chain: &Chain, labels: &[u32]) -> ClusterQuality {
    let mut cluster_size: HashMap<u32, u64> = HashMap::new();
    let mut entity_size: HashMap<u32, u64> = HashMap::new();
    let mut joint: HashMap<(u32, u32), u64> = HashMap::new();
    for (a, &c) in labels.iter().enumerate() {
        let e = chain.address_entity[a];
        *cluster_size.entry(c).or_default() += 1;
        *entity_size.entry(e).or_default() += 1;
        *joint.entry((c, e)).or_default() += 1;
    }
    let pairs = |n: u64| n * n.saturating_sub(1) / 2;
    let tp: u64 = joint.values().map(|&n| pairs(n)).sum();
    let predicted: u64 = cluster_size.values().map(|&n| pairs(n)).sum();
    let actual: u64 = entity_size.values().map(|&n| pairs(n)).sum();
    ClusterQuality {
        n_clusters: cluster_size.len(),
        largest: *cluster_size.values().max().unwrap_or(&0) as usize,
        precision: if predicted == 0 {
            1.0
        } else {
            tp as f64 / predicted as f64
        },
        recall: if actual == 0 {
            1.0
        } else {
            tp as f64 / actual as f64
        },
    }
}

/// **Heuristic 1 (STUB).** Union every pair of addresses that appear as
/// inputs to the same transaction.
pub fn multi_input_clusters(chain: &Chain) -> Vec<u32> {
    let _ = chain;
    todo!(
        "for each non-coinbase transaction, union the addresses of all its inputs together, then return UnionFind::labels(). One pass over the transactions; no pairwise work needed."
    )
}

/// **Heuristic 2 (STUB).** Given a transaction, which of its outputs (if
/// any) is the sender's one-time change address? Return `None` unless
/// all four of Definition 4.3's conditions hold.
///
/// `seen_as_output` must contain every address that appeared as an
/// output of an *earlier* transaction — condition 1 is about first
/// appearance, so this has to be maintained as you scan forward.
pub fn change_output(
    chain: &Chain,
    tx_id: usize,
    seen_as_output: &HashSet<u32>,
) -> Option<usize> {
    let _ = (chain, tx_id, seen_as_output);
    todo!(
        "Definition 4.3, all four conditions: (1) the output's address is not in seen_as_output; (2) the transaction is not a coinbase; (3) no output address also appears among the input addresses; (4) exactly one output satisfies (1) - if two outputs are both fresh the transaction is ambiguous and you must label neither."
    )
}

/// **Heuristic 1 + 2 (STUB).** Multi-input clustering, plus a union
/// between each detected change output and its transaction's inputs.
pub fn full_clusters(chain: &Chain) -> Vec<u32> {
    let _ = chain;
    todo!(
        "scan transactions in order maintaining `seen_as_output`. For each: union the input addresses (Heuristic 1), then ask change_output() and union its address with the inputs too. Update seen_as_output with every output address AFTER processing the transaction, not before."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{chain_instance, seeded_rng, ChainConfig};

    fn chain(change_reuse_rate: f64) -> Chain {
        let mut rng = seeded_rng(11);
        chain_instance(
            &mut rng,
            &ChainConfig {
                n_entities: 120,
                n_txs: 8_000,
                change_reuse_rate,
                ..ChainConfig::default()
            },
        )
    }

    #[test]
    fn co_spending_never_lies() {
        // Heuristic 1 keys on a property of the protocol, not on a habit:
        // an address cannot be co-spent with yours unless you hold its
        // key. Precision is 1.0 by construction, and stays there.
        let c = chain(0.0);
        let q = quality(&c, &multi_input_clusters(&c));
        assert_eq!(q.precision, 1.0, "co-spend produced a false merge");
        assert!(q.recall < 0.95, "recall {} — nothing left to find?", q.recall);
    }

    #[test]
    fn the_change_heuristic_buys_recall() {
        let c = chain(0.0);
        let h1 = quality(&c, &multi_input_clusters(&c));
        let both = quality(&c, &full_clusters(&c));
        assert!(
            both.recall > h1.recall,
            "change heuristic added no recall: {} vs {}",
            both.recall,
            h1.recall
        );
        assert!(both.n_clusters < h1.n_clusters);
    }

    #[test]
    fn one_reused_change_address_in_twenty_collapses_the_graph() {
        // The Meiklejohn §4.5 failure, planted. When the sender reuses an
        // address for change, the RECIPIENT's fresh address becomes the
        // only first-appearance output — so the heuristic labels the
        // payee's address as the payer's change and welds two strangers
        // together. Union-find makes it transitive, and it snowballs.
        let c = chain(0.05);
        let safe = quality(&c, &multi_input_clusters(&c));
        let collapsed = quality(&c, &full_clusters(&c));
        assert_eq!(safe.precision, 1.0);
        assert!(
            collapsed.precision < 0.2,
            "expected a super-cluster collapse, got precision {}",
            collapsed.precision
        );
        assert!(
            collapsed.largest > c.n_addresses() / 10,
            "largest cluster {} of {} addresses",
            collapsed.largest,
            c.n_addresses()
        );
    }

    #[test]
    fn definition_4_3_refuses_ambiguous_transactions() {
        // Condition 4: if both outputs are fresh addresses, there is no
        // way to tell payment from change, and the heuristic must decline.
        let c = chain(0.0);
        let mut seen: HashSet<u32> = HashSet::new();
        let mut ambiguous = 0usize;
        for (i, tx) in c.txs.iter().enumerate() {
            if !tx.is_coinbase() {
                let fresh = tx
                    .outputs
                    .iter()
                    .filter(|&&o| !seen.contains(&c.outputs[o].address))
                    .count();
                if fresh > 1 {
                    assert!(change_output(&c, i, &seen).is_none());
                    ambiguous += 1;
                }
            }
            for &o in &tx.outputs {
                seen.insert(c.outputs[o].address);
            }
        }
        assert!(ambiguous > 0, "the test never exercised condition 4");
    }

    #[test]
    fn clustering_is_order_independent() {
        let c = chain(0.0);
        let a = multi_input_clusters(&c);
        let b = multi_input_clusters(&c);
        assert_eq!(a, b);
    }
}
