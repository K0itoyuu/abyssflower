/// Control Flow Graph module.
///
/// Pipeline:
///   CodeAttribute → builder::build() → Cfg → dominators::DomTree
pub mod block;
pub mod graph;
pub mod builder;
pub mod dominators;

pub use graph::Cfg;
pub use block::{BasicBlock, BlockId, ExceptionRange, ENTRY_BLOCK, EXIT_BLOCK};
pub use dominators::{DomTree, NaturalLoop, find_natural_loops};
