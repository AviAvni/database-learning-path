//! The seeded social graph (PROVIDED).
//!
//! The query is the three-hop Cypher pattern
//!
//! ```text
//! MATCH (a:Person)-[:KNOWS]->(b:Person)-[:LIKES]->(m:Post)-[:HAS_TAG]->(t:Tag)
//! WHERE t.class = 'rare'
//! RETURN count(*)
//! ```
//!
//! as a chain of four relations — `knows(a,b) ⋈ likes(b,m) ⋈ has_tag(m,t) ⋈
//! rare_tag(t)` — where the only selective predicate sits on the relation
//! three joins away from the biggest one. Every edge endpoint is drawn
//! uniformly from a ChaCha8 stream, so each expected size is one line of
//! arithmetic (see README Step 1) and every count reproduces exactly.

use crate::rel::{Query, Relation};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub const A: usize = 0;
pub const B: usize = 1;
pub const M: usize = 2;
pub const T: usize = 3;

#[derive(Clone, Copy, Debug)]
pub struct GraphParams {
    pub persons: u32,
    pub knows_per_person: u32,
    pub likes_per_person: u32,
    pub posts: u32,
    pub tags_per_post: u32,
    pub tags: u32,
    /// A tag is "rare" when `tag % rare_every == 0`: selectivity 1/rare_every.
    pub rare_every: u32,
    pub seed: u64,
}

impl GraphParams {
    /// The lane-1 instance: 200,000 KNOWS edges, 100,000 LIKES, 100,000
    /// HAS_TAG, and 10 rare tags out of 1,000 (1%).
    pub const LANE1: GraphParams = GraphParams {
        persons: 20_000,
        knows_per_person: 10,
        likes_per_person: 5,
        posts: 50_000,
        tags_per_post: 2,
        tags: 1_000,
        rare_every: 100,
        seed: 47,
    };

    /// A tenth of the persons and posts, the same 10 rare tags of 1,000 — for tests.
    pub const SMALL: GraphParams = GraphParams {
        persons: 2_000,
        knows_per_person: 10,
        likes_per_person: 5,
        posts: 5_000,
        tags_per_post: 2,
        tags: 1_000,
        rare_every: 100,
        seed: 7,
    };
}

/// Build the four relations. Generation order is fixed (knows, likes,
/// has_tag; each by source id ascending), and the stream is consumed in that
/// order, so the same seed always gives the same rows in the same positions.
pub fn social_graph(p: GraphParams) -> Query {
    let mut rng = ChaCha8Rng::seed_from_u64(p.seed);

    let (mut ka, mut kb) = (Vec::new(), Vec::new());
    for a in 0..p.persons {
        for _ in 0..p.knows_per_person {
            // uniform over the other persons: no self-loops
            let mut b = rng.gen_range(0..p.persons - 1);
            if b >= a {
                b += 1;
            }
            ka.push(a);
            kb.push(b);
        }
    }

    let (mut lb, mut lm) = (Vec::new(), Vec::new());
    for b in 0..p.persons {
        for _ in 0..p.likes_per_person {
            lb.push(b);
            lm.push(rng.gen_range(0..p.posts));
        }
    }

    let (mut hm, mut ht) = (Vec::new(), Vec::new());
    for m in 0..p.posts {
        for _ in 0..p.tags_per_post {
            hm.push(m);
            ht.push(rng.gen_range(0..p.tags));
        }
    }

    // σ(class = 'rare') applied at the scan: the filtered tag relation.
    let rare: Vec<u32> = (0..p.tags).filter(|t| t % p.rare_every == 0).collect();

    Query {
        var_names: vec!["a", "b", "m", "t"],
        rels: vec![
            Relation::new("knows", vec![A, B], vec![ka, kb]),
            Relation::new("likes", vec![B, M], vec![lb, lm]),
            Relation::new("has_tag", vec![M, T], vec![hm, ht]),
            Relation::new("rare_tag", vec![T], vec![rare]),
        ],
    }
}

/// The two left-deep orders lane 1 compares, as positions in `Query::rels`.
/// GOOD starts at the predicate; BAD starts at the biggest relation and meets
/// the predicate last — what a planner does when it believes `rare_tag` is
/// large or `knows` is selective.
pub const GOOD: [usize; 4] = [3, 2, 1, 0];
pub const BAD: [usize; 4] = [0, 1, 2, 3];
