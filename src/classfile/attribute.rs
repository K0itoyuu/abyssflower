/// JVM class file attributes — §4.7 of the JVM spec.
///
/// We decode only the attributes actually needed for decompilation.
/// Unknown attributes are preserved as raw bytes so they can be
/// inspected later without re-reading the file.
use crate::classfile::constant_pool::ConstantPool;
use crate::classfile::cursor::Cursor;
use crate::classfile::instruction::{self, Instruction};
use crate::error::{DecompileError, Result};

// ── Exception table entry (inside Code attribute) ──────────────────────────

#[derive(Debug, Clone)]
pub struct ExceptionHandler {
    /// Start of the guarded region (inclusive), in bytecode offsets.
    pub start_pc: u16,
    /// End of the guarded region (exclusive).
    pub end_pc: u16,
    /// Start of the handler block.
    pub handler_pc: u16,
    /// Caught type — `None` means `finally` (catch-all).
    pub catch_type: Option<String>,
}

// ── InnerClass entry ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct InnerClassInfo {
    pub inner_class_name: Option<String>,
    pub outer_class_name: Option<String>,
    pub simple_name: Option<String>,
    pub inner_access_flags: u16,
}

// ── LocalVariable table entry ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LocalVariable {
    pub start_pc: u16,
    pub length: u16,
    pub name: String,
    pub descriptor: String,
    pub index: u16,
}

// ── LocalVariableType table entry ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LocalVariableType {
    pub start_pc: u16,
    pub length: u16,
    pub name: String,
    pub signature: String,
    pub index: u16,
}

// ── LineNumber table entry ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LineNumber {
    pub start_pc: u16,
    pub line_number: u16,
}

// ── BootstrapMethods entry ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BootstrapMethod {
    pub bootstrap_method_ref: u16,
    pub arguments: Vec<u16>,
}

// ── Annotation ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Annotation {
    pub type_descriptor: String,
    pub elements: Vec<(String, ElementValue)>,
}

#[derive(Debug, Clone)]
pub enum ElementValue {
    Byte(i32),
    Char(i32),
    Double(f64),
    Float(f32),
    Int(i32),
    Long(i64),
    Short(i32),
    Boolean(bool),
    String(String),
    EnumConst {
        type_name: String,
        const_name: String,
    },
    ClassInfo(String),
    Annotation(Annotation),
    Array(Vec<ElementValue>),
}

// ── StackMapTable (stored as raw frames for now) ──────────────────────────

#[derive(Debug, Clone)]
pub struct StackMapTable {
    pub raw_bytes: Vec<u8>,
}

// ── Top-level Attribute enum ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Attribute {
    Code(CodeAttribute),
    ConstantValue(ConstantValueAttribute),
    Exceptions(ExceptionsAttribute),
    InnerClasses(Vec<InnerClassInfo>),
    EnclosingMethod {
        class_name: String,
        method_name: Option<String>,
        method_descriptor: Option<String>,
    },
    Synthetic,
    Signature(String),
    SourceFile(String),
    LineNumberTable(Vec<LineNumber>),
    LocalVariableTable(Vec<LocalVariable>),
    LocalVariableTypeTable(Vec<LocalVariableType>),
    Deprecated,
    RuntimeVisibleAnnotations(Vec<Annotation>),
    RuntimeInvisibleAnnotations(Vec<Annotation>),
    RuntimeVisibleParameterAnnotations(Vec<Vec<Annotation>>),
    RuntimeInvisibleParameterAnnotations(Vec<Vec<Annotation>>),
    AnnotationDefault(ElementValue),
    BootstrapMethods(Vec<BootstrapMethod>),
    MethodParameters(Vec<MethodParameter>),
    Module(ModuleAttribute),
    NestHost(String),
    NestMembers(Vec<String>),
    Record(Vec<RecordComponent>),
    PermittedSubclasses(Vec<String>),
    StackMapTable(StackMapTable),
    Unknown {
        name: String,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub struct CodeAttribute {
    pub max_stack: u16,
    pub max_locals: u16,
    pub instructions: Vec<Instruction>,
    pub exception_table: Vec<ExceptionHandler>,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone)]
pub struct ConstantValueAttribute {
    pub constant_value_index: u16,
}

#[derive(Debug, Clone)]
pub struct ExceptionsAttribute {
    pub exception_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MethodParameter {
    pub name: Option<String>,
    pub access_flags: u16,
}

#[derive(Debug, Clone)]
pub struct ModuleAttribute {
    pub module_name: String,
    pub module_flags: u16,
    pub module_version: Option<String>,
    pub requires: Vec<ModuleRequires>,
    pub exports: Vec<ModuleExports>,
    pub opens: Vec<ModuleOpens>,
    pub uses: Vec<String>,
    pub provides: Vec<ModuleProvides>,
}

#[derive(Debug, Clone)]
pub struct ModuleRequires {
    pub module_name: String,
    pub flags: u16,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModuleExports {
    pub package_name: String,
    pub flags: u16,
    pub to_modules: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ModuleOpens {
    pub package_name: String,
    pub flags: u16,
    pub to_modules: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ModuleProvides {
    pub service_name: String,
    pub with: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RecordComponent {
    pub name: String,
    pub descriptor: String,
    pub attributes: Vec<Attribute>,
}

// ── attribute parsing ──────────────────────────────────────────────────────

/// Parse `count` attributes from `cur`, resolving names via `pool`.
///
/// `(major, minor)` is the class file version, used to detect pre-JVM-spec
/// Code attribute layouts (e.g. HotJava 1.0 alpha: major=45, minor<3).
pub fn parse_attributes(
    cur: &mut Cursor,
    pool: &ConstantPool,
    count: u16,
    version: (u16, u16),
) -> Result<Vec<Attribute>> {
    let mut attrs = Vec::with_capacity(count as usize);
    for _ in 0..count {
        attrs.push(parse_one(cur, pool, version)?);
    }
    Ok(attrs)
}

fn parse_one(cur: &mut Cursor, pool: &ConstantPool, version: (u16, u16)) -> Result<Attribute> {
    let name_idx = cur.read_u16()?;
    let length = cur.read_u32()? as usize;
    let name = pool.utf8(name_idx).unwrap_or("<unknown>").to_string();

    let mut attr_cur = cur.sub_cursor(length)?;

    let attr = match name.as_str() {
        "Code" => parse_code(&mut attr_cur, pool, version)?,
        "ConstantValue" => parse_constant_value(&mut attr_cur)?,
        "Exceptions" => parse_exceptions(&mut attr_cur, pool)?,
        "InnerClasses" => parse_inner_classes(&mut attr_cur, pool)?,
        "EnclosingMethod" => parse_enclosing_method(&mut attr_cur, pool)?,
        "Synthetic" => Attribute::Synthetic,
        "Signature" => parse_signature(&mut attr_cur, pool)?,
        "SourceFile" => parse_source_file(&mut attr_cur, pool)?,
        "LineNumberTable" => parse_line_number_table(&mut attr_cur)?,
        "LocalVariableTable" => parse_local_variable_table(&mut attr_cur, pool)?,
        "LocalVariableTypeTable" => parse_local_variable_type_table(&mut attr_cur, pool)?,
        "Deprecated" => Attribute::Deprecated,
        "RuntimeVisibleAnnotations" => {
            Attribute::RuntimeVisibleAnnotations(parse_annotations(&mut attr_cur, pool)?)
        }
        "RuntimeInvisibleAnnotations" => {
            Attribute::RuntimeInvisibleAnnotations(parse_annotations(&mut attr_cur, pool)?)
        }
        "RuntimeVisibleParameterAnnotations" => Attribute::RuntimeVisibleParameterAnnotations(
            parse_parameter_annotations(&mut attr_cur, pool)?,
        ),
        "RuntimeInvisibleParameterAnnotations" => Attribute::RuntimeInvisibleParameterAnnotations(
            parse_parameter_annotations(&mut attr_cur, pool)?,
        ),
        "AnnotationDefault" => {
            Attribute::AnnotationDefault(parse_element_value(&mut attr_cur, pool)?)
        }
        "BootstrapMethods" => parse_bootstrap_methods(&mut attr_cur)?,
        "MethodParameters" => parse_method_parameters(&mut attr_cur, pool)?,
        "Module" => parse_module(&mut attr_cur, pool)?,
        "NestHost" => {
            let idx = attr_cur.read_u16()?;
            Attribute::NestHost(pool.class_name(idx)?.to_string())
        }
        "NestMembers" => parse_nest_members(&mut attr_cur, pool)?,
        "Record" => parse_record(&mut attr_cur, pool)?,
        "PermittedSubclasses" => parse_permitted_subclasses(&mut attr_cur, pool)?,
        "StackMapTable" => Attribute::StackMapTable(StackMapTable {
            raw_bytes: attr_cur.remaining_slice().to_vec(),
        }),
        _ => Attribute::Unknown {
            name,
            bytes: attr_cur.remaining_slice().to_vec(),
        },
    };

    Ok(attr)
}

// ── individual attribute parsers ───────────────────────────────────────────

fn parse_code(cur: &mut Cursor, pool: &ConstantPool, version: (u16, u16)) -> Result<Attribute> {
    // Pre-JVM-spec HotJava alpha (major=45, minor<3) used:
    //   max_stack u1, max_locals u1, code_length u2
    // JDK 1.0.2+ (major=45, minor≥3) and all later versions use:
    //   max_stack u2, max_locals u2, code_length u4
    let (major, minor) = version;
    let (max_stack, max_locals, code_len) = if major == 45 && minor < 3 {
        let ms = cur.read_u8()? as u16;
        let ml = cur.read_u8()? as u16;
        let cl = cur.read_u16()? as usize;
        (ms, ml, cl)
    } else {
        (cur.read_u16()?, cur.read_u16()?, cur.read_u32()? as usize)
    };
    let code_bytes = cur.sub_cursor(code_len)?.remaining_slice().to_vec();
    let instructions = instruction::decode(&code_bytes)?;

    let ex_count = cur.read_u16()?;
    let mut exception_table = Vec::with_capacity(ex_count as usize);
    for _ in 0..ex_count {
        let start_pc = cur.read_u16()?;
        let end_pc = cur.read_u16()?;
        let handler_pc = cur.read_u16()?;
        let type_idx = cur.read_u16()?;
        let catch_type = if type_idx == 0 {
            None
        } else {
            Some(pool.class_name(type_idx)?.to_string())
        };
        exception_table.push(ExceptionHandler {
            start_pc,
            end_pc,
            handler_pc,
            catch_type,
        });
    }

    let attr_count = cur.read_u16()?;
    let attributes = parse_attributes(cur, pool, attr_count, version)?;

    Ok(Attribute::Code(CodeAttribute {
        max_stack,
        max_locals,
        instructions,
        exception_table,
        attributes,
    }))
}

fn parse_constant_value(cur: &mut Cursor) -> Result<Attribute> {
    let index = cur.read_u16()?;
    Ok(Attribute::ConstantValue(ConstantValueAttribute {
        constant_value_index: index,
    }))
}

fn parse_exceptions(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let idx = cur.read_u16()?;
        names.push(pool.class_name(idx)?.to_string());
    }
    Ok(Attribute::Exceptions(ExceptionsAttribute {
        exception_names: names,
    }))
}

fn parse_inner_classes(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut classes = Vec::with_capacity(count);
    for _ in 0..count {
        let inner_idx = cur.read_u16()?;
        let outer_idx = cur.read_u16()?;
        let name_idx = cur.read_u16()?;
        let flags = cur.read_u16()?;
        classes.push(InnerClassInfo {
            inner_class_name: if inner_idx == 0 {
                None
            } else {
                Some(pool.class_name(inner_idx)?.to_string())
            },
            outer_class_name: if outer_idx == 0 {
                None
            } else {
                Some(pool.class_name(outer_idx)?.to_string())
            },
            simple_name: if name_idx == 0 {
                None
            } else {
                Some(pool.utf8(name_idx)?.to_string())
            },
            inner_access_flags: flags,
        });
    }
    Ok(Attribute::InnerClasses(classes))
}

fn parse_enclosing_method(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let class_idx = cur.read_u16()?;
    let method_idx = cur.read_u16()?;
    let class_name = pool.class_name(class_idx)?.to_string();
    let (method_name, method_descriptor) = if method_idx == 0 {
        (None, None)
    } else {
        use crate::classfile::constant_pool::CpEntry;
        match pool.get(method_idx)? {
            CpEntry::NameAndType { name, descriptor } => {
                (Some(name.clone()), Some(descriptor.clone()))
            }
            _ => {
                return Err(DecompileError::CpTypeMismatch {
                    index: method_idx,
                    expected: "NameAndType",
                    found: "other",
                })
            }
        }
    };
    Ok(Attribute::EnclosingMethod {
        class_name,
        method_name,
        method_descriptor,
    })
}

fn parse_signature(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let idx = cur.read_u16()?;
    Ok(Attribute::Signature(pool.utf8(idx)?.to_string()))
}

fn parse_source_file(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let idx = cur.read_u16()?;
    Ok(Attribute::SourceFile(pool.utf8(idx)?.to_string()))
}

fn parse_line_number_table(cur: &mut Cursor) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut table = Vec::with_capacity(count);
    for _ in 0..count {
        table.push(LineNumber {
            start_pc: cur.read_u16()?,
            line_number: cur.read_u16()?,
        });
    }
    Ok(Attribute::LineNumberTable(table))
}

fn parse_local_variable_table(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut table = Vec::with_capacity(count);
    for _ in 0..count {
        table.push(LocalVariable {
            start_pc: cur.read_u16()?,
            length: cur.read_u16()?,
            name: pool.utf8(cur.read_u16()?)?.to_string(),
            descriptor: pool.utf8(cur.read_u16()?)?.to_string(),
            index: cur.read_u16()?,
        });
    }
    Ok(Attribute::LocalVariableTable(table))
}

fn parse_local_variable_type_table(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut table = Vec::with_capacity(count);
    for _ in 0..count {
        table.push(LocalVariableType {
            start_pc: cur.read_u16()?,
            length: cur.read_u16()?,
            name: pool.utf8(cur.read_u16()?)?.to_string(),
            signature: pool.utf8(cur.read_u16()?)?.to_string(),
            index: cur.read_u16()?,
        });
    }
    Ok(Attribute::LocalVariableTypeTable(table))
}

fn parse_bootstrap_methods(cur: &mut Cursor) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut methods = Vec::with_capacity(count);
    for _ in 0..count {
        let ref_idx = cur.read_u16()?;
        let arg_count = cur.read_u16()? as usize;
        let mut args = Vec::with_capacity(arg_count);
        for _ in 0..arg_count {
            args.push(cur.read_u16()?);
        }
        methods.push(BootstrapMethod {
            bootstrap_method_ref: ref_idx,
            arguments: args,
        });
    }
    Ok(Attribute::BootstrapMethods(methods))
}

fn parse_method_parameters(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u8()? as usize;
    let mut params = Vec::with_capacity(count);
    for _ in 0..count {
        let name_idx = cur.read_u16()?;
        let flags = cur.read_u16()?;
        let name = if name_idx == 0 {
            None
        } else {
            Some(pool.utf8(name_idx)?.to_string())
        };
        params.push(MethodParameter {
            name,
            access_flags: flags,
        });
    }
    Ok(Attribute::MethodParameters(params))
}

fn parse_nest_members(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut members = Vec::with_capacity(count);
    for _ in 0..count {
        members.push(pool.class_name(cur.read_u16()?)?.to_string());
    }
    Ok(Attribute::NestMembers(members))
}

fn parse_permitted_subclasses(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut classes = Vec::with_capacity(count);
    for _ in 0..count {
        classes.push(pool.class_name(cur.read_u16()?)?.to_string());
    }
    Ok(Attribute::PermittedSubclasses(classes))
}

fn parse_record(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    let count = cur.read_u16()? as usize;
    let mut components = Vec::with_capacity(count);
    for _ in 0..count {
        let name = pool.utf8(cur.read_u16()?)?.to_string();
        let descriptor = pool.utf8(cur.read_u16()?)?.to_string();
        let attr_count = cur.read_u16()?;
        let attributes = parse_attributes(cur, pool, attr_count, (0, 0))?;
        components.push(RecordComponent {
            name,
            descriptor,
            attributes,
        });
    }
    Ok(Attribute::Record(components))
}

fn parse_module(cur: &mut Cursor, pool: &ConstantPool) -> Result<Attribute> {
    use crate::classfile::constant_pool::CpEntry;
    let mod_idx = cur.read_u16()?;
    let module_name = match pool.get(mod_idx)? {
        CpEntry::Module(s) => s.clone(),
        _ => {
            return Err(DecompileError::CpTypeMismatch {
                index: mod_idx,
                expected: "Module",
                found: "other",
            })
        }
    };
    let module_flags = cur.read_u16()?;
    let ver_idx = cur.read_u16()?;
    let module_version = if ver_idx == 0 {
        None
    } else {
        Some(pool.utf8(ver_idx)?.to_string())
    };

    let req_count = cur.read_u16()? as usize;
    let mut requires = Vec::with_capacity(req_count);
    for _ in 0..req_count {
        let ridx = cur.read_u16()?;
        let rname = match pool.get(ridx)? {
            CpEntry::Module(s) => s.clone(),
            _ => String::new(),
        };
        let flags = cur.read_u16()?;
        let vidx = cur.read_u16()?;
        let ver = if vidx == 0 {
            None
        } else {
            Some(pool.utf8(vidx)?.to_string())
        };
        requires.push(ModuleRequires {
            module_name: rname,
            flags,
            version: ver,
        });
    }

    let exp_count = cur.read_u16()? as usize;
    let mut exports = Vec::with_capacity(exp_count);
    for _ in 0..exp_count {
        let pidx = cur.read_u16()?;
        let pname = match pool.get(pidx)? {
            CpEntry::Package(s) => s.clone(),
            _ => String::new(),
        };
        let flags = cur.read_u16()?;
        let to_count = cur.read_u16()? as usize;
        let mut to = Vec::with_capacity(to_count);
        for _ in 0..to_count {
            let tidx = cur.read_u16()?;
            to.push(match pool.get(tidx)? {
                CpEntry::Module(s) => s.clone(),
                _ => String::new(),
            });
        }
        exports.push(ModuleExports {
            package_name: pname,
            flags,
            to_modules: to,
        });
    }

    let open_count = cur.read_u16()? as usize;
    let mut opens = Vec::with_capacity(open_count);
    for _ in 0..open_count {
        let pidx = cur.read_u16()?;
        let pname = match pool.get(pidx)? {
            CpEntry::Package(s) => s.clone(),
            _ => String::new(),
        };
        let flags = cur.read_u16()?;
        let to_count = cur.read_u16()? as usize;
        let mut to = Vec::with_capacity(to_count);
        for _ in 0..to_count {
            let tidx = cur.read_u16()?;
            to.push(match pool.get(tidx)? {
                CpEntry::Module(s) => s.clone(),
                _ => String::new(),
            });
        }
        opens.push(ModuleOpens {
            package_name: pname,
            flags,
            to_modules: to,
        });
    }

    let use_count = cur.read_u16()? as usize;
    let mut uses = Vec::with_capacity(use_count);
    for _ in 0..use_count {
        uses.push(pool.class_name(cur.read_u16()?)?.to_string());
    }

    let prov_count = cur.read_u16()? as usize;
    let mut provides = Vec::with_capacity(prov_count);
    for _ in 0..prov_count {
        let svc = pool.class_name(cur.read_u16()?)?.to_string();
        let wc = cur.read_u16()? as usize;
        let mut with = Vec::with_capacity(wc);
        for _ in 0..wc {
            with.push(pool.class_name(cur.read_u16()?)?.to_string());
        }
        provides.push(ModuleProvides {
            service_name: svc,
            with,
        });
    }

    Ok(Attribute::Module(ModuleAttribute {
        module_name,
        module_flags,
        module_version,
        requires,
        exports,
        opens,
        uses,
        provides,
    }))
}

// ── annotation helpers ─────────────────────────────────────────────────────

fn parse_annotations(cur: &mut Cursor, pool: &ConstantPool) -> Result<Vec<Annotation>> {
    let count = cur.read_u16()? as usize;
    let mut anns = Vec::with_capacity(count);
    for _ in 0..count {
        anns.push(parse_annotation(cur, pool)?);
    }
    Ok(anns)
}

fn parse_annotation(cur: &mut Cursor, pool: &ConstantPool) -> Result<Annotation> {
    let type_idx = cur.read_u16()?;
    let type_descriptor = pool.utf8(type_idx)?.to_string();
    let pair_count = cur.read_u16()? as usize;
    let mut elements = Vec::with_capacity(pair_count);
    for _ in 0..pair_count {
        let name = pool.utf8(cur.read_u16()?)?.to_string();
        let value = parse_element_value(cur, pool)?;
        elements.push((name, value));
    }
    Ok(Annotation {
        type_descriptor,
        elements,
    })
}

fn parse_element_value(cur: &mut Cursor, pool: &ConstantPool) -> Result<ElementValue> {
    let tag = cur.read_u8()? as char;
    use crate::classfile::constant_pool::CpEntry;
    let val = match tag {
        'B' => {
            let idx = cur.read_u16()?;
            ElementValue::Byte(match pool.get(idx)? {
                CpEntry::Integer(v) => *v,
                _ => 0,
            })
        }
        'C' => {
            let idx = cur.read_u16()?;
            ElementValue::Char(match pool.get(idx)? {
                CpEntry::Integer(v) => *v,
                _ => 0,
            })
        }
        'D' => {
            let idx = cur.read_u16()?;
            ElementValue::Double(match pool.get(idx)? {
                CpEntry::Double(v) => *v,
                _ => 0.0,
            })
        }
        'F' => {
            let idx = cur.read_u16()?;
            ElementValue::Float(match pool.get(idx)? {
                CpEntry::Float(v) => *v,
                _ => 0.0,
            })
        }
        'I' => {
            let idx = cur.read_u16()?;
            ElementValue::Int(match pool.get(idx)? {
                CpEntry::Integer(v) => *v,
                _ => 0,
            })
        }
        'J' => {
            let idx = cur.read_u16()?;
            ElementValue::Long(match pool.get(idx)? {
                CpEntry::Long(v) => *v,
                _ => 0,
            })
        }
        'S' => {
            let idx = cur.read_u16()?;
            ElementValue::Short(match pool.get(idx)? {
                CpEntry::Integer(v) => *v,
                _ => 0,
            })
        }
        'Z' => {
            let idx = cur.read_u16()?;
            ElementValue::Boolean(match pool.get(idx)? {
                CpEntry::Integer(v) => *v != 0,
                _ => false,
            })
        }
        's' => {
            let idx = cur.read_u16()?;
            ElementValue::String(pool.utf8(idx)?.to_string())
        }
        'e' => {
            let type_idx = cur.read_u16()?;
            let const_idx = cur.read_u16()?;
            ElementValue::EnumConst {
                type_name: pool.utf8(type_idx)?.to_string(),
                const_name: pool.utf8(const_idx)?.to_string(),
            }
        }
        'c' => {
            let idx = cur.read_u16()?;
            ElementValue::ClassInfo(pool.utf8(idx)?.to_string())
        }
        '@' => ElementValue::Annotation(parse_annotation(cur, pool)?),
        '[' => {
            let count = cur.read_u16()? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                arr.push(parse_element_value(cur, pool)?);
            }
            ElementValue::Array(arr)
        }
        _ => {
            return Err(DecompileError::MalformedAttribute(
                "Annotation",
                format!("unknown element_value tag '{}'", tag),
            ))
        }
    };
    Ok(val)
}

fn parse_parameter_annotations(
    cur: &mut Cursor,
    pool: &ConstantPool,
) -> Result<Vec<Vec<Annotation>>> {
    let param_count = cur.read_u8()? as usize;
    let mut result = Vec::with_capacity(param_count);
    for _ in 0..param_count {
        result.push(parse_annotations(cur, pool)?);
    }
    Ok(result)
}
