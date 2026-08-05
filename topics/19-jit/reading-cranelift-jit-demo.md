# Cranelift in 461 lines: AST to function pointer

The implementation manual for our stub: a toy language compiled to
callable machine code, and the entire cranelift JIT recipe fits in
one file. This chapter builds the recipe step by step — what a JIT
library actually has to hand you, the object ladder, the
declare/define/finalize ceremony, the per-node translation table,
and the lifetime contract that makes the final `transmute` sound —
then maps each step into jit.rs. Read it before touching
experiments/src/jit.rs, because every ceremony the stub needs
appears here first.

**Version.** Anchors are against `bytecodealliance/cranelift-jit-demo`
at the pin in `resources/codebases.md`, **`3e5e9b6`**, whose
`Cargo.toml` pins `cranelift`, `cranelift-module`, `cranelift-jit`
and `cranelift-native` all at **0.125.3**, edition 2024. This is not
pedantry: cranelift's builder API churns hard enough that most
tutorials on the web will not compile against 0.125. Step 6 lists
the three renames visible in this very file. Fetch any anchor with
`python3 tools/pinned-source.py show cranelift-jit-demo src/jit.rs -r 53:93`.

## The problem in one sentence

M19 needs to turn an `Expr` tree into a `fn(*const f64) -> f64` it
can call millions of times — which means generating machine code
into executable memory at runtime, in ~tens of microseconds, without
ever letting the function pointer outlive the memory it points into.

## The concepts, step by step

### Step 1 — what a JIT library does: IR in, function pointer out

> **In:** an `Expr` tree, in memory, in our process.
> **Out:** the vocabulary for what the library takes (CLIF, in SSA
> form) and what it returns (`*const u8`) — the two ends of every
> later step.

Cranelift is a code generator: you hand it a function written in
**CLIF** (Cranelift's intermediate representation — typed
instructions like `iadd`/`load` organized in basic blocks), and it
gives back native machine code placed in executable memory, plus a
raw pointer you can call. CLIF is in **SSA** form (static single
assignment — every value is defined exactly once; re-assignment
becomes new values, and control-flow merges pass values as block
parameters). You never write SSA by hand: a helper called
`FunctionBuilder` maintains it while you emit instructions one at a
time.

The demo names all four pieces in one struct:

```rust
// cranelift-jit-demo/src/jit.rs — the whole state of a JIT, 10-26
  10	pub struct JIT {
  11	    /// The function builder context, which is reused across multiple
  12	    /// FunctionBuilder instances.
  13	    builder_context: FunctionBuilderContext,
  14
  15	    /// The main Cranelift context, which holds the state for codegen. Cranelift
  16	    /// separates this from `Module` to allow for parallel compilation, with a
  17	    /// context per thread, though this isn't in the simple demo here.
  18	    ctx: codegen::Context,
  19
  20	    /// The data description, which is to data objects what `ctx` is to functions.
  21	    data_description: DataDescription,
  22
  23	    /// The module, with the jit backend, which manages the JIT'd
  24	    /// functions.
  25	    module: JITModule,
  26	}
```

Line 16's comment is the one to remember: `ctx` is separate from
`module` **so that you can have one context per thread**. That is
the API telling you what is cheap to duplicate and what is not — the
subject of Step 2.

So the whole job of our stub is a recursive walk: Expr node in, CLIF
instruction out, then one call to compile.

### Step 2 — the object ladder (compare wgpu's, topic 18)

> **In:** Step 1's four objects. **Out:** which of them to create
> once per process and which per expression — i.e. what the constant
> term in your compile-time measurement is made of (question 3).

Like every runtime-code system, cranelift splits expensive
long-lived containers from cheap per-function scratch:

```
 JITBuilder ──► JITModule            (owns memory for code+data)
                  ├─ ctx: codegen::Context     (one function's CLIF)
                  ├─ builder_context: FunctionBuilderContext (reused scratch)
                  └─ declare/define/finalize API

 FunctionBuilder(&mut ctx.func, &mut builder_context)
                                     (SSA construction helper —
                                      you emit ops, IT handles
                                      block params)
```

The construction is `impl Default for JIT`, `src/jit.rs:28-49`:

```rust
// cranelift-jit-demo/src/jit.rs — building the module once, 30-42
  30	        let mut flag_builder = settings::builder();
  31	        flag_builder.set("use_colocated_libcalls", "false").unwrap();
  32	        flag_builder.set("is_pic", "false").unwrap();
// ... 33-38: cranelift_native::builder() detects the host ISA; panics
// ...        on an unsupported host ...
  39	        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
  40
  41	        builder.symbol("hello", hello as *const u8);
  42	        let module = JITModule::new(builder);
```

Line 33's `cranelift_native::builder()` is the expensive rung — it
queries the host CPU for available ISA features. Line 41 is the
mechanism you will need for anything the generated code must call
(here, a `hello` function): **symbols must be registered on the
builder before `JITModule::new`**, i.e. before you know what your
expression contains. Keep that in mind for M19's fallback boundary
(question 5).

Same shape as topic 18's Instance→Device→Pipeline: expensive
long-lived containers (`JITModule` owns the executable memory),
cheap per-function contexts (reused between compiles), and an
explicit "finalize" moment after which you hold a raw pointer.
The ladder tells you what to hoist — create the module once, reuse
the contexts per expression. Question 3 measures the difference, and
Step 6 tells you what to expect.

### Step 3 — the compile ladder: declare, define, finalize (memorize this)

> **In:** an AST and the `JIT` from Step 2. **Out:** a `*const u8`
> that points at executable machine code — and, critically, *not*
> a typed function; the cast is a separate act (Step 5).

Compilation is a fixed sequence, and the split between `define`
(generate code) and `finalize` (patch relocations — addresses of
other functions/data unknown until everything is placed) is the part
that surprises. Here is the whole function, comments intact, because
the comments *are* the documentation for this API:

```rust
// cranelift-jit-demo/src/jit.rs — compile(), the entire ladder, 53-93
  53	    pub fn compile(&mut self, input: &str) -> Result<*const u8, String> {
  54	        // First, parse the string, producing AST nodes.
  55	        let (name, params, the_return, stmts) =
  56	            parser::function(input).map_err(|e| e.to_string())?;
  57
  58	        // Then, translate the AST nodes into Cranelift IR.
  59	        self.translate(params, the_return, stmts)?;
// ... 61-66: comment — functions must be declared before defined ...
  67	        let id = self
  68	            .module
  69	            .declare_function(&name, Linkage::Export, &self.ctx.func.signature)
  70	            .map_err(|e| e.to_string())?;
  71
  72	        // Define the function to jit. This finishes compilation, although
  73	        // there may be outstanding relocations to perform. Currently, jit
  74	        // cannot finish relocations until all functions to be called are
  75	        // defined. For this toy demo for now, we'll just finalize the
  76	        // function below.
  77	        self.module
  78	            .define_function(id, &mut self.ctx)
  79	            .map_err(|e| e.to_string())?;
  80
  81	        // Now that compilation is finished, we can clear out the context state.
  82	        self.module.clear_context(&mut self.ctx);
  83
  84	        // Finalize the functions which we just defined, which resolves any
  85	        // outstanding relocations (patching in addresses, now that they're
  86	        // available).
  87	        self.module.finalize_definitions().unwrap();
  88
  89	        // We can now retrieve a pointer to the machine code.
  90	        let code = self.module.get_finalized_function(id);
  91
  92	        Ok(code)
  93	    }
```

**Corrected anchor.** `compile()` is `src/jit.rs:53-93`, not
"55-92". And read line 92 carefully: **`compile()` returns
`*const u8`. There is no `transmute` in this function.** The cast
lives somewhere else entirely — Step 5.

The seven rungs, and what each buys:

```
 1. translate AST → CLIF                    :59   (Step 4)
 2. declare_function(name, Linkage::Export)  :69  → a FuncId; the
      name now exists in the module's symbol table, so other
      functions may reference it before it has a body
 3. define_function(id, &mut ctx)            :78  ← CODEGEN HAPPENS
      machine code exists, but call targets are still placeholders
 4. clear_context(&mut ctx)                  :82  ← reuse the scratch
 5. finalize_definitions()                   :87  ← relocations patched;
      the code becomes executable (W^X flip happens here)
 6. get_finalized_function(id)               :90  → *const u8
 7. transmute to a typed fn                  src/bin/toy.rs:51
```

Rung 5 is why rung 3 does not hand you something callable. A
**relocation** is a hole in the emitted code where an address
belongs — `call <somewhere>` — that cannot be filled until the
target's final address is known. Line 87's comment says it exactly:
"resolves any outstanding relocations (patching in addresses, now
that they're available)". Question 1 asks which of *our* `Expr`
nodes creates one; Step 4 has the answer.

The same ladder as our stub will run it:

```rust
// ILLUSTRATION — not quoted from cranelift-jit-demo. This is the
// jit.rs:53-93 ladder above, rewritten for the pure-expression signature
// the stub needs (experiments/src/jit.rs:42 is the function to fill in).
fn compile(&mut self, expr: &Expr) -> fn(*const f64) -> f64 {
    let mut b = FunctionBuilder::new(&mut self.ctx.func, &mut self.b_ctx);
    let block = b.create_block();
    b.append_block_params_for_function_params(block);
    b.switch_to_block(block);
    b.seal_block(block);                          // one block: seal immediately
    let row_ptr = b.block_params(block)[0];
    let v = translate(&mut b, expr, row_ptr);     // the Step-4 table, recursively
    b.ins().return_(&[v]);
    b.finalize();
    let id = self.module.declare_function("f", Linkage::Export, &sig)?;
    self.module.define_function(id, &mut self.ctx)?;  // ← compilation happens
    self.module.clear_context(&mut self.ctx);
    self.module.finalize_definitions()?;              // ← relocations patched
    unsafe { mem::transmute(self.module.get_finalized_function(id)) }
}   // sound only while the JITModule lives — CompiledExpr must own it
```

The SSA ceremony (`create_block`, `append_block_params...`,
`switch_to_block`, `seal_block`) collapses to four lines because a
pure expression needs exactly one block. In the demo those same
calls are `src/jit.rs:138`, `:144`, `:147`, `:152`, and the
`FunctionBuilder::new` is `:135` with `builder.finalize()` at
`:180`. **Sealing** tells the builder no more predecessors will
arrive for that block, so it can resolve block parameters; with one
block and no jumps you can seal immediately.

### Step 4 — translating an expression: one CLIF op per Expr node

> **In:** Step 3's rung 1 and a `FunctionBuilder` positioned in a
> sealed block. **Out:** a single CLIF `Value` for the whole
> expression, produced by one recursive match — the code M19 asks
> you to write.

The demo's translator is `FunctionTranslator`, and the anchor here
needs fixing too:

```rust
// cranelift-jit-demo/src/jit.rs — the translator's state and its core match, 187-208
 187	struct FunctionTranslator<'a> {
 188	    int: types::Type,
 189	    builder: FunctionBuilder<'a>,
 190	    variables: HashMap<String, Variable>,
 191	    module: &'a mut JITModule,
 192	}
// ... 194-196: impl block + the doc comment "you get back `Value`s" ...
 197	    fn translate_expr(&mut self, expr: Expr) -> Value {
 198	        match expr {
 199	            Expr::Literal(literal) => {
 200	                let imm: i32 = literal.parse().unwrap();
 201	                self.builder.ins().iconst(self.int, i64::from(imm))
 202	            }
 203
 204	            Expr::Add(lhs, rhs) => {
 205	                let lhs = self.translate_expr(*lhs);
 206	                let rhs = self.translate_expr(*rhs);
 207	                self.builder.ins().iadd(lhs, rhs)
 208	            }
```

`struct FunctionTranslator` is **`:187-192`** (the old "189-191"
lands on two of its four fields), and `translate_expr` is
**`:197-249`**.

**A correction that changes what you can copy.** The demo's toy
language is **integer-only**. Line 188's field is `int`, seeded at
`:122-124`:

> `// Our toy language currently only supports I64 values, though Cranelift`
> `// supports other types.`
> `let int = self.module.target_config().pointer_type();`

There is no `fadd`, no `fmul`, no `f64const` and no `fcmp` anywhere
in this repository. The operations you will actually read are
`iconst` (`:201`), `iadd` (`:207`), `isub` (`:213`), `imul`
(`:219`), `udiv` (`:225`) and `icmp` (`:264`). So the f64 table
below is **our design for M19**, not something you can lift from the
demo — read it as a specification, and read the demo's integer
`translate_expr` for the *shape*.

```rust
// ILLUSTRATION — NOT in cranelift-jit-demo, which is I64-only
// (see the comment at src/jit.rs:122-123). This is M19's own spec; the
// authoritative copy is the stub's module docs at experiments/src/jit.rs:11-16.
// The integer analogues you CAN read are iadd (jit.rs:207), imul (:219),
// icmp (:264).
Col(i)   => b.ins().load(F64, MemFlags::trusted(), row_ptr, (i * 8) as i32),
Const(c) => b.ins().f64const(c),
Add(a,b) => b.ins().fadd(va, vb),
Mul(a,b) => b.ins().fmul(va, vb),
Lt(a,b)  => { let cmp = b.ins().fcmp(FloatCC::LessThan, va, vb);
              b.ins().select(cmp, one, zero) }   // f64 1.0 / 0.0
And(a,b) => // both sides already 0.0/1.0 → fmul is branch-free AND
           b.ins().fmul(va, vb),
```

Signature: `fn(*const f64) -> f64` — one pointer param
(`AbiParam::new(module.target_config().pointer_type())`, exactly as
the demo does at `:127`), one F64 return (the demo pushes `int` at
`:132`; you push `types::F64`). The comparisons stay branch-free
(`fcmp` + `select`, values not jumps) — generated straight-line code
with no control flow is exactly what Step 6's "quality gap
vanishes" claim relies on. It is also topic 17's predication
instinct, now applied in codegen rather than in hand-written Rust.

Two demo details worth stealing even though our `Expr` has no
control flow. First, cranelift has **no phi nodes** — merges use
block parameters, and the source says so:

```rust
// cranelift-jit-demo/src/jit.rs — block params instead of phis, 279-284 and 323
 279	        // If-else constructs in the toy language have a return value.
 280	        // In traditional SSA form, this would produce a PHI between
 281	        // the then and else bodies. Cranelift uses block parameters,
 282	        // so set up a parameter in the merge block, and we'll pass
 283	        // the return values to it from the branches.
 284	        self.builder.append_block_param(merge_block, self.int);
// ... 286-322: brif to then/else, each jumping to merge with its value ...
 323	        let phi = self.builder.block_params(merge_block)[0];
```

That is question 2's answer, verbatim from the source. Second,
**the only thing in this demo that creates a relocation** is a call:

```rust
// cranelift-jit-demo/src/jit.rs — translate_call, the relocation source, 372-376
 372	        let callee = self
 373	            .module
 374	            .declare_function(&name, Linkage::Import, &sig)
 375	            .expect("problem declaring function");
 376	        let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
```

`Linkage::Import` at `:374` says "this lives elsewhere"; `:376`
records a reference from the current function to it, which becomes a
hole to patch at `finalize_definitions()`. Pure arithmetic never
reaches this code, which is exactly why our `Expr` produces zero
relocations — and why adding a single `pow()` would change that.

### Step 5 — the lifetime contract: the pointer is borrowed, not owned

> **In:** Step 3's `*const u8` at rung 6. **Out:** a typed function
> pointer plus the full list of invariants that makes the cast
> sound — the boundary where Rust stops helping.

`get_finalized_function` returns a raw pointer into memory the
`JITModule` owns. The cast is in `src/bin/toy.rs`, not in `jit.rs`:

```rust
// cranelift-jit-demo/src/bin/toy.rs — the ONLY transmute in the demo, 44-54
  44	/// input and output types. Using incorrect types at this point may corrupt the program's state.
  45	unsafe fn run_code<I, O>(jit: &mut jit::JIT, code: &str, input: I) -> Result<O, String> { unsafe {
  46	    // Pass the string to the JIT, and it returns a raw pointer to machine code.
  47	    let code_ptr = jit.compile(code)?;
  48	    // Cast the raw pointer to a typed function pointer. This is unsafe, because
  49	    // this is the critical point where you have to trust that the generated code
  50	    // is safe to be called.
  51	    let code_fn = mem::transmute::<_, fn(I) -> O>(code_ptr);
  52	    // And now we can call it!
  53	    Ok(code_fn(input))
  54	}}
```

**Corrected claim.** The previous version of this guide said "the
demo transmutes to `fn(f64) -> f64`". It does not. Line 51
transmutes to a *generic* `fn(I) -> O` inside an `unsafe fn`, and
because the toy language is I64-only (Step 4), every actual call
site in `main` uses integer types. The `f64` in the old sentence was
imported from our own stub's signature.

Line 45's `unsafe fn` and line 44's doc comment are doing real work:
the demo pushes the entire type-correctness obligation onto the
caller, in the type system, by making `I` and `O` free parameters.
Our stub does the opposite — it fixes the signature at
`experiments/src/jit.rs:30` (`func: fn(*const f64) -> f64`) so the
obligation is discharged once, inside `compile()`.

The pointer is valid exactly as long as the JITModule lives — so
`CompiledExpr` must own the module. The stub already encodes this:

```rust
// database-learning-path/topics/19-jit/experiments/src/jit.rs — the ownership fix, 26-31
  26	pub struct CompiledExpr {
  27	    /// Keeps the executable memory alive. Never dropped before `func`
  28	    /// stops being called.
  29	    _module: JITModule,
  30	    func: fn(*const f64) -> f64,
  31	}
```

Field order matters: Rust drops fields in declaration order, so
`_module` at `:29` is dropped *before* `func` at `:30` — which is
harmless because `func` is a plain pointer with no destructor, but
reverse the fields and you have written a footgun for the next
person. Postgres solves the same lifetime with per-context resource
trackers (`llvmjit.c:288-289`, see `reading-postgres-jit.md`); the
obligation is universal to JITs, only the spelling differs.

### Step 6 — the design point: cranelift vs LLVM, and the gotcha list

> **In:** everything above — a working compile path. **Out:** where
> this compiler sits on the compile-time/code-quality frontier, what
> that predicts for our lane, and the version hazards that will
> actually cost you an afternoon.

```
                 cranelift            LLVM -O3
 compile speed   much faster          baseline
 code quality    roughly -O0..-O1     best
 written in      Rust (no FFI)        C++ (bindgen pain)
 designed for    wasmtime JIT         everything
```

**Claim removed as unverifiable.** The previous version of this
guide printed "~10-100× faster than LLVM" and "e-graph based mid-end
(aegraph)". Neither is checkable from anything in this repo's pin
table — no cranelift-vs-LLVM benchmark ships in `cranelift-jit-demo`
— so both are gone rather than repeated. What *is* measured, in a
peer-reviewed paper this topic already reads, is the same design
point for a different fast compiler:

```
 Umbra's Flying Start vs LLVM -O3, geometric mean over TPC-H
 (Kersten/Leis/Neumann, VLDBJ 2021, Table 3, SF=1, 20 threads):
     compilation   108× faster
     execution     1.2× slower       → 1/1.2 = 83% of -O3's speed

 Copy-and-Patch vs LLVM (Xu & Kjolstad, OOPSLA 2021, Fig. 24):
     compile       up to  276× faster than -O0
                   up to 1435× faster than -O1/-O2/-O3
     execution     14% FASTER than -O0, 24% slower than -O3
```

Cranelift occupies the same region of that frontier: single-pass-ish
compilation, code roughly at `-O0`/`-O1`, no LLVM dependency. Use
those two rows as the *prior* for what to expect from our lane, then
measure — that is exactly what `notes.md`'s prediction worksheet is
for.

For straight-line f64 arithmetic the quality gap vs LLVM should
nearly vanish — there are no loops to optimize, and OUR loop (over
rows) stays in Rust and gets `rustc -O`. Predict that before you
measure it, then check whether the JIT lane beats `vector`'s
measured 11.8 M rows/s at 511 nodes (`notes.md`). The topic's own
prediction is that it will *not* clearly win, because the vectorized
lane gets SIMD from autovectorization and scalar CLIF does not.

**Gotchas for the stub, with the evidence for each.**

- **Version lock.** cranelift crates move together —
  `Cargo.toml:11-15` pins `cranelift`, `cranelift-module`,
  `cranelift-jit`, `cranelift-native` all at `0.125.3`. Three API
  changes are visible in this file alone, and each will break a
  tutorial written a year ago:
  - jump arguments are now `&[BlockArg::Value(v)]` (`:301`, `:313`;
    the import is `use cranelift::codegen::ir::BlockArg;` at
    `:2`), not a bare `&[Value]`;
  - `builder.declare_var(int)` **returns** the `Variable` (`:460`),
    replacing the older `Variable::new(idx)` + `declare_var(var,
    ty)` pair;
  - the conditional branch is `brif(cond, then, &[], else, &[])`
    (`:289`, `:339`), replacing `brz`/`brnz`.
- `cranelift_native::builder()` (`:33`) detects the host ISA; the
  demo sets `is_pic` to `"false"` at `:32`, which is right for a
  JIT that never writes a shared object.
- `MemFlags::trusted()` = aligned + notrap: we promise `row_ptr` is
  valid — the unsafe contract lives at the `eval()` call site
  (`experiments/src/jit.rs:34-38` documents it as a caller
  contract).
- Floats: use `fcmp` + `select`, NOT bitcast tricks — CLIF's
  boolean handling has changed across versions and `select` on f64
  is the stable spelling.
- The module must not be dropped: `CompiledExpr { _module, func }`
  (`experiments/src/jit.rs:26-31`), with `func` called through the
  stored pointer.

### Step 7 — the arithmetic: what compile time has to beat

> **In:** Step 6's design point and `notes.md`'s measured per-row
> rates. **Out:** the number `compile()` must come in under for
> M19's JIT lane to be worth shipping — computed, not guessed.

The bench harness runs a fixed number of rows per expression. Turn
that into a compile-time budget:

```
 Measured (notes.md, Apple M3 Pro, 2026-07-10, N_COLS=4,
 depth 8 = 511 nodes, best-of-3):
   interp lane   0.95 M rows/s  →  1.053  µs/row
   vector lane  11.8  M rows/s  →  0.0847 µs/row

 Suppose the JIT lane lands at the vector lane's rate (the topic's
 own prediction). Then, per row, compiling saves:
   vs interp:  1.053  − 0.0847 = 0.9683 µs
   vs vector:  0.0847 − 0.0847 = 0        ← nothing to win

 Break-even against the INTERPRETER, rows = compile_µs / 0.9683:
   compile in  100 µs →    103 rows
   compile in  500 µs →    516 rows
   compile in 5000 µs →  5,164 rows

 Break-even against the VECTORIZED lane: undefined — the
 denominator is zero or negative unless the JIT is genuinely
 faster per row than autovectorized Rust. THAT is the real
 experiment M19 is asking you to run, and the reason notes.md
 wants a prediction first.

 Now invert it. If jit_bench feeds 2,000,000 rows and you want the
 JIT to pay for itself in under 1% of total runtime:
   interp time for 2M rows = 2e6 × 1.053 µs = 2.106 s
   1% budget               = 21.06 ms
 So any compile under ~21 ms is invisible at this row count.
 Cranelift on a 511-node expression will be far under that — which
 means at 2M rows the compile fee is NOT the interesting variable.
 Re-run the sum at 1,000 rows and it becomes the ONLY variable.
```

That last inversion is the whole reason this topic exists. Compile
time is not expensive or cheap in the abstract; it is expensive or
cheap *relative to a row count you must name*. Postgres names it
with an estimate and gets it wrong (`reading-postgres-jit.md`);
Umbra refuses to name it and switches tiers mid-query
(`reading-umbra-tidy-tuples.md`); SQLite names it as "about five"
and correctly never compiles at all (`reading-sqlite-vdbe.md`).

## Where each step lives in the code

All anchors are `cranelift-jit-demo` at `3e5e9b6` (461 lines total).

| anchor | what it is | step |
|---|---|---|
| `src/jit.rs:10-26` | the four state objects, with the "one context per thread" note at `:16` | 1, 2 |
| `src/jit.rs:28-49` | `impl Default for JIT` — ISA detection and module construction | 2 |
| `src/jit.rs:32-33` | `is_pic=false`; `cranelift_native::builder()` | 2 |
| `src/jit.rs:39-42` | `JITBuilder::with_isa(...)`, `builder.symbol(...)`, `JITModule::new` | 2 |
| `src/jit.rs:53-93` | `compile()` — the whole ladder; returns `*const u8`, **no transmute** | 3 |
| `src/jit.rs:67-70` / `:77-79` / `:82` / `:87` / `:90` | declare / define / clear / finalize / get | 3 |
| `src/jit.rs:116-182` | `translate()` — signature, entry block, seal, `return_` (`:177`) | 3 |
| `src/jit.rs:122-124` | **"Our toy language currently only supports I64 values"** | 4 |
| `src/jit.rs:135` | `FunctionBuilder::new(&mut ctx.func, &mut builder_context)` | 3 |
| `src/jit.rs:138` / `:144` / `:147` / `:152` | `create_block`, `append_block_params_for_function_params`, `switch_to_block`, `seal_block` | 3 |
| `src/jit.rs:180` | `builder.finalize()` — seals the CLIF function | 3 |
| `src/jit.rs:187-192` | `FunctionTranslator` — AST→CLIF recursion state | 4 |
| `src/jit.rs:197-249` | `translate_expr` — `iconst` `:201`, `iadd` `:207`, `imul` `:219` | 4 |
| `src/jit.rs:251-395` | the helper emitters: assign `:251`, icmp `:261`, if/else `:267`, while `:328`, call `:360`, global data `:386` | 4 |
| `src/jit.rs:279-284`, `:323` | block parameters instead of phi nodes | 4 |
| `src/jit.rs:372-376` | `Linkage::Import` + `declare_func_in_func` — the only relocation source | 3, 4 |
| `src/jit.rs:398-461` | `declare_variables*` — note `declare_var` returns the `Variable` at `:460` | 6 |
| `src/bin/toy.rs:44-54` | `unsafe fn run_code<I, O>` — the **only** `transmute`, at `:51` | 5 |
| `Cargo.toml:11-15` | the version lock: cranelift 0.125.3 × 4 crates | 6 |
| `src/frontend.rs` | the toy parser (87 lines — ignore, we have `Expr`) | — |

**Anchor corrections against the previous version of this guide:**
`compile()` is `:53-93` (was "55-92"); `struct FunctionTranslator`
is `:187-192` (was "189-191"); the `JIT` struct is `:10-26` (was
"12-25"); and the helper emitters are `:251-395`, **not `:400+`** —
`:398-461` is variable declaration, a different subject.

Read `jit.rs` top to bottom once, then re-read `compile()`
(`:53-93`) against Step 3's seven rungs until each line maps to a
rung. Then read `translate_expr` (`:197-249`) as Step 4 with
statements added that our pure `Expr` doesn't need, and finish with
`translate_if_else` (`:267-326`) for the block-parameter idiom.

## Questions for notes.md

1. Why does `define_function` (`src/jit.rs:78`) not yet give you a
   callable — what do relocations still need? Read the comment at
   `:72-76` and `:84-86` for the library's own answer. Which of our
   `Expr` nodes would introduce one? (Trace it: the only path to a
   relocation in this demo is `translate_call` at `:372-376`, via
   `Linkage::Import`. Pure arithmetic never gets there. What would
   adding `pow()` cost, and where would you have to register the
   symbol — see `:41`?)
2. `FunctionBuilder` "handles SSA construction" — what does that
   mean concretely for a variable assigned in two branches? The
   demo answers it in a comment at `:279-284` and uses the result
   at `:323`. State the difference between a phi node and a block
   parameter, and say which one cranelift has.
3. Time `compile()` in jit_bench across expr depths 2..12. Is it
   linear in node count? Where does the constant term come from?
   Hoist GLOBAL vs per-expr state and measure both ways — the
   candidates are the ISA detection at `:33`, `JITModule::new` at
   `:42`, and the per-call `FunctionBuilder::new` at `:135`. Then
   plug your measured `compile_µs` into Step 7's division and say
   how many rows the lane needs.
4. `src/bin/toy.rs:51` transmutes to a generic `fn(I) -> O` inside
   an `unsafe fn`. Spell out every precondition that makes our
   `fn(*const f64) -> f64` transmute sound: ABI match, signature
   match (params AND return type AND count), the module still
   alive, `row_ptr` valid for `n_cols * 8` bytes and aligned
   (because we used `MemFlags::trusted()`), and W^X already flipped
   by `finalize_definitions()`. Which of those does the type system
   check for you? (Answer: none.)
5. M19: FalkorDB's values aren't all f64 (nodes, strings, nulls).
   Which subset of Cypher expressions compiles to this f64 scheme
   directly, and what's the fallback boundary — per-node fallback
   (call back into the interpreter for one node) vs whole-expression
   bailout? Pick one and defend it. Note the constraint from
   `:41`: symbols the generated code may call must be registered on
   the `JITBuilder` *before* `JITModule::new`, i.e. before you have
   seen the expression.

## Done when

Answer each before unfolding it.

- [ ] You can recite the compile ladder: declare, define, finalize — and say why `define_function` alone does not give you a callable.

  <details><summary>Answer</summary>

  translate → `declare_function` (`:69`) → `define_function`
  (`:78`) → `clear_context` (`:82`) → `finalize_definitions`
  (`:87`) → `get_finalized_function` (`:90`) → transmute
  (`src/bin/toy.rs:51`). `define_function` runs codegen and emits
  machine code, but any address the code needs — another function,
  a data object — is still a **relocation**: a hole waiting for a
  final address. The source says so at `:72-76`. Only
  `finalize_definitions()` patches those holes and makes the memory
  executable, which is why `compile()` calls it before touching
  `get_finalized_function`. Note also that `compile()` returns
  `*const u8` (`:53`, `:92`) — the typed cast is not part of the
  ladder.
  </details>

- [ ] You can explain what `FunctionBuilder` handling SSA construction saves you from doing.

  <details><summary>Answer</summary>

  SSA requires every value to be defined once, so a variable
  assigned on both sides of an `if` needs a merge construct. In
  classical SSA that is a phi node; cranelift instead uses **block
  parameters** — the merge block declares a parameter
  (`append_block_param`, `:284`) and each incoming branch passes its
  value as a jump argument (`jump(merge_block,
  &[BlockArg::Value(then_return)])`, `:301`/`:313`), then the merged
  value is read back with `block_params(merge_block)[0]` (`:323`).
  The comment at `:279-283` states this explicitly. What
  `FunctionBuilder` saves you from is doing the dominance analysis
  yourself: you write `declare_var`/`def_var`/`use_var` and it
  inserts the parameters and arguments. `seal_block` is the one
  obligation it hands back — you must tell it when a block has no
  further predecessors.
  </details>

- [ ] You can state the lifetime contract on the returned pointer and every invariant the `transmute` is assuming.

  <details><summary>Answer</summary>

  `get_finalized_function` (`:90`) returns a borrow into memory the
  `JITModule` owns; dropping the module unmaps the code. So the
  typed pointer is valid exactly as long as the module lives, and
  `CompiledExpr` must own it — `experiments/src/jit.rs:26-31` does
  this, with the comment at `:27-28` recording why. The transmute at
  `src/bin/toy.rs:51` additionally assumes: the target type's ABI
  matches the CLIF signature's calling convention; the parameter
  count, parameter types and return type all match exactly; the
  memory has already been made executable (true after
  `finalize_definitions()`); and — for our signature — that
  `row_ptr` points to at least `n_cols` aligned `f64`s, because
  Step 4 emits `MemFlags::trusted()` which promises aligned and
  non-trapping. The compiler checks none of these; that is why
  `run_code` is an `unsafe fn` and why its doc comment at `:44`
  warns that wrong types "may corrupt the program's state".
  </details>

- [ ] You can time `compile()` across expression depths and say whether it is linear in node count, and convert that into a break-even row count.

  <details><summary>Answer</summary>

  Expect near-linear in node count with a constant term, because
  the translation is one recursive pass emitting one CLIF
  instruction per node and cranelift's backend is single-pass-ish.
  The constant comes from the per-call `FunctionBuilder::new`
  (`:135`) and the declare/define/finalize ceremony, *not* from ISA
  detection (`:33`) or `JITModule::new` (`:42`) if you hoisted
  those — which is the point of measuring both ways. Then:
  `rows = compile_µs / (µs_per_row_interp − µs_per_row_jit)`. With
  `notes.md`'s depth-8 rates (interp 1.053 µs/row; assume the JIT
  reaches the vector lane's 0.0847 µs/row), the denominator is
  0.9683 µs, so 100 µs of compile pays back in 103 rows and 500 µs
  in 516. Against the *vectorized* lane the denominator may be zero
  or negative — record that as a finding, not a failure.
  </details>

- [ ] You can name the three API changes in this pin that will break an older cranelift tutorial.

  <details><summary>Answer</summary>

  At `3e5e9b6` / cranelift 0.125.3: (1) jump arguments are
  `&[BlockArg::Value(v)]` (`src/jit.rs:301`, `:313`, with
  `use cranelift::codegen::ir::BlockArg;` at `:2`), not `&[Value]`;
  (2) `builder.declare_var(ty)` **returns** the `Variable`
  (`:460`), replacing `Variable::new(idx)` followed by
  `declare_var(var, ty)`; (3) conditional branches are
  `brif(cond, then_block, &[], else_block, &[])` (`:289`, `:339`),
  replacing the older `brz`/`brnz` pair. This is why the
  `Cargo.toml:11-15` version lock is in Step 6's gotcha list rather
  than a footnote.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including how you will handle non-f64 values in M19.

  <details><summary>Answer</summary>

  The constraint that decides question 5: `src/jit.rs:41`
  (`builder.symbol("hello", hello as *const u8)`) registers callable
  symbols on the `JITBuilder` **before** `JITModule::new` at `:42`.
  If you choose per-node fallback — calling back into the
  interpreter for the nodes you cannot compile — you must register
  those callbacks up front, and each one becomes a relocation
  (`:372-376`) and a real call in the hot loop, which is precisely
  what Neumann's §4.1 "the hot path is pure LLVM" rule warns
  against. Whole-expression bailout keeps the compiled path
  call-free at the cost of compiling nothing when any node is
  unsupported. Defend whichever you pick with the fraction of real
  Cypher expressions that are pure numeric — count it, don't guess.
  </details>

## References

**Code** — all anchors verified at `cranelift-jit-demo` `3e5e9b6`

| file | what to read |
|---|---|
| `src/jit.rs` | read top to bottom; `:53-93` is the ladder, `:197-249` the translator, `:267-326` the block-parameter idiom |
| `src/bin/toy.rs:44-54` | the only `transmute`, and the `unsafe fn` that documents its obligations |
| `Cargo.toml:11-15` | the version lock (cranelift 0.125.3) |
| `src/frontend.rs` | the toy parser, 87 lines — skippable, we already have `Expr` |

Fetch without a clone:
`python3 tools/pinned-source.py show cranelift-jit-demo src/jit.rs -r 187:250`.

**Elsewhere in this repo**
- `experiments/src/jit.rs:11-21` — the stub's own spec for the f64
  translation table and the ownership rule; `:26-31` the
  `CompiledExpr` shape; `:42` the function to fill in
- `reading-postgres-jit.md` — the same lifetime problem solved with
  ORC resource trackers (`llvmjit.c:288-289`)
- `reading-umbra-tidy-tuples.md` — the measured compile-time /
  code-quality trade-off Step 6 borrows its numbers from
- `reading-neumann-vldb11.md` — §4.1's rule about the hot path not
  crossing a function boundary, which decides question 5

**Papers cited for the design-point numbers**
- Kersten, Leis, Neumann — "Tidy Tuples and Flying Start" (VLDB
  Journal 2021), **Table 3**: Flying Start vs LLVM -O3 — 108×
  faster to compile, 1.2× slower to execute.
- Xu, Kjolstad — "Copy-and-Patch Compilation" (OOPSLA 2021),
  **Fig. 24**: up to 276× faster than LLVM -O0 (1435× vs -O1..-O3),
  producing code 14% faster than -O0 and 24% slower than -O3.
