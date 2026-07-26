//! PROVIDED — a synthetic knowledge graph that makes vector-RAG's failure
//! measurable: the path-finding question.
//!
//! HippoRAG's Figure 1 in miniature. Two query entities (say *Stanford*
//! and *Alzheimer's*) each connect to many candidates; exactly one
//! candidate — the answer — connects to BOTH, and no passage ever
//! mentions both query entities together. A retriever that scores each
//! passage independently against the query (vector RAG's shape, modeled
//! here as mention-count ranking) works when the answer sits one hop
//! from both seeds, because the answer is then named in two matching
//! passages and every distractor in one. Push the answer to two hops —
//! evidence chains u→a→x and w→b→x whose interior passages mention no
//! query entity at all — and every candidate scores zero: ranking is
//! chance. Coverage isn't the problem (BFS reaches everything);
//! *ranking by association* is, and that is what ppr.rs restores.
//!
//! One passage per fact (edge), like HippoRAG's OpenIE triples.

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub fn seeded_rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

/// One fact: "src —rel→ dst", stated by exactly one passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Passage {
    pub src: usize,
    pub dst: usize,
}

pub struct Kg {
    pub n_nodes: usize,
    pub passages: Vec<Passage>,
    /// Undirected adjacency (node -> neighbor node ids).
    pub adj: Vec<Vec<usize>>,
    /// node -> passage ids that mention it.
    pub mentions: Vec<Vec<usize>>,
}

impl Kg {
    pub fn new(n_nodes: usize) -> Self {
        Kg {
            n_nodes,
            passages: Vec::new(),
            adj: vec![Vec::new(); n_nodes],
            mentions: vec![Vec::new(); n_nodes],
        }
    }

    pub fn add_fact(&mut self, src: usize, dst: usize) {
        let pid = self.passages.len();
        self.passages.push(Passage { src, dst });
        self.adj[src].push(dst);
        self.adj[dst].push(src);
        self.mentions[src].push(pid);
        self.mentions[dst].push(pid);
    }

    /// Hop distance from any seed to every node (multi-source BFS).
    pub fn bfs_dist(&self, seeds: &[usize]) -> Vec<usize> {
        let mut dist = vec![usize::MAX; self.n_nodes];
        let mut queue = std::collections::VecDeque::new();
        for &s in seeds {
            dist[s] = 0;
            queue.push_back(s);
        }
        while let Some(v) = queue.pop_front() {
            for &n in &self.adj[v] {
                if dist[n] == usize::MAX {
                    dist[n] = dist[v] + 1;
                    queue.push_back(n);
                }
            }
        }
        dist
    }
}

/// A path-finding instance: which candidate connects seed u AND seed w?
pub struct Instance {
    pub kg: Kg,
    pub seeds: [usize; 2],
    /// All answer candidates (2*distractors + 1), each `hops` from a seed.
    pub candidates: Vec<usize>,
    /// The one candidate reachable from BOTH seeds.
    pub answer: usize,
    /// Passage ids on the two evidence chains to the answer (2*hops).
    pub gold_passages: Vec<usize>,
    pub hops: usize,
}

/// Build one instance. Seeds u and w each grow `distractors` chains of
/// length `hops` to dead-end candidates, plus one chain each that meets
/// at the shared answer candidate. No passage mentions both seeds.
pub fn path_finding_instance(hops: usize, distractors: usize) -> Instance {
    assert!(hops >= 1);
    // Node budget: 2 seeds + answer + per-seed chains.
    // Each chain (gold or distractor) has `hops` edges: hops-1 interior
    // nodes plus its terminal candidate (gold chains share the answer).
    let per_seed_chains = distractors + 1;
    let interior = 2 * per_seed_chains * (hops - 1);
    let dead_ends = 2 * distractors;
    let n_nodes = 2 + 1 + interior + dead_ends;
    let mut kg = Kg::new(n_nodes);
    let (u, w, answer) = (0usize, 1usize, 2usize);
    let mut next = 3usize;
    let mut gold_passages = Vec::new();
    let mut candidates = vec![answer];

    for &seed in &[u, w] {
        for chain in 0..per_seed_chains {
            let is_gold = chain == 0;
            let mut prev = seed;
            for step in 0..hops {
                let last = step == hops - 1;
                let node = if last && is_gold {
                    answer
                } else {
                    let n = next;
                    next += 1;
                    if last {
                        candidates.push(n);
                    }
                    n
                };
                let pid = kg.passages.len();
                kg.add_fact(prev, node);
                if is_gold {
                    gold_passages.push(pid);
                }
                prev = node;
            }
        }
    }

    Instance {
        kg,
        seeds: [u, w],
        candidates,
        answer,
        gold_passages,
        hops,
    }
}

/// Mention-count ranking — vector RAG's shape on this corpus. Each
/// candidate is scored by how many of its passages mention a query
/// entity (the seeds); ties break randomly. Returns the answer's rank
/// (1 = best) among all candidates.
pub fn mention_rank(rng: &mut impl Rng, inst: &Instance) -> usize {
    score_rank(rng, inst, |kg, cand| {
        kg.mentions[cand]
            .iter()
            .filter(|&&pid| {
                let p = kg.passages[pid];
                inst.seeds.contains(&p.src) || inst.seeds.contains(&p.dst)
            })
            .count() as f64
    })
}

/// BFS-distance ranking — closer to a seed is better. Coverage without
/// association: every candidate sits at the same depth, so this is
/// chance too. Returns the answer's rank among candidates.
pub fn bfs_rank(rng: &mut impl Rng, inst: &Instance) -> usize {
    let dist = inst.kg.bfs_dist(&inst.seeds);
    score_rank(rng, inst, |_, cand| -(dist[cand] as f64))
}

/// Rank the answer among candidates under an arbitrary score (higher =
/// better); random tie-break via pre-shuffle.
pub fn score_rank(
    rng: &mut impl Rng,
    inst: &Instance,
    score: impl Fn(&Kg, usize) -> f64,
) -> usize {
    let mut order: Vec<usize> = inst.candidates.clone();
    order.shuffle(rng);
    order.sort_by(|&a, &b| score(&inst.kg, b).total_cmp(&score(&inst.kg, a)));
    1 + order.iter().position(|&c| c == inst.answer).unwrap()
}

/// Mean rank of the answer over `trials` instances.
pub fn mean_rank(
    rng: &mut impl Rng,
    trials: usize,
    hops: usize,
    distractors: usize,
    rank_fn: impl Fn(&mut ChaCha8Rng, &Instance) -> usize,
) -> f64 {
    let mut total = 0usize;
    for _ in 0..trials {
        let inst = path_finding_instance(hops, distractors);
        let mut trial_rng = ChaCha8Rng::seed_from_u64(rng.gen());
        total += rank_fn(&mut trial_rng, &inst);
    }
    total as f64 / trials as f64
}

/// A random KG for throughput runs: n nodes, ~avg_deg edges per node
/// (one passage per edge, like everything else here).
pub fn random_kg(rng: &mut impl Rng, n: usize, avg_deg: usize) -> Kg {
    let mut kg = Kg::new(n);
    for _ in 0..n * avg_deg / 2 {
        let a = rng.gen_range(0..n);
        let b = rng.gen_range(0..n);
        if a != b {
            kg.add_fact(a, b);
        }
    }
    kg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_shape_is_as_advertised() {
        let inst = path_finding_instance(2, 8);
        // 2*(8+1) chains of 2 edges = 36 passages; 1 + 2*8 = 17 candidates.
        assert_eq!(inst.kg.passages.len(), 36);
        assert_eq!(inst.candidates.len(), 17);
        assert_eq!(inst.gold_passages.len(), 4);
        // The answer is 2 hops from a seed; no passage mentions both seeds.
        let dist = inst.kg.bfs_dist(&inst.seeds);
        assert_eq!(dist[inst.answer], 2);
        assert!(inst.kg.passages.iter().all(|p| {
            !(inst.seeds.contains(&p.src) && inst.seeds.contains(&p.dst))
        }));
    }

    #[test]
    fn mention_ranking_solves_one_hop() {
        // h=1: the answer is named in TWO seed passages, distractors in one.
        let mut rng = seeded_rng(1);
        let mean = mean_rank(&mut rng, 200, 1, 8, mention_rank);
        assert!(mean < 1.05, "mean rank {mean} — should be ~1");
    }

    #[test]
    fn mention_ranking_collapses_at_two_hops() {
        // h=2: interior passages mention no seed; every candidate scores
        // the same, so the answer's mean rank is chance: (1+17)/2 = 9.
        let mut rng = seeded_rng(2);
        let mean = mean_rank(&mut rng, 400, 2, 8, mention_rank);
        assert!(
            (mean - 9.0).abs() < 1.0,
            "mean rank {mean} — should be ~9 (chance among 17)"
        );
    }
}
