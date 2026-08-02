pub mod expr;
pub mod recovery;
pub mod stack_sim;
/// Structured IR — the output of Phase 4 control-flow recovery.
pub mod stmt;

pub use expr::{
    BinOp, CastKind, ConstExpr, ConstValue, Expr, FieldDir, InvokeKind, LambdaBootstrap,
    LocalVarExpr, NewKind, TernaryCondition, UnOp,
};
pub use recovery::{recover, recover_with_branch_convergence};
pub use stack_sim::{
    simulate_block, simulate_block_with_context, LocalScope, SimResult, SimulationContext,
    SimulationError, SimulationErrorKind, SlotInfo,
};
pub use stmt::{CaseArm, CatchClause, LoopKind, Stmt, StmtArena, StmtId};
