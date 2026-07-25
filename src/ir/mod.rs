/// Structured IR — the output of Phase 4 control-flow recovery.
pub mod stmt;
pub mod recovery;
pub mod expr;
pub mod stack_sim;

pub use stmt::{Stmt, StmtId, StmtArena, LoopKind, CaseArm, CatchClause};
pub use recovery::recover;
pub use expr::{Expr, BinOp, UnOp, CastKind, InvokeKind, FieldDir, NewKind,
               ConstExpr, ConstValue, LocalVarExpr};
pub use stack_sim::{simulate_block, SlotInfo, SimResult};
