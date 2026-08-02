/// Control Flow Graph module.
///
/// Pipeline:
///   CodeAttribute → builder::build() → Cfg → dominators::DomTree
pub mod block;
pub mod builder;
pub mod dominators;
pub mod graph;

pub use block::{BasicBlock, BlockId, ExceptionRange, ENTRY_BLOCK, EXIT_BLOCK};
pub use dominators::{find_natural_loops, DomTree, NaturalLoop, PostDomTree};
pub use graph::Cfg;
