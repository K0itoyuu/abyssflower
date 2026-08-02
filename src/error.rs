use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecompileError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid class file magic: expected 0xCAFEBABE, got 0x{0:08X}")]
    BadMagic(u32),

    #[error("Unexpected end of bytecode at offset {0}")]
    UnexpectedEof(usize),

    #[error("Invalid constant pool tag {tag} at index {index}")]
    InvalidCpTag { tag: u8, index: u16 },

    #[error("Constant pool index {0} is out of range")]
    CpIndexOutOfRange(u16),

    #[error("Constant pool index {index} expected type {expected}, found {found}")]
    CpTypeMismatch {
        index: u16,
        expected: &'static str,
        found: &'static str,
    },

    #[error("Invalid opcode 0x{0:02X} at offset {1}")]
    InvalidOpcode(u8, u32),

    #[error("Malformed attribute '{0}': {1}")]
    MalformedAttribute(&'static str, String),

    #[error("Unsupported class file version {major}.{minor}")]
    UnsupportedVersion { major: u16, minor: u16 },

    #[error("Input is too large: {actual} bytes exceeds the {limit}-byte limit")]
    InputTooLarge { actual: u64, limit: u64 },

    #[error("Invalid JVM class name '{0}'")]
    InvalidClassName(String),

    #[error("JAR entry '{0}' was not found")]
    JarEntryNotFound(String),

    #[error("Kotlin metadata could not be decoded")]
    InvalidKotlinMetadata,
}

pub type Result<T> = std::result::Result<T, DecompileError>;
