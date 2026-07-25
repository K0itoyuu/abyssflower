/// Parsed class member — shared fields between StructField and StructMethod.
use crate::classfile::attribute::Attribute;

/// Access flags — §4.1 / §4.5 / §4.6 of the JVM spec.
pub mod flags {
    pub const PUBLIC:       u16 = 0x0001;
    pub const PRIVATE:      u16 = 0x0002;
    pub const PROTECTED:    u16 = 0x0004;
    pub const STATIC:       u16 = 0x0008;
    pub const FINAL:        u16 = 0x0010;
    pub const SYNCHRONIZED: u16 = 0x0020;
    pub const SUPER:        u16 = 0x0020; // class flag
    pub const VOLATILE:     u16 = 0x0040; // field
    pub const BRIDGE:       u16 = 0x0040; // method
    pub const TRANSIENT:    u16 = 0x0080; // field
    pub const VARARGS:      u16 = 0x0080; // method
    pub const NATIVE:       u16 = 0x0100;
    pub const INTERFACE:    u16 = 0x0200;
    pub const ABSTRACT:     u16 = 0x0400;
    pub const STRICT:       u16 = 0x0800;
    pub const SYNTHETIC:    u16 = 0x1000;
    pub const ANNOTATION:   u16 = 0x2000;
    pub const ENUM:         u16 = 0x4000;
    pub const MODULE:       u16 = 0x8000;
}

/// A field declared in a class.
#[derive(Debug, Clone)]
pub struct Field {
    pub access_flags: u16,
    pub name:         String,
    pub descriptor:   String,
    pub attributes:   Vec<Attribute>,
}

impl Field {
    pub fn is_static(&self)    -> bool { self.access_flags & flags::STATIC    != 0 }
    pub fn is_final(&self)     -> bool { self.access_flags & flags::FINAL     != 0 }
    pub fn is_private(&self)   -> bool { self.access_flags & flags::PRIVATE   != 0 }
    pub fn is_public(&self)    -> bool { self.access_flags & flags::PUBLIC    != 0 }
    pub fn is_synthetic(&self) -> bool { self.access_flags & flags::SYNTHETIC != 0 }
    pub fn is_enum(&self)      -> bool { self.access_flags & flags::ENUM      != 0 }
    pub fn is_volatile(&self)  -> bool { self.access_flags & flags::VOLATILE  != 0 }
    pub fn is_transient(&self) -> bool { self.access_flags & flags::TRANSIENT != 0 }
}

/// A method declared in a class.
#[derive(Debug, Clone)]
pub struct Method {
    pub access_flags: u16,
    pub name:         String,
    pub descriptor:   String,
    pub attributes:   Vec<Attribute>,
}

impl Method {
    pub fn is_static(&self)       -> bool { self.access_flags & flags::STATIC       != 0 }
    pub fn is_final(&self)        -> bool { self.access_flags & flags::FINAL        != 0 }
    pub fn is_private(&self)      -> bool { self.access_flags & flags::PRIVATE      != 0 }
    pub fn is_public(&self)       -> bool { self.access_flags & flags::PUBLIC       != 0 }
    pub fn is_abstract(&self)     -> bool { self.access_flags & flags::ABSTRACT     != 0 }
    pub fn is_native(&self)       -> bool { self.access_flags & flags::NATIVE       != 0 }
    pub fn is_synthetic(&self)    -> bool { self.access_flags & flags::SYNTHETIC    != 0 }
    pub fn is_bridge(&self)       -> bool { self.access_flags & flags::BRIDGE       != 0 }
    pub fn is_varargs(&self)      -> bool { self.access_flags & flags::VARARGS      != 0 }
    pub fn is_synchronized(&self) -> bool { self.access_flags & flags::SYNCHRONIZED != 0 }
    pub fn is_constructor(&self)  -> bool { self.name == "<init>" }
    pub fn is_static_init(&self)  -> bool { self.name == "<clinit>" }

    /// Find the Code attribute for this method, if any.
    pub fn code(&self) -> Option<&crate::classfile::attribute::CodeAttribute> {
        for attr in &self.attributes {
            if let crate::classfile::attribute::Attribute::Code(code) = attr {
                return Some(code);
            }
        }
        None
    }
}
