/// Control Flow Graph — the central data structure for analysis.
///
/// `blocks[0]` is always the synthetic ENTRY block.
/// The EXIT block (id = `EXIT_BLOCK`) is always the last element.
use crate::cfg::block::{BasicBlock, BlockId, ExceptionRange, ENTRY_BLOCK, EXIT_BLOCK};

#[derive(Debug)]
pub struct Cfg {
    /// All blocks in build order.  ENTRY = index 0, EXIT = last.
    pub blocks:           Vec<BasicBlock>,
    /// Exception ranges attached to this method.
    pub exception_ranges: Vec<ExceptionRange>,
}

impl Cfg {
    /// An empty CFG with only ENTRY and EXIT blocks.
    pub fn empty() -> Self {
        Cfg {
            blocks: vec![
                BasicBlock::synthetic(ENTRY_BLOCK),
                BasicBlock::synthetic(EXIT_BLOCK),
            ],
            exception_ranges: Vec::new(),
        }
    }

    // ── block lookup ───────────────────────────────────────────────────

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        self.blocks.iter().find(|b| b.id == id)
            .expect("BlockId not found in CFG")
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        self.blocks.iter_mut().find(|b| b.id == id)
            .expect("BlockId not found in CFG")
    }

    pub fn entry(&self) -> &BasicBlock { self.block(ENTRY_BLOCK) }
    pub fn exit(&self)  -> &BasicBlock { self.block(EXIT_BLOCK)  }

    /// All non-synthetic (real) blocks.
    pub fn real_blocks(&self) -> impl Iterator<Item = &BasicBlock> {
        self.blocks.iter().filter(|b| !b.is_synthetic())
    }

    /// Total number of blocks including synthetic ones.
    pub fn len(&self) -> usize { self.blocks.len() }

    // ── graph traversal ────────────────────────────────────────────────

    /// Reverse post-order traversal starting from ENTRY.
    /// This is the standard order for dataflow analyses.
    pub fn rpo(&self) -> Vec<BlockId> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self.dfs_post(ENTRY_BLOCK, &mut visited, &mut result);
        result.reverse();
        result
    }

    fn dfs_post(
        &self,
        id: BlockId,
        visited: &mut std::collections::HashSet<BlockId>,
        out: &mut Vec<BlockId>,
    ) {
        if !visited.insert(id) { return; }
        let block = self.block(id);
        // Visit regular successors then exception successors
        for &succ in &block.succs {
            self.dfs_post(succ, visited, out);
        }
        for &succ in &block.succ_exceptions {
            self.dfs_post(succ, visited, out);
        }
        out.push(id);
    }

    // ── statistics ─────────────────────────────────────────────────────

    pub fn edge_count(&self) -> usize {
        self.blocks.iter()
            .map(|b| b.succs.len() + b.succ_exceptions.len())
            .sum()
    }

    pub fn instruction_count(&self) -> usize {
        self.blocks.iter().map(|b| b.instructions.len()).sum()
    }

    // ── debug display ──────────────────────────────────────────────────

    pub fn dump(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            if block.id == ENTRY_BLOCK {
                out.push_str("Block ENTRY\n");
            } else if block.id == EXIT_BLOCK {
                out.push_str("Block EXIT\n");
            } else {
                out.push_str(&format!(
                    "Block {} [pc {:#x}..{:#x}] ({} insns)\n",
                    block.id, block.start_offset, block.end_offset,
                    block.instructions.len()
                ));
                for insn in &block.instructions {
                    out.push_str(&format!(
                        "    {:#06x}  {}\n",
                        insn.offset,
                        crate::classfile::opcodes::opc::name(insn.opcode)
                    ));
                }
            }
            if !block.succs.is_empty() {
                out.push_str(&format!("  → succs:  {:?}\n", block.succs));
            }
            if !block.succ_exceptions.is_empty() {
                out.push_str(&format!("  → except: {:?}\n", block.succ_exceptions));
            }
        }
        out
    }
}
