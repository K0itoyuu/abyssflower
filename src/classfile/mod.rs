pub mod attribute;
#[allow(clippy::module_inception)]
pub mod classfile;
pub mod constant_pool;
/// JVM class file parsing layer.
///
/// Pipeline:
///   raw bytes → Cursor → ConstantPool → Attributes → ClassFile
pub mod cursor;
pub mod instruction;
pub mod member;
pub mod opcodes;

pub use classfile::ClassFile;
