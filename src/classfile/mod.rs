/// JVM class file parsing layer.
///
/// Pipeline:
///   raw bytes → Cursor → ConstantPool → Attributes → ClassFile
pub mod cursor;
pub mod constant_pool;
pub mod opcodes;
pub mod instruction;
pub mod attribute;
pub mod member;
pub mod classfile;

pub use classfile::ClassFile;
