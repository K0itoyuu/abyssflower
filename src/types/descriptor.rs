/// JVM field and method descriptor parser — §4.3 of the JVM spec.
///
/// Field descriptor:  `[[[Ljava/lang/String;`  →  `String[][][]`
/// Method descriptor: `(ILjava/lang/String;)[B` →  `(int, String) -> byte[]`
use crate::error::{DecompileError, Result};
use crate::types::java_type::JavaType;

// ── FieldType ─────────────────────────────────────────────────────────────

/// A parsed field (or local variable) type.
pub type FieldType = JavaType;

/// Parse a single field descriptor from `s`, returning the type and the
/// number of characters consumed.
///
/// Grammar (informal):
/// ```text
/// FieldDescriptor := BaseType | ObjectType | ArrayType
/// BaseType        := B | C | D | F | I | J | S | Z
/// ObjectType      := 'L' ClassName ';'
/// ArrayType       := '[' FieldDescriptor
/// ```
pub fn parse_field_descriptor(s: &str) -> Result<(FieldType, usize)> {
    parse_type_at(s, 0)
}

/// Parse a type starting at byte `offset` within `s`.
/// Returns `(type, bytes_consumed)`.
fn parse_type_at(s: &str, offset: usize) -> Result<(JavaType, usize)> {
    let bytes = s.as_bytes();
    let mut pos = offset;

    // Count array dimensions
    let mut array_dim: u8 = 0;
    while pos < bytes.len() && bytes[pos] == b'[' {
        array_dim += 1;
        pos += 1;
    }

    if pos >= bytes.len() {
        return Err(DecompileError::MalformedAttribute(
            "descriptor", format!("unexpected end at offset {pos}"),
        ));
    }

    let ch = bytes[pos] as char;
    let (base, consumed) = match ch {
        'L' => {
            // Object: 'L' BinaryName ';'
            let end = s[pos..].find(';').ok_or_else(|| {
                DecompileError::MalformedAttribute(
                    "descriptor", format!("missing ';' after 'L' at offset {pos}"),
                )
            })? + pos;
            let class_name = &s[pos + 1..end];
            (JavaType::object(class_name), end + 1 - offset)
        }
        'V' => (JavaType::VOID,    pos + 1 - offset),
        'B' => (JavaType::BYTE,    pos + 1 - offset),
        'C' => (JavaType::CHAR,    pos + 1 - offset),
        'D' => (JavaType::DOUBLE,  pos + 1 - offset),
        'F' => (JavaType::FLOAT,   pos + 1 - offset),
        'I' => (JavaType::INT,     pos + 1 - offset),
        'J' => (JavaType::LONG,    pos + 1 - offset),
        'S' => (JavaType::SHORT,   pos + 1 - offset),
        'Z' => (JavaType::BOOLEAN, pos + 1 - offset),
        other => {
            return Err(DecompileError::MalformedAttribute(
                "descriptor",
                format!("unknown type char '{other}' at offset {pos}"),
            ));
        }
    };

    Ok((base.with_dims(array_dim), consumed))
}

// ── MethodDescriptor ──────────────────────────────────────────────────────

/// A parsed method descriptor.
#[derive(Debug, Clone)]
pub struct MethodDescriptor {
    pub params:     Vec<JavaType>,
    pub return_type: JavaType,
}

impl MethodDescriptor {
    /// Parse `(param1param2…)ReturnType`.
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() || s.as_bytes()[0] != b'(' {
            return Err(DecompileError::MalformedAttribute(
                "method descriptor", format!("must start with '(': {s}"),
            ));
        }

        let close = s.find(')').ok_or_else(|| {
            DecompileError::MalformedAttribute("method descriptor", "missing ')'".into())
        })?;

        let params_str = &s[1..close];
        let return_str = &s[close + 1..];

        // Parse parameters
        let mut params = Vec::new();
        let mut pos = 0;
        while pos < params_str.len() {
            let (ty, consumed) = parse_type_at(params_str, pos)?;
            params.push(ty);
            pos += consumed;
        }

        // Parse return type
        let (return_type, _) = parse_type_at(return_str, 0)?;

        Ok(MethodDescriptor { params, return_type })
    }

    /// Number of JVM local-variable slots consumed by the parameters.
    /// (double/long occupy 2 slots each.)
    pub fn param_slots(&self) -> usize {
        self.params.iter().map(|t| t.stack_size() as usize).sum()
    }
}

impl std::fmt::Display for MethodDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(")?;
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 { write!(f, ", ")?; }
            write!(f, "{p}")?;
        }
        write!(f, ") -> {}", self.return_type)
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Parse all types in a concatenated descriptor string (e.g. the parameters
/// portion of a method descriptor with the parens stripped).
pub fn parse_type_list(s: &str) -> Result<Vec<JavaType>> {
    let mut types = Vec::new();
    let mut pos = 0;
    while pos < s.len() {
        let (ty, consumed) = parse_type_at(s, pos)?;
        types.push(ty);
        pos += consumed;
    }
    Ok(types)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_primitive_field() {
        let (ty, n) = parse_field_descriptor("I").unwrap();
        assert_eq!(ty, JavaType::INT);
        assert_eq!(n, 1);
    }

    #[test]
    fn test_object_field() {
        let (ty, n) = parse_field_descriptor("Ljava/lang/String;").unwrap();
        assert_eq!(ty.to_string(), "java.lang.String");
        assert_eq!(n, 18);
    }

    #[test]
    fn test_array_field() {
        let (ty, _) = parse_field_descriptor("[[[Ljava/lang/String;").unwrap();
        assert_eq!(ty.to_string(), "java.lang.String[][][]");
    }

    #[test]
    fn test_method_descriptor() {
        let md = MethodDescriptor::parse("(ILjava/lang/String;)[B").unwrap();
        assert_eq!(md.params.len(), 2);
        assert_eq!(md.params[0], JavaType::INT);
        assert_eq!(md.params[1].to_string(), "java.lang.String");
        assert_eq!(md.return_type.to_string(), "byte[]");
    }

    #[test]
    fn test_void_method() {
        let md = MethodDescriptor::parse("()V").unwrap();
        assert!(md.params.is_empty());
        assert!(md.return_type.is_void());
    }
}
