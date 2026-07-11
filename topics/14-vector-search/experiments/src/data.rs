//! Provided: seeded clustered vectors — Gaussian blobs, not uniform
//! noise. Uniform random vectors in high d are all nearly equidistant
//! (curse of dimensionality) and make every ANN index look useless;
//! real embeddings cluster. Topic 10's lesson: uniform synthetic data
//! lies.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct Dataset {
    pub dim: usize,
    /// row-major: vectors[i*dim..(i+1)*dim]
    pub vectors: Vec<f32>,
}

impl Dataset {
    pub fn get(&self, i: u32) -> &[f32] {
        &self.vectors[i as usize * self.dim..(i as usize + 1) * self.dim]
    }

    pub fn len(&self) -> u32 {
        (self.vectors.len() / self.dim) as u32
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }
}

/// `n` vectors around `n_clusters` Gaussian centers.
pub fn clustered(n: u32, dim: usize, n_clusters: usize, seed: u64) -> Dataset {
    let mut rng = StdRng::seed_from_u64(seed);
    let centers: Vec<f32> = (0..n_clusters * dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
    let mut vectors = Vec::with_capacity(n as usize * dim);
    for _ in 0..n {
        let c = rng.gen_range(0..n_clusters);
        for j in 0..dim {
            let noise: f32 = {
                // Box-Muller: cheap seeded gaussian
                let (u1, u2): (f32, f32) = (rng.gen_range(1e-6..1.0f32), rng.gen());
                (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
            };
            vectors.push(centers[c * dim + j] + noise * 0.15);
        }
    }
    Dataset { dim, vectors }
}

/// Query vectors: perturbed dataset rows — near-neighbor structure
/// guaranteed, like ann-benchmarks' held-out splits.
pub fn queries(base: &Dataset, n: u32, seed: u64) -> Dataset {
    let mut rng = StdRng::seed_from_u64(seed ^ 0x9e37_79b9_7f4a_7c15);
    let mut vectors = Vec::with_capacity(n as usize * base.dim);
    for _ in 0..n {
        let row = base.get(rng.gen_range(0..base.len()));
        for &x in row {
            vectors.push(x + rng.gen_range(-0.05..0.05f32));
        }
    }
    Dataset { dim: base.dim, vectors }
}
