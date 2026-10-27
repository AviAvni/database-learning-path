/* graphblas_bytes.c -- bytes per edge of a SuiteSparse:GraphBLAS matrix, as
 * FalkorDB links it (v10.5.0, deps/GraphBLAS, libgraphblas.a from its build).
 *
 * Optional probe, not part of the cargo crate: it needs a FalkorDB checkout
 * that has been built once.  Build and run (macOS arm64 shown; on Linux drop
 * the -L/-lomp pair and add -fopenmp):
 *
 *   FDB=~/repos/FalkorDB
 *   clang -O2 -I$FDB/deps/GraphBLAS/Include graphblas_bytes.c \
 *       $FDB/bin/macos-arm64v8-release/GraphBLAS/libgraphblas.a \
 *       -o graphblas_bytes -lm -lpthread -L/opt/homebrew/opt/libomp/lib -lomp
 *   ./graphblas_bytes
 *
 * n = 1,000,000 vertices, 8,000,000 uniformly random edge draws (duplicates
 * collapse), built with setElement + wait, then asked for its memory. Three
 * matrices: GrB_BOOL as built, the same with the iso flag cleared, and
 * GrB_UINT64 holding an edge id per entry (FalkorDB's relation matrix).
 */
#include <stdio.h>
#include <stdlib.h>
#include "GraphBLAS.h"

static void report(const char *label, GrB_Matrix A)
{
    GrB_Index nv;
    GrB_Matrix_nvals(&nv, A);
    size_t sz;
    GxB_Matrix_memoryUsage(&sz, A);
    int32_t pb, ib, iso, sp;
    GxB_Matrix_Option_get_INT32(A, GxB_OFFSET_INTEGER_BITS, &pb);
    GxB_Matrix_Option_get_INT32(A, GxB_COLINDEX_INTEGER_BITS, &ib);
    GxB_Matrix_Option_get_INT32(A, GxB_ISO, &iso);
    GxB_Matrix_Option_get_INT32(A, GxB_SPARSITY_STATUS, &sp);
    printf("%-22s nvals=%lu bytes=%zu bytes/edge=%.3f offset_bits=%d index_bits=%d iso=%d sparsity_status=%d\n",
           label, (unsigned long) nv, sz, (double) sz / nv, pb, ib, iso, sp);
}

static void fill(GrB_Matrix A, uint64_t n, uint64_t m)
{
    GrB_Type t;
    GxB_Matrix_type(&t, A);
    int is_u64 = (t == GrB_UINT64);
    uint64_t s = 12345;
    for (uint64_t e = 0; e < m; e++) {
        s = s * 6364136223846793005ULL + 1442695040888963407ULL;
        uint64_t i = (s >> 20) % n;
        s = s * 6364136223846793005ULL + 1442695040888963407ULL;
        uint64_t j = (s >> 20) % n;
        if (is_u64) GrB_Matrix_setElement_UINT64(A, e, i, j);   /* an edge id: distinct values, not iso */
        else GrB_Matrix_setElement_BOOL(A, true, i, j);
    }
    GrB_Matrix_wait(A, GrB_MATERIALIZE);
}

int main(void)
{
    GrB_init(GrB_NONBLOCKING);
    uint64_t n = 1000000, m = 8000000;
    GrB_Matrix A;
    GrB_Matrix_new(&A, GrB_BOOL, n, n);
    fill(A, n, m);
    report("GrB_BOOL as built", A);
    GxB_Matrix_Option_set_INT32(A, GxB_ISO, 0);
    GrB_Matrix_wait(A, GrB_MATERIALIZE);
    report("GrB_BOOL, ISO off", A);
    GrB_Matrix_free(&A);

    /* FalkorDB's relation matrix (src/graph/tensor/tensor.c:50) is GrB_UINT64
       holding edge ids, plus a GrB_BOOL transposed copy. */
    GrB_Matrix_new(&A, GrB_UINT64, n, n);
    fill(A, n, m);
    report("GrB_UINT64 (edge ids)", A);
    GrB_Matrix_free(&A);

    GrB_finalize();
    return 0;
}
