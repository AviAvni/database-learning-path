# Cranelift in 461 lines: AST to function pointer

The implementation manual for our stub: a toy language compiled to
callable machine code, and the entire cranelift JIT recipe fits in
one file. This chapter walks jit.rs top to bottom — read it before
touching experiments/src/jit.rs, because every ceremony the stub
needs (module lifetimes, SSA plumbing, the transmute contract)
appears here first.

## Anchor map

| anchor | what it is |
|---|---|
| src/jit.rs:39-41 | `JITBuilder::with_isa(...)` → `JITModule::new` |
| src/jit.rs:12-25 | the four state objects (see §1) |
| src/jit.rs:55-92 | `compile()` — the whole ladder, annotated below |
| src/jit.rs:135 | `FunctionBuilder::new(&mut ctx.func, &mut builder_context)` |
| src/jit.rs:180 | `builder.finalize()` — seals the CLIF function |
| src/jit.rs:189-191 | `FunctionTranslator` — AST→CLIF recursion lives here |
| src/jit.rs:400+ | helper emitters (calls, comparisons) |
| src/frontend.rs | the toy parser (87 lines — ignore, we have `Expr`) |

## 1. The object ladder (compare wgpu's, topic 18)

```
 JITBuilder ──► JITModule            (owns memory for code+data)
                  ├─ ctx: codegen::Context     (one function's CLIF)
                  ├─ builder_context: FunctionBuilderContext (reused scratch)
                  └─ declare/define/finalize API

 FunctionBuilder(&mut ctx.func)      (SSA construction helper —
                                      you emit ops, IT handles
                                      block params/phi nodes)
```

Same shape as topic 18's Instance→Device→Pipeline: expensive
long-lived containers, cheap per-function contexts, and an explicit
"finalize" moment after which you hold a raw pointer.

## 2. The compile ladder (jit.rs:55-92, memorize this)

```
 1. translate AST → CLIF            (FunctionTranslator walk)
 2. module.declare_function(name, Linkage::Export, &sig)  → id
 3. module.define_function(id, &mut ctx)   ← compilation happens
 4. module.clear_context(&mut ctx)         ← reuse scratch
 5. module.finalize_definitions()          ← relocations patched
 6. module.get_finalized_function(id)      → *const u8   (:90)
 7. unsafe { mem::transmute::<_, fn(f64...)->f64>(ptr) }
```

The same ladder as our stub will run it:

```rust
// CLIF in, callable pointer out — the whole recipe
fn compile(&mut self, expr: &Expr) -> fn(*const f64) -> f64 {
    let mut b = FunctionBuilder::new(&mut self.ctx.func, &mut self.b_ctx);
    let block = b.create_block();
    b.append_block_params_for_function_params(block);
    b.switch_to_block(block);
    b.seal_block(block);                          // one block: seal immediately
    let row_ptr = b.block_params(block)[0];
    let v = translate(&mut b, expr, row_ptr);     // the §3 table, recursively
    b.ins().return_(&[v]);
    b.finalize();
    let id = self.module.declare_function("f", Linkage::Export, &sig)?;
    self.module.define_function(id, &mut self.ctx)?;  // ← compilation happens
    self.module.clear_context(&mut self.ctx);
    self.module.finalize_definitions()?;              // ← relocations patched
    unsafe { mem::transmute(self.module.get_finalized_function(id)) }
}   // sound only while the JITModule lives — CompiledExpr must own it
```

The pointer is valid as long as the JITModule lives — our
`CompiledExpr` must own the module (drop order = use-after-free
otherwise; postgres solves the same lifetime with per-context
resource trackers, llvmjit.c:288).

## 3. Translating an expression (what the stub must do)

The demo's translator (jit.rs:189+) is statement-oriented; our
`Expr` is pure — simpler. Per node:

```
 Col(i)   → load: builder.ins().load(F64, MemFlags::trusted(),
                                     row_ptr, (i*8) as i32)
 Const(c) → builder.ins().f64const(c)
 Add(a,b) → builder.ins().fadd(va, vb)
 Mul(a,b) → builder.ins().fmul(va, vb)
 Lt(a,b)  → cmp = builder.ins().fcmp(FloatCC::LessThan, va, vb)
            → select(cmp, one, zero)  (we keep f64 1.0/0.0)
 And(a,b) → both sides as f64 0/1 → fmin or fmul (branch-free —
            topic 17's predication instinct, now in codegen)
```

Signature: `fn(*const f64) -> f64` — one pointer param
(`AbiParam::new(types::I64)` or a real pointer type via
`module.target_config().pointer_type()`), one F64 return. SSA
plumbing: one block, `append_block_params_for_function_params`,
`switch_to_block`, `seal_block` — see jit.rs:135-180 for the
exact ceremony.

## 4. Cranelift vs LLVM in one table

```
                 cranelift            LLVM -O3
 compile speed   ~10-100× faster      baseline
 code quality    ~ -O0..-O1           best
 passes          e-graph based        ~100 passes
                 mid-end (aegraph)
 written in      Rust (no FFI)        C++ (bindgen pain)
 designed for    wasmtime JIT         everything
```

Cranelift ≈ Umbra's Flying Start as a design point (fast,
single-tier, good-enough). For straight-line f64 arithmetic the
quality gap vs LLVM nearly vanishes — no loops to optimize, and
OUR loop (over rows) stays in Rust and gets rustc -O.

## 5. Gotchas for the stub

- Version lock: cranelift crates move together — Cargo.toml pins
  matching versions of cranelift-{jit,module,frontend,codegen,native}.
- `cranelift_native::builder()` detects the host ISA; enable
  `is_pic` false default is fine for JIT.
- `MemFlags::trusted()` = aligned + notrap: we promise row_ptr is
  valid — the unsafe contract lives at the `eval()` call site.
- Floats: use `fcmp`+`select`, NOT bint/bitcast tricks — CLIF's
  bool handling changed across versions; select on f64 is stable.
- The module must not be dropped: `CompiledExpr { module, func }`
  with func called through a stored raw pointer.

## Questions for notes.md

1. Why does define_function (:78) not yet give you a callable —
   what do relocations still need (addresses of other functions/
   data), and which of our Expr nodes would introduce one (none —
   pure arithmetic; a `pow()` call would)?
2. FunctionBuilder "handles SSA construction" — what does that
   mean concretely for a `var` assigned in two branches (block
   params instead of phi nodes — how do they differ)?
3. Time `compile()` in jit_bench across expr depths 2..12. Is it
   linear in node count? Where does the constant term come from
   (ISA setup? module init? — hoist GLOBAL vs per-expr state and
   measure both ways)?
4. The demo transmutes to `fn(f64) -> f64`. Spell out every
   precondition that makes our `fn(*const f64) -> f64` transmute
   sound (ABI = System V default? signature match? module alive?
   W^X handled by JITModule?).
5. M19: eval.rs values aren't all f64 (nodes, strings, nulls).
   Which subset of Cypher expressions compiles to this f64 scheme
   directly, and what's the fallback boundary (per-node fallback
   vs whole-expression bailout — pick one and defend it)?

## References

**Code**
- [cranelift-jit-demo](https://github.com/bytecodealliance/cranelift-jit-demo)
  — `src/jit.rs` — read it top to bottom; `src/frontend.rs` (the toy
  parser) can be skipped, we already have `Expr`
