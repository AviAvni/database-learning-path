//! Skip-gram with negative sampling over random walks = DeepWalk/node2vec's
//! training loop. Walks are "sentences", vertices are "words"; the whole
//! word2vec machine transfers unchanged.

use crate::dense::Mat;

/// Mean cosine over up to `max_pairs` sampled (i, j) pairs from the given
/// index sets (provided — used by the contract test and the bench lane).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[inline]
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// STUB — train skip-gram embeddings with negative sampling (SGNS).
///
/// Model: TWO matrices, "in" Z (n x dim, returned) and "out" C (n x dim).
/// For each walk, each center position i, each context position j with
/// |i - j| <= window, j != i:
///   pos pair (u = walk[i], c = walk[j]):
///     s = sigmoid(z_u . c_c);       g = lr * (1 - s)
///     z_u += g * c_c;  c_c += g * z_u   (use the PRE-update z_u)
///   then `negs` negative samples c' drawn uniformly from 0..n:
///     s = sigmoid(z_u . c_c');      g = lr * (0 - s)
///     symmetric update with c_c'.
/// (That's the gradient of  log sigma(z.c) + sum_neg log sigma(-z.c') —
/// PyG's Node2Vec.loss at node2vec.py:135 is this exact expression.)
///
/// Init both matrices U(-0.5/dim, 0.5/dim) from ChaCha8Rng::seed_from_u64(seed).
/// `epochs` full passes over the walks; linear LR decay per epoch is fine
/// but not required by the tests. Return Z.
pub fn train_skipgram(
    _walks: &[Vec<u32>],
    _n: usize,
    _dim: usize,
    _window: usize,
    _negs: usize,
    _epochs: usize,
    _lr: f32,
    _seed: u64,
) -> Mat {
    todo!("SGNS: sliding window, sigmoid updates, uniform negative samples")
}

/// Mean cosine between embedding rows for pairs where pred(i, j) holds,
/// sampled on a fixed grid (deterministic, no rng).
pub fn mean_pair_cosine<F: Fn(usize, usize) -> bool>(z: &Mat, pred: F, max_pairs: usize) -> f32 {
    let mut sum = 0.0f32;
    let mut cnt = 0usize;
    let stride = 7usize;
    'outer: for i in (0..z.rows).step_by(stride) {
        for j in (i + 1..z.rows).step_by(stride + 4) {
            if pred(i, j) {
                sum += cosine(z.row(i), z.row(j));
                cnt += 1;
                if cnt >= max_pairs {
                    break 'outer;
                }
            }
        }
    }
    if cnt == 0 {
        0.0
    } else {
        sum / cnt as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::gen_sbm;
    use crate::walks::uniform_walks;

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 1.0], &[-1.0, -1.0]) + 1.0).abs() < 1e-6);
    }

    // THE contract: embeddings trained on SBM walks must place same-block
    // vertices closer (higher cosine) than cross-block vertices, by a real
    // margin. This is what "embeddings capture community structure" means,
    // reduced to an assertion.
    #[test]
    fn sbm_blocks_separate_in_embedding_space() {
        let (g, labels) = gen_sbm(4, 48, 0.25, 0.01, 11);
        let walks = uniform_walks(&g, 20, 8, 13);
        let z = train_skipgram(&walks, g.n, 32, 4, 5, 3, 0.05, 17);
        assert_eq!((z.rows, z.cols), (g.n, 32));
        let intra = mean_pair_cosine(&z, |i, j| labels[i] == labels[j], 400);
        let inter = mean_pair_cosine(&z, |i, j| labels[i] != labels[j], 400);
        assert!(
            intra > inter + 0.2,
            "intra {:.3} not separated from inter {:.3}",
            intra,
            inter
        );
    }
}
