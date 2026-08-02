/// Control flow recovery — converts a flat CFG into a structured Stmt tree.
///
/// Algorithm overview (one pass per structuring rule):
///
///  1. Detect and record natural loops (from Phase 3 dominators).
///  2. For each loop (innermost-first):
///     a. Classify as while / do-while / infinite.
///     b. Wrap the loop body blocks into a LoopStmt.
///  3. Recover if/else patterns from conditional branches.
///  4. Wrap exception ranges into TryCatch nodes.
///  5. Arrange remaining blocks into a linear Seq.
///
/// This is a structural analysis approach (similar to Cifuentes' technique)
/// rather than the full SSA-based approach used by Vineflower.  It handles
/// the common 95%+ of real Java bytecode; irreducible CFGs fall back to
/// a labeled-block representation.
use std::collections::{HashMap, HashSet, VecDeque};

use crate::cfg::dominators::find_natural_loops;
use crate::cfg::{BlockId, Cfg, DomTree, NaturalLoop, PostDomTree, ENTRY_BLOCK, EXIT_BLOCK};
use crate::classfile::attribute::CodeAttribute;
use crate::classfile::instruction::InsnKind;
use crate::classfile::opcodes::opc;
use crate::ir::stmt::{
    BlockStmt, BreakIfStmt, CaseArm, CatchClause, IfStmt, LoopKind, LoopStmt, SeqStmt, Stmt,
    StmtArena, StmtId, SwitchStmt, TryCatchStmt,
};

// ── RecoveryCtx ───────────────────────────────────────────────────────────

/// Working context for one method's structuring pass.
pub struct RecoveryCtx<'a> {
    pub cfg: &'a Cfg,
    pub dom: &'a DomTree,
    pub post_dom: PostDomTree,
    pub code: &'a CodeAttribute,
    pub arena: StmtArena,
    /// Blocks that have already been claimed by a higher-level construct.
    claimed: HashSet<BlockId>,
    /// Structured statement rooted at a block, used when short-circuit paths
    /// share a value-producing target that an inner region claimed first.
    block_stmts: HashMap<BlockId, StmtId>,
    /// Loop info keyed by header block.
    loops: HashMap<BlockId, NaturalLoop>,
    loop_exit_guards: HashMap<BlockId, bool>,
    use_nearest_branch_convergence: bool,
}

impl<'a> RecoveryCtx<'a> {
    fn new(
        cfg: &'a Cfg,
        dom: &'a DomTree,
        code: &'a CodeAttribute,
        use_nearest_branch_convergence: bool,
    ) -> Self {
        let loops_vec = find_natural_loops(cfg, dom);
        let mut loop_exit_guards = HashMap::<BlockId, (usize, bool)>::new();
        for natural_loop in &loops_vec {
            let body = natural_loop.body.iter().copied().collect::<HashSet<_>>();
            for block_id in &natural_loop.body {
                let block = cfg.block(*block_id);
                let Some(last) = block.last_insn() else {
                    continue;
                };
                if !is_conditional_branch(last.opcode) || block.succs.len() != 2 {
                    continue;
                }
                let taken_exits = !body.contains(&block.succs[0]);
                let fallthrough_exits = !body.contains(&block.succs[1]);
                if taken_exits == fallthrough_exits {
                    continue;
                }
                if *block_id == natural_loop.tail && block.succs.contains(&natural_loop.header) {
                    continue;
                }
                // A pre-test loop header owns its exit condition; every other
                // conditional exit is an explicit break inside the source loop.
                if *block_id == natural_loop.header && taken_exits {
                    continue;
                }
                let candidate = (natural_loop.body.len(), fallthrough_exits);
                if loop_exit_guards
                    .get(block_id)
                    .is_none_or(|existing| candidate.0 < existing.0)
                {
                    loop_exit_guards.insert(*block_id, candidate);
                }
            }
        }
        let loop_exit_guards = loop_exit_guards
            .into_iter()
            .map(|(block, (_, negated))| (block, negated))
            .collect();
        let loops: HashMap<BlockId, NaturalLoop> =
            loops_vec.into_iter().map(|l| (l.header, l)).collect();
        RecoveryCtx {
            cfg,
            dom,
            code,
            post_dom: PostDomTree::compute(cfg),
            arena: StmtArena::new(),
            claimed: HashSet::new(),
            block_stmts: HashMap::new(),
            loops,
            loop_exit_guards,
            use_nearest_branch_convergence,
        }
    }

    fn claim(&mut self, id: BlockId) {
        self.claimed.insert(id);
    }
    fn is_claimed(&self, id: BlockId) -> bool {
        self.claimed.contains(&id)
    }
}

// ── Public entry point ─────────────────────────────────────────────────────

/// Recover structured control flow for a single method.
///
/// Returns a `(StmtArena, root_StmtId)` pair.  The root statement is
/// always a `Stmt::Seq` that covers the entire method body.
pub fn recover(cfg: &Cfg, dom: &DomTree, code: &CodeAttribute) -> (StmtArena, StmtId) {
    recover_with_options(cfg, dom, code, false)
}

/// Recover a normalized coroutine CFG whose nested early returns may make the
/// formal post-dominator too late to represent the source-level branch join.
pub fn recover_with_branch_convergence(
    cfg: &Cfg,
    dom: &DomTree,
    code: &CodeAttribute,
) -> (StmtArena, StmtId) {
    recover_with_options(cfg, dom, code, true)
}

fn recover_with_options(
    cfg: &Cfg,
    dom: &DomTree,
    code: &CodeAttribute,
    use_nearest_branch_convergence: bool,
) -> (StmtArena, StmtId) {
    let mut ctx = RecoveryCtx::new(cfg, dom, code, use_nearest_branch_convergence);

    // Get blocks in RPO — we process outer constructs first (by header block RPO order).
    let rpo = cfg.rpo();

    // Collect blocks we need to structure (skip ENTRY and EXIT).
    let blocks: Vec<BlockId> = rpo
        .iter()
        .copied()
        .filter(|&id| id != ENTRY_BLOCK && id != EXIT_BLOCK)
        .collect();

    let root_id = recover_region(&mut ctx, &blocks);
    (ctx.arena, root_id)
}

// ── Region recovery ────────────────────────────────────────────────────────

/// Recover a contiguous region of blocks into a Stmt.
/// `blocks` must be in RPO order.
fn recover_region(ctx: &mut RecoveryCtx, blocks: &[BlockId]) -> StmtId {
    let mut stmt_ids: Vec<StmtId> = Vec::new();
    let mut i = 0;

    while i < blocks.len() {
        let bid = blocks[i];
        if ctx.is_claimed(bid) {
            i += 1;
            continue;
        }

        // Exception edges can place a handler before its protected range in
        // RPO (especially inside loops). Do not emit it as a plain block;
        // try_recover_try_catch will attach it when the range head is reached.
        if should_defer_handler(ctx, bid, blocks) {
            i += 1;
            continue;
        }

        // ── exception range? ────────────────────────────────────────
        if let Some(stmt_id) = try_recover_try_catch(ctx, bid, blocks) {
            stmt_ids.push(stmt_id);
            i += 1;
            continue;
        }

        // ── loop header? ────────────────────────────────────────────
        if let Some(stmt_id) = try_recover_loop(ctx, bid, blocks) {
            stmt_ids.push(stmt_id);
            i += 1;
            continue;
        }

        // ── conditional branch (if/else)? ───────────────────────────
        if let Some(negated) = ctx.loop_exit_guards.get(&bid).copied() {
            let block = ctx.cfg.block(bid);
            let stmt = ctx.arena.alloc(Stmt::BreakIf(BreakIfStmt {
                cond_block: bid,
                cond_insns: block.instructions.clone(),
                negated,
            }));
            ctx.claim(bid);
            ctx.block_stmts.insert(bid, stmt);
            stmt_ids.push(stmt);
            i += 1;
            continue;
        }

        if let Some(stmt_id) = try_recover_if(ctx, bid, blocks) {
            stmt_ids.push(stmt_id);
            i += 1;
            continue;
        }

        // ── switch? ─────────────────────────────────────────────────
        if let Some(stmt_id) = try_recover_switch(ctx, bid, blocks) {
            stmt_ids.push(stmt_id);
            i += 1;
            continue;
        }

        // ── plain basic block ────────────────────────────────────────
        let block = ctx.cfg.block(bid);
        let bs = BlockStmt {
            block_id: bid,
            instructions: block.instructions.clone(),
            succs: block.succs.clone(),
        };
        let s = ctx.arena.alloc(Stmt::Block(bs));
        ctx.claim(bid);
        ctx.block_stmts.insert(bid, s);
        stmt_ids.push(s);
        i += 1;
    }

    // Exit placeholder
    if stmt_ids.is_empty() {
        return ctx.arena.alloc(Stmt::Exit);
    }

    if stmt_ids.len() == 1 {
        return stmt_ids[0];
    }

    ctx.arena.alloc(Stmt::Seq(SeqStmt { children: stmt_ids }))
}

fn should_defer_handler(ctx: &RecoveryCtx<'_>, block: BlockId, region: &[BlockId]) -> bool {
    let block_offset = ctx.cfg.block(block).start_offset;
    ctx.code.exception_table.iter().any(|handler| {
        if handler.handler_pc as u32 != block_offset {
            return false;
        }
        region.iter().any(|candidate| {
            let candidate_block = ctx.cfg.block(*candidate);
            candidate_block.start_offset == handler.start_pc as u32 && !ctx.is_claimed(*candidate)
        })
    })
}

fn recover_branch_region(ctx: &mut RecoveryCtx, blocks: &[BlockId]) -> StmtId {
    if !blocks.is_empty() && blocks.iter().all(|block| ctx.is_claimed(*block)) {
        if let Some(first) = blocks
            .first()
            .and_then(|block| ctx.block_stmts.get(block))
            .copied()
        {
            if !matches!(ctx.arena.get(first), Stmt::Block(_)) {
                return first;
            }
        }
        let children: Vec<StmtId> = blocks
            .iter()
            .filter_map(|block| ctx.block_stmts.get(block).copied())
            .collect();
        if children.len() == blocks.len() {
            return if children.len() == 1 {
                children[0]
            } else {
                ctx.arena.alloc(Stmt::Seq(SeqStmt { children }))
            };
        }
    }
    recover_region(ctx, blocks)
}

// ── Loop recovery ──────────────────────────────────────────────────────────

fn try_recover_loop(
    ctx: &mut RecoveryCtx,
    header: BlockId,
    _all_blocks: &[BlockId],
) -> Option<StmtId> {
    let lp = ctx.loops.get(&header)?.clone();

    // Don't re-process
    if ctx.is_claimed(header) {
        return None;
    }

    // Determine loop kind from the header block's last instruction.
    let header_block = ctx.cfg.block(header);
    let last_opcode = header_block.last_insn().map(|i| i.opcode).unwrap_or(0);
    let is_conditional_at_header = is_conditional_branch(last_opcode);

    // Snapshot header instructions before any mutation
    let header_insns = header_block.instructions.clone();

    // Find blocks *inside* the loop in RPO order (excluding header)
    let body_blocks: Vec<BlockId> = ctx
        .cfg
        .rpo()
        .into_iter()
        .filter(|&id| lp.body.contains(&id) && id != ENTRY_BLOCK && id != EXIT_BLOCK)
        .collect();

    // Check tail block for do-while
    let tail_insns = ctx.cfg.block(lp.tail).instructions.clone();
    let tail_opcode = tail_insns.last().map(|i| i.opcode).unwrap_or(0);
    let is_conditional_at_tail = is_conditional_branch(tail_opcode);

    // A conditional branch at the header is only a `while` test if the *taken*
    // path (succs[0] = branch target) leaves the loop.  When the taken path is
    // a back-edge that stays inside the loop (e.g. `ifgt self` in a single-block
    // do-while) the condition keeps the loop running, so negation is not needed.
    let header_taken_exits = header_block
        .succs
        .first()
        .map(|s| !lp.body.contains(s))
        .unwrap_or(false);

    let kind = if is_conditional_at_header && header_taken_exits {
        LoopKind::While
    } else if is_conditional_at_tail {
        LoopKind::DoWhile
    } else {
        LoopKind::Infinite
    };

    // The instructions that contain the condition expression
    let cond_insns = if kind == LoopKind::While {
        header_insns.clone()
    } else if kind == LoopKind::DoWhile {
        tail_insns
    } else {
        vec![]
    };

    // Claim the header block to prevent re-processing
    ctx.claim(header);

    // Find loop exit block (first block in the CFG after the loop)
    let post_block = find_loop_exit(ctx, &lp);

    // Recursively structure the loop body (excluding the header for while loops)
    let inner_blocks: Vec<BlockId> = body_blocks
        .iter()
        .copied()
        .filter(|&id| id != header)
        .collect();

    let body_stmt = if kind == LoopKind::DoWhile && inner_blocks.is_empty() {
        // Single-block do-while: the header contains both the body instructions
        // and the trailing conditional branch (the back-edge test).  The body is
        // everything up to (but not including) the conditional branch.
        let body_insns: Vec<_> = header_insns
            .iter()
            .take_while(|i| !is_conditional_branch(i.opcode))
            .cloned()
            .collect();
        let hb = ctx.cfg.block(header);
        ctx.arena.alloc(Stmt::Block(BlockStmt {
            block_id: header,
            instructions: body_insns,
            succs: hb.succs.clone(),
        }))
    } else {
        let rest = recover_region(ctx, &inner_blocks);
        if kind == LoopKind::While {
            rest
        } else if let Some(negated) = ctx.loop_exit_guards.get(&header).copied() {
            let guard = ctx.arena.alloc(Stmt::BreakIf(BreakIfStmt {
                cond_block: header,
                cond_insns: header_insns.clone(),
                negated,
            }));
            if matches!(ctx.arena.get(rest), Stmt::Exit) {
                guard
            } else {
                ctx.arena.alloc(Stmt::Seq(SeqStmt {
                    children: vec![guard, rest],
                }))
            }
        } else {
            // For non-pre-test loops the natural-loop header is source body,
            // not merely a condition block. Preserve its straight-line prefix
            // before nested loops and latch guards. The trailing conditional
            // is represented by the nested construct or an explicit BreakIf.
            let prefix = header_insns
                .iter()
                .take_while(|instruction| !is_conditional_branch(instruction.opcode))
                .cloned()
                .collect::<Vec<_>>();
            if prefix.is_empty() {
                rest
            } else {
                let prefix_stmt = ctx.arena.alloc(Stmt::Block(BlockStmt {
                    block_id: header,
                    instructions: prefix,
                    succs: header_block.succs.clone(),
                }));
                if matches!(ctx.arena.get(rest), Stmt::Exit) {
                    prefix_stmt
                } else {
                    ctx.arena.alloc(Stmt::Seq(SeqStmt {
                        children: vec![prefix_stmt, rest],
                    }))
                }
            }
        }
    };

    // Claim any remaining unclaimed body blocks
    for &bid in &body_blocks {
        ctx.claim(bid);
    }

    let cond_negated = kind == LoopKind::While;
    let loop_stmt = LoopStmt {
        kind,
        header_block: header,
        tail_block: lp.tail,
        body_blocks,
        body: body_stmt,
        post_block,
        cond_insns,
        // Only a `while` header branch jumps out of the loop, so only there is
        // the printed condition the negation of the branch predicate.
        cond_negated,
    };

    Some(ctx.arena.alloc(Stmt::Loop(loop_stmt)))
}

fn find_loop_exit(ctx: &RecoveryCtx, lp: &NaturalLoop) -> Option<BlockId> {
    // The loop exit is the first block reachable from a loop block
    // that is NOT in the loop body.
    for &bid in &lp.body {
        if bid == ENTRY_BLOCK || bid == EXIT_BLOCK {
            continue;
        }
        let block = ctx.cfg.block(bid);
        for &succ in &block.succs {
            if !lp.body.contains(&succ) && succ != EXIT_BLOCK {
                return Some(succ);
            }
        }
    }
    None
}

// ── If/else recovery ───────────────────────────────────────────────────────

fn try_recover_if(ctx: &mut RecoveryCtx, head: BlockId, all_blocks: &[BlockId]) -> Option<StmtId> {
    let head_block = ctx.cfg.block(head);
    let last = head_block.last_insn()?;

    if !is_conditional_branch(last.opcode) {
        return None;
    }

    let succs = &head_block.succs;
    if succs.len() != 2 {
        return None;
    }

    // In JVM, branch instruction: succs[0] = branch target, succs[1] = fall-through
    // (populated by builder in that order for conditional jumps)
    let (branch_target, fall_through) = (succs[0], succs[1]);

    let post = ctx
        .use_nearest_branch_convergence
        .then(|| nearest_branch_convergence(ctx, branch_target, fall_through, all_blocks))
        .flatten()
        .or_else(|| ctx.post_dom.immediately_post_dominates(head));

    // Decide which path is "then" and which is "else"
    // Convention: fall-through path = then-branch (most common in javac output)
    // We only build an else branch if both paths lead to the same post block.

    // Collect then-blocks: fall-through path up to (but not including) post
    let then_blocks: Vec<BlockId> = collect_path_blocks(ctx, fall_through, post, all_blocks);
    let else_blocks: Vec<BlockId> = collect_path_blocks(ctx, branch_target, post, all_blocks);

    // Snapshot the condition block instructions before claiming anything.
    let cond_insns = ctx.cfg.block(head).instructions.clone();

    // Claim the head first so recursive calls don't re-enter it.
    ctx.claim(head);

    // Recover branches — claim their blocks only AFTER recovery so recursive
    // calls inside recover_region can still find and process them.
    let then_stmt = if then_blocks.is_empty() {
        ctx.arena.alloc(Stmt::Exit)
    } else {
        let s = recover_branch_region(ctx, &then_blocks);
        for &bid in &then_blocks {
            ctx.claim(bid);
        }
        s
    };

    let else_stmt = if else_blocks.is_empty() {
        None
    } else {
        let s = recover_branch_region(ctx, &else_blocks);
        for &bid in &else_blocks {
            ctx.claim(bid);
        }
        Some(s)
    };

    let if_stmt = IfStmt {
        cond_block: head,
        cond_insns,
        then_branch: then_stmt,
        else_branch: else_stmt,
        // `then_branch` is the fall-through path, which runs when the branch is
        // NOT taken.  The printed condition must therefore be the negation of
        // the branch opcode's predicate: `ifle L` means the fall-through runs
        // when `x > 0`, so we print `if (x > 0)`.
        negated: true,
        post_block: post,
    };

    let stmt = ctx.arena.alloc(Stmt::If(if_stmt));
    ctx.block_stmts.insert(head, stmt);
    Some(stmt)
}

/// Find the first block both branch successors can reach.  A formal immediate
/// post-dominator can be much later when one nested path returns early.  Using
/// that late block as an if join duplicates the shared continuation into both
/// arms, exponentially for a chain of guards.  The nearest convergence keeps
/// the continuation outside the conditional while the nested early-return
/// branch remains inside its own region.
fn nearest_branch_convergence(
    ctx: &RecoveryCtx<'_>,
    branch_target: BlockId,
    fall_through: BlockId,
    all_blocks: &[BlockId],
) -> Option<BlockId> {
    let allowed = all_blocks.iter().copied().collect::<HashSet<_>>();
    let branch_distances = regular_reachable_distances(ctx, branch_target, &allowed);
    let fall_distances = regular_reachable_distances(ctx, fall_through, &allowed);
    let rpo_order = all_blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (*block, index))
        .collect::<HashMap<_, _>>();

    branch_distances
        .iter()
        .filter_map(|(block, branch_distance)| {
            let fall_distance = fall_distances.get(block)?;
            Some((
                *block,
                (*branch_distance).max(*fall_distance),
                *branch_distance + *fall_distance,
                rpo_order.get(block).copied().unwrap_or(usize::MAX),
            ))
        })
        .min_by_key(|(_, maximum, total, rpo)| (*maximum, *total, *rpo))
        .map(|(block, _, _, _)| block)
}

fn regular_reachable_distances(
    ctx: &RecoveryCtx<'_>,
    start: BlockId,
    allowed: &HashSet<BlockId>,
) -> HashMap<BlockId, usize> {
    let mut distances = HashMap::new();
    let mut queue = VecDeque::from([(start, 0usize)]);
    while let Some((block, distance)) = queue.pop_front() {
        if block == EXIT_BLOCK || !allowed.contains(&block) || distances.contains_key(&block) {
            continue;
        }
        distances.insert(block, distance);
        for successor in &ctx.cfg.block(block).succs {
            queue.push_back((*successor, distance + 1));
        }
    }
    distances
}

/// Blocks reachable from `start` following succs, stopping at `stop` or
/// blocks not in `all_blocks`.
fn reachable_before(
    ctx: &RecoveryCtx,
    start: BlockId,
    stop: BlockId,
    all_blocks: &[BlockId],
) -> HashSet<BlockId> {
    let all_set: HashSet<BlockId> = all_blocks.iter().copied().collect();
    let mut visited = HashSet::new();
    let mut stack = vec![start];
    while let Some(bid) = stack.pop() {
        if bid == stop || bid == EXIT_BLOCK || !all_set.contains(&bid) {
            continue;
        }
        if !visited.insert(bid) {
            continue;
        }
        for &s in &ctx.cfg.block(bid).succs {
            stack.push(s);
        }
    }
    visited
}

/// Blocks strictly on the path from `start` to `post` (not including `post`).
fn collect_path_blocks(
    ctx: &RecoveryCtx,
    start: BlockId,
    post: Option<BlockId>,
    all_blocks: &[BlockId],
) -> Vec<BlockId> {
    let stop = post.unwrap_or(EXIT_BLOCK);
    if start == stop || start == EXIT_BLOCK {
        return vec![];
    }

    let reachable = reachable_before(ctx, start, stop, all_blocks);
    // Return them in RPO order
    all_blocks
        .iter()
        .copied()
        .filter(|b| reachable.contains(b))
        .collect()
}

// ── Switch recovery ────────────────────────────────────────────────────────

fn try_recover_switch(
    ctx: &mut RecoveryCtx,
    head: BlockId,
    all_blocks: &[BlockId],
) -> Option<StmtId> {
    let head_block = ctx.cfg.block(head);
    let last = head_block.last_insn()?;

    let (keys, default_target) = match &last.kind {
        InsnKind::TableSwitch {
            low,
            high: _,
            offsets,
            default_offset,
        } => {
            let base = last.offset as i64;
            let default_target = (base + *default_offset as i64) as u32;
            let keys_targets: Vec<(Option<i32>, BlockId)> = offsets
                .iter()
                .enumerate()
                .map(|(i, &off)| {
                    let val = *low + i as i32;
                    let target = (base + off as i64) as u32;
                    (Some(val), offset_to_block_id(ctx, target))
                })
                .filter_map(|(v, opt)| opt.map(|b| (v, b)))
                .collect();
            (keys_targets, offset_to_block_id(ctx, default_target))
        }
        InsnKind::LookupSwitch {
            pairs,
            default_offset,
        } => {
            let base = last.offset as i64;
            let default_target = (base + *default_offset as i64) as u32;
            let keys_targets: Vec<(Option<i32>, BlockId)> = pairs
                .iter()
                .map(|&(val, off)| {
                    let target = (base + off as i64) as u32;
                    (Some(val), offset_to_block_id(ctx, target))
                })
                .filter_map(|(v, opt)| opt.map(|b| (v, b)))
                .collect();
            (keys_targets, offset_to_block_id(ctx, default_target))
        }
        _ => return None,
    };

    let post = ctx.post_dom.immediately_post_dominates(head);

    // Build case arms. Several case values may share one bytecode target, so
    // recover each target once and reuse its statement body for every label.
    let mut arms: Vec<CaseArm> = Vec::new();
    let mut recovered_targets: HashMap<BlockId, StmtId> = HashMap::new();

    // default arm
    if let Some(def_blk) = default_target {
        if def_blk != post.unwrap_or(EXIT_BLOCK) {
            let body_blocks = collect_path_blocks(ctx, def_blk, post, all_blocks);
            let body = recover_region(ctx, &body_blocks);
            for &b in &body_blocks {
                ctx.claim(b);
            }
            recovered_targets.insert(def_blk, body);
            arms.push(CaseArm {
                value: None,
                body,
                breaks: true,
            });
        }
    }

    // case arms
    for (val, case_blk) in keys {
        if Some(case_blk) == post {
            continue;
        }
        let body = if let Some(&body) = recovered_targets.get(&case_blk) {
            body
        } else {
            let body_blocks = collect_path_blocks(ctx, case_blk, post, all_blocks);
            let body = recover_region(ctx, &body_blocks);
            for &b in &body_blocks {
                ctx.claim(b);
            }
            recovered_targets.insert(case_blk, body);
            body
        };
        arms.push(CaseArm {
            value: val,
            body,
            breaks: true,
        });
    }

    // Sort arms by value for deterministic output
    arms.sort_by_key(|a| a.value);

    ctx.claim(head);

    let sw = SwitchStmt {
        switch_block: head,
        switch_insns: head_block.instructions.clone(),
        arms,
        post_block: post,
    };
    let stmt = ctx.arena.alloc(Stmt::Switch(sw));
    ctx.block_stmts.insert(head, stmt);
    Some(stmt)
}

fn offset_to_block_id(ctx: &RecoveryCtx, offset: u32) -> Option<BlockId> {
    ctx.cfg
        .real_blocks()
        .find(|b| b.start_offset == offset)
        .map(|b| b.id)
}

// ── Try-catch recovery ─────────────────────────────────────────────────────

fn try_recover_try_catch(
    ctx: &mut RecoveryCtx,
    head: BlockId,
    all_blocks: &[BlockId],
) -> Option<StmtId> {
    // Find ALL exception ranges whose start_pc matches this block.
    let head_block = ctx.cfg.block(head);
    let start_pc = head_block.start_offset;

    let ranges: Vec<_> = ctx
        .code
        .exception_table
        .iter()
        .filter(|r| r.start_pc as u32 == start_pc)
        .collect();

    if ranges.is_empty() {
        return None;
    }

    // Use the first range to establish the protected region extent.
    let end_pc = ranges[0].end_pc as u32;

    // Collect all blocks in the protected range [start_pc, end_pc)
    let try_blocks: Vec<BlockId> = all_blocks
        .iter()
        .copied()
        .filter(|&bid| {
            let b = ctx.cfg.block(bid);
            b.start_offset >= start_pc && b.start_offset < end_pc
        })
        .collect();

    if try_blocks.is_empty() {
        return None;
    }

    let handler_ids: Vec<BlockId> = ranges
        .iter()
        .filter_map(|range| offset_to_block_id(ctx, range.handler_pc as u32))
        .collect();
    let handler_join = nearest_common_handler_post_dominator(ctx, &handler_ids);

    // Claim head FIRST to stop infinite recursion: recover_region would see head,
    // call try_recover_try_catch again, and loop.  With head already claimed,
    // recover_region processes it as a plain Stmt::Block (the fallback path).
    ctx.claim(head);

    // Build the try body: head block (plain) + the inner try blocks.
    let head_block = ctx.cfg.block(head);
    let head_stmt = ctx.arena.alloc(Stmt::Block(BlockStmt {
        block_id: head,
        instructions: head_block.instructions.clone(),
        succs: head_block.succs.clone(),
    }));

    let inner_try_blocks: Vec<BlockId> =
        try_blocks.iter().copied().filter(|&b| b != head).collect();
    let inner_body = recover_region(ctx, &inner_try_blocks);

    let try_body = if matches!(ctx.arena.get(inner_body), Stmt::Exit) {
        head_stmt
    } else {
        ctx.arena.alloc(Stmt::Seq(SeqStmt {
            children: vec![head_stmt, inner_body],
        }))
    };

    // Build one CatchClause per exception handler, in declaration order.
    let mut catches: Vec<CatchClause> = Vec::new();
    let mut finally_body: Option<StmtId> = None;

    for range in &ranges {
        let handler_pc = range.handler_pc as u32;

        // Find the handler start block
        let handler_id = match ctx
            .cfg
            .real_blocks()
            .find(|b| b.start_offset == handler_pc)
            .map(|b| b.id)
        {
            Some(id) => id,
            None => continue,
        };

        // Skip if already claimed (shared handler between multiple ranges)
        if ctx.is_claimed(handler_id) {
            continue;
        }

        // Handler bytecode is not necessarily laid out before its common
        // continuation, and RPO can reverse sibling handlers.  Bound all
        // clauses at their nearest common post-dominator so the continuation
        // remains outside the final catch body.
        let handler_blocks = collect_path_blocks(ctx, handler_id, handler_join, all_blocks);

        // Claim the header up front so recover_region on the remaining blocks
        // cannot re-enter this handler, then emit the header as a plain Block
        // and sequence the rest after it.
        ctx.claim(handler_id);
        let hb = ctx.cfg.block(handler_id);
        let header_stmt = ctx.arena.alloc(Stmt::Block(BlockStmt {
            block_id: handler_id,
            instructions: hb.instructions.clone(),
            succs: hb.succs.clone(),
        }));

        let rest: Vec<BlockId> = handler_blocks
            .iter()
            .copied()
            .filter(|&b| b != handler_id)
            .collect();
        let rest_body = recover_region(ctx, &rest);

        let handler_body = if matches!(ctx.arena.get(rest_body), Stmt::Exit) {
            header_stmt
        } else {
            ctx.arena.alloc(Stmt::Seq(SeqStmt {
                children: vec![header_stmt, rest_body],
            }))
        };

        if range.catch_type.is_none() {
            finally_body = Some(handler_body);
        } else {
            catches.push(CatchClause {
                catch_type: range.catch_type.clone(),
                body: handler_body,
            });
        }
    }

    if catches.is_empty() && finally_body.is_none() {
        return None;
    }

    let tc = TryCatchStmt {
        try_body,
        catches,
        finally_body,
    };
    Some(ctx.arena.alloc(Stmt::TryCatch(tc)))
}

fn nearest_common_handler_post_dominator(
    ctx: &RecoveryCtx<'_>,
    handlers: &[BlockId],
) -> Option<BlockId> {
    let first = *handlers.first()?;
    let mut candidate = ctx.post_dom.immediately_post_dominates(first);
    while let Some(block) = candidate {
        if handlers
            .iter()
            .all(|handler| ctx.post_dom.post_dominates(block, *handler))
        {
            return Some(block);
        }
        candidate = ctx.post_dom.immediately_post_dominates(block);
    }
    None
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn is_conditional_branch(opcode: u8) -> bool {
    matches!(
        opcode,
        opc::ifeq
            | opc::ifne
            | opc::iflt
            | opc::ifge
            | opc::ifgt
            | opc::ifle
            | opc::if_icmpeq
            | opc::if_icmpne
            | opc::if_icmplt
            | opc::if_icmpge
            | opc::if_icmpgt
            | opc::if_icmple
            | opc::if_acmpeq
            | opc::if_acmpne
            | opc::ifnull
            | opc::ifnonnull
    )
}
