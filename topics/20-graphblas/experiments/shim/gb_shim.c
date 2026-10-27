// gb_shim.c — the thinnest C layer that lets a Rust bench call SuiteSparse:GraphBLAS.
//
// Why a C shim and not hand-written `extern "C"` declarations: GxB_Container is a
// 512-byte struct whose layout lives in GraphBLAS.h, and GrB_Matrix / GrB_Vector
// are opaque pointers. Compiling against the real header means the layout is the
// library's, not a copy that can drift. Every function here is one GraphBLAS call
// (or three, for the Container hand-off); nothing in this file is timed work of
// its own.
#include <stdint.h>
#include <stdlib.h>
#include "GraphBLAS.h"

typedef struct
{
    GrB_Matrix A;
    GxB_Container C;
} gbs_mat;

// The arrays of a CSR matrix as GraphBLAS holds them, for unload/reload.
typedef struct
{
    void *p, *i, *x;                // Ap (rowptr), Ai (colidx), Ax (values)
    uint64_t p_len, i_len, x_len;   // entries, not bytes
    uint64_t p_mem, i_mem, x_mem;   // bytes
    int p_h, i_h, x_h;              // handling: 0 = GraphBLAS owns, else readonly
    int p_bits, i_bits;             // 32 or 64
} gbs_views;

#define OK(x) do { GrB_Info e_ = (x); if (e_ != GrB_SUCCESS) return (int) e_; } while (0)

int gbs_init(void)
{
    OK (GrB_init (GrB_NONBLOCKING)) ;
    // never compile a kernel inside a timed region
    OK (GrB_Global_set_INT32 (GrB_GLOBAL, GxB_JIT_OFF, GxB_JIT_C_CONTROL)) ;
    return 0 ;
}
void gbs_version(int *out) { out[0] = GxB_IMPLEMENTATION_MAJOR; out[1] = GxB_IMPLEMENTATION_MINOR; out[2] = GxB_IMPLEMENTATION_SUB; }
int gbs_set_threads(int t) { return (int) GrB_Global_set_INT32 (GrB_GLOBAL, t, GxB_NTHREADS) ; }
// GxB_JIT_OFF (0) or GxB_JIT_ON (4): the first call that needs a kernel compiles it, so warm up
// before timing. Returns the GrB_Info of the set, and the JIT level actually in force via *level.
int gbs_set_jit(int on, int *level)
{
    OK (GrB_Global_set_INT32 (GrB_GLOBAL, on ? GxB_JIT_ON : GxB_JIT_OFF, GxB_JIT_C_CONTROL)) ;
    int32_t l = -1 ; GrB_Global_get_INT32 (GrB_GLOBAL, &l, GxB_JIT_C_CONTROL) ; *level = l ;
    return 0 ;
}
int gbs_burble(int on) { return (int) GrB_Global_set_INT32 (GrB_GLOBAL, on, GxB_BURBLE) ; }
int gbs_get_threads(void) { int32_t t = 0 ; GrB_Global_get_INT32 (GrB_GLOBAL, &t, GxB_NTHREADS) ; return t ; }

gbs_mat *gbs_mat_new(uint64_t n)
{
    gbs_mat *m = calloc (1, sizeof (gbs_mat)) ;
    if (!m) return NULL ;
    if (GrB_Matrix_new (&m->A, GrB_FP64, n, n) != GrB_SUCCESS || GxB_Container_new (&m->C) != GrB_SUCCESS) return NULL ;
    return m ;
}
void gbs_mat_free(gbs_mat *m) { GrB_Matrix_free (&m->A) ; GxB_Container_free (&m->C) ; free (m) ; }
uint64_t gbs_nvals(gbs_mat *m) { GrB_Index nv = 0 ; GrB_Matrix_nvals (&nv, m->A) ; return nv ; }

// GrB_Matrix_build: tuples in, GraphBLAS sorts and compresses (the copy path).
int gbs_build(gbs_mat *m, uint64_t n, uint64_t nnz, const uint64_t *rows, const uint64_t *cols, const double *vals)
{
    OK (GrB_Matrix_clear (m->A)) ;
    OK (GrB_Matrix_build_FP64 (m->A, rows, cols, vals, nnz, GrB_PLUS_FP64)) ;
    return (int) GrB_wait (m->A, GrB_MATERIALIZE) ;
}

// GrB_Matrix_extractTuples: tuples out (the copy path).
int gbs_extract(gbs_mat *m, uint64_t *nnz, uint64_t *rows, uint64_t *cols, double *vals)
{
    return (int) GrB_Matrix_extractTuples_FP64 (rows, cols, vals, nnz, m->A) ;
}

// The Container load: hand CSR arrays to GraphBLAS. A handling of GxB_IS_READONLY
// keeps the arrays the caller's (a "shallow" matrix); 0 transfers ownership.
// Three GxB_Vector_load calls plus one GxB_load_Matrix_from_Container: no loop.
int gbs_load_csr(gbs_mat *m, uint64_t n, uint64_t nnz, const gbs_views *v)
{
    GxB_Container C = m->C ;
    void *p = v->p, *i = v->i, *x = v->x ;
    OK (GxB_Vector_load (C->p, &p, v->p_bits == 32 ? GrB_UINT32 : GrB_UINT64, v->p_len, v->p_mem, v->p_h, NULL)) ;
    OK (GxB_Vector_load (C->i, &i, v->i_bits == 32 ? GrB_UINT32 : GrB_UINT64, v->i_len, v->i_mem, v->i_h, NULL)) ;
    OK (GxB_Vector_load (C->x, &x, GrB_FP64, v->x_len, v->x_mem, v->x_h, NULL)) ;
    C->nrows = n ; C->ncols = n ; C->nvals = nnz ;
    C->nrows_nonempty = -1 ; C->ncols_nonempty = -1 ;
    C->format = GxB_SPARSE ; C->orientation = GrB_ROWMAJOR ;
    C->iso = false ; C->jumbled = false ;
    return (int) GxB_load_Matrix_from_Container (m->A, C, NULL) ;
}

// The Container unload: take the arrays back out. The matrix is left empty.
int gbs_unload_csr(gbs_mat *m, gbs_views *v)
{
    GxB_Container C = m->C ;
    GrB_Type t ;
    OK (GxB_unload_Matrix_into_Container (m->A, C, NULL)) ;
    OK (GxB_Vector_unload (C->p, &v->p, &t, &v->p_len, &v->p_mem, &v->p_h, NULL)) ;
    v->p_bits = (t == GrB_UINT32 || t == GrB_INT32) ? 32 : 64 ;
    OK (GxB_Vector_unload (C->i, &v->i, &t, &v->i_len, &v->i_mem, &v->i_h, NULL)) ;
    v->i_bits = (t == GrB_UINT32 || t == GrB_INT32) ? 32 : 64 ;
    OK (GxB_Vector_unload (C->x, &v->x, &t, &v->x_len, &v->x_mem, &v->x_h, NULL)) ;
    return 0 ;
}

// What GraphBLAS built, as the Container reports it after an unload.
int gbs_container_format(gbs_mat *m) { return m->C->format ; }
int gbs_container_orient(gbs_mat *m) { return m->C->orientation ; }
int gbs_container_iso(gbs_mat *m) { return m->C->iso ; }

// Free arrays that an unload handed to the caller as GraphBLAS-owned (handling 0).
void gbs_free_views(gbs_views *v)
{
    if (v->p_h == 0) free (v->p) ;
    if (v->i_h == 0) free (v->i) ;
    if (v->x_h == 0) free (v->x) ;
}

// --- vectors, for GrB_mxv ----------------------------------------------------
typedef struct { GrB_Vector v ; } gbs_vec ;
gbs_vec *gbs_vec_new(uint64_t n) { gbs_vec *r = calloc (1, sizeof *r) ; if (GrB_Vector_new (&r->v, GrB_FP64, n) != GrB_SUCCESS) return NULL ; return r ; }
void gbs_vec_free(gbs_vec *r) { GrB_Vector_free (&r->v) ; free (r) ; }
// zero-copy dense vector over caller memory
int gbs_vec_load_ro(gbs_vec *r, double *x, uint64_t n)
{
    void *p = x ;
    return (int) GxB_Vector_load (r->v, &p, GrB_FP64, n, n * sizeof (double), GxB_IS_READONLY, NULL) ;
}
int gbs_mxv(gbs_vec *y, gbs_mat *A, gbs_vec *x)
{
    return (int) GrB_mxv (y->v, NULL, NULL, GrB_PLUS_TIMES_SEMIRING_FP64, A->A, x->v, NULL) ;
}
int gbs_vec_sum(gbs_vec *y, double *s)
{
    return (int) GrB_Vector_reduce_FP64 (s, NULL, GrB_PLUS_MONOID_FP64, y->v, NULL) ;
}
int gbs_vec_nvals(gbs_vec *y, uint64_t *nv) { return (int) GrB_Vector_nvals (nv, y->v) ; }
