/// Dominator tree computation using the simple iterative algorithm.
///
/// For a CFG with n nodes, this runs in O(n²) in the worst case but is
/// extremely fast in practice for method-level bytecode CFGs (typically
/// < 200 blocks).  A future upgrade path is Cooper-Harvey-Kennedy (also
/// O(n²) worst-case but with a much smaller constant).
///
/// Reference: Cooper, Keith D., Timothy J. Harvey, and Ken Kennedy.
/// "A simple, fast dominance algorithm." (2001).
use crate::cfg::block::{BlockId, ENTRY_BLOCK};
use crate::cfg::Cfg;
use std::collections::HashMap;

/// Dominator tree node.
#[derive(Debug, Clone)]
pub struct DomTree {
    /// Immediate dominator of each block.  `idom[ENTRY] = ENTRY`.
    pub idom: HashMap<BlockId, BlockId>,
    /// RPO numbering for each block (used internally; not pub to avoid dead_code lint).
    #[allow(dead_code)]
    rpo_num: HashMap<BlockId, usize>,
    /// RPO list (block ids in reverse post-order).
    pub rpo: Vec<BlockId>,
}

impl DomTree {
    /// Compute the dominator tree for `cfg`.
    pub fn compute(cfg: &Cfg) -> Self {
        let rpo = cfg.rpo();
        let rpo_num: HashMap<BlockId, usize> = rpo.iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();

        let mut idom: HashMap<BlockId, BlockId> = HashMap::new();
        // ENTRY dominates itself
        idom.insert(ENTRY_BLOCK, ENTRY_BLOCK);

        // Iterative fixed-point
        let mut changed = true;
        while changed {
            changed = false;
            for &b in &rpo {
                if b == ENTRY_BLOCK { continue; }

                let block = cfg.block(b);
                // Use ALL predecessors — both regular and exception edges.
                // Exception handler blocks are only reachable via pred_exceptions,
                // so we must include them here or they will never get an idom.
                let all_preds: Vec<BlockId> = block.preds.iter()
                    .chain(block.pred_exceptions.iter())
                    .copied()
                    .collect();

                let new_idom_opt = all_preds.iter()
                    .filter(|&&p| idom.contains_key(&p))
                    .copied()
                    .reduce(|acc, p| intersect(acc, p, &idom, &rpo_num));

                if let Some(new_idom) = new_idom_opt {
                    let old = idom.get(&b).copied();
                    if old != Some(new_idom) {
                        idom.insert(b, new_idom);
                        changed = true;
                    }
                }
            }
        }

        DomTree { idom, rpo_num, rpo }
    }

    /// True iff `a` strictly dominates `b`  (a dom b and a ≠ b).
    pub fn strictly_dominates(&self, a: BlockId, b: BlockId) -> bool {
        if a == b { return false; }
        self.dominates(a, b)
    }

    /// True iff `a` dominates `b` (every path from ENTRY to `b` passes through `a`).
    pub fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        if a == b { return true; }
        let mut cur = b;
        loop {
            let idom = match self.idom.get(&cur).copied() {
                Some(id) => id,
                None     => return false,
            };
            if idom == a { return true;  }
            if idom == cur { return false; } // reached root without finding a
            cur = idom;
        }
    }

    /// Immediate dominator of `b`, or `None` for the entry block.
    pub fn idom(&self, b: BlockId) -> Option<BlockId> {
        let d = self.idom.get(&b).copied()?;
        if d == b { None } else { Some(d) }
    }

    /// All blocks that `a` immediately dominates (its children in the dom tree).
    pub fn children(&self, a: BlockId) -> Vec<BlockId> {
        self.idom.iter()
            .filter(|(&b, &d)| d == a && b != a)
            .map(|(&b, _)| b)
            .collect()
    }

    /// All blocks dominated by `a` (inclusive).
    pub fn dominated_set(&self, a: BlockId) -> Vec<BlockId> {
        let mut result = Vec::new();
        for &b in &self.rpo {
            if self.dominates(a, b) {
                result.push(b);
            }
        }
        result
    }

    /// Dominance frontier of `b`:
    /// DF(b) = { y | ∃x ∈ pred(y) s.t. b dom x and b !sdom y }
    pub fn dominance_frontier(&self, cfg: &Cfg, b: BlockId) -> Vec<BlockId> {
        let mut df = Vec::new();
        for block in cfg.blocks.iter() {
            let y = block.id;
            for &x in &block.preds {
                if self.dominates(b, x) && !self.strictly_dominates(b, y) {
                    if !df.contains(&y) { df.push(y); }
                }
            }
        }
        df
    }
}

/// The "intersect" step in Cooper's algorithm.
/// Finds the LCA (in the dom tree, using RPO numbering) of `b1` and `b2`.
fn intersect(
    mut b1: BlockId,
    mut b2: BlockId,
    idom: &HashMap<BlockId, BlockId>,
    rpo_num: &HashMap<BlockId, usize>,
) -> BlockId {
    while b1 != b2 {
        while rpo_num[&b1] > rpo_num[&b2] {
            b1 = idom[&b1];
        }
        while rpo_num[&b2] > rpo_num[&b1] {
            b2 = idom[&b2];
        }
    }
    b1
}

// ── Natural loop detection ─────────────────────────────────────────────────

/// A natural loop identified from a back-edge.
#[derive(Debug, Clone)]
pub struct NaturalLoop {
    /// The loop header — the single entry point and the target of the back-edge.
    pub header: BlockId,
    /// The tail block that has the back-edge to `header`.
    pub tail:   BlockId,
    /// All blocks belonging to this loop (includes header and tail).
    pub body:   Vec<BlockId>,
}

/// Find all natural loops in `cfg` using the dominator tree.
///
/// A back-edge is an edge `tail → header` where `header` dom `tail`.
pub fn find_natural_loops(cfg: &Cfg, dom: &DomTree) -> Vec<NaturalLoop> {
    let mut loops = Vec::new();

    for block in cfg.blocks.iter() {
        for &succ in &block.succs {
            // back-edge: succ dominates block
            if dom.dominates(succ, block.id) {
                let header = succ;
                let tail   = block.id;
                let body   = find_loop_body(cfg, header, tail);
                loops.push(NaturalLoop { header, tail, body });
            }
        }
    }

    loops
}

/// Find all blocks in the natural loop with back-edge `tail → header`.
/// Uses a reverse DFS from `tail` that stops at `header`.
fn find_loop_body(cfg: &Cfg, header: BlockId, tail: BlockId) -> Vec<BlockId> {
    let mut body  = vec![header];
    let mut stack = vec![tail];
    let mut visited = std::collections::HashSet::new();
    visited.insert(header);

    while let Some(b) = stack.pop() {
        if visited.insert(b) {
            body.push(b);
            // Walk predecessors (reverse CFG)
            for &pred in &cfg.block(b).preds {
                if !visited.contains(&pred) {
                    stack.push(pred);
                }
            }
        }
    }

    body
}
