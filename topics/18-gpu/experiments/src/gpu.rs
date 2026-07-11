//! PROVIDED GPU infrastructure. Pipelines are built once in
//! `GpuCtx::new`; each call uploads, dispatches, and reads back —
//! that round trip is the whole lesson.

use std::time::Instant;
use wgpu::util::DeviceExt;

const WG: u32 = 256; // workgroup size
const PER_THREAD: u32 = 4; // elements each invocation folds
pub const ELEMS_PER_GROUP: u32 = WG * PER_THREAD; // 1024

#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    /// buffer creation + staged upload (host → GPU-visible memory)
    pub upload_us: f64,
    /// submit → queue idle (kernel + internal copies)
    pub gpu_us: f64,
    /// map + copy result back to host
    pub readback_us: f64,
}

impl Timings {
    pub fn total_us(&self) -> f64 {
        self.upload_us + self.gpu_us + self.readback_us
    }
}

pub struct GpuCtx {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    sum_pipeline: wgpu::ComputePipeline,
    pub adapter_name: String,
}

impl GpuCtx {
    pub fn new() -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }),
        )
        .expect("no GPU adapter (Metal expected on macOS)");
        let adapter_name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default(), None),
        )
        .expect("device");

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sum"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/sum.wgsl").into()),
        });
        let sum_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("sum"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });

        Self { device, queue, sum_pipeline, adapter_name }
    }

    /// PROVIDED: sum via one workgroup-reduction pass (1024 elems →
    /// 1 partial each), partials summed on the host. End-to-end,
    /// transfers included — that's the point.
    pub fn sum(&self, vals: &[f32]) -> (f32, Timings) {
        let mut t = Timings::default();
        let n_groups = (vals.len() as u32).div_ceil(ELEMS_PER_GROUP).max(1);

        let t0 = Instant::now();
        let input = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(vals),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let partials = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: n_groups as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: n_groups as u64 * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        t.upload_us = t0.elapsed().as_secs_f64() * 1e6;

        let t1 = Instant::now();
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.sum_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: partials.as_entire_binding() },
            ],
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.sum_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(n_groups, 1, 1);
        }
        enc.copy_buffer_to_buffer(&partials, 0, &readback, 0, n_groups as u64 * 4);
        self.queue.submit([enc.finish()]);
        self.device.poll(wgpu::Maintain::Wait);
        t.gpu_us = t1.elapsed().as_secs_f64() * 1e6;

        let t2 = Instant::now();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        self.device.poll(wgpu::Maintain::Wait);
        let sum: f64 = {
            let data = slice.get_mapped_range();
            bytemuck::cast_slice::<u8, f32>(&data).iter().map(|&v| v as f64).sum()
        };
        t.readback_us = t2.elapsed().as_secs_f64() * 1e6;

        (sum as f32, t)
    }

    /// YOURS: count of vals < t on the GPU.
    ///
    /// Write `shaders/filter_count.wgsl` (skeleton provided): each
    /// invocation folds PER_THREAD elements into a private count,
    /// workgroup-reduce in shared memory (like sum.wgsl), then ONE
    /// `atomicAdd` per workgroup on a single u32 counter — not one
    /// per element (atomics serialize; amortize them). Pass `t` via
    /// a uniform buffer. Wire the pipeline here like `sum`.
    pub fn filter_count(&self, _vals: &[f32], _t: f32) -> (u32, Timings) {
        todo!("filter_count.wgsl + pipeline: workgroup reduce, one atomicAdd per group")
    }

    /// YOURS: squared L2 of one query against M targets (dim ≤ 1024).
    ///
    /// One invocation per target vector: loop dim, accumulate
    /// (q[i]-t[i])², write out[target]. Query in a uniform or
    /// storage buffer. Then answer in notes.md: why is
    /// one-thread-per-TARGET right here and one-thread-per-DIM
    /// wrong (coalescing: adjacent threads read adjacent targets'
    /// SAME component only if targets are column-major — which
    /// layout did you pick?).
    pub fn l2_batch(&self, _query: &[f32], _targets: &[f32], _dim: usize) -> (Vec<f32>, Timings) {
        todo!("l2_batch.wgsl + pipeline: one invocation per target")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cpu, gen_f32};

    #[test]
    fn gpu_sum_matches_cpu() {
        let ctx = GpuCtx::new();
        for n in [1usize, 1023, 1024, 1025, 100_000] {
            let vals = gen_f32(n, n as u64);
            let (got, _) = ctx.sum(&vals);
            let expect: f64 = vals.iter().map(|&v| v as f64).sum();
            let rel = ((got as f64 - expect) / expect.max(1.0)).abs();
            assert!(rel < 1e-4, "n={n}: {got} vs {expect}");
        }
    }

    #[test]
    fn gpu_filter_count_matches_cpu() {
        let ctx = GpuCtx::new();
        let vals = gen_f32(100_003, 7);
        let (got, _) = ctx.filter_count(&vals, 0.42);
        assert_eq!(got, cpu::filter_count(&vals, 0.42));
    }

    #[test]
    fn gpu_l2_batch_matches_cpu() {
        let ctx = GpuCtx::new();
        let dim = 128;
        let query = gen_f32(dim, 1);
        let targets = gen_f32(dim * 1000, 2);
        let (got, _) = ctx.l2_batch(&query, &targets, dim);
        let expect = cpu::l2_batch(&query, &targets, dim);
        assert_eq!(got.len(), expect.len());
        for (i, (g, e)) in got.iter().zip(&expect).enumerate() {
            assert!((g - e).abs() / e.max(1e-3) < 1e-3, "target {i}: {g} vs {e}");
        }
    }
}
