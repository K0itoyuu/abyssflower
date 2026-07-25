/// Code generation module — Phase 6.
/// Renders Expr/Stmt IR trees to Java source text.
pub mod expr_writer;
pub mod stmt_writer;
pub mod class_writer;

pub use class_writer::render_class;
pub use expr_writer::{render_expr, IndentWriter};
pub use stmt_writer::render_method_body;
