//! Topic 18 experiments — GPU compute via wgpu (Metal on this Mac).
//!
//! PROVIDED: GpuCtx + a working sum-reduction kernel (upload →
//! dispatch → readback, with phase timings). YOURS: filter_count and
//! l2_batch shaders + wrappers. The bench finds the CPU/GPU
//! crossover batch size INCLUDING transfer time.

pub mod cpu;
pub mod gpu;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub fn gen_f32(n: usize, seed: u64) -> Vec<f32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n).map(|_| rng.gen::<f32>()).collect()
}
