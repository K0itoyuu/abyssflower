/// abyssflower — high-performance Java/Kotlin bytecode decompiler.
pub mod error;
pub mod classfile;
pub mod types;
pub mod cfg;
pub mod ir;
pub mod codegen;
pub mod kotlin;
pub mod ffi;

#[cfg(feature = "mcp")]
pub mod mcp;

pub use classfile::ClassFile;
pub use error::{DecompileError, Result};
