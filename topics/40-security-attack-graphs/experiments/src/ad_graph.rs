//! PROVIDED — a synthetic Active-Directory-shaped identity graph, plus the
//! two baselines that make the topic's point: what the *list* view of
//! privilege says vs what the *graph* view says.
//!
//! BloodHound's whole thesis in one sentence: defenders enumerate
//! privilege ("who is in Domain Admins?" — a `MemberOf` list), attackers
//! traverse it ("what sequence of rights gets me there?" — a path over
//! `MemberOf` + `AdminTo` + `HasSession` + ACL edges). The list answer is
//! a handful of accounts. The graph answer is most of the directory.
//!
//! Node id layout (one flat id space so everything is a `usize`):
//!
//! ```text
//!   [0, n_users)                                users
//!   [n_users, n_users + n_groups)               groups   (tier zero = last)
//!   [n_users + n_groups, .. + n_computers)      computers
//! ```
//!
//! Edge semantics — every edge means "control of the source yields
//! control of the target", which is exactly BloodHound's traversal rule:
//!
//! | kind          | from → to          | meaning                                  |
//! |---------------|--------------------|------------------------------------------|
//! | `MemberOf`    | user → group       | member inherits the group's rights       |
//! | `MemberOf`    | group → group      | nesting; rights flow up the chain        |
//! | `AdminTo`     | group → computer   | local admin on the machine               |
//! | `HasSession`  | computer → user    | that user's token/creds sit on the box   |
//! | `GenericAll`  | principal → group  | can rewrite the group's ACL, so join it  |
//!
//! `HasSession` is the edge that turns a tree into a strongly connected
//! mess: a Domain Admin logging into one workstation makes every local
//! admin of that workstation a Domain Admin, transitively. It is the
//! reason attack-path reachability is not a membership query.

use rand::Rng;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::collections::VecDeque;

pub fn seeded_rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    MemberOf,
    AdminTo,
    HasSession,
    GenericAll,
}

/// Four groups are reserved at the top of the group id space so the
/// planted structure stays interpretable — the same trick as topic 39's
/// planted fraud block:
///
/// ```text
///   n_groups-1  tier zero   ("Domain Admins")
///   n_groups-2  t0_ops      nested INTO tier zero — what "expand nested
///                            groups" in the AD console finds
///   n_groups-3  helpdesk    AdminTo on every session host
///   n_groups-4  staff       nested into helpdesk — the choke point
/// ```
pub const RESERVED_GROUPS: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct AdConfig {
    pub n_users: usize,
    pub n_groups: usize,
    pub n_computers: usize,
    /// `MemberOf` edges per user, into ordinary groups.
    pub groups_per_user: usize,
    /// Random group → group nesting edges among ordinary groups.
    pub group_nesting: usize,
    /// Random `AdminTo` edges (ordinary group → ordinary computer).
    pub admin_edges: usize,
    /// Computers `[0, session_hosts)` are the tier-1 servers where
    /// privileged tokens actually land. Separately administered, so
    /// random `AdminTo` edges never target them.
    pub session_hosts: usize,
    /// How many users are "privileged operators" whose tokens are worth
    /// stealing.
    pub privileged_users: usize,
    /// `HasSession` edges from session hosts to privileged users. This
    /// is the dial lane 1 sweeps: operational hygiene, nothing else.
    pub privileged_sessions: usize,
    /// `HasSession` edges from ordinary computers to ordinary users.
    pub ordinary_sessions: usize,
    /// `GenericAll` edges (principal → ordinary group) — misconfigured
    /// ACLs.
    pub acl_edges: usize,
    /// Users with a direct `MemberOf` edge to tier zero.
    pub tier_zero_members: usize,
    /// Members of the nested `t0_ops` group.
    pub t0_ops_members: usize,
    /// Fraction of ordinary users who are members of `staff`.
    pub staff_fraction: f64,
    /// Narrow second-best paths onto the session hosts: dedicated
    /// service-account groups with `AdminTo` on one host each, held by a
    /// handful of members and wired into nothing else. Without these the
    /// top choke point is a silver bullet, which no real directory ever
    /// gives you; with them, remediation is a curve.
    pub service_groups: usize,
    pub service_group_members: usize,
    /// Ordinary workstations that hold a *privileged* token in violation
    /// of tiering policy — the classic "the Domain Admin RDP'd into a
    /// helpdesk box" finding. Each one is another way around whatever
    /// choke point you were about to remediate.
    pub da_on_workstation: usize,
}

impl AdConfig {
    /// The tiered directory Microsoft's administrative-tier model asks
    /// for and BloodHound's Tier Zero concept formalises: privileged
    /// tokens land only on separately administered hosts, and exactly
    /// one group is local admin on them. Structurally this is what
    /// *creates* choke points — see lane 2.
    pub fn tiered() -> Self {
        AdConfig {
            service_groups: 0,
            da_on_workstation: 0,
            ..AdConfig::default()
        }
    }
}

impl Default for AdConfig {
    fn default() -> Self {
        AdConfig {
            n_users: 2_000,
            n_groups: 400,
            n_computers: 1_000,
            groups_per_user: 3,
            group_nesting: 600,
            admin_edges: 1_200,
            session_hosts: 25,
            privileged_users: 40,
            privileged_sessions: 600,
            ordinary_sessions: 2_000,
            acl_edges: 150,
            tier_zero_members: 5,
            t0_ops_members: 3,
            staff_fraction: 0.55,
            service_groups: 3,
            service_group_members: 8,
            da_on_workstation: 2,
        }
    }
}

pub struct AdGraph {
    pub n_users: usize,
    pub n_groups: usize,
    pub n_computers: usize,
    /// Flat id of the tier-zero group ("Domain Admins").
    pub tier_zero: usize,
    /// Flat id of the group nested inside tier zero.
    pub t0_ops: usize,
    /// Flat id of the group with `AdminTo` on every session host.
    pub helpdesk: usize,
    /// Flat id of the group nested inside `helpdesk`.
    pub staff: usize,
    /// Service-account groups: narrow alternate routes onto a session host.
    pub service_group_ids: Vec<usize>,
    /// Ordinary workstations holding a privileged token against policy.
    pub violation_hosts: Vec<usize>,
    /// `(from, to, kind)`, deduplicated.
    pub edges: Vec<(usize, usize, EdgeKind)>,
    /// Forward adjacency over all edge kinds.
    pub adj: Vec<Vec<usize>>,
    /// Forward adjacency over `MemberOf` only — the "AD console" view.
    pub memberof_adj: Vec<Vec<usize>>,
}

impl AdGraph {
    pub fn n_nodes(&self) -> usize {
        self.n_users + self.n_groups + self.n_computers
    }
    pub fn is_user(&self, id: usize) -> bool {
        id < self.n_users
    }
    pub fn is_group(&self, id: usize) -> bool {
        id >= self.n_users && id < self.n_users + self.n_groups
    }
    pub fn is_computer(&self, id: usize) -> bool {
        id >= self.n_users + self.n_groups
    }
    pub fn group(&self, g: usize) -> usize {
        self.n_users + g
    }
    pub fn computer(&self, c: usize) -> usize {
        self.n_users + self.n_groups + c
    }
    /// Reverse adjacency (used by the choke-point analysis).
    pub fn reverse_adj(&self) -> Vec<Vec<usize>> {
        let mut rev = vec![Vec::new(); self.n_nodes()];
        for &(u, v, _) in &self.edges {
            rev[v].push(u);
        }
        rev
    }
}

/// Build the graph. Deterministic given the rng seed.
pub fn ad_instance(rng: &mut ChaCha8Rng, cfg: &AdConfig) -> AdGraph {
    let n = cfg.n_users + cfg.n_groups + cfg.n_computers;
    let tier_zero = cfg.n_users + cfg.n_groups - 1;
    let t0_ops = cfg.n_users + cfg.n_groups - 2;
    let helpdesk = cfg.n_users + cfg.n_groups - 3;
    let staff = cfg.n_users + cfg.n_groups - 4;
    // Ordinary groups split into the general mesh and a band of
    // service-account groups the mesh never touches.
    let n_ordinary = cfg.n_groups - RESERVED_GROUPS;
    let n_mesh = n_ordinary - cfg.service_groups;
    let mut edges: Vec<(usize, usize, EdgeKind)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push = |edges: &mut Vec<(usize, usize, EdgeKind)>,
                    seen: &mut std::collections::HashSet<(usize, usize)>,
                    a: usize,
                    b: usize,
                    k: EdgeKind| {
        if a != b && seen.insert((a, b)) {
            edges.push((a, b, k));
        }
    };

    // Groups nest, but only "upward": a lower-numbered group may be
    // nested inside a higher-numbered one. Real directories have cycles
    // too; the analysis handles them, the generator keeps them rare so
    // the planted structure stays interpretable.
    for _ in 0..cfg.group_nesting {
        let a = rng.gen_range(0..n_mesh);
        let b = rng.gen_range(0..n_mesh);
        let (lo, hi) = (a.min(b), a.max(b));
        if lo != hi {
            let (lo, hi) = (cfg.n_users + lo, cfg.n_users + hi);
            push(&mut edges, &mut seen, lo, hi, EdgeKind::MemberOf);
        }
    }

    // Ordinary users join a few ordinary groups.
    for u in 0..cfg.n_users {
        for _ in 0..cfg.groups_per_user {
            let g = cfg.n_users + rng.gen_range(0..n_mesh);
            push(&mut edges, &mut seen, u, g, EdgeKind::MemberOf);
        }
    }

    // The handful of accounts the AD console will show you, plus the one
    // nested group that "expand nested groups" turns up.
    for u in 0..cfg.tier_zero_members {
        push(&mut edges, &mut seen, u, tier_zero, EdgeKind::MemberOf);
    }
    push(&mut edges, &mut seen, t0_ops, tier_zero, EdgeKind::MemberOf);
    for i in 0..cfg.t0_ops_members {
        push(
            &mut edges,
            &mut seen,
            cfg.tier_zero_members + i,
            t0_ops,
            EdgeKind::MemberOf,
        );
    }

    // The planted choke structure: most of the company is in `staff`,
    // `staff` is nested inside `helpdesk`, and `helpdesk` is local admin
    // on every machine where an operator token lands. Nobody designed
    // this; it accreted.
    push(&mut edges, &mut seen, staff, helpdesk, EdgeKind::MemberOf);
    for h in 0..cfg.session_hosts {
        push(
            &mut edges,
            &mut seen,
            helpdesk,
            cfg.n_users + cfg.n_groups + h,
            EdgeKind::AdminTo,
        );
    }
    for u in 0..cfg.n_users {
        if rng.gen::<f64>() < cfg.staff_fraction {
            push(&mut edges, &mut seen, u, staff, EdgeKind::MemberOf);
        }
    }
    // Narrow second-best paths: each service-account group is local
    // admin on one session host and is reachable only by its own members.
    let mut service_group_ids = Vec::new();
    for i in 0..cfg.service_groups {
        let g = cfg.n_users + n_mesh + i;
        service_group_ids.push(g);
        let c = cfg.n_users + cfg.n_groups + (i % cfg.session_hosts.max(1));
        push(&mut edges, &mut seen, g, c, EdgeKind::AdminTo);
        for _ in 0..cfg.service_group_members {
            let u = rng.gen_range(0..cfg.n_users);
            push(&mut edges, &mut seen, u, g, EdgeKind::MemberOf);
        }
    }

    // AdminTo: ordinary groups grant local admin on ordinary machines.
    for _ in 0..cfg.admin_edges {
        let g = cfg.n_users + rng.gen_range(0..n_mesh);
        let c = cfg.n_users + cfg.n_groups + rng.gen_range(cfg.session_hosts..cfg.n_computers);
        push(&mut edges, &mut seen, g, c, EdgeKind::AdminTo);
    }

    // HasSession: the hygiene dial. Privileged users are the ones whose
    // tokens are worth stealing; their sessions land on the tier-1
    // session hosts.
    let priv_lo = 0;
    let priv_hi = cfg.privileged_users.min(cfg.n_users);
    for _ in 0..cfg.privileged_sessions {
        let c = cfg.n_users + cfg.n_groups + rng.gen_range(0..cfg.session_hosts);
        let u = rng.gen_range(priv_lo..priv_hi.max(1));
        push(&mut edges, &mut seen, c, u, EdgeKind::HasSession);
    }
    for _ in 0..cfg.ordinary_sessions {
        let c = cfg.n_users + cfg.n_groups + rng.gen_range(cfg.session_hosts..cfg.n_computers);
        let u = rng.gen_range(priv_hi.max(1)..cfg.n_users);
        push(&mut edges, &mut seen, c, u, EdgeKind::HasSession);
    }
    // Policy violations: a privileged token on an ordinary workstation.
    let mut violation_hosts = Vec::new();
    for i in 0..cfg.da_on_workstation {
        let c = cfg.n_users + cfg.n_groups + cfg.session_hosts + i;
        violation_hosts.push(c);
        push(&mut edges, &mut seen, c, i % priv_hi.max(1), EdgeKind::HasSession);
    }

    // GenericAll: a principal that can rewrite a group's ACL can add
    // itself to the group. Misconfiguration, not design.
    for _ in 0..cfg.acl_edges {
        let src = rng.gen_range(0..cfg.n_users + n_mesh);
        let g = cfg.n_users + rng.gen_range(0..n_mesh);
        push(&mut edges, &mut seen, src, g, EdgeKind::GenericAll);
    }

    let mut adj = vec![Vec::new(); n];
    let mut memberof_adj = vec![Vec::new(); n];
    for &(u, v, k) in &edges {
        adj[u].push(v);
        if k == EdgeKind::MemberOf {
            memberof_adj[u].push(v);
        }
    }

    AdGraph {
        n_users: cfg.n_users,
        n_groups: cfg.n_groups,
        n_computers: cfg.n_computers,
        tier_zero,
        t0_ops,
        helpdesk,
        staff,
        service_group_ids,
        violation_hosts,
        edges,
        adj,
        memberof_adj,
    }
}

/// Nodes that can reach `target` along `adj`, by one reverse BFS.
///
/// The whole reason a defender should think in graphs: this costs one
/// traversal, not one traversal per principal.
pub fn reaches(rev: &[Vec<usize>], target: usize) -> Vec<bool> {
    let mut seen = vec![false; rev.len()];
    let mut q = VecDeque::new();
    seen[target] = true;
    q.push_back(target);
    while let Some(v) = q.pop_front() {
        for &u in &rev[v] {
            if !seen[u] {
                seen[u] = true;
                q.push_back(u);
            }
        }
    }
    seen
}

fn reverse_of(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut rev = vec![Vec::new(); adj.len()];
    for (u, outs) in adj.iter().enumerate() {
        for &v in outs {
            rev[v].push(u);
        }
    }
    rev
}

/// The list view: users with a `MemberOf` edge straight into tier zero.
/// This is the number that goes in the compliance report.
pub fn direct_tier_zero_members(g: &AdGraph) -> usize {
    g.edges
        .iter()
        .filter(|&&(u, v, k)| v == g.tier_zero && k == EdgeKind::MemberOf && g.is_user(u))
        .count()
}

/// The slightly better list view: transitive `MemberOf` closure. This is
/// what "expand nested groups" in the AD console gives you.
pub fn memberof_reachable_users(g: &AdGraph) -> usize {
    let rev = reverse_of(&g.memberof_adj);
    let seen = reaches(&rev, g.tier_zero);
    (0..g.n_users).filter(|&u| seen[u]).count()
}

/// The graph view: users with *any* attack path to tier zero.
pub fn attack_path_reachable_users(g: &AdGraph) -> usize {
    let rev = g.reverse_adj();
    let seen = reaches(&rev, g.tier_zero);
    (0..g.n_users).filter(|&u| seen[u]).count()
}

/// Shortest attack-path length (in edges) from each reaching user to
/// tier zero; returns (count, mean, max). Short paths are the finding
/// that gets an engagement report written.
pub fn attack_path_lengths(g: &AdGraph) -> (usize, f64, usize) {
    let rev = g.reverse_adj();
    let mut dist = vec![usize::MAX; g.n_nodes()];
    dist[g.tier_zero] = 0;
    let mut q = VecDeque::new();
    q.push_back(g.tier_zero);
    while let Some(v) = q.pop_front() {
        for &u in &rev[v] {
            if dist[u] == usize::MAX {
                dist[u] = dist[v] + 1;
                q.push_back(u);
            }
        }
    }
    let ds: Vec<usize> = (0..g.n_users)
        .filter(|&u| dist[u] != usize::MAX)
        .map(|u| dist[u])
        .collect();
    if ds.is_empty() {
        return (0, 0.0, 0);
    }
    let mean = ds.iter().sum::<usize>() as f64 / ds.len() as f64;
    (ds.len(), mean, *ds.iter().max().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_view_undercounts_the_graph_view() {
        let mut rng = seeded_rng(1);
        let g = ad_instance(&mut rng, &AdConfig::default());
        let direct = direct_tier_zero_members(&g);
        let nested = memberof_reachable_users(&g);
        let paths = attack_path_reachable_users(&g);
        assert_eq!(direct, 5, "the compliance-report number");
        assert_eq!(nested, 8, "5 direct + 3 via the nested t0_ops group");
        assert!(
            paths > 50 * nested,
            "attack paths {paths} vs membership {nested}"
        );
    }

    #[test]
    fn sessions_are_what_create_the_exposure() {
        // Same directory, no privileged sessions: the graph view
        // collapses back toward the list view.
        let mut rng = seeded_rng(2);
        let cfg = AdConfig {
            privileged_sessions: 0,
            ordinary_sessions: 0,
            ..AdConfig::tiered()
        };
        let g = ad_instance(&mut rng, &cfg);
        let paths = attack_path_reachable_users(&g);
        // Without a single stolen token the graph view collapses back
        // onto the list view: only the accounts actually granted the
        // privilege have it.
        assert_eq!(paths, memberof_reachable_users(&g));
        assert!(paths < g.n_users / 100, "no sessions: {paths} users reach TZ");
    }

    #[test]
    fn two_misplaced_tokens_are_worth_hundreds_of_users() {
        // Nothing else changes: same groups, same ACLs, zero sessions on
        // the tier-1 hosts. Two Domain Admin tokens left on ordinary
        // workstations are the entire difference.
        let clean = AdConfig {
            privileged_sessions: 0,
            ordinary_sessions: 0,
            ..AdConfig::tiered()
        };
        let sloppy = AdConfig {
            da_on_workstation: 2,
            ..clean
        };
        let a = attack_path_reachable_users(&ad_instance(&mut seeded_rng(2), &clean));
        let b = attack_path_reachable_users(&ad_instance(&mut seeded_rng(2), &sloppy));
        assert_eq!(a, 8);
        assert!(b > 20 * a, "{a} -> {b}");
    }
}
