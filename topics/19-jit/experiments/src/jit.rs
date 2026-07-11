//! STUB — cranelift JIT: compile an Expr to native `fn(*const f64) -> f64`.
//!
//! Recipe: reading-cranelift-jit-demo.md (the whole ladder is
//! ~/repos/cranelift-jit-demo/src/jit.rs:39-92). Sketch:
//!
//!   1. isa: cranelift_native::builder() → isa_builder.finish(settings)
//!   2. JITBuilder::with_isa(isa, default_libcall_names()) → JITModule::new
//!   3. signature: one param pointer_type() (row ptr), one return F64
//!   4. FunctionBuilder over module.make_context(); one block;
//!      append_block_params_for_function_params, switch_to_block, seal_block
//!   5. walk the Expr, one CLIF value per node:
//!        Col(i)   → ins().load(F64, MemFlags::trusted(), row_ptr, (i*8) as i32)
//!        Const(c) → ins().f64const(c)
//!        Add/Mul  → ins().fadd / fmul
//!        Lt       → ins().fcmp(FloatCC::LessThan) → ins().select(cmp, one, zero)
//!        And      → a*b != 0 trick or fcmp≠0 both sides + band → select
//!      (must match interp.rs semantics BIT-EXACTLY — tests compare with ==)
//!   6. ins().return_(&[val]); declare_function → define_function →
//!      finalize_definitions → get_finalized_function → transmute
//!   7. CompiledExpr must OWN the JITModule — the code pointer dies
//!      with it (postgres pins JITed code the same way: llvmjit.c:288).

use crate::expr::Expr;
use cranelift_jit::JITModule;

pub struct CompiledExpr {
    /// Keeps the executable memory alive. Never dropped before `func`
    /// stops being called.
    _module: JITModule,
    func: fn(*const f64) -> f64,
}

impl CompiledExpr {
    /// Caller contract: `row` has at least as many columns as the
    /// compiled expression references (gen_expr's n_cols).
    pub fn eval(&self, row: &[f64]) -> f64 {
        (self.func)(row.as_ptr())
    }
}

/// Compile `expr` into straight-line native code.
pub fn compile(expr: &Expr) -> CompiledExpr {
    let _ = expr;
    todo!("cranelift: Expr → CLIF → machine code (see module docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{expr::gen_expr, gen_cols, interp, to_rows};

    #[test]
    fn jit_matches_interpreter() {
        let cols = gen_cols(4, 100, 3);
        let rows = to_rows(&cols);
        for seed in 0..8 {
            let e = gen_expr(5, 4, seed);
            let compiled = compile(&e);
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(compiled.eval(row), interp::eval(&e, row), "seed {seed} row {i}");
            }
        }
    }

    #[test]
    fn jit_handles_leaf_exprs() {
        let c = compile(&Expr::Const(1.5));
        assert_eq!(c.eval(&[0.0]), 1.5);
        let col = compile(&Expr::Col(2));
        assert_eq!(col.eval(&[0.0, 1.0, 7.25]), 7.25);
    }
}
