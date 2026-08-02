/// JVM Constant Pool — §4.4 of the JVM spec.
///
/// The pool is 1-indexed: index 0 is unused (represented as `None`).
/// Long and Double entries occupy two slots; the second slot is `None`.
use crate::classfile::cursor::Cursor;
use crate::error::{DecompileError, Result};

// ── tag constants ──────────────────────────────────────────────────────────

pub const TAG_UTF8: u8 = 1;
pub const TAG_INTEGER: u8 = 3;
pub const TAG_FLOAT: u8 = 4;
pub const TAG_LONG: u8 = 5;
pub const TAG_DOUBLE: u8 = 6;
pub const TAG_CLASS: u8 = 7;
pub const TAG_STRING: u8 = 8;
pub const TAG_FIELDREF: u8 = 9;
pub const TAG_METHODREF: u8 = 10;
pub const TAG_INTERFACE_METHODREF: u8 = 11;
pub const TAG_NAME_AND_TYPE: u8 = 12;
pub const TAG_METHOD_HANDLE: u8 = 15;
pub const TAG_METHOD_TYPE: u8 = 16;
pub const TAG_DYNAMIC: u8 = 17;
pub const TAG_INVOKE_DYNAMIC: u8 = 18;
pub const TAG_MODULE: u8 = 19;
pub const TAG_PACKAGE: u8 = 20;

// ── raw pool entry (first-pass, indices not yet resolved) ──────────────────

#[derive(Debug, Clone)]
pub enum RawCpEntry {
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    /// Stores the name_index; resolved to a String after first pass.
    Class {
        name_index: u16,
    },
    /// Stores the string_index; resolved to a String after first pass.
    String {
        string_index: u16,
    },
    Fieldref {
        class_index: u16,
        name_and_type_index: u16,
    },
    Methodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    InterfaceMethodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    NameAndType {
        name_index: u16,
        descriptor_index: u16,
    },
    MethodHandle {
        reference_kind: u8,
        reference_index: u16,
    },
    MethodType {
        descriptor_index: u16,
    },
    Dynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    InvokeDynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    Module {
        name_index: u16,
    },
    Package {
        name_index: u16,
    },
}

// ── resolved pool entry ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CpEntry {
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(String),  // binary name, e.g. "java/lang/Object"
    String(String), // string value
    Fieldref(MemberRef),
    Methodref(MemberRef),
    InterfaceMethodref(MemberRef),
    NameAndType {
        name: String,
        descriptor: String,
    },
    MethodHandle {
        reference_kind: u8,
        reference: Box<CpEntry>,
    },
    MethodType(String), // descriptor
    Dynamic {
        bootstrap_attr_index: u16,
        name: String,
        descriptor: String,
    },
    InvokeDynamic {
        bootstrap_attr_index: u16,
        name: String,
        descriptor: String,
    },
    Module(String),
    Package(String),
}

#[derive(Debug, Clone)]
pub struct MemberRef {
    pub class_name: String,
    pub name: String,
    pub descriptor: String,
}

// ── constant pool ──────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ConstantPool {
    /// 1-indexed: entries[0] is always None.
    entries: Vec<Option<CpEntry>>,
}

impl ConstantPool {
    /// Parse the constant pool from the class file cursor.
    /// The cursor must be positioned right after the class file magic+version,
    /// i.e., at the `constant_pool_count` field.
    pub fn parse(cur: &mut Cursor) -> Result<Self> {
        let count = cur.read_u16()? as usize;

        // First pass: read raw entries
        let mut raw: Vec<Option<RawCpEntry>> = Vec::with_capacity(count);
        raw.push(None); // index 0 unused

        let mut i = 1usize;
        while i < count {
            let tag = cur.read_u8()?;
            let entry = match tag {
                TAG_UTF8 => RawCpEntry::Utf8(cur.read_mutf8()?),
                TAG_INTEGER => RawCpEntry::Integer(cur.read_i32()?),
                TAG_FLOAT => RawCpEntry::Float(cur.read_f32()?),
                TAG_LONG => {
                    let v = RawCpEntry::Long(cur.read_i64()?);
                    raw.push(Some(v));
                    raw.push(None); // occupies two slots
                    i += 2;
                    continue;
                }
                TAG_DOUBLE => {
                    let v = RawCpEntry::Double(cur.read_f64()?);
                    raw.push(Some(v));
                    raw.push(None);
                    i += 2;
                    continue;
                }
                TAG_CLASS => RawCpEntry::Class {
                    name_index: cur.read_u16()?,
                },
                TAG_STRING => RawCpEntry::String {
                    string_index: cur.read_u16()?,
                },
                TAG_FIELDREF => RawCpEntry::Fieldref {
                    class_index: cur.read_u16()?,
                    name_and_type_index: cur.read_u16()?,
                },
                TAG_METHODREF => RawCpEntry::Methodref {
                    class_index: cur.read_u16()?,
                    name_and_type_index: cur.read_u16()?,
                },
                TAG_INTERFACE_METHODREF => RawCpEntry::InterfaceMethodref {
                    class_index: cur.read_u16()?,
                    name_and_type_index: cur.read_u16()?,
                },
                TAG_NAME_AND_TYPE => RawCpEntry::NameAndType {
                    name_index: cur.read_u16()?,
                    descriptor_index: cur.read_u16()?,
                },
                TAG_METHOD_HANDLE => RawCpEntry::MethodHandle {
                    reference_kind: cur.read_u8()?,
                    reference_index: cur.read_u16()?,
                },
                TAG_METHOD_TYPE => RawCpEntry::MethodType {
                    descriptor_index: cur.read_u16()?,
                },
                TAG_DYNAMIC => RawCpEntry::Dynamic {
                    bootstrap_method_attr_index: cur.read_u16()?,
                    name_and_type_index: cur.read_u16()?,
                },
                TAG_INVOKE_DYNAMIC => RawCpEntry::InvokeDynamic {
                    bootstrap_method_attr_index: cur.read_u16()?,
                    name_and_type_index: cur.read_u16()?,
                },
                TAG_MODULE => RawCpEntry::Module {
                    name_index: cur.read_u16()?,
                },
                TAG_PACKAGE => RawCpEntry::Package {
                    name_index: cur.read_u16()?,
                },
                _ => {
                    return Err(DecompileError::InvalidCpTag {
                        tag,
                        index: i as u16,
                    })
                }
            };
            raw.push(Some(entry));
            i += 1;
        }

        // Helper: resolve a Utf8 string from raw entries
        let get_utf8 = |raw: &Vec<Option<RawCpEntry>>, idx: u16| -> Result<String> {
            match raw.get(idx as usize).and_then(|e| e.as_ref()) {
                Some(RawCpEntry::Utf8(s)) => Ok(s.clone()),
                Some(_) => Err(DecompileError::CpTypeMismatch {
                    index: idx,
                    expected: "Utf8",
                    found: "other",
                }),
                None => Err(DecompileError::CpIndexOutOfRange(idx)),
            }
        };

        // Second pass: resolve indices into strings / sub-entries
        let mut entries: Vec<Option<CpEntry>> = Vec::with_capacity(count);
        for (idx, raw_opt) in raw.iter().enumerate() {
            let resolved = match raw_opt {
                None => None,
                Some(raw_entry) => Some(match raw_entry {
                    RawCpEntry::Utf8(s) => CpEntry::Utf8(s.clone()),
                    RawCpEntry::Integer(v) => CpEntry::Integer(*v),
                    RawCpEntry::Float(v) => CpEntry::Float(*v),
                    RawCpEntry::Long(v) => CpEntry::Long(*v),
                    RawCpEntry::Double(v) => CpEntry::Double(*v),
                    RawCpEntry::Class { name_index } => {
                        CpEntry::Class(get_utf8(&raw, *name_index)?)
                    }
                    RawCpEntry::String { string_index } => {
                        CpEntry::String(get_utf8(&raw, *string_index)?)
                    }
                    RawCpEntry::Module { name_index } => {
                        CpEntry::Module(get_utf8(&raw, *name_index)?)
                    }
                    RawCpEntry::Package { name_index } => {
                        CpEntry::Package(get_utf8(&raw, *name_index)?)
                    }
                    RawCpEntry::MethodType { descriptor_index } => {
                        CpEntry::MethodType(get_utf8(&raw, *descriptor_index)?)
                    }
                    RawCpEntry::NameAndType {
                        name_index,
                        descriptor_index,
                    } => CpEntry::NameAndType {
                        name: get_utf8(&raw, *name_index)?,
                        descriptor: get_utf8(&raw, *descriptor_index)?,
                    },
                    RawCpEntry::Fieldref {
                        class_index,
                        name_and_type_index,
                    }
                    | RawCpEntry::Methodref {
                        class_index,
                        name_and_type_index,
                    }
                    | RawCpEntry::InterfaceMethodref {
                        class_index,
                        name_and_type_index,
                    } => {
                        let class_name = match raw
                            .get(*class_index as usize)
                            .and_then(|e| e.as_ref())
                        {
                            Some(RawCpEntry::Class { name_index }) => get_utf8(&raw, *name_index)?,
                            _ => return Err(DecompileError::CpIndexOutOfRange(*class_index)),
                        };
                        let (name, descriptor) = match raw
                            .get(*name_and_type_index as usize)
                            .and_then(|e| e.as_ref())
                        {
                            Some(RawCpEntry::NameAndType {
                                name_index,
                                descriptor_index,
                            }) => (
                                get_utf8(&raw, *name_index)?,
                                get_utf8(&raw, *descriptor_index)?,
                            ),
                            _ => {
                                return Err(DecompileError::CpIndexOutOfRange(*name_and_type_index))
                            }
                        };
                        let mr = MemberRef {
                            class_name,
                            name,
                            descriptor,
                        };
                        match raw_entry {
                            RawCpEntry::Fieldref { .. } => CpEntry::Fieldref(mr),
                            RawCpEntry::Methodref { .. } => CpEntry::Methodref(mr),
                            RawCpEntry::InterfaceMethodref { .. } => {
                                CpEntry::InterfaceMethodref(mr)
                            }
                            _ => unreachable!(),
                        }
                    }
                    RawCpEntry::Dynamic {
                        bootstrap_method_attr_index,
                        name_and_type_index,
                    }
                    | RawCpEntry::InvokeDynamic {
                        bootstrap_method_attr_index,
                        name_and_type_index,
                    } => {
                        let (name, descriptor) = match raw
                            .get(*name_and_type_index as usize)
                            .and_then(|e| e.as_ref())
                        {
                            Some(RawCpEntry::NameAndType {
                                name_index,
                                descriptor_index,
                            }) => (
                                get_utf8(&raw, *name_index)?,
                                get_utf8(&raw, *descriptor_index)?,
                            ),
                            _ => {
                                return Err(DecompileError::CpIndexOutOfRange(*name_and_type_index))
                            }
                        };
                        match raw_entry {
                            RawCpEntry::Dynamic { .. } => CpEntry::Dynamic {
                                bootstrap_attr_index: *bootstrap_method_attr_index,
                                name,
                                descriptor,
                            },
                            _ => CpEntry::InvokeDynamic {
                                bootstrap_attr_index: *bootstrap_method_attr_index,
                                name,
                                descriptor,
                            },
                        }
                    }
                    RawCpEntry::MethodHandle {
                        reference_kind,
                        reference_index,
                    } => {
                        // Resolve the referenced Methodref/Fieldref/InterfaceMethodref entry.
                        let referenced =
                            raw.get(*reference_index as usize).and_then(|e| e.as_ref());
                        let resolved_ref = match referenced {
                            Some(RawCpEntry::Fieldref {
                                class_index,
                                name_and_type_index,
                            })
                            | Some(RawCpEntry::Methodref {
                                class_index,
                                name_and_type_index,
                            })
                            | Some(RawCpEntry::InterfaceMethodref {
                                class_index,
                                name_and_type_index,
                            }) => {
                                let class_name = match raw
                                    .get(*class_index as usize)
                                    .and_then(|e| e.as_ref())
                                {
                                    Some(RawCpEntry::Class { name_index }) => {
                                        get_utf8(&raw, *name_index)?
                                    }
                                    _ => {
                                        return Err(DecompileError::CpIndexOutOfRange(*class_index))
                                    }
                                };
                                let (name, descriptor) = match raw
                                    .get(*name_and_type_index as usize)
                                    .and_then(|e| e.as_ref())
                                {
                                    Some(RawCpEntry::NameAndType {
                                        name_index,
                                        descriptor_index,
                                    }) => (
                                        get_utf8(&raw, *name_index)?,
                                        get_utf8(&raw, *descriptor_index)?,
                                    ),
                                    _ => {
                                        return Err(DecompileError::CpIndexOutOfRange(
                                            *name_and_type_index,
                                        ))
                                    }
                                };
                                let mr = MemberRef {
                                    class_name,
                                    name,
                                    descriptor,
                                };
                                match referenced {
                                    Some(RawCpEntry::Fieldref { .. }) => CpEntry::Fieldref(mr),
                                    Some(RawCpEntry::Methodref { .. }) => CpEntry::Methodref(mr),
                                    Some(RawCpEntry::InterfaceMethodref { .. }) => {
                                        CpEntry::InterfaceMethodref(mr)
                                    }
                                    _ => unreachable!(),
                                }
                            }
                            _ => CpEntry::Utf8(format!("#{}", reference_index)),
                        };
                        CpEntry::MethodHandle {
                            reference_kind: *reference_kind,
                            reference: Box::new(resolved_ref),
                        }
                    }
                }),
            };
            entries.push(resolved);
            // The loop over raw already accounts for double-slots (Long/Double pushed two None)
            // so we just mirror the structure.
            if idx == 0 {
                continue;
            } // skip 0 → already pushed once
        }

        Ok(ConstantPool { entries })
    }

    /// Get an entry by 1-based index.
    pub fn get(&self, index: u16) -> Result<&CpEntry> {
        self.entries
            .get(index as usize)
            .and_then(|o| o.as_ref())
            .ok_or(DecompileError::CpIndexOutOfRange(index))
    }

    /// Convenience: get a Utf8 string.
    pub fn utf8(&self, index: u16) -> Result<&str> {
        match self.get(index)? {
            CpEntry::Utf8(s) => Ok(s.as_str()),
            _ => Err(DecompileError::CpTypeMismatch {
                index,
                expected: "Utf8",
                found: "other",
            }),
        }
    }

    /// Convenience: get a class binary name.
    pub fn class_name(&self, index: u16) -> Result<&str> {
        match self.get(index)? {
            CpEntry::Class(s) => Ok(s.as_str()),
            _ => Err(DecompileError::CpTypeMismatch {
                index,
                expected: "Class",
                found: "other",
            }),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.len() <= 1
    }

    /// Iterate over all non-null pool entries.
    pub fn entries(&self) -> impl Iterator<Item = &CpEntry> {
        self.entries.iter().filter_map(|e| e.as_ref())
    }
}
