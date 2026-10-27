//! Topic 55 — Datalog engines: recursion as joins and as sparse algebra.
//!
//! Lane 1 (provided): transitive closure and same-generation on seeded graphs
//! of two shapes, evaluated naively and semi-naively over hash-indexed
//! relations. The headline is counts that reproduce exactly: iterations,
//! derived tuples, and how many derivations were duplicates.
//!
//! Lane 2 (exercise): the same fixpoint over sorted, merge-maintained
//! relations (the shape of Soufflé's B-tree and Cozo's BTreeMap stores), with
//! a join-order choice between the left- and right-linear forms of TC.
//!
//! Lane 3 (exercise): the GraphBLAS formulation — a repeated masked sparse
//! matrix product with a complement mask over the boolean semiring.

pub mod algebra;
pub mod eval;
pub mod gen;
pub mod hash;
pub mod sorted;
