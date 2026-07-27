//! Three answers to "where did the stolen money go?", and only one of
//! them is usable.
//!
//! Möser, Böhme & Breuker named the first two. **Poison**: if any input
//! is tainted, every output is entirely tainted. **Haircut**: each output
//! is tainted by the fraction of input value that was tainted. Haircut
//! became the industry default, and Anderson, Shumailov, Ahmed &
//! Rietmann (*Bitcoin Redux*, WEIS'18) measured what it does over a real
//! chain:
//!
//! ```text
//!   the 2012 Linode theft of 46,653 BTC, traced to 2016
//!     haircut ... 16,855,619 addresses tainted   (93% of all addresses)
//!     FIFO ......... 245,120 addresses tainted   (1.35%)
//!
//!   the 2014 Flexcoin hack
//!     haircut ... 10,421,112 addresses           (57%)
//!     FIFO .......... 15,265 addresses
//! ```
//!
//! A rule that taints 93% of everyone is not a forensic tool, it is a
//! tax. The third answer comes from an 1816 English court case. When a
//! bank failed and nobody could say which deposits paid for which
//! withdrawals, the Master of the Rolls in *Clayton's Case* set
//! first-in-first-out: withdrawals are drawn against the earliest
//! deposits. Applied to a transaction, FIFO lays the input satoshis end
//! to end and cuts the outputs off the front of the queue in order.
//!
//! The property that makes FIFO work is that it is **lossless**: a
//! satoshi is stolen or it is not, the transaction "processes it in a
//! lossless way", and so taint can be traced backwards as well as
//! forwards. Haircut destroys that — after two hops every number is a
//! fraction of a fraction and nothing can be reversed.
//!
//! ```text
//!   inputs                    FIFO outputs
//!   ┌──────┐ clean 3          ┌──────┐  D: 3 clean
//!   │      │                  │      │
//!   ├──────┤ STOLEN 2   ==>   ├──────┤  E: 2 STOLEN   <- all of it, here
//!   ├──────┤ clean 4          ├──────┤  F: 4 clean
//!   └──────┘                  └──────┘
//!
//!   haircut: every output gets 2/9 stolen. Three tainted outputs
//!   instead of one, and the next hop multiplies that fan-out again.
//! ```

use crate::chain::Chain;
use std::collections::VecDeque;

/// One contiguous run of same-provenance value, in satoshis.
/// `name == 0` is clean money; other names are distinct crime sources.
/// This is `TaintPart` from RustyTaintChain, the reference
/// implementation of Clayton's Case for Bitcoin.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaintPart {
    pub name: u16,
    pub value: u64,
}

pub const CLEAN: u16 = 0;
pub const STOLEN: u16 = 1;

/// Per-output tainted value, in satoshis. Index is the output id.
pub type Taint = Vec<u64>;

/// Outputs still unspent that carry any taint at all.
pub fn flagged_utxos(chain: &Chain, taint: &Taint) -> Vec<usize> {
    chain
        .utxo_set()
        .into_iter()
        .filter(|&o| taint[o] > 0)
        .collect()
}

/// Total tainted value sitting in the UTXO set.
pub fn flagged_value(chain: &Chain, taint: &Taint) -> u64 {
    chain
        .utxo_set()
        .iter()
        .filter(|&&o| taint[o] > 0)
        .map(|&o| taint[o])
        .sum()
}

/// **Haircut (PROVIDED).** Each output inherits the fraction of input
/// value that was tainted. Total tainted value is conserved — and
/// smeared across every output of every descendant transaction, which
/// is the whole problem.
pub fn haircut(chain: &Chain, origin: usize) -> Taint {
    let mut taint: Taint = vec![0; chain.n_outputs()];
    taint[origin] = chain.outputs[origin].value;
    // Transactions are in topological order: a transaction only spends
    // outputs created by earlier transactions.
    for tx in &chain.txs {
        if tx.is_coinbase() {
            continue;
        }
        let total_in: u64 = tx.inputs.iter().map(|&o| chain.outputs[o].value).sum();
        let dirty_in: u64 = tx.inputs.iter().map(|&o| taint[o]).sum();
        if dirty_in == 0 || total_in == 0 {
            continue;
        }
        let frac = dirty_in as f64 / total_in as f64;
        for &o in &tx.outputs {
            taint[o] = (chain.outputs[o].value as f64 * frac).round() as u64;
        }
    }
    taint
}

/// **Poison (STUB).** Any tainted input makes every output entirely
/// tainted. Simple, and it counts far more value as stolen than was ever
/// stolen — the number grows without bound as the chain fans out.
pub fn poison(chain: &Chain, origin: usize) -> Taint {
    let _ = (chain, origin);
    todo!(
        "walk the transactions in order; if any input carries taint, set every output's taint to that output's FULL value. Note what this does to the total: it is not conserved, and that is the point."
    )
}

/// Cut `value` satoshis off the front of a taint queue, splitting the
/// run at the boundary and pushing the remainder back. This is
/// `extract_taint` from RustyTaintChain, and it is the whole of
/// Clayton's Case in fifteen lines.
///
/// (STUB.)
pub fn extract_taint(given: &mut VecDeque<TaintPart>, value: u64) -> VecDeque<TaintPart> {
    let _ = (given, value);
    todo!(
        "pop runs off the front until `value` satoshis have been taken. If a run is larger than what remains, SPLIT it: take the piece you need and push the shortened original back on the front. If the queue runs dry, the rest is CLEAN."
    )
}

/// **FIFO (STUB).** Clayton's Case, 1816. Concatenate the input queues
/// in input order, then cut each output off the front in output order.
/// Lossless: every satoshi keeps exactly one provenance.
pub fn fifo(chain: &Chain, origin: usize) -> Taint {
    let _ = (chain, origin);
    todo!(
        "give every output a VecDeque<TaintPart>; the origin's is one STOLEN run of its full value. For each transaction in order, concatenate its inputs' queues (an input with an empty queue contributes one CLEAN run of its value) and call extract_taint once per output, in output order. Then sum the non-CLEAN runs per output into the Taint vector."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{chain_instance, descendants, seeded_rng, ChainConfig};

    fn small() -> Chain {
        let mut rng = seeded_rng(7);
        chain_instance(
            &mut rng,
            &ChainConfig {
                n_entities: 60,
                n_txs: 3_000,
                ..ChainConfig::default()
            },
        )
    }

    #[test]
    fn extract_taint_splits_runs_at_the_boundary() {
        // The example from the module header, as a queue operation.
        let mut q: VecDeque<TaintPart> = VecDeque::from(vec![
            TaintPart { name: CLEAN, value: 3 },
            TaintPart { name: STOLEN, value: 2 },
            TaintPart { name: CLEAN, value: 4 },
        ]);
        let d = extract_taint(&mut q, 3);
        assert_eq!(d, VecDeque::from(vec![TaintPart { name: CLEAN, value: 3 }]));
        let e = extract_taint(&mut q, 2);
        assert_eq!(e, VecDeque::from(vec![TaintPart { name: STOLEN, value: 2 }]));
        let f = extract_taint(&mut q, 4);
        assert_eq!(f, VecDeque::from(vec![TaintPart { name: CLEAN, value: 4 }]));
        assert!(q.is_empty());

        // And a cut that lands mid-run must split it, not round it.
        let mut q: VecDeque<TaintPart> = VecDeque::from(vec![TaintPart { name: STOLEN, value: 10 }]);
        let a = extract_taint(&mut q, 4);
        assert_eq!(a, VecDeque::from(vec![TaintPart { name: STOLEN, value: 4 }]));
        assert_eq!(q, VecDeque::from(vec![TaintPart { name: STOLEN, value: 6 }]));
    }

    #[test]
    fn fifo_is_lossless() {
        // Every stolen satoshi is somewhere, and no satoshi is stolen
        // twice. This is the property haircut approximates and poison
        // abandons.
        let c = small();
        let t = fifo(&c, c.stolen_output);
        assert_eq!(
            flagged_value(&c, &t),
            c.stolen_value,
            "FIFO must conserve the stolen amount exactly"
        );
    }

    #[test]
    fn haircut_conserves_value_but_not_focus() {
        // Haircut also conserves the total (up to rounding), yet spreads
        // it over every descendant output rather than a few.
        let c = small();
        let h = haircut(&c, c.stolen_output);
        let f = fifo(&c, c.stolen_output);
        let hv = flagged_value(&c, &h) as f64;
        assert!(
            (hv - c.stolen_value as f64).abs() / (c.stolen_value as f64) < 0.01,
            "haircut total {hv} vs stolen {}",
            c.stolen_value
        );
        assert!(
            flagged_utxos(&c, &h).len() > 5 * flagged_utxos(&c, &f).len(),
            "haircut {} utxos vs fifo {}",
            flagged_utxos(&c, &h).len(),
            flagged_utxos(&c, &f).len()
        );
    }

    #[test]
    fn poison_overcounts_wildly() {
        // Poison declares more money stolen than ever existed at the
        // scene, because it re-counts the full value of every output.
        let c = small();
        let p = poison(&c, c.stolen_output);
        assert!(
            flagged_value(&c, &p) > 10 * c.stolen_value,
            "poison flagged {} vs stolen {}",
            flagged_value(&c, &p),
            c.stolen_value
        );
    }

    #[test]
    fn every_policy_stays_inside_the_descendant_set() {
        // No policy may taint an output the money never reached. Poison
        // marks the whole descendant set; FIFO marks a subset of it.
        let c = small();
        let d = descendants(&c, c.stolen_output);
        for t in [
            fifo(&c, c.stolen_output),
            haircut(&c, c.stolen_output),
            poison(&c, c.stolen_output),
        ] {
            for o in 0..c.n_outputs() {
                if t[o] > 0 {
                    assert!(d.contains(&o), "output {o} tainted but not a descendant");
                }
            }
        }
        let f = fifo(&c, c.stolen_output);
        let p = poison(&c, c.stolen_output);
        for o in 0..c.n_outputs() {
            if f[o] > 0 {
                assert!(p[o] > 0, "FIFO ⊄ poison at output {o}");
            }
        }
    }
}
