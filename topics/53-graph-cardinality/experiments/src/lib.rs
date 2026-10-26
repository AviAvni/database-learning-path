//! Topic 53 — cardinality estimation for graph patterns.
//!
//! Lane 1 (provided): a seeded labelled graph with Zipfian degrees, an exact
//! oracle for path and triangle counts, and the estimators three production
//! engines actually ship — the independence / average-degree product
//! (Kùzu's `getExtensionRate`, Neo4j's node-connection multiplier), Memgraph's
//! constant fan-out, and the pessimistic max-degree and degree-sequence
//! bounds — scored by q-error per hop count, plus whether the estimate
//! picks the cheaper of the two expansion directions.
//!
//! Lane 2 (exercise): a characteristic-set estimator (Neumann & Moerkotte,
//! as G-CARE implements it).
//!
//! Lane 3 (exercise): a GLogue-style catalog of exact two-edge path counts,
//! chained by the order-2 Markov rule GOpt's Equation 1 states.

pub mod catalog;
pub mod cset;
pub mod est;
pub mod gen;
pub mod oracle;
pub mod plan;
