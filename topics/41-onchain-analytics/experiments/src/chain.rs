//! PROVIDED — a synthetic UTXO chain with ground truth, which the real
//! blockchain does not come with.
//!
//! Every analysis in this topic is an inference from a public ledger that
//! records *transactions*, not *people*. To measure whether an inference
//! is any good you need to know the answer, so this generator plants it:
//! `address_entity` maps every address to the entity that controls it,
//! and `stolen_output` marks one coinbase output as the proceeds of a
//! theft. Real analysts get neither, which is why the field runs on
//! heuristics whose false-positive rates are themselves estimated.
//!
//! The model is Bitcoin's, minus the cryptography:
//!
//! ```text
//!   Tx { inputs: [output ids],  outputs: [output ids] }
//!   Output { value, address, creating_tx }
//!
//!   sum(inputs) == sum(outputs)      no fees, so taint conservation is
//!                                    exact and testable. (In Bitcoin the
//!                                    difference is the miner's fee, and
//!                                    FIFO taints that too.)
//! ```
//!
//! Two idioms of use are planted deliberately, because they are what the
//! clustering heuristics key on:
//!
//! * **Co-spending.** A transaction spends several of the sender's own
//!   outputs at once. Whoever signed it held every one of those private
//!   keys — Meiklejohn's Heuristic 1.
//! * **Change.** A payment rarely matches a UTXO exactly, so the
//!   remainder goes back to a fresh address the wallet generated. Spot
//!   the change output and you have linked the sender's new address to
//!   their old ones — Heuristic 2, and the one that can go badly wrong.

use rand::seq::SliceRandom;
use rand::Rng;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::collections::HashSet;

pub fn seeded_rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

#[derive(Clone, Debug)]
pub struct Output {
    pub value: u64,
    pub address: u32,
    pub creating_tx: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Tx {
    /// Output ids being spent. Empty for a coinbase.
    pub inputs: Vec<usize>,
    /// Output ids created.
    pub outputs: Vec<usize>,
}

impl Tx {
    pub fn is_coinbase(&self) -> bool {
        self.inputs.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ChainConfig {
    pub n_entities: usize,
    pub n_txs: usize,
    /// Value of each entity's opening coinbase output.
    pub initial_value: u64,
    /// Maximum outputs co-spent in one transaction. 1 disables
    /// Heuristic 1 entirely, which is a useful thing to try.
    pub max_inputs: usize,
    /// Probability that a sender reuses one of its existing addresses
    /// for change instead of generating a fresh one. This is the wallet
    /// behaviour that makes Heuristic 2 misfire: with the sender's
    /// change address already seen, the *recipient's* fresh address
    /// becomes the only first-appearance output, and a naive change
    /// detector labels it as the sender's change — merging two entities
    /// that never shared a key. Meiklejohn §4.5's super-cluster, planted.
    pub change_reuse_rate: f64,
    /// Probability that a payment goes to an address the recipient has
    /// used before rather than a fresh one. Without this the heuristic
    /// could never fire: every transaction would have *two* fresh
    /// outputs and condition 4 would refuse them all. Real chains sit in
    /// between — BlockSci reports only 8.6% of Bitcoin addresses are
    /// used more than once, but those account for 51% of occurrences.
    pub recipient_reuse_rate: f64,
}

impl Default for ChainConfig {
    fn default() -> Self {
        ChainConfig {
            n_entities: 400,
            n_txs: 20_000,
            initial_value: 1_000_000,
            max_inputs: 3,
            change_reuse_rate: 0.0,
            recipient_reuse_rate: 0.5,
        }
    }
}

pub struct Chain {
    pub outputs: Vec<Output>,
    pub txs: Vec<Tx>,
    /// Output id → the transaction that spent it, if any.
    pub spent_by: Vec<Option<usize>>,
    /// GROUND TRUTH. Address → controlling entity. Not observable.
    pub address_entity: Vec<u32>,
    pub n_entities: usize,
    /// The planted theft: this output is the crime proceeds.
    pub stolen_output: usize,
    pub stolen_value: u64,
}

impl Chain {
    pub fn n_addresses(&self) -> usize {
        self.address_entity.len()
    }
    pub fn n_outputs(&self) -> usize {
        self.outputs.len()
    }
    /// Outputs never spent — the UTXO set, i.e. where the money is now.
    pub fn utxo_set(&self) -> Vec<usize> {
        (0..self.outputs.len())
            .filter(|&o| self.spent_by[o].is_none())
            .collect()
    }
    pub fn total_value(&self) -> u64 {
        self.utxo_set().iter().map(|&o| self.outputs[o].value).sum()
    }
}

pub fn chain_instance(rng: &mut ChaCha8Rng, cfg: &ChainConfig) -> Chain {
    let mut outputs: Vec<Output> = Vec::new();
    let mut txs: Vec<Tx> = Vec::new();
    let mut address_entity: Vec<u32> = Vec::new();
    // Per-entity: the addresses it has used, and its unspent outputs.
    let mut entity_addresses: Vec<Vec<u32>> = vec![Vec::new(); cfg.n_entities];
    let mut unspent: Vec<Vec<usize>> = vec![Vec::new(); cfg.n_entities];

    let fresh_address = |e: usize,
                             address_entity: &mut Vec<u32>,
                             entity_addresses: &mut Vec<Vec<u32>>|
     -> u32 {
        let a = address_entity.len() as u32;
        address_entity.push(e as u32);
        entity_addresses[e].push(a);
        a
    };

    // Opening coinbases: one per entity.
    for e in 0..cfg.n_entities {
        let a = fresh_address(e, &mut address_entity, &mut entity_addresses);
        let oid = outputs.len();
        outputs.push(Output {
            value: cfg.initial_value,
            address: a,
            creating_tx: txs.len(),
        });
        txs.push(Tx {
            inputs: vec![],
            outputs: vec![oid],
        });
        unspent[e].push(oid);
    }
    // The theft: entity 0's opening balance is the crime proceeds.
    let stolen_output = 0usize;
    let stolen_value = cfg.initial_value;

    let mut spent_by: Vec<Option<usize>> = vec![None; outputs.len()];

    for _ in 0..cfg.n_txs {
        // Pick a sender that has something to spend.
        let mut sender = rng.gen_range(0..cfg.n_entities);
        let mut tries = 0;
        while unspent[sender].is_empty() && tries < 32 {
            sender = rng.gen_range(0..cfg.n_entities);
            tries += 1;
        }
        if unspent[sender].is_empty() {
            continue;
        }

        // Co-spend: take up to max_inputs of the sender's own outputs.
        let k = rng
            .gen_range(1..=cfg.max_inputs)
            .min(unspent[sender].len());
        unspent[sender].shuffle(rng);
        let inputs: Vec<usize> = unspent[sender].drain(..k).collect();
        let total_in: u64 = inputs.iter().map(|&o| outputs[o].value).sum();
        if total_in < 2 {
            unspent[sender].extend(inputs);
            continue;
        }

        let mut recipient = rng.gen_range(0..cfg.n_entities);
        if recipient == sender {
            recipient = (recipient + 1) % cfg.n_entities;
        }

        // Payment takes 10–70% of the inputs; the rest is change.
        let pay = (total_in as f64 * rng.gen_range(0.10..0.70)) as u64;
        let pay = pay.max(1).min(total_in - 1);
        let change = total_in - pay;

        let tx_id = txs.len();
        let pay_reuse =
            rng.gen::<f64>() < cfg.recipient_reuse_rate && !entity_addresses[recipient].is_empty();
        let pay_addr = if pay_reuse {
            *entity_addresses[recipient].choose(rng).unwrap()
        } else {
            fresh_address(recipient, &mut address_entity, &mut entity_addresses)
        };
        let pay_oid = outputs.len();
        outputs.push(Output {
            value: pay,
            address: pay_addr,
            creating_tx: tx_id,
        });
        spent_by.push(None);

        // Change: fresh address, or a reused one at the configured rate.
        let reuse = rng.gen::<f64>() < cfg.change_reuse_rate && !entity_addresses[sender].is_empty();
        let change_addr = if reuse {
            *entity_addresses[sender].choose(rng).unwrap()
        } else {
            fresh_address(sender, &mut address_entity, &mut entity_addresses)
        };
        let change_oid = outputs.len();
        outputs.push(Output {
            value: change,
            address: change_addr,
            creating_tx: tx_id,
        });
        spent_by.push(None);

        // Output order is arbitrary in a real transaction, so shuffle it.
        // Heuristic 2 must not be allowed to cheat by reading positions.
        let mut outs = vec![pay_oid, change_oid];
        outs.shuffle(rng);

        for &i in &inputs {
            spent_by[i] = Some(tx_id);
        }
        txs.push(Tx {
            inputs,
            outputs: outs,
        });
        unspent[recipient].push(pay_oid);
        unspent[sender].push(change_oid);
    }

    Chain {
        outputs,
        txs,
        spent_by,
        address_entity,
        n_entities: cfg.n_entities,
        stolen_output,
        stolen_value,
    }
}

/// Every output reachable from `origin` by following the transaction
/// graph forwards. This is what "descended from the theft" means in the
/// weakest possible sense — it is *poison*'s answer, and the set inside
/// which every other taint policy must live.
pub fn descendants(chain: &Chain, origin: usize) -> HashSet<usize> {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut stack = vec![origin];
    seen.insert(origin);
    while let Some(o) = stack.pop() {
        if let Some(tx) = chain.spent_by[o] {
            for &next in &chain.txs[tx].outputs {
                if seen.insert(next) {
                    stack.push(next);
                }
            }
        }
    }
    seen
}

/// Distinct addresses touched by a set of outputs.
pub fn addresses_of(chain: &Chain, outs: impl IntoIterator<Item = usize>) -> HashSet<u32> {
    outs.into_iter().map(|o| chain.outputs[o].address).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_conserves_value() {
        // No fees, so every transaction's outputs sum to its inputs and
        // the UTXO set always holds exactly what was mined. Taint
        // conservation is only testable because of this.
        let mut rng = seeded_rng(1);
        let cfg = ChainConfig {
            n_entities: 50,
            n_txs: 2_000,
            ..ChainConfig::default()
        };
        let c = chain_instance(&mut rng, &cfg);
        for tx in &c.txs {
            if tx.is_coinbase() {
                continue;
            }
            let i: u64 = tx.inputs.iter().map(|&o| c.outputs[o].value).sum();
            let o: u64 = tx.outputs.iter().map(|&o| c.outputs[o].value).sum();
            assert_eq!(i, o);
        }
        assert_eq!(c.total_value(), cfg.n_entities as u64 * cfg.initial_value);
    }

    #[test]
    fn addresses_vastly_outnumber_entities() {
        // The pseudonymity illusion, in one assertion: a few hundred
        // actors wearing tens of thousands of names.
        let mut rng = seeded_rng(2);
        let c = chain_instance(&mut rng, &ChainConfig::default());
        assert!(
            c.n_addresses() > 20 * c.n_entities,
            "{} addresses for {} entities",
            c.n_addresses(),
            c.n_entities
        );
    }

    #[test]
    fn the_theft_spreads_through_the_graph() {
        let mut rng = seeded_rng(3);
        let c = chain_instance(&mut rng, &ChainConfig::default());
        let d = descendants(&c, c.stolen_output);
        assert!(d.len() > 100, "only {} descendants", d.len());
    }
}
