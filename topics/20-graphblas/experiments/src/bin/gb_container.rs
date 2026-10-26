//! gb_container — what does it cost to move a matrix across the Rust/GraphBLAS boundary?
//!
//! Lane A: `GxB_Container` load/unload (O(1), zero-copy) vs `GrB_Matrix_build` /
//!         `GrB_Matrix_extractTuples` (O(nnz) or worse, copying) vs a plain memcpy of
//!         the same arrays (the floor for any copying hand-off).
//! Lane B: the topic's Rust SpMV kernel vs GraphBLAS's own `GrB_mxv` on the SAME seeded
//!         matrix — the Rust CSR itself, handed over via Container.
//!
//! Needs a SuiteSparse:GraphBLAS v10 install (see build.rs). Without one it prints a
//! skip note and exits 0.
//!
//! cargo run --release --bin gb_container

#[cfg(not(have_graphblas))]
fn main() {
    println!("=== gb_container ===\n");
    println!("[skip — libgraphblas not found: set GRAPHBLAS_DIR to a SuiteSparse:GraphBLAS v10 install prefix]");
}

#[cfg(have_graphblas)]
fn main() {
    lanes::run();
}

#[cfg(have_graphblas)]
mod lanes {
    use graphblas_experiments::{
        csr::{rmat, Csr},
        gbffi::{self, Mat, Vector},
        spmv,
    };
    use rand::seq::SliceRandom;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::time::Instant;

    pub fn run() {
        println!("=== gb_container ===\n");
        gbffi::init();
        if std::env::var("GB_BURBLE").is_ok() {
            gbffi::burble(true);
        }
        let (a, b, c) = gbffi::version();
        println!(
            "libgraphblas {a}.{b}.{c} from {} ({} threads by default)\n",
            env!("GRAPHBLAS_PREFIX"),
            gbffi::threads()
        );
        let scales = [14u32, 16, 18, 20];
        handoff(&scales);
        spmv_vs_mxv(&scales);
    }

    fn best(reps: usize, mut f: impl FnMut()) -> f64 {
        let mut b = f64::INFINITY;
        for _ in 0..reps {
            let t0 = Instant::now();
            f();
            b = b.min(t0.elapsed().as_secs_f64() * 1e6);
        }
        b
    }

    fn tuples(a: &Csr) -> (Vec<u64>, Vec<u64>, Vec<f64>) {
        let mut i = Vec::with_capacity(a.nnz());
        let mut j = Vec::with_capacity(a.nnz());
        for r in 0..a.n {
            let (cols, _) = a.row(r);
            for &c in cols {
                i.push(r as u64);
                j.push(c as u64);
            }
        }
        (i, j, a.vals.clone())
    }

    /// Read a GraphBLAS-owned index array of 32 or 64 bits as u64s (for comparison only).
    unsafe fn idx(p: *const u8, bits: i32, k: usize) -> u64 {
        if bits == 32 {
            *(p as *const u32).add(k) as u64
        } else {
            *(p as *const u64).add(k)
        }
    }

    fn handoff(scales: &[u32]) {
        println!("-- Lane A: moving an RMAT edge_factor-8 matrix between Rust and GraphBLAS, best of N, µs --");
        println!(
            "{:>5} {:>9} {:>9} | {:>10} {:>11} {:>12} | {:>9} {:>9} | {:>9} {:>9} | {:>10}",
            "scale", "nnz", "MB", "build", "build", "extract", "load", "unload", "unload", "reload", "memcpy"
        );
        println!(
            "{:>5} {:>9} {:>9} | {:>10} {:>11} {:>12} | {:>9} {:>9} | {:>9} {:>9} | {:>10}",
            "", "", "", "sorted", "shuffled", "Tuples", "Rust CSR", "Rust CSR", "GB-owned", "GB-owned", "of CSR"
        );
        for &scale in scales {
            let a = rmat(scale, 8, 42);
            let n = a.n;
            let nnz = a.nnz();
            let mb = (a.rowptr.len() * 8 + nnz * 12) as f64 / 1e6;
            let reps = if scale >= 20 { 3 } else { 5 };

            // --- copy path: tuples in -----------------------------------------------------
            let (ti, tj, tx) = tuples(&a);
            let mut perm: Vec<usize> = (0..nnz).collect();
            perm.shuffle(&mut ChaCha8Rng::seed_from_u64(7));
            let si: Vec<u64> = perm.iter().map(|&k| ti[k]).collect();
            let sj: Vec<u64> = perm.iter().map(|&k| tj[k]).collect();
            let sx: Vec<f64> = perm.iter().map(|&k| tx[k]).collect();

            let mut m = Mat::new(n);
            let us_build_sorted = best(reps, || m.build(&ti, &tj, &tx));
            let us_build_shuf = best(reps, || m.build(&si, &sj, &sx));
            assert_eq!(m.nvals(), nnz, "build lost or merged entries");

            // --- copy path: tuples out ----------------------------------------------------
            let (mut oi, mut oj, mut ox) = (vec![0u64; nnz], vec![0u64; nnz], vec![0f64; nnz]);
            let us_extract = best(reps, || {
                std::hint::black_box(m.extract(&mut oi, &mut oj, &mut ox));
            });
            // extractTuples of a CSR matrix returns row-major sorted tuples: identical to ours
            assert!(oi == ti && oj == tj && ox == tx, "extractTuples disagrees with the seeded CSR");

            // --- Container: Rust-owned CSR in, readonly (zero-copy) ------------------------
            // One iteration = load (timed) then unload (timed). The unload leaves the matrix
            // empty, so the next load starts from the same state every time.
            let mut mc = Mat::new(n);
            let (mut best_load, mut best_unload) = (f64::INFINITY, f64::INFINITY);
            let mut views;
            for _ in 0..reps * 4 {
                let t0 = Instant::now();
                mc.load_csr_readonly(&a);
                best_load = best_load.min(t0.elapsed().as_secs_f64() * 1e6);
                let t0 = Instant::now();
                views = mc.unload();
                best_unload = best_unload.min(t0.elapsed().as_secs_f64() * 1e6);
                // the pointers coming back are the ones Rust gave: nothing was copied
                assert_eq!(views.p as *const u8, a.rowptr.as_ptr() as *const u8);
                assert_eq!(views.i as *const u8, a.colidx.as_ptr() as *const u8);
                assert_eq!(views.x as *const u8, a.vals.as_ptr() as *const u8);
            }
            mc.load_csr_readonly(&a);
            assert_eq!(mc.nvals(), nnz);

            // --- Container: GraphBLAS-owned matrix out and back ----------------------------
            // m was made by GrB_Matrix_build, so GraphBLAS allocated its arrays (handling 0).
            let (mut best_un, mut best_re) = (f64::INFINITY, f64::INFINITY);
            let mut desc = String::new();
            for rep in 0..reps * 4 {
                let t0 = Instant::now();
                let v = m.unload();
                best_un = best_un.min(t0.elapsed().as_secs_f64() * 1e6);
                if rep == 0 {
                    let l = m.layout();
                    desc = format!(
                        "format {} {} iso={} Ap u{} Ai u{} owned={}",
                        match l.format {
                            1 => "hypersparse",
                            2 => "sparse",
                            4 => "bitmap",
                            8 => "full",
                            _ => "?",
                        },
                        if l.row_major { "by-row" } else { "by-col" },
                        l.iso,
                        v.p_bits,
                        v.i_bits,
                        v.p_h == 0
                    );
                    // GraphBLAS's own CSR, read in place from Rust, equals the seeded one
                    assert_eq!(l.format, 2, "expected GraphBLAS to hold RMAT as sparse CSR");
                    assert!(l.row_major);
                    unsafe {
                        for r in 0..=n {
                            assert_eq!(idx(v.p, v.p_bits, r), a.rowptr[r] as u64);
                        }
                        for k in 0..nnz {
                            assert_eq!(idx(v.i, v.i_bits, k), a.colidx[k] as u64);
                            assert_eq!(*(v.x as *const f64).add(k), a.vals[k]);
                        }
                    }
                }
                let t0 = Instant::now();
                m.load_views(&v, nnz);
                best_re = best_re.min(t0.elapsed().as_secs_f64() * 1e6);
            }
            assert_eq!(m.nvals(), nnz);

            // --- the floor for any copying hand-off: memcpy of the same three arrays ------
            let us_memcpy = best(reps, || {
                let c = (a.rowptr.clone(), a.colidx.clone(), a.vals.clone());
                std::hint::black_box(&c);
            });

            println!(
                "{:>5} {:>9} {:>9.1} | {:>10.0} {:>11.0} {:>12.0} | {:>9.2} {:>9.2} | {:>9.2} {:>9.2} | {:>10.0}",
                scale, nnz, mb, us_build_sorted, us_build_shuf, us_extract, best_load, best_unload, best_un, best_re,
                us_memcpy
            );
            if scale == scales[0] {
                println!("      (GraphBLAS built that matrix as: {desc})");
            }
        }
        println!();
    }

    /// One row of lane B: Rust kernel, then GrB_mxv on the Rust CSR (Container, 1 thread),
    /// on a GraphBLAS-built copy (1 thread), and on the Rust CSR with every thread.
    fn mxv_row(a: &Csr, x: &[f64], default_threads: i32) -> (f64, f64, f64) {
        let ysum_rust: f64 = spmv::spmv(a, x).iter().sum();
        let mut m = Mat::new(a.n);
        m.load_csr_readonly(a);
        let mut xv = Vector::new(a.n);
        xv.load_readonly(x);
        let mut y = Vector::new(a.n);
        let timed = |m: &Mat, y: &mut Vector, threads: i32| {
            gbffi::set_threads(threads);
            gbffi::mxv(y, m, &xv); // warm-up: the first call allocates the result (and compiles, with the JIT on)
            best(5, || gbffi::mxv(y, m, &xv))
        };
        let us_1 = timed(&m, &mut y, 1);
        // y has one entry per non-empty row of A (GraphBLAS stores no explicit zeros it was not given)
        let nonempty = (0..a.n).filter(|&r| a.rowptr[r + 1] > a.rowptr[r]).count();
        assert_eq!(y.nvals() as usize, nonempty, "y has one entry per non-empty row of A");
        let chk = y.sum();
        assert!(
            (chk - ysum_rust).abs() <= 1e-9 * ysum_rust.abs().max(1.0),
            "mxv sum {chk} vs Rust {ysum_rust}"
        );
        // the same product on a matrix GraphBLAS built itself, in case a Container-loaded
        // (shallow) matrix were handicapped in some way
        let (ti, tj, tx) = tuples(a);
        let mut mb = Mat::new(a.n);
        mb.build(&ti, &tj, &tx);
        let us_built = timed(&mb, &mut y, 1);
        let us_n = timed(&m, &mut y, default_threads);
        gbffi::set_threads(default_threads);
        (us_1, us_built, us_n)
    }

    fn spmv_vs_mxv(scales: &[u32]) {
        println!("-- Lane B: y = A*x over (PLUS,TIMES) f64 — Rust kernel vs GrB_mxv, same seeded matrix, best of 5, µs --");
        println!("   GraphBLAS reads the Rust CSR in place (Container load, readonly); the Rust kernel is single-threaded.");
        println!("   GB/s uses lane 1's byte count (spmv::spmv_bytes) for every column, so the columns compare like with like.");
        let default_threads = gbffi::threads();
        let mats: Vec<(Csr, Vec<f64>)> = scales
            .iter()
            .map(|&s| {
                let a = rmat(s, 8, 42);
                let x: Vec<f64> = (0..a.n).map(|i| (i % 7) as f64).collect();
                (a, x)
            })
            .collect();
        let rust_us: Vec<f64> = mats
            .iter()
            .map(|(a, x)| {
                best(5, || {
                    std::hint::black_box(spmv::spmv(a, x));
                })
            })
            .collect();
        for jit in [false, true] {
            let level = gbffi::set_jit(jit);
            println!(
                "\n   GraphBLAS JIT {} (GxB_JIT_C_CONTROL = {level}):",
                if jit { "ON — the dot kernel is compiled for FP64 PLUS_TIMES before timing" } else { "OFF — generic kernel, one function-pointer call per entry" }
            );
            if jit && level == 0 {
                println!("   [skip — the JIT could not be enabled in this build]");
                continue;
            }
            println!(
                "{:>5} {:>9} | {:>9} {:>6} | {:>12} {:>6} | {:>14} {:>6} | {:>13} {:>6}",
                "scale", "nnz", "Rust µs", "GB/s", "mxv 1thr µs", "GB/s", "mxv built 1thr", "GB/s",
                &format!("mxv {}thr µs", default_threads), "GB/s"
            );
            for (k, (a, x)) in mats.iter().enumerate() {
                let bytes = spmv::spmv_bytes(a) as f64;
                let g = |us: f64| bytes / us / 1e3;
                let (us_1, us_built, us_n) = mxv_row(a, x, default_threads);
                println!(
                    "{:>5} {:>9} | {:>9.0} {:>6.2} | {:>12.0} {:>6.2} | {:>14.0} {:>6.2} | {:>13.0} {:>6.2}",
                    scales[k], a.nnz(), rust_us[k], g(rust_us[k]), us_1, g(us_1), us_built, g(us_built), us_n, g(us_n)
                );
            }
        }
        println!("\n   sum(y) of GrB_mxv matches the Rust kernel to 1e-9 relative on every row.");
    }
}
