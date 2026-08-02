/// JVM generic signature parser — §4.7.9.1 of the JVM spec.
///
/// Handles class/method/field signatures containing type parameters,
/// wildcards, and type variable references, e.g.:
///   `<T:Ljava/lang/Comparable<TT;>;>(TT;Ljava/util/List<+TT;>;)TT;`
use crate::error::{DecompileError, Result};
use crate::types::java_type::{binary_name_to_source, JavaType};
use std::fmt;

// ── Generic type argument wildcard ────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wildcard {
    None,    // no wildcard (exact type)
    Extends, // `? extends T`  ('+' in JVM signature)
    Super,   // `? super T`    ('-' in JVM signature)
    Any,     // `?`            (just '*')
}

// ── GenericType ───────────────────────────────────────────────────────────

/// A fully-resolved generic type as it appears in a JVM signature.
///
/// This is a superset of `JavaType` — it adds type arguments and wildcards.
#[derive(Debug, Clone)]
pub enum GenericType {
    /// A primitive or void type (no generics possible).
    Base(JavaType),
    /// A type-variable reference, e.g. `T`.
    TypeVar(String),
    /// A class or interface type, possibly with type arguments.
    Class {
        class_name: String,
        args: Vec<TypeArg>,
        array_dim: u8,
    },
    /// An array whose element type is itself a generic type (e.g. `T[]`).
    Array { element: Box<GenericType>, dims: u8 },
}

/// A single type argument in a parameterized type.
#[derive(Debug, Clone)]
pub enum TypeArg {
    /// `?`
    Wildcard,
    /// `? extends Foo` or exact `Foo`
    Bounded {
        wildcard: Wildcard,
        ty: Box<GenericType>,
    },
}

// ── Formal type parameter ─────────────────────────────────────────────────

/// A single formal type parameter declaration: `T extends Bound1 & Bound2`.
#[derive(Debug, Clone)]
pub struct TypeParam {
    pub name: String,
    /// Class bound (at most one, from the first `:` without a preceding name)
    pub class_bound: Option<Box<GenericType>>,
    /// Interface bounds (subsequent `:` separated types)
    pub iface_bounds: Vec<GenericType>,
}

// ── Top-level signature descriptors ───────────────────────────────────────

/// Parsed class signature (`Signature` attribute on a class).
#[derive(Debug, Clone)]
pub struct ClassSignature {
    pub type_params: Vec<TypeParam>,
    pub superclass: GenericType,
    pub superinterfaces: Vec<GenericType>,
}

/// Parsed method signature.
#[derive(Debug, Clone)]
pub struct MethodSignature {
    pub type_params: Vec<TypeParam>,
    pub params: Vec<GenericType>,
    pub return_type: GenericType,
    pub throws: Vec<GenericType>,
}

/// Parsed field signature (just a reference type, possibly generic).
#[derive(Debug, Clone)]
pub struct FieldSignature {
    pub ty: GenericType,
}

// ── Parser ────────────────────────────────────────────────────────────────

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser {
            src: s.as_bytes(),
            pos: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.src.len() - self.pos
    }
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn consume(&mut self) -> Result<u8> {
        if self.pos >= self.src.len() {
            return Err(DecompileError::MalformedAttribute(
                "signature",
                "unexpected end of signature".into(),
            ));
        }
        let b = self.src[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        let got = self.consume()?;
        if got != c {
            Err(DecompileError::MalformedAttribute(
                "signature",
                format!("expected '{}' got '{}'", c as char, got as char),
            ))
        } else {
            Ok(())
        }
    }

    fn consume_if(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Read characters until `delimiter` (exclusive), return the slice.
    fn read_until(&mut self, delimiter: u8) -> Result<&'a str> {
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != delimiter {
            self.pos += 1;
        }
        if self.pos >= self.src.len() {
            return Err(DecompileError::MalformedAttribute(
                "signature",
                format!("expected '{}' but reached end", delimiter as char),
            ));
        }
        // SAFETY: input is a valid &str, so sub-slices of ASCII content are valid.
        Ok(std::str::from_utf8(&self.src[start..self.pos]).unwrap())
    }

    // ── type parameter parsing ─────────────────────────────────────────

    /// Parse `< TypeParam+ >` if present.
    fn parse_type_params(&mut self) -> Result<Vec<TypeParam>> {
        if self.peek() != Some(b'<') {
            return Ok(Vec::new());
        }
        self.pos += 1; // consume '<'
        let mut params = Vec::new();
        while self.peek() != Some(b'>') {
            params.push(self.parse_type_param()?);
        }
        self.pos += 1; // consume '>'
        Ok(params)
    }

    /// Parse one formal type parameter: `Identifier : ClassBound InterfaceBound*`
    fn parse_type_param(&mut self) -> Result<TypeParam> {
        let name = self.read_until(b':')?.to_string();
        self.pos += 1; // consume ':'

        // Optional class bound (may be empty, just the ':')
        let class_bound = if self.peek() != Some(b':') && self.peek() != Some(b'>') {
            Some(Box::new(self.parse_ref_type_sig()?))
        } else {
            None
        };

        // Interface bounds: ':' InterfaceTypeSig
        let mut iface_bounds = Vec::new();
        while self.peek() == Some(b':') {
            self.pos += 1; // consume ':'
            iface_bounds.push(self.parse_ref_type_sig()?);
        }

        Ok(TypeParam {
            name,
            class_bound,
            iface_bounds,
        })
    }

    // ── type signature parsing ─────────────────────────────────────────

    /// Parse any type signature (reference or base type).
    fn parse_type_sig(&mut self) -> Result<GenericType> {
        match self.peek() {
            Some(b'[') => self.parse_array_type_sig(),
            Some(b'L') => self.parse_class_type_sig(0),
            Some(b'T') => self.parse_type_var(),
            Some(b'V') => {
                self.pos += 1;
                Ok(GenericType::Base(JavaType::VOID))
            }
            Some(c) => {
                let ty = JavaType::from_descriptor_char(c as char).ok_or_else(|| {
                    DecompileError::MalformedAttribute(
                        "signature",
                        format!("unknown base type '{}'", c as char),
                    )
                })?;
                self.pos += 1;
                Ok(GenericType::Base(ty))
            }
            None => Err(DecompileError::MalformedAttribute(
                "signature",
                "empty signature".into(),
            )),
        }
    }

    /// Parse a reference type (ClassType | ArrayType | TypeVar).
    fn parse_ref_type_sig(&mut self) -> Result<GenericType> {
        match self.peek() {
            Some(b'[') => self.parse_array_type_sig(),
            Some(b'L') => self.parse_class_type_sig(0),
            Some(b'T') => self.parse_type_var(),
            other => Err(DecompileError::MalformedAttribute(
                "signature",
                format!(
                    "expected reference type, got {:?}",
                    other.map(|c| c as char)
                ),
            )),
        }
    }

    fn parse_array_type_sig(&mut self) -> Result<GenericType> {
        let mut dims: u8 = 0;
        while self.peek() == Some(b'[') {
            self.pos += 1;
            dims += 1;
        }
        let element = self.parse_type_sig()?;
        Ok(GenericType::Array {
            element: Box::new(element),
            dims,
        })
    }

    fn parse_type_var(&mut self) -> Result<GenericType> {
        self.expect(b'T')?;
        let name = self.read_until(b';')?.to_string();
        self.pos += 1; // consume ';'
        Ok(GenericType::TypeVar(name))
    }

    /// Parse `L` PackageSpecifier* SimpleClassTypeSignature (`/` …)* `;`
    /// also handles inner class separators `.`
    fn parse_class_type_sig(&mut self, array_dim: u8) -> Result<GenericType> {
        self.expect(b'L')?;
        let mut class_name = String::new();
        let mut args = Vec::new();

        loop {
            match self.peek() {
                Some(b';') => {
                    self.pos += 1;
                    break;
                }
                Some(b'<') => {
                    // type arguments
                    self.pos += 1; // consume '<'
                    while self.peek() != Some(b'>') {
                        args.push(self.parse_type_arg()?);
                    }
                    self.pos += 1; // consume '>'
                }
                Some(b'.') => {
                    // inner class separator — append '$' for binary name
                    class_name.push('$');
                    self.pos += 1;
                }
                Some(c) => {
                    class_name.push(c as char);
                    self.pos += 1;
                }
                None => {
                    return Err(DecompileError::MalformedAttribute(
                        "signature",
                        "unterminated class type signature".into(),
                    ))
                }
            }
        }

        Ok(GenericType::Class {
            class_name,
            args,
            array_dim,
        })
    }

    fn parse_type_arg(&mut self) -> Result<TypeArg> {
        match self.peek() {
            Some(b'*') => {
                self.pos += 1;
                Ok(TypeArg::Wildcard)
            }
            Some(b'+') => {
                self.pos += 1;
                let ty = self.parse_ref_type_sig()?;
                Ok(TypeArg::Bounded {
                    wildcard: Wildcard::Extends,
                    ty: Box::new(ty),
                })
            }
            Some(b'-') => {
                self.pos += 1;
                let ty = self.parse_ref_type_sig()?;
                Ok(TypeArg::Bounded {
                    wildcard: Wildcard::Super,
                    ty: Box::new(ty),
                })
            }
            _ => {
                let ty = self.parse_ref_type_sig()?;
                Ok(TypeArg::Bounded {
                    wildcard: Wildcard::None,
                    ty: Box::new(ty),
                })
            }
        }
    }
}

// ── Public parse functions ─────────────────────────────────────────────────

/// Parse a class signature string.
pub fn parse_class_signature(sig: &str) -> Result<ClassSignature> {
    let mut p = Parser::new(sig);
    let type_params = p.parse_type_params()?;
    let superclass = p.parse_class_type_sig(0)?;
    let mut superinterfaces = Vec::new();
    while p.remaining() > 0 {
        superinterfaces.push(p.parse_ref_type_sig()?);
    }
    Ok(ClassSignature {
        type_params,
        superclass,
        superinterfaces,
    })
}

/// Parse a method signature string.
pub fn parse_method_signature(sig: &str) -> Result<MethodSignature> {
    let mut p = Parser::new(sig);
    let type_params = p.parse_type_params()?;
    p.expect(b'(')?;
    let mut params = Vec::new();
    while p.peek() != Some(b')') {
        params.push(p.parse_type_sig()?);
    }
    p.pos += 1; // consume ')'
    let return_type = p.parse_type_sig()?;
    let mut throws = Vec::new();
    while p.consume_if(b'^') {
        throws.push(p.parse_ref_type_sig()?);
    }
    Ok(MethodSignature {
        type_params,
        params,
        return_type,
        throws,
    })
}

/// Parse a field/variable signature string.
pub fn parse_field_signature(sig: &str) -> Result<FieldSignature> {
    let mut p = Parser::new(sig);
    let ty = p.parse_ref_type_sig()?;
    Ok(FieldSignature { ty })
}

// ── Display ────────────────────────────────────────────────────────────────

impl fmt::Display for GenericType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenericType::Base(t) => write!(f, "{t}"),
            GenericType::TypeVar(n) => write!(f, "{n}"),
            GenericType::Class {
                class_name,
                args,
                array_dim,
            } => {
                let src_name = binary_name_to_source(class_name);
                write!(f, "{src_name}")?;
                if !args.is_empty() {
                    write!(f, "<")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ">")?;
                }
                for _ in 0..*array_dim {
                    write!(f, "[]")?;
                }
                Ok(())
            }
            GenericType::Array { element, dims } => {
                write!(f, "{element}")?;
                for _ in 0..*dims {
                    write!(f, "[]")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for TypeArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeArg::Wildcard => write!(f, "?"),
            TypeArg::Bounded { wildcard, ty } => match wildcard {
                Wildcard::None => write!(f, "{ty}"),
                Wildcard::Extends => write!(f, "? extends {ty}"),
                Wildcard::Super => write!(f, "? super {ty}"),
                Wildcard::Any => write!(f, "?"),
            },
        }
    }
}

impl fmt::Display for TypeParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(bound) = &self.class_bound {
            write!(f, " extends {bound}")?;
            for ib in &self.iface_bounds {
                write!(f, " & {ib}")?;
            }
        } else {
            for (i, ib) in self.iface_bounds.iter().enumerate() {
                if i == 0 {
                    write!(f, " extends {ib}")?;
                } else {
                    write!(f, " & {ib}")?;
                }
            }
        }
        Ok(())
    }
}
