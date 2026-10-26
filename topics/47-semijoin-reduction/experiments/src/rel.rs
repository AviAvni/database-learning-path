//! Relations and conjunctive queries (PROVIDED).
//!
//! A relation is column-major: one `Vec<u32>` per variable it binds. Every row
//! also carries the index it had in the base relation (`ids`), so a reduced
//! relation can always say *which* base rows survived — lane 2's false-positive
//! accounting is built on that.

/// A variable of the query, an index into `Query::var_names`.
pub type Var = usize;

#[derive(Clone, Debug)]
pub struct Relation {
    pub name: &'static str,
    /// The variables this relation binds, one per column.
    pub vars: Vec<Var>,
    /// `cols[c][r]` is row r's value for `vars[c]`.
    pub cols: Vec<Vec<u32>>,
    /// The base-relation row index of each row.
    pub ids: Vec<u32>,
}

impl Relation {
    pub fn new(name: &'static str, vars: Vec<Var>, cols: Vec<Vec<u32>>) -> Self {
        assert_eq!(vars.len(), cols.len());
        let n = cols.first().map_or(0, |c| c.len());
        assert!(cols.iter().all(|c| c.len() == n));
        Relation { name, vars, cols, ids: (0..n as u32).collect() }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The column index holding `v`, if this relation binds it.
    pub fn col_of(&self, v: Var) -> Option<usize> {
        self.vars.iter().position(|&x| x == v)
    }

    /// The values of variable `v`, row by row. Panics if `v` is not bound here.
    pub fn col(&self, v: Var) -> &[u32] {
        &self.cols[self.col_of(v).expect("variable not in relation")]
    }

    /// The rows for which `keep(row)` is true, in their original order.
    pub fn filter(&self, mut keep: impl FnMut(usize) -> bool) -> Relation {
        let mut cols: Vec<Vec<u32>> = vec![Vec::new(); self.cols.len()];
        let mut ids = Vec::new();
        for r in 0..self.len() {
            if keep(r) {
                for (c, col) in self.cols.iter().enumerate() {
                    cols[c].push(col[r]);
                }
                ids.push(self.ids[r]);
            }
        }
        Relation { name: self.name, vars: self.vars.clone(), cols, ids }
    }

    /// The single variable this relation shares with `other`. Every query in
    /// this crate is a chain, so a pair of relations shares at most one.
    pub fn shared_var(&self, other: &Relation) -> Option<Var> {
        let shared: Vec<Var> = self.vars.iter().copied().filter(|v| other.vars.contains(v)).collect();
        assert!(shared.len() <= 1, "{} and {} share more than one variable", self.name, other.name);
        shared.first().copied()
    }
}

/// A conjunctive query: the natural join of `rels` on shared variable names.
#[derive(Clone, Debug)]
pub struct Query {
    pub var_names: Vec<&'static str>,
    pub rels: Vec<Relation>,
}

impl Query {
    pub fn nvars(&self) -> usize {
        self.var_names.len()
    }

    pub fn total_rows(&self) -> usize {
        self.rels.iter().map(|r| r.len()).sum()
    }

    /// Position of the relation called `name`.
    pub fn index_of(&self, name: &str) -> usize {
        self.rels.iter().position(|r| r.name == name).expect("no such relation")
    }
}
