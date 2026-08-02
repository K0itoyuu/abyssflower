/// Structured control-flow IR.
///
/// After CFG recovery, each method body is represented as a tree of `Stmt`
/// nodes. This IR sits between the raw CFG (Phase 3) and the code generator
/// (Phase 6): it captures loop/branch/exception nesting without caring about
/// individual expressions (those are handled in Phase 5).
use crate::cfg::block::BlockId;
use crate::classfile::instruction::Instruction;

// ── StmtId ─────────────────────────────────────────────────────────────────

/// Arena index for a `Stmt`.  Uses `u32` to keep the struct compact.
pub type StmtId = u32;

// ── LoopKind ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopKind {
    /// `while (cond) { … }` — condition at loop header, tested before body.
    While,
    /// `do { … } while (cond)` — condition at loop tail, tested after body.
    DoWhile,
    /// `for (init; cond; update) { … }` — synthesized later.
    For,
    /// `while (true) { … }` — no condition found; break/return exits.
    Infinite,
}

// ── CaseArm ───────────────────────────────────────────────────────────────

/// One arm of a `switch` statement.
#[derive(Debug, Clone)]
pub struct CaseArm {
    /// `None` = `default:`, `Some(v)` = `case v:`.
    pub value: Option<i32>,
    /// The body of this arm (may fall through to the next).
    pub body: StmtId,
    /// Whether this arm has an explicit `break` at the end.
    pub breaks: bool,
}

// ── CatchClause ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CatchClause {
    /// Caught exception type, or `None` for catch-all / finally.
    pub catch_type: Option<String>,
    /// Handler body.
    pub body: StmtId,
}

// ── Stmt ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Stmt {
    /// A single basic block of raw bytecode instructions.
    Block(BlockStmt),

    /// An ordered sequence of child statements.
    Seq(SeqStmt),

    /// Conditional branch.
    If(IfStmt),

    /// Loop (while / do-while / infinite).
    Loop(LoopStmt),

    /// Conditional edge that exits the innermost natural loop.
    BreakIf(BreakIfStmt),

    /// switch / tableswitch / lookupswitch.
    Switch(SwitchStmt),

    /// try { … } catch (E) { … } [finally { … }]
    TryCatch(TryCatchStmt),

    /// synchronized (expr) { … }
    Synchronized(SyncStmt),

    /// Synthetic exit placeholder (maps to the EXIT block).
    Exit,
}

// ── BlockStmt ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BlockStmt {
    pub block_id: BlockId,
    pub instructions: Vec<Instruction>,
    /// Successor hint: resolved after structuring.
    pub succs: Vec<BlockId>,
}

// ── SeqStmt ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SeqStmt {
    pub children: Vec<StmtId>,
}

// ── IfStmt ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IfStmt {
    /// The block containing the conditional branch instruction.
    pub cond_block: BlockId,
    /// The instructions of the condition block — stored here so the writer
    /// doesn't have to search the entire method for the right branch.
    pub cond_insns: Vec<Instruction>,
    /// True-branch body (`None` if the condition was negated and only the
    /// fall-through path exists).
    pub then_branch: StmtId,
    /// Optional false-branch (`else { … }`).
    pub else_branch: Option<StmtId>,
    /// Whether the condition was logically inverted during structuring.
    pub negated: bool,
    /// The "post" block — first block after the if/else merges.
    pub post_block: Option<BlockId>,
}

// ── LoopStmt ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LoopStmt {
    pub kind: LoopKind,
    /// The loop header block id (target of the back-edge).
    pub header_block: BlockId,
    /// The back-edge tail block id.
    pub tail_block: BlockId,
    /// All block ids that belong to the loop body.
    pub body_blocks: Vec<BlockId>,
    /// The loop body as a structured statement.
    pub body: StmtId,
    /// Block id immediately after the loop (the loop exit).
    pub post_block: Option<BlockId>,
    /// Instructions of the condition block (header for while, tail for do-while).
    /// Stored here so the writer can simulate exactly the right instructions.
    pub cond_insns: Vec<Instruction>,
    /// True when the printed condition is the negation of the branch opcode's
    /// predicate — i.e. the conditional branch *leaves* the loop and control
    /// stays in the loop on fall-through.  False when the branch is a back-edge
    /// that continues the loop.
    pub cond_negated: bool,
}

#[derive(Debug, Clone)]
pub struct BreakIfStmt {
    pub cond_block: BlockId,
    pub cond_insns: Vec<Instruction>,
    /// True when the fall-through edge exits and the branch predicate must be inverted.
    pub negated: bool,
}

// ── SwitchStmt ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SwitchStmt {
    /// Block containing the tableswitch / lookupswitch instruction.
    pub switch_block: BlockId,
    pub switch_insns: Vec<Instruction>,
    pub arms: Vec<CaseArm>,
    /// Block after the switch (post-dominator).
    pub post_block: Option<BlockId>,
}

// ── TryCatchStmt ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TryCatchStmt {
    /// The protected body.
    pub try_body: StmtId,
    /// The catch / finally clauses.
    pub catches: Vec<CatchClause>,
    /// Optional finally body (always-runs block).
    pub finally_body: Option<StmtId>,
}

// ── SyncStmt ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SyncStmt {
    pub monitor_block: BlockId,
    pub body: StmtId,
}

// ── StmtArena ────────────────────────────────────────────────────────────

/// Bump-style arena for `Stmt` nodes.
///
/// All statements in a method are owned here; parent nodes reference
/// children by `StmtId` index.
#[derive(Debug, Default)]
pub struct StmtArena {
    stmts: Vec<Stmt>,
}

impl StmtArena {
    pub fn new() -> Self {
        StmtArena { stmts: Vec::new() }
    }

    pub fn alloc(&mut self, stmt: Stmt) -> StmtId {
        let id = self.stmts.len() as StmtId;
        self.stmts.push(stmt);
        id
    }

    pub fn get(&self, id: StmtId) -> &Stmt {
        &self.stmts[id as usize]
    }

    pub fn get_mut(&mut self, id: StmtId) -> &mut Stmt {
        &mut self.stmts[id as usize]
    }

    pub fn len(&self) -> usize {
        self.stmts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stmts.is_empty()
    }
}
