/// Top-level parsed class file — §4 of the JVM spec.
use crate::classfile::attribute::{parse_attributes, Attribute};
use crate::classfile::constant_pool::ConstantPool;
use crate::classfile::cursor::Cursor;
use crate::classfile::member::{Field, Method};
use crate::error::Result;

pub const MAGIC: u32 = 0xCAFE_BABE;

/// Parsed representation of a single `.class` file.
#[derive(Debug)]
pub struct ClassFile {
    pub minor_version:   u16,
    pub major_version:   u16,
    pub constant_pool:   ConstantPool,
    pub access_flags:    u16,
    /// Binary name of this class, e.g. `"java/lang/Object"`.
    pub this_class:      String,
    /// Binary name of the superclass, or `None` for `java/lang/Object`.
    pub super_class:     Option<String>,
    pub interfaces:      Vec<String>,
    pub fields:          Vec<Field>,
    pub methods:         Vec<Method>,
    pub attributes:      Vec<Attribute>,
}

impl ClassFile {
    /// Parse a `.class` file from a raw byte slice.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(bytes);

        // magic
        let magic = cur.read_u32()?;
        if magic != MAGIC {
            return Err(crate::error::DecompileError::BadMagic(magic));
        }

        let minor_version = cur.read_u16()?;
        let major_version = cur.read_u16()?;

        let constant_pool = ConstantPool::parse(&mut cur)?;

        let access_flags  = cur.read_u16()?;
        let this_idx      = cur.read_u16()?;
        let super_idx     = cur.read_u16()?;

        let this_class  = constant_pool.class_name(this_idx)?.to_string();
        let super_class = if super_idx == 0 {
            None
        } else {
            Some(constant_pool.class_name(super_idx)?.to_string())
        };

        // interfaces
        let iface_count = cur.read_u16()? as usize;
        let mut interfaces = Vec::with_capacity(iface_count);
        for _ in 0..iface_count {
            let idx = cur.read_u16()?;
            interfaces.push(constant_pool.class_name(idx)?.to_string());
        }

        // fields
        let field_count = cur.read_u16()? as usize;
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            fields.push(parse_member_field(&mut cur, &constant_pool, (major_version, minor_version))?);
        }

        // methods
        let method_count = cur.read_u16()? as usize;
        let mut methods = Vec::with_capacity(method_count);
        for _ in 0..method_count {
            methods.push(parse_member_method(&mut cur, &constant_pool, (major_version, minor_version))?);
        }

        // class attributes
        let attr_count = cur.read_u16()?;
        let attributes = parse_attributes(&mut cur, &constant_pool, attr_count, (major_version, minor_version))?;

        Ok(ClassFile {
            minor_version,
            major_version,
            constant_pool,
            access_flags,
            this_class,
            super_class,
            interfaces,
            fields,
            methods,
            attributes,
        })
    }

    // ── convenience accessors ──────────────────────────────────────────

    pub fn is_interface(&self)  -> bool { self.access_flags & crate::classfile::member::flags::INTERFACE  != 0 }
    pub fn is_abstract(&self)   -> bool { self.access_flags & crate::classfile::member::flags::ABSTRACT   != 0 }
    pub fn is_final(&self)      -> bool { self.access_flags & crate::classfile::member::flags::FINAL      != 0 }
    pub fn is_enum(&self)       -> bool { self.access_flags & crate::classfile::member::flags::ENUM       != 0 }
    pub fn is_annotation(&self) -> bool { self.access_flags & crate::classfile::member::flags::ANNOTATION != 0 }
    pub fn is_synthetic(&self)  -> bool { self.access_flags & crate::classfile::member::flags::SYNTHETIC  != 0 }
    pub fn is_module(&self)     -> bool { self.access_flags & crate::classfile::member::flags::MODULE     != 0 }

    /// Simple class name (last component after `/`).
    pub fn simple_name(&self) -> &str {
        self.this_class.rsplit('/').next().unwrap_or(&self.this_class)
    }

    /// The source file name, if the SourceFile attribute is present.
    pub fn source_file(&self) -> Option<&str> {
        for attr in &self.attributes {
            if let Attribute::SourceFile(s) = attr { return Some(s.as_str()); }
        }
        None
    }

    /// The generic signature string, if present.
    pub fn signature(&self) -> Option<&str> {
        for attr in &self.attributes {
            if let Attribute::Signature(s) = attr { return Some(s.as_str()); }
        }
        None
    }

    /// Java class file major version → language version string.
    pub fn java_version(&self) -> &'static str {
        match self.major_version {
            45 => "Java 1.1",
            46 => "Java 1.2",
            47 => "Java 1.3",
            48 => "Java 1.4",
            49 => "Java 5",
            50 => "Java 6",
            51 => "Java 7",
            52 => "Java 8",
            53 => "Java 9",
            54 => "Java 10",
            55 => "Java 11",
            56 => "Java 12",
            57 => "Java 13",
            58 => "Java 14",
            59 => "Java 15",
            60 => "Java 16",
            61 => "Java 17",
            62 => "Java 18",
            63 => "Java 19",
            64 => "Java 20",
            65 => "Java 21",
            66 => "Java 22",
            67 => "Java 23",
            68 => "Java 24",
            _  => "Java (unknown)",
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn parse_member_field(cur: &mut Cursor, pool: &ConstantPool, version: (u16, u16)) -> Result<Field> {
    let access_flags = cur.read_u16()?;
    let name         = pool.utf8(cur.read_u16()?)?.to_string();
    let descriptor   = pool.utf8(cur.read_u16()?)?.to_string();
    let attr_count   = cur.read_u16()?;
    let attributes   = parse_attributes(cur, pool, attr_count, version)?;
    Ok(Field { access_flags, name, descriptor, attributes })
}

fn parse_member_method(cur: &mut Cursor, pool: &ConstantPool, version: (u16, u16)) -> Result<Method> {
    let access_flags = cur.read_u16()?;
    let name         = pool.utf8(cur.read_u16()?)?.to_string();
    let descriptor   = pool.utf8(cur.read_u16()?)?.to_string();
    let attr_count   = cur.read_u16()?;
    let attributes   = parse_attributes(cur, pool, attr_count, version)?;
    Ok(Method { access_flags, name, descriptor, attributes })
}
