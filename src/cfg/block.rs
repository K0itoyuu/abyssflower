/// A basic block in the control flow graph.
///
/// All edges are stored as `BlockId` indices into `Cfg::blocks`.
/// The graph uses index-based representation (no `Rc`/`Box` cycles)
/// for safety, cache friendliness, and easy serialization.
use crate::classfile::instruction::Instruction;

/// Opaque handle to a basic block.  `0` is reserved for the synthetic
/// *entry* block; `u32::MAX` is reserved for the synthetic *exit* block.
pub type BlockId = u32;

pub const ENTRY_BLOCK: BlockId = 0;
pub const EXIT_BLOCK:  BlockId = u32::MAX;

// ── ExceptionEdge ─────────────────────────────────────────────────────────

/// A single exception range: blocks `[start, end)` by bytecode offset
/// propagate to `handler` on exception.
#[derive(Debug, Clone)]
pub struct ExceptionRange {
    /// Inclusive start bytecode offset of the guarded region.
    pub start_pc:     u32,
    /// Exclusive end bytecode offset.
    pub end_pc:       u32,
    /// Block that handles the exception.
    pub handler:      BlockId,
    /// Caught type, `None` = catch-all (finally).
    pub catch_type:   Option<String>,
}

// ── BasicBlock ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,

    /// Instructions in program order.  Empty for the synthetic entry/exit blocks.
    pub instructions: Vec<Instruction>,

    /// Bytecode offset of the first instruction (or 0 for entry/exit).
    pub start_offset: u32,
    /// Bytecode offset *past the end* of the last instruction.
    pub end_offset:   u32,

    /// Regular predecessor blocks (fall-through or jump).
    pub preds:      Vec<BlockId>,
    /// Regular successor blocks.
    pub succs:      Vec<BlockId>,

    /// Predecessor blocks that reach this one via an exception edge.
    pub pred_exceptions: Vec<BlockId>,
    /// Successor handler blocks reachable via exception from this block.
    pub succ_exceptions: Vec<BlockId>,
}

impl BasicBlock {
    /// Create a normal (non-synthetic) block with no edges yet.
    pub fn new(id: BlockId, instructions: Vec<Instruction>) -> Self {
        let start_offset = instructions.first().map(|i| i.offset).unwrap_or(0);
        let last = instructions.last();
        let end_offset = last.map(|i| i.offset + i.kind.encoded_length(i.opcode, i.offset) as u32)
                             .unwrap_or(0);
        BasicBlock {
            id,
            instructions,
            start_offset,
            end_offset,
            preds:            Vec::new(),
            succs:            Vec::new(),
            pred_exceptions:  Vec::new(),
            succ_exceptions:  Vec::new(),
        }
    }

    /// Synthetic entry or exit block.
    pub fn synthetic(id: BlockId) -> Self {
        BasicBlock {
            id,
            instructions:     Vec::new(),
            start_offset:     0,
            end_offset:       0,
            preds:            Vec::new(),
            succs:            Vec::new(),
            pred_exceptions:  Vec::new(),
            succ_exceptions:  Vec::new(),
        }
    }

    /// Last instruction, if any.
    pub fn last_insn(&self) -> Option<&Instruction> {
        self.instructions.last()
    }

    /// True for the synthetic entry/exit blocks.
    pub fn is_synthetic(&self) -> bool {
        self.id == ENTRY_BLOCK || self.id == EXIT_BLOCK
    }
}
