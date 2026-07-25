/// Control Flow Graph builder.
///
/// Algorithm (mirrors Vineflower's ControlFlowGraph.buildBlocks):
///   1. Scan instructions to find all block-leader offsets.
///   2. Slice the instruction list into BasicBlocks.
///   3. Connect blocks with regular edges (fall-through + branch).
///   4. Add exception edges from the Code attribute's exception table.
use std::collections::HashMap;

use crate::classfile::attribute::{CodeAttribute, ExceptionHandler};
use crate::classfile::instruction::Instruction;
use crate::classfile::opcodes::opc;
use crate::cfg::block::{BasicBlock, BlockId, ExceptionRange, ENTRY_BLOCK, EXIT_BLOCK};
use crate::cfg::Cfg;

// ── public entry point ─────────────────────────────────────────────────────

/// Build a `Cfg` from a parsed `CodeAttribute`.
///
/// The graph always has two synthetic blocks:
///  - `ENTRY_BLOCK` (id = 0): no instructions, single successor = first real block.
///  - `EXIT_BLOCK`  (id = u32::MAX): no instructions, collects returns/throws.
pub fn build(code: &CodeAttribute) -> Cfg {
    let insns = &code.instructions;
    if insns.is_empty() {
        return Cfg::empty();
    }

    // ── step 1: find all block leaders ──────────────────────────────────
    let leaders = find_leaders(insns, &code.exception_table);

    // ── step 2: slice into blocks ───────────────────────────────────────
    // Map: first-instruction index → BlockId (used for edge construction)
    let mut index_to_block: HashMap<usize, BlockId> = HashMap::new();

    let mut blocks: Vec<BasicBlock> = Vec::new();
    let mut next_id: BlockId = 1; // 0 = ENTRY; incremented for each new real block

    // Add ENTRY synthetic block
    blocks.push(BasicBlock::synthetic(ENTRY_BLOCK));

    let mut slice_start = 0usize;
    let mut i = 0usize;
    while i <= insns.len() {
        let is_boundary = i == insns.len()
            || (i > 0 && leaders.contains(&insns[i].offset));

        if is_boundary && i > slice_start {
            let block_insns: Vec<Instruction> = insns[slice_start..i].to_vec();
            let block_id = next_id;
            next_id += 1;
            index_to_block.insert(slice_start, block_id);
            blocks.push(BasicBlock::new(block_id, block_insns));
            slice_start = i;
        }

        if i < insns.len() && leaders.contains(&insns[i].offset) && i == slice_start && i > 0 {
            // Already handled above; just advance
        }
        i += 1;
    }
    // Handle the last slice if not yet added
    if slice_start < insns.len() {
        let block_insns: Vec<Instruction> = insns[slice_start..].to_vec();
        let block_id = next_id;
        // next_id is not incremented further — no more blocks are created after this
        index_to_block.insert(slice_start, block_id);
        blocks.push(BasicBlock::new(block_id, block_insns));
    }

    // Add EXIT synthetic block
    // We reserve u32::MAX but store it at a known Vec index for lookup.
    let _exit_vec_index = blocks.len();
    blocks.push(BasicBlock::synthetic(EXIT_BLOCK));

    // ── helper: offset → BlockId ─────────────────────────────────────────
    // Build offset → block-id map for fast lookup during edge construction.
    let offset_to_block_id: HashMap<u32, BlockId> = blocks.iter()
        .filter(|b| !b.is_synthetic())
        .map(|b| (b.start_offset, b.id))
        .collect();

    // ── step 3: ENTRY → first real block ─────────────────────────────────
    if let Some(&first_id) = offset_to_block_id.get(&insns[0].offset) {
        add_edge(&mut blocks, ENTRY_BLOCK, first_id);
    }

    // ── step 4: regular edges ────────────────────────────────────────────
    // We need a snapshot of block ids to avoid borrow issues
    let block_ids_and_last: Vec<(BlockId, Option<Instruction>)> = blocks.iter()
        .filter(|b| !b.is_synthetic())
        .map(|b| (b.id, b.last_insn().cloned()))
        .collect();

    for (blk_id, last_opt) in &block_ids_and_last {
        let last = match last_opt { Some(i) => i, None => continue };
        let blk_id = *blk_id;

        // Branch targets
        let targets = last.kind.branch_targets(last.offset, last.opcode);
        for target in &targets {
            if let Some(&succ_id) = offset_to_block_id.get(target) {
                add_edge(&mut blocks, blk_id, succ_id);
            }
        }

        // Fall-through
        if last.can_fall_through() {
            let fall_offset = last.offset + last.kind.encoded_length(last.opcode, last.offset) as u32;
            if let Some(&succ_id) = offset_to_block_id.get(&fall_offset) {
                add_edge(&mut blocks, blk_id, succ_id);
            }
        }

        // Returns / throws → EXIT
        if is_exit_opcode(last.opcode) {
            add_edge(&mut blocks, blk_id, EXIT_BLOCK);
        }
    }

    // ── step 5: exception edges ──────────────────────────────────────────
    let mut exception_ranges: Vec<ExceptionRange> = Vec::new();

    for handler in &code.exception_table {
        let start = handler.start_pc as u32;
        let end   = handler.end_pc   as u32;

        let handler_id = match offset_to_block_id.get(&(handler.handler_pc as u32)) {
            Some(&id) => id,
            None => continue,
        };

        // Add exception edges from every block whose bytecode range
        // intersects [start_pc, end_pc).
        for blk in blocks.iter_mut().filter(|b| !b.is_synthetic()) {
            if blk.start_offset >= start && blk.start_offset < end {
                if !blk.succ_exceptions.contains(&handler_id) {
                    blk.succ_exceptions.push(handler_id);
                }
            }
        }
        // Add pred_exception edges to handler
        for blk in blocks.iter_mut() {
            if blk.id == handler_id {
                // Will be fixed in a second pass below
                break;
            }
        }

        exception_ranges.push(ExceptionRange {
            start_pc:   start,
            end_pc:     end,
            handler:    handler_id,
            catch_type: handler.catch_type.clone(),
        });
    }

    // Second pass: fill pred_exceptions
    // Collect all (protected_block_id, handler_id) pairs first
    let exc_edges: Vec<(BlockId, BlockId)> = blocks.iter()
        .filter(|b| !b.is_synthetic())
        .flat_map(|b| b.succ_exceptions.iter().map(move |&h| (b.id, h)))
        .collect();
    for (from_id, handler_id) in exc_edges {
        if let Some(handler_block) = blocks.iter_mut().find(|b| b.id == handler_id) {
            if !handler_block.pred_exceptions.contains(&from_id) {
                handler_block.pred_exceptions.push(from_id);
            }
        }
    }

    Cfg { blocks, exception_ranges }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Collect the set of bytecode offsets that start a new basic block.
fn find_leaders(insns: &[Instruction], exc_table: &[ExceptionHandler]) -> std::collections::HashSet<u32> {
    let mut leaders = std::collections::HashSet::new();

    // First instruction is always a leader
    if let Some(first) = insns.first() {
        leaders.insert(first.offset);
    }

    // Exception handler start, guarded range start and end
    for handler in exc_table {
        leaders.insert(handler.start_pc as u32);
        leaders.insert(handler.end_pc   as u32);
        leaders.insert(handler.handler_pc as u32);
    }

    // Build offset set for quick "offset is valid" checks
    let valid_offsets: std::collections::HashSet<u32> = insns.iter()
        .map(|i| i.offset)
        .collect();

    for insn in insns {
        let targets = insn.kind.branch_targets(insn.offset, insn.opcode);
        for t in targets {
            leaders.insert(t);
        }

        // Instruction after a branch / return / throw is a new leader
        let falls = insn.can_fall_through();
        let is_branch = !insn.kind.branch_targets(insn.offset, insn.opcode).is_empty();
        if is_branch || !falls {
            let next = insn.offset + insn.kind.encoded_length(insn.opcode, insn.offset) as u32;
            if valid_offsets.contains(&next) {
                leaders.insert(next);
            }
        }
    }

    leaders
}

/// Add a directed edge from `from` to `to`, updating both pred and succ lists.
fn add_edge(blocks: &mut Vec<BasicBlock>, from: BlockId, to: BlockId) {
    // Avoid duplicate edges
    let already = blocks.iter()
        .find(|b| b.id == from)
        .map(|b| b.succs.contains(&to))
        .unwrap_or(false);
    if already { return; }

    // Add to → preds
    if let Some(to_block) = blocks.iter_mut().find(|b| b.id == to) {
        if !to_block.preds.contains(&from) {
            to_block.preds.push(from);
        }
    }
    // Add from → succs
    if let Some(from_block) = blocks.iter_mut().find(|b| b.id == from) {
        if !from_block.succs.contains(&to) {
            from_block.succs.push(to);
        }
    }
}

fn is_exit_opcode(op: u8) -> bool {
    matches!(op,
        opc::ireturn | opc::lreturn | opc::freturn |
        opc::dreturn | opc::areturn | opc::r#return |
        opc::athrow
    )
}
