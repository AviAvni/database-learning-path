//! `Vec<Vec<u32>>`: one sorted vector per vertex. The textbook adjacency list
//! — cheap writes into small rows, one heap allocation per vertex.

use crate::gen::EdgeList;
use crate::store::{GraphRead, GraphWrite};

pub struct VecVec {
    pub adj: Vec<Vec<u32>>,
    edges: usize,
}

impl VecVec {
    pub fn from_edges(el: &EdgeList) -> VecVec {
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); el.n as usize];
        for &(u, v) in &el.edges {
            adj[u as usize].push(v);
        }
        for row in &mut adj {
            row.shrink_to_fit();
        }
        VecVec { adj, edges: el.edges.len() }
    }
}

impl GraphRead for VecVec {
    fn name(&self) -> String {
        "Vec<Vec<u32>>".into()
    }
    fn num_vertices(&self) -> usize {
        self.adj.len()
    }
    fn num_edges(&self) -> usize {
        self.edges
    }
    #[inline]
    fn for_each<F: FnMut(u32)>(&self, u: u32, mut f: F) {
        for &x in &self.adj[u as usize] {
            f(x);
        }
    }
    fn contains(&self, u: u32, v: u32) -> bool {
        self.adj[u as usize].binary_search(&v).is_ok()
    }
    fn bytes(&self) -> usize {
        24 * self.adj.capacity() + self.adj.iter().map(|r| 4 * r.capacity()).sum::<usize>()
    }
}

impl GraphWrite for VecVec {
    fn insert(&mut self, u: u32, v: u32) -> bool {
        let row = &mut self.adj[u as usize];
        match row.binary_search(&v) {
            Ok(_) => false,
            Err(i) => {
                row.insert(i, v);
                self.edges += 1;
                true
            }
        }
    }
    fn delete(&mut self, u: u32, v: u32) -> bool {
        let row = &mut self.adj[u as usize];
        match row.binary_search(&v) {
            Err(_) => false,
            Ok(i) => {
                row.remove(i);
                self.edges -= 1;
                true
            }
        }
    }
}
