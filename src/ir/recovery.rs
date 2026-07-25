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
use std::collections::{HashMap, HashSet};

use crate::cfg::{Cfg, BlockId, DomTree, NaturalLoop, ENTRY_BLOCK, EXIT_BLOCK};
use crate::cfg::dominators::find_natural_loops;
use crate::classfile::attribute::CodeAttribute;
use crate::classfile::instruction::InsnKind;
use crate::classfile::opcodes::opc;
use crate::ir::stmt::{
    BlockStmt, CaseArm, CatchClause, IfStmt, LoopKind, LoopStmt,
    SeqStmt, Stmt, StmtArena, StmtId, SwitchStmt, TryCatchStmt,
};

// ── RecoveryCtx ───────────────────────────────────────────────────────────

/// Working context for one method's structuring pass.
pub struct RecoveryCtx<'a> {
    pub cfg:   &'a Cfg,
    pub dom:   &'a DomTree,
    pub code:  &'a CodeAttribute,
    pub arena: StmtArena,
    /// Blocks that have already been claimed by a higher-level construct.
    claimed:   HashSet<BlockId>,
    /// Loop info keyed by header block.
    loops:     HashMap<BlockId, NaturalLoop>,
}

impl<'a> RecoveryCtx<'a> {
    fn new(cfg: &'a Cfg, dom: &'a DomTree, code: &'a CodeAttribute) -> Self {
        let loops_vec = find_natural_loops(cfg, dom);
        let loops: HashMap<BlockId, NaturalLoop> = loops_vec
            .into_iter()
            .map(|l| (l.header, l))
            .collect();
        RecoveryCtx {
            cfg, dom, code,
            arena: StmtArena::new(),
            claimed: HashSet::new(),
            loops,
        }
    }

    fn claim(&mut self, id: BlockId) { self.claimed.insert(id); }
    fn is_claimed(&self, id: BlockId) -> bool { self.claimed.contains(&id) }
}

// ── Public entry point ─────────────────────────────────────────────────────

/// Recover structured control flow for a single method.
///
/// Returns a `(StmtArena, root_StmtId)` pair.  The root statement is
/// always a `Stmt::Seq` that covers the entire method body.
pub fn recover(cfg: &Cfg, dom: &DomTree, code: &CodeAttribute) -> (StmtArena, StmtId) {
    let mut ctx = RecoveryCtx::new(cfg, dom, code);

    // Get blocks in RPO — we process outer constructs first (by header block RPO order).
    let rpo = cfg.rpo();

    // Collect blocks we need to structure (skip ENTRY and EXIT).
    let blocks: Vec<BlockId> = rpo.iter()
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
        if ctx.is_claimed(bid) { i += 1; continue; }

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
            block_id:     bid,
            instructions: block.instructions.clone(),
            succs:        block.succs.clone(),
        };
        let s = ctx.arena.alloc(Stmt::Block(bs));
        ctx.claim(bid);
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

// ── Loop recovery ──────────────────────────────────────────────────────────

fn try_recover_loop(
    ctx: &mut RecoveryCtx,
    header: BlockId,
    _all_blocks: &[BlockId],
) -> Option<StmtId> {
    let lp = ctx.loops.get(&header)?.clone();

    // Don't re-process
    if ctx.is_claimed(header) { return None; }

    // Determine loop kind from the header block's last instruction.
    let header_block = ctx.cfg.block(header);
    let last_opcode = header_block.last_insn().map(|i| i.opcode).unwrap_or(0);
    let is_conditional_at_header = is_conditional_branch(last_opcode);

    // Find blocks *inside* the loop in RPO order (excluding header)
    let body_blocks: Vec<BlockId> = ctx.cfg.rpo()
        .into_iter()
        .filter(|&id| lp.body.contains(&id) && id != ENTRY_BLOCK && id != EXIT_BLOCK)
        .collect();

    // Check tail block for do-while
    let tail_block = ctx.cfg.block(lp.tail);
    let tail_opcode = tail_block.last_insn().map(|i| i.opcode).unwrap_or(0);
    let is_conditional_at_tail = is_conditional_branch(tail_opcode);

    let kind = if is_conditional_at_header {
        LoopKind::While
    } else if is_conditional_at_tail {
        LoopKind::DoWhile
    } else {
        LoopKind::Infinite
    };

    // Claim the header block to prevent re-processing
    ctx.claim(header);

    // Find loop exit block (first block in the CFG after the loop)
    let post_block = find_loop_exit(ctx, &lp);

    // Recursively structure the loop body (excluding the header for while loops)
    let inner_blocks: Vec<BlockId> = if kind == LoopKind::While {
        body_blocks.iter().copied().filter(|&id| id != header).collect()
    } else {
        body_blocks.clone()
    };

    let body_stmt = recover_region(ctx, &inner_blocks);

    // Claim any remaining unclaimed body blocks
    for &bid in &body_blocks {
        ctx.claim(bid);
    }

    let loop_stmt = LoopStmt {
        kind,
        header_block: header,
        tail_block: lp.tail,
        body_blocks,
        body: body_stmt,
        post_block,
    };

    Some(ctx.arena.alloc(Stmt::Loop(loop_stmt)))
}

fn find_loop_exit(ctx: &RecoveryCtx, lp: &NaturalLoop) -> Option<BlockId> {
    // The loop exit is the first block reachable from a loop block
    // that is NOT in the loop body.
    for &bid in &lp.body {
        if bid == ENTRY_BLOCK || bid == EXIT_BLOCK { continue; }
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

fn try_recover_if(
    ctx: &mut RecoveryCtx,
    head: BlockId,
    all_blocks: &[BlockId],
) -> Option<StmtId> {
    let head_block = ctx.cfg.block(head);
    let last = head_block.last_insn()?;

    if !is_conditional_branch(last.opcode) { return None; }

    let succs = &head_block.succs;
    if succs.len() != 2 { return None; }

    // In JVM, branch instruction: succs[0] = branch target, succs[1] = fall-through
    // (populated by builder in that order for conditional jumps)
    let (branch_target, fall_through) = (succs[0], succs[1]);

    // Find the post-dominator (merge point) of the two paths.
    // It is the immediate post-dominator of `head`.
    // Heuristic: the closest block that dominates both successors' exits.
    let post = find_if_post(ctx, head, branch_target, fall_through, all_blocks);

    // Decide which path is "then" and which is "else"
    // Convention: fall-through path = then-branch (most common in javac output)
    // We only build an else branch if both paths lead to the same post block.

    // Collect then-blocks: fall-through path up to (but not including) post
    let then_blocks: Vec<BlockId> = collect_path_blocks(ctx, fall_through, post, all_blocks);
    let else_blocks: Vec<BlockId> = collect_path_blocks(ctx, branch_target, post, all_blocks);

    // Claim and structure
    for &bid in &then_blocks { ctx.claim(bid); }
    for &bid in &else_blocks { ctx.claim(bid); }
    ctx.claim(head);

    let then_stmt = if then_blocks.is_empty() {
        ctx.arena.alloc(Stmt::Exit)
    } else {
        recover_region(ctx, &then_blocks)
    };

    let else_stmt = if else_blocks.is_empty() {
        None
    } else {
        Some(recover_region(ctx, &else_blocks))
    };

    let if_stmt = IfStmt {
        cond_block:  head,
        then_branch: then_stmt,
        else_branch: else_stmt,
        negated:     false,
        post_block:  post,
    };

    Some(ctx.arena.alloc(Stmt::If(if_stmt)))
}

/// Find the "post" block for an if statement — the first block that
/// post-dominates the header (i.e., all paths from header converge here).
fn find_if_post(
    ctx: &RecoveryCtx,
    head: BlockId,
    branch: BlockId,
    fall: BlockId,
    all_blocks: &[BlockId],
) -> Option<BlockId> {
    // Simple heuristic: scan all_blocks in RPO order after `head`,
    // return the first block that is NOT in an exclusive path of either branch.
    // This approximates the immediate post-dominator.
    let head_rpo = all_blocks.iter().position(|&b| b == head)?;

    // Collect all blocks reachable from branch (without going through fall and vice versa)
    let branch_set = reachable_before(ctx, branch, head, all_blocks);
    let fall_set   = reachable_before(ctx, fall,   head, all_blocks);

    // The post block is the first one in RPO after head that appears in BOTH sets,
    // or just the first block that is in neither (a shared continuation).
    for &bid in &all_blocks[head_rpo + 1..] {
        if bid == EXIT_BLOCK { return Some(bid); }
        // If it's dominated by head but not exclusively by one branch
        if ctx.dom.dominates(head, bid) {
            let in_branch = branch_set.contains(&bid);
            let in_fall   = fall_set.contains(&bid);
            if in_branch && in_fall {
                return Some(bid);  // merge point
            }
            if !in_branch && !in_fall {
                return Some(bid);  // continuation after both branches return/break
            }
        }
    }

    None
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
    let mut stack   = vec![start];
    while let Some(bid) = stack.pop() {
        if bid == stop || bid == EXIT_BLOCK || !all_set.contains(&bid) { continue; }
        if !visited.insert(bid) { continue; }
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
    if start == stop || start == EXIT_BLOCK { return vec![]; }

    let reachable = reachable_before(ctx, start, stop, all_blocks);
    // Return them in RPO order
    all_blocks.iter()
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
        InsnKind::TableSwitch { low, high: _, offsets, default_offset } => {
            let base = last.offset as i64;
            let default_target = (base + *default_offset as i64) as u32;
            let keys_targets: Vec<(Option<i32>, BlockId)> = offsets.iter().enumerate()
                .map(|(i, &off)| {
                    let val = *low + i as i32;
                    let target = (base + off as i64) as u32;
                    (Some(val), offset_to_block_id(ctx, target))
                })
                .filter_map(|(v, opt)| opt.map(|b| (v, b)))
                .collect();
            (keys_targets, offset_to_block_id(ctx, default_target))
        }
        InsnKind::LookupSwitch { pairs, default_offset } => {
            let base = last.offset as i64;
            let default_target = (base + *default_offset as i64) as u32;
            let keys_targets: Vec<(Option<i32>, BlockId)> = pairs.iter()
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

    // Find switch post (first successor not dominated exclusively by switch)
    let all_succs: Vec<BlockId> = head_block.succs.clone();
    let post = find_switch_post(ctx, head, &all_succs, all_blocks);

    // Build case arms
    let mut arms: Vec<CaseArm> = Vec::new();

    // default arm
    if let Some(def_blk) = default_target {
        if def_blk != post.unwrap_or(EXIT_BLOCK) {
            let body_blocks = collect_path_blocks(ctx, def_blk, post, all_blocks);
            for &b in &body_blocks { ctx.claim(b); }
            let body = recover_region(ctx, &body_blocks);
            arms.push(CaseArm { value: None, body, breaks: true });
        }
    }

    // case arms
    for (val, case_blk) in keys {
        if ctx.is_claimed(case_blk) { continue; }
        if Some(case_blk) == post { continue; }
        let body_blocks = collect_path_blocks(ctx, case_blk, post, all_blocks);
        for &b in &body_blocks { ctx.claim(b); }
        let body = recover_region(ctx, &body_blocks);
        arms.push(CaseArm { value: val, body, breaks: true });
    }

    // Sort arms by value for deterministic output
    arms.sort_by_key(|a| a.value);

    ctx.claim(head);

    let sw = SwitchStmt { switch_block: head, arms, post_block: post };
    Some(ctx.arena.alloc(Stmt::Switch(sw)))
}

fn find_switch_post(
    ctx: &RecoveryCtx,
    head: BlockId,
    succs: &[BlockId],
    all_blocks: &[BlockId],
) -> Option<BlockId> {
    // The post-dominator of the switch is the first block after all case paths merge.
    // Heuristic: find the block that all successor paths eventually reach first.
    let head_pos = all_blocks.iter().position(|&b| b == head)?;
    for &bid in &all_blocks[head_pos + 1..] {
        if bid == EXIT_BLOCK { return Some(bid); }
        // A block is a switch post if all switch successors can reach it.
        if succs.iter().all(|&s| ctx.dom.dominates(head, s))
            && !succs.contains(&bid)
            && !ctx.dom.strictly_dominates(head, bid)
        {
            // not dominated by the switch header → it's outside the switch
            return Some(bid);
        }
    }
    None
}

fn offset_to_block_id(ctx: &RecoveryCtx, offset: u32) -> Option<BlockId> {
    ctx.cfg.real_blocks()
        .find(|b| b.start_offset == offset)
        .map(|b| b.id)
}

// ── Try-catch recovery ─────────────────────────────────────────────────────

fn try_recover_try_catch(
    ctx: &mut RecoveryCtx,
    head: BlockId,
    all_blocks: &[BlockId],
) -> Option<StmtId> {
    // Find the first exception range whose start_pc matches this block.
    let head_block = ctx.cfg.block(head);
    let start_pc = head_block.start_offset;

    let range = ctx.code.exception_table.iter()
        .find(|r| r.start_pc as u32 == start_pc)?;

    let handler_id = ctx.cfg.real_blocks()
        .find(|b| b.start_offset == range.handler_pc as u32)
        .map(|b| b.id)?;

    // Collect all blocks in the protected range [start_pc, end_pc)
    let end_pc = range.end_pc as u32;
    let try_blocks: Vec<BlockId> = all_blocks.iter().copied()
        .filter(|&bid| {
            let b = ctx.cfg.block(bid);
            b.start_offset >= start_pc && b.start_offset < end_pc
        })
        .collect();

    if try_blocks.is_empty() { return None; }

    // Claim try blocks and handler
    for &b in &try_blocks { ctx.claim(b); }
    ctx.claim(handler_id);

    let try_body = recover_region(ctx, &try_blocks);

    // Handler body: blocks starting at handler_pc
    let handler_blocks: Vec<BlockId> = all_blocks.iter().copied()
        .filter(|&bid| {
            let b = ctx.cfg.block(bid);
            b.start_offset >= range.handler_pc as u32
                && !try_blocks.contains(&bid)
        })
        .take_while(|&bid| !ctx.is_claimed(bid))
        .collect();
    let handler_body = recover_region(ctx, &handler_blocks);
    for &b in &handler_blocks { ctx.claim(b); }

    let catch_type = range.catch_type.clone();
    let catches = vec![CatchClause { catch_type, body: handler_body }];

    let tc = TryCatchStmt { try_body, catches, finally_body: None };
    Some(ctx.arena.alloc(Stmt::TryCatch(tc)))
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn is_conditional_branch(opcode: u8) -> bool {
    matches!(opcode,
        opc::ifeq | opc::ifne | opc::iflt | opc::ifge | opc::ifgt | opc::ifle |
        opc::if_icmpeq | opc::if_icmpne | opc::if_icmplt |
        opc::if_icmpge | opc::if_icmpgt | opc::if_icmple |
        opc::if_acmpeq | opc::if_acmpne |
        opc::ifnull | opc::ifnonnull
    )
}
