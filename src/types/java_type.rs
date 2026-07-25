/// Core Java type representation used throughout the decompiler.
///
/// Mirrors Vineflower's `VarType` design: a flat struct with a `kind` tag
/// and an `array_dim` counter, avoiding expensive recursive `Box` allocation
/// for the common case (most types have array_dim == 0).
use std::fmt;

// ── TypeKind ───────────────────────────────────────────────────────────────

/// The base kind of a Java type (sans array dimensions).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeKind {
    // ── primitive types ─────────────────────────────────────────────────
    Byte,
    Char,
    Double,
    Float,
    Int,
    Long,
    Short,
    Boolean,
    Void,

    // ── reference types ──────────────────────────────────────────────────
    /// An object reference. `class_name` is the JVM binary name, e.g.
    /// `"java/lang/String"`. Inner classes use `$`, e.g. `"java/util/Map$Entry"`.
    Object,

    // ── special types (used internally during stack analysis) ────────────
    /// The null literal — lives below all object types in the lattice.
    Null,
    /// Placeholder for the "upper half" slot of a category-2 type (long/double).
    /// Not visible in generated source.
    Group2Empty,
    /// jsr/ret return address.
    Address,
    /// `int`-range type that could be either byte or char.
    ByteChar,
    /// `int`-range type that could be either short or char.
    ShortChar,
    /// Completely unknown type — the bottom of every lattice.
    Unknown,
    /// A type-variable reference (e.g. `T` in `T extends Comparable<T>`).
    /// `class_name` stores the variable name.
    GenVar,
}

// ── TypeFamily ────────────────────────────────────────────────────────────

/// Coarse-grained family classification, used for stack-slot sizing and
/// lattice comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeFamily {
    Integer,  // byte, char, short, int, boolean, bytechar, shortchar
    Long,
    Float,
    Double,
    Object,   // any reference type, array, null
    Boolean,
    Unknown,
}

// ── JavaType ──────────────────────────────────────────────────────────────

/// A fully-qualified Java type, including array dimensions.
///
/// `class_name` is `Some` iff `kind` is `Object` or `GenVar`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JavaType {
    pub kind:       TypeKind,
    /// Number of array dimensions. 0 = not an array.
    pub array_dim:  u8,
    /// Binary class name (for Object) or type variable name (for GenVar).
    pub class_name: Option<String>,
}

// ── well-known constants ──────────────────────────────────────────────────

impl JavaType {
    // primitives
    pub const BYTE:    JavaType = JavaType { kind: TypeKind::Byte,    array_dim: 0, class_name: None };
    pub const CHAR:    JavaType = JavaType { kind: TypeKind::Char,    array_dim: 0, class_name: None };
    pub const DOUBLE:  JavaType = JavaType { kind: TypeKind::Double,  array_dim: 0, class_name: None };
    pub const FLOAT:   JavaType = JavaType { kind: TypeKind::Float,   array_dim: 0, class_name: None };
    pub const INT:     JavaType = JavaType { kind: TypeKind::Int,     array_dim: 0, class_name: None };
    pub const LONG:    JavaType = JavaType { kind: TypeKind::Long,    array_dim: 0, class_name: None };
    pub const SHORT:   JavaType = JavaType { kind: TypeKind::Short,   array_dim: 0, class_name: None };
    pub const BOOLEAN: JavaType = JavaType { kind: TypeKind::Boolean, array_dim: 0, class_name: None };
    pub const VOID:    JavaType = JavaType { kind: TypeKind::Void,    array_dim: 0, class_name: None };

    // special
    pub const NULL:        JavaType = JavaType { kind: TypeKind::Null,       array_dim: 0, class_name: None };
    pub const UNKNOWN:     JavaType = JavaType { kind: TypeKind::Unknown,    array_dim: 0, class_name: None };
    pub const GROUP2EMPTY: JavaType = JavaType { kind: TypeKind::Group2Empty,array_dim: 0, class_name: None };
    pub const ADDRESS:     JavaType = JavaType { kind: TypeKind::Address,    array_dim: 0, class_name: None };
    pub const BYTECHAR:    JavaType = JavaType { kind: TypeKind::ByteChar,   array_dim: 0, class_name: None };
    pub const SHORTCHAR:   JavaType = JavaType { kind: TypeKind::ShortChar,  array_dim: 0, class_name: None };

    // common object types
    pub fn object(class_name: impl Into<String>) -> Self {
        JavaType { kind: TypeKind::Object, array_dim: 0, class_name: Some(class_name.into()) }
    }

    pub fn genvar(name: impl Into<String>) -> Self {
        JavaType { kind: TypeKind::GenVar, array_dim: 0, class_name: Some(name.into()) }
    }

    /// Wrap this type in one additional array dimension.
    pub fn array_of(mut self) -> Self {
        self.array_dim += 1;
        self
    }

    /// Return this type with the specified array dimensionality.
    pub fn with_dims(mut self, dims: u8) -> Self {
        self.array_dim = dims;
        self
    }

    // ── queries ────────────────────────────────────────────────────────

    pub fn is_primitive(&self) -> bool {
        self.array_dim == 0 && matches!(self.kind,
            TypeKind::Byte | TypeKind::Char | TypeKind::Double | TypeKind::Float |
            TypeKind::Int  | TypeKind::Long | TypeKind::Short  | TypeKind::Boolean)
    }

    pub fn is_void(&self) -> bool {
        self.array_dim == 0 && self.kind == TypeKind::Void
    }

    pub fn is_reference(&self) -> bool {
        self.array_dim > 0 || matches!(self.kind,
            TypeKind::Object | TypeKind::Null | TypeKind::GenVar)
    }

    pub fn is_array(&self) -> bool { self.array_dim > 0 }

    pub fn is_wide(&self) -> bool {
        self.array_dim == 0 && matches!(self.kind, TypeKind::Long | TypeKind::Double)
    }

    /// JVM operand stack slots occupied by this type.
    pub fn stack_size(&self) -> u8 {
        if self.array_dim > 0 { return 1; }
        match self.kind {
            TypeKind::Long | TypeKind::Double => 2,
            TypeKind::Void | TypeKind::Group2Empty => 0,
            _ => 1,
        }
    }

    pub fn family(&self) -> TypeFamily {
        if self.array_dim > 0 {
            return TypeFamily::Object;
        }
        match self.kind {
            TypeKind::Byte | TypeKind::Char | TypeKind::Short |
            TypeKind::Int  | TypeKind::ByteChar | TypeKind::ShortChar => TypeFamily::Integer,
            TypeKind::Boolean  => TypeFamily::Boolean,
            TypeKind::Long     => TypeFamily::Long,
            TypeKind::Float    => TypeFamily::Float,
            TypeKind::Double   => TypeFamily::Double,
            TypeKind::Object | TypeKind::Null | TypeKind::GenVar => TypeFamily::Object,
            _ => TypeFamily::Unknown,
        }
    }

    /// The class name, panics if `kind` is not Object or GenVar.
    pub fn class_name(&self) -> &str {
        self.class_name.as_deref().expect("JavaType has no class name")
    }

    /// Simple class name: last segment after `/`.
    pub fn simple_class_name(&self) -> &str {
        let n = self.class_name();
        n.rfind('/').map(|i| &n[i+1..]).unwrap_or(n)
    }
}

// ── Display ───────────────────────────────────────────────────────────────

impl fmt::Display for JavaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Write the base type
        match &self.kind {
            TypeKind::Byte    => write!(f, "byte")?,
            TypeKind::Char    => write!(f, "char")?,
            TypeKind::Double  => write!(f, "double")?,
            TypeKind::Float   => write!(f, "float")?,
            TypeKind::Int     => write!(f, "int")?,
            TypeKind::Long    => write!(f, "long")?,
            TypeKind::Short   => write!(f, "short")?,
            TypeKind::Boolean => write!(f, "boolean")?,
            TypeKind::Void    => write!(f, "void")?,
            TypeKind::Null    => write!(f, "null")?,
            TypeKind::Unknown => write!(f, "/*unknown*/")?,
            TypeKind::Group2Empty => write!(f, "/*group2empty*/")?,
            TypeKind::Address => write!(f, "/*address*/")?,
            TypeKind::ByteChar  => write!(f, "/*byte|char*/")?,
            TypeKind::ShortChar => write!(f, "/*short|char*/")?,
            TypeKind::GenVar  => {
                write!(f, "{}", self.class_name.as_deref().unwrap_or("?"))?;
            }
            TypeKind::Object => {
                if let Some(name) = &self.class_name {
                    // Convert binary name to source name: replace '/' with '.'
                    // and inner class '$' with '.'.
                    write!(f, "{}", name.replace('/', ".").replace('$', "."))?;
                } else {
                    write!(f, "Object")?;
                }
            }
        }
        // Write array brackets
        for _ in 0..self.array_dim {
            write!(f, "[]")?;
        }
        Ok(())
    }
}

// ── JVM descriptor char → TypeKind ───────────────────────────────────────

impl JavaType {
    /// Parse a single JVM base-type character (`B`, `C`, `I`, …).
    pub fn from_descriptor_char(c: char) -> Option<JavaType> {
        Some(match c {
            'B' => JavaType::BYTE,
            'C' => JavaType::CHAR,
            'D' => JavaType::DOUBLE,
            'F' => JavaType::FLOAT,
            'I' => JavaType::INT,
            'J' => JavaType::LONG,
            'S' => JavaType::SHORT,
            'Z' => JavaType::BOOLEAN,
            'V' => JavaType::VOID,
            _ => return None,
        })
    }

    /// Render back to JVM descriptor form (without array brackets — caller adds `[`s).
    pub fn to_descriptor_base(&self) -> String {
        match &self.kind {
            TypeKind::Byte    => "B".into(),
            TypeKind::Char    => "C".into(),
            TypeKind::Double  => "D".into(),
            TypeKind::Float   => "F".into(),
            TypeKind::Int     => "I".into(),
            TypeKind::Long    => "J".into(),
            TypeKind::Short   => "S".into(),
            TypeKind::Boolean => "Z".into(),
            TypeKind::Void    => "V".into(),
            TypeKind::Object  => format!("L{};", self.class_name.as_deref().unwrap_or("")),
            TypeKind::GenVar  => format!("T{};", self.class_name.as_deref().unwrap_or("")),
            _                 => "?".into(),
        }
    }

    /// Full JVM descriptor string including array prefix.
    pub fn to_descriptor(&self) -> String {
        let mut s = String::with_capacity(self.array_dim as usize + 32);
        for _ in 0..self.array_dim { s.push('['); }
        s.push_str(&self.to_descriptor_base());
        s
    }
}
