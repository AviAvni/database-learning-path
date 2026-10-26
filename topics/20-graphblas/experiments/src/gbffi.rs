//! Safe-ish Rust wrappers over `shim/gb_shim.c` — compiled only when `build.rs`
//! found a SuiteSparse:GraphBLAS v10 install (`cfg(have_graphblas)`).
//!
//! The point of this module is `Mat::load_csr_readonly`: hand a Rust-owned
//! [`Csr`](crate::csr::Csr) to GraphBLAS through `GxB_Container` without copying it.

use crate::csr::Csr;
use std::os::raw::{c_double, c_int};

/// `GxB_IS_READONLY` in GraphBLAS.h: the caller keeps ownership of the array.
pub const IS_READONLY: c_int = 7078;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Views {
    pub p: *mut u8,
    pub i: *mut u8,
    pub x: *mut u8,
    pub p_len: u64,
    pub i_len: u64,
    pub x_len: u64,
    pub p_mem: u64,
    pub i_mem: u64,
    pub x_mem: u64,
    pub p_h: c_int,
    pub i_h: c_int,
    pub x_h: c_int,
    pub p_bits: c_int,
    pub i_bits: c_int,
}

impl Views {
    pub fn zeroed() -> Views {
        // all-zero is a valid (null, empty) Views
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
pub struct RawMat {
    _p: [u8; 0],
}
#[repr(C)]
pub struct RawVec {
    _p: [u8; 0],
}

extern "C" {
    fn gbs_init() -> c_int;
    fn gbs_version(out: *mut c_int);
    fn gbs_set_threads(t: c_int) -> c_int;
    fn gbs_get_threads() -> c_int;
    fn gbs_burble(on: c_int) -> c_int;
    fn gbs_set_jit(on: c_int, level: *mut c_int) -> c_int;
    fn gbs_mat_new(n: u64) -> *mut RawMat;
    fn gbs_mat_free(m: *mut RawMat);
    fn gbs_nvals(m: *mut RawMat) -> u64;
    fn gbs_build(m: *mut RawMat, n: u64, nnz: u64, i: *const u64, j: *const u64, x: *const c_double) -> c_int;
    fn gbs_extract(m: *mut RawMat, nnz: *mut u64, i: *mut u64, j: *mut u64, x: *mut c_double) -> c_int;
    fn gbs_load_csr(m: *mut RawMat, n: u64, nnz: u64, v: *const Views) -> c_int;
    fn gbs_unload_csr(m: *mut RawMat, v: *mut Views) -> c_int;
    fn gbs_container_format(m: *mut RawMat) -> c_int;
    fn gbs_container_orient(m: *mut RawMat) -> c_int;
    fn gbs_container_iso(m: *mut RawMat) -> c_int;
    fn gbs_free_views(v: *mut Views);
    fn gbs_vec_new(n: u64) -> *mut RawVec;
    fn gbs_vec_free(v: *mut RawVec);
    fn gbs_vec_load_ro(v: *mut RawVec, x: *mut c_double, n: u64) -> c_int;
    fn gbs_mxv(y: *mut RawVec, a: *mut RawMat, x: *mut RawVec) -> c_int;
    fn gbs_vec_sum(y: *mut RawVec, s: *mut c_double) -> c_int;
    fn gbs_vec_nvals(y: *mut RawVec, nv: *mut u64) -> c_int;
}

fn ck(what: &str, info: c_int) {
    assert_eq!(info, 0, "{what} returned GrB_Info {info}");
}

pub fn init() {
    ck("GrB_init", unsafe { gbs_init() });
}
pub fn version() -> (i32, i32, i32) {
    let mut v = [0 as c_int; 3];
    unsafe { gbs_version(v.as_mut_ptr()) };
    (v[0], v[1], v[2])
}
pub fn set_threads(t: i32) {
    ck("GxB_NTHREADS", unsafe { gbs_set_threads(t) });
}
/// Switch the CPU JIT on or off; returns the level GraphBLAS reports afterwards.
pub fn set_jit(on: bool) -> i32 {
    let mut level = -1;
    ck("GxB_JIT_C_CONTROL", unsafe { gbs_set_jit(on as c_int, &mut level) });
    level
}
/// GxB_BURBLE: GraphBLAS prints which method each call chose (stdout).
pub fn burble(on: bool) {
    ck("GxB_BURBLE", unsafe { gbs_burble(on as c_int) });
}
pub fn threads() -> i32 {
    unsafe { gbs_get_threads() }
}

pub struct Mat {
    raw: *mut RawMat,
    pub n: usize,
}

/// Which CSR arrays does this unload expose, and how wide are the indices?
pub struct Layout {
    pub format: i32, // GxB_HYPERSPARSE=1, SPARSE=2, BITMAP=4, FULL=8
    pub row_major: bool,
    pub iso: bool,
}

impl Mat {
    pub fn new(n: usize) -> Mat {
        let raw = unsafe { gbs_mat_new(n as u64) };
        assert!(!raw.is_null());
        Mat { raw, n }
    }
    pub fn nvals(&self) -> usize {
        unsafe { gbs_nvals(self.raw) as usize }
    }
    /// `GrB_Matrix_build_FP64` with a PLUS duplicate operator, then `GrB_wait`.
    pub fn build(&mut self, i: &[u64], j: &[u64], x: &[f64]) {
        ck("GrB_Matrix_build", unsafe {
            gbs_build(self.raw, self.n as u64, i.len() as u64, i.as_ptr(), j.as_ptr(), x.as_ptr())
        });
    }
    /// `GrB_Matrix_extractTuples_FP64` into caller buffers (capacity = nnz).
    pub fn extract(&self, i: &mut [u64], j: &mut [u64], x: &mut [f64]) -> usize {
        let mut nnz = i.len() as u64;
        ck("GrB_Matrix_extractTuples", unsafe {
            gbs_extract(self.raw, &mut nnz, i.as_mut_ptr(), j.as_mut_ptr(), x.as_mut_ptr())
        });
        nnz as usize
    }
    /// Container load, zero-copy: GraphBLAS keeps *pointers into `a`* (a "shallow",
    /// read-only matrix). `a` must outlive every use of this matrix until it is
    /// unloaded or replaced. rowptr goes in as UINT64 and colidx as UINT32 — the
    /// exact widths `Csr` already stores, so nothing is converted.
    pub fn load_csr_readonly(&mut self, a: &Csr) {
        let v = Views {
            p: a.rowptr.as_ptr() as *mut u8,
            i: a.colidx.as_ptr() as *mut u8,
            x: a.vals.as_ptr() as *mut u8,
            p_len: a.rowptr.len() as u64,
            i_len: a.colidx.len() as u64,
            x_len: a.vals.len() as u64,
            p_mem: (a.rowptr.len() * 8) as u64,
            i_mem: (a.colidx.len() * 4) as u64,
            x_mem: (a.vals.len() * 8) as u64,
            p_h: IS_READONLY,
            i_h: IS_READONLY,
            x_h: IS_READONLY,
            p_bits: 64,
            i_bits: 32,
        };
        ck("load_Matrix_from_Container", unsafe { gbs_load_csr(self.raw, a.n as u64, a.nnz() as u64, &v) });
    }
    /// Re-load arrays previously returned by [`Mat::unload`], with the same handling.
    pub fn load_views(&mut self, v: &Views, nnz: usize) {
        ck("load_Matrix_from_Container", unsafe { gbs_load_csr(self.raw, self.n as u64, nnz as u64, v) });
    }
    /// Container unload: the matrix becomes empty, its arrays come back as raw pointers.
    pub fn unload(&mut self) -> Views {
        let mut v = Views::zeroed();
        ck("unload_Matrix_into_Container", unsafe { gbs_unload_csr(self.raw, &mut v) });
        v
    }
    pub fn layout(&self) -> Layout {
        Layout {
            format: unsafe { gbs_container_format(self.raw) },
            row_major: unsafe { gbs_container_orient(self.raw) } == 0,
            iso: unsafe { gbs_container_iso(self.raw) } != 0,
        }
    }
}

impl Drop for Mat {
    fn drop(&mut self) {
        unsafe { gbs_mat_free(self.raw) }
    }
}

/// Free GraphBLAS-owned arrays that an unload returned and that were not re-loaded.
pub fn free_views(v: &mut Views) {
    unsafe { gbs_free_views(v) }
}

pub struct Vector {
    raw: *mut RawVec,
}

impl Vector {
    pub fn new(n: usize) -> Vector {
        let raw = unsafe { gbs_vec_new(n as u64) };
        assert!(!raw.is_null());
        Vector { raw }
    }
    /// Dense vector over Rust memory, zero-copy (read-only).
    pub fn load_readonly(&mut self, x: &[f64]) {
        ck("GxB_Vector_load", unsafe { gbs_vec_load_ro(self.raw, x.as_ptr() as *mut f64, x.len() as u64) });
    }
    pub fn sum(&self) -> f64 {
        let mut s = 0.0;
        ck("GrB_Vector_reduce", unsafe { gbs_vec_sum(self.raw, &mut s) });
        s
    }
    pub fn nvals(&self) -> u64 {
        let mut nv = 0;
        ck("GrB_Vector_nvals", unsafe { gbs_vec_nvals(self.raw, &mut nv) });
        nv
    }
}
impl Drop for Vector {
    fn drop(&mut self) {
        unsafe { gbs_vec_free(self.raw) }
    }
}

/// `y = A*x` over (PLUS, TIMES, FP64) — `GrB_mxv`.
pub fn mxv(y: &mut Vector, a: &Mat, x: &Vector) {
    ck("GrB_mxv", unsafe { gbs_mxv(y.raw, a.raw, x.raw) });
}
