pub mod cfg;
pub mod classfile;
pub mod codegen;
pub mod decompiler;
/// abyssflower — high-performance Java/Kotlin bytecode decompiler.
pub mod error;
pub mod ffi;
pub mod ir;
pub mod kotlin;
pub mod types;

#[cfg(feature = "mcp")]
pub mod mcp;

pub use classfile::ClassFile;
pub use decompiler::{
    DecompileDiagnostic, DecompileLanguage, DecompileOptions, DecompileOutput, Decompiler,
    DiagnosticLevel, DEFAULT_MAX_ARCHIVE_ENTRY_SIZE, DEFAULT_MAX_ARCHIVE_SIZE,
    DEFAULT_MAX_CLASS_SIZE,
};
pub use error::{DecompileError, Result};
