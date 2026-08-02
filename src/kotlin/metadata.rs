/// Kotlin metadata parser — decodes @kotlin/Metadata protobuf into structured types.
///
/// Implements enough of the kotlin.metadata protobuf schema to support decompilation
/// of: data class, object, companion object, sealed class, enum class, extension functions,
/// suspend functions, properties (val/var), nullability, type parameters.
use super::protobuf::{self, ProtoReader, WireType};
use crate::classfile::attribute::{Annotation, ElementValue};

// ── Top-level metadata ────────────────────────────────────────────────────

/// The kind of Kotlin class (from the `k` field of @kotlin/Metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataKind {
    Class = 1,
    FileFacade = 2,
    SyntheticClass = 3,
    MultiFileFacade = 4,
    MultiFilePart = 5,
}

/// Parsed Kotlin metadata from a class file.
#[derive(Debug, Clone)]
pub struct KotlinMetadata {
    pub kind: MetadataKind,
    pub metadata_version: Vec<i32>,
    pub extra_int: i32,
    pub extra_string: Option<String>,
    pub package_name: Option<String>,
    pub multi_file_parts: Vec<String>,
    pub class: Option<KClass>,
    pub package: Option<KPackage>,
    pub lambda_function: Option<KFunction>,
}

// ── Class ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    Class = 0,
    Interface = 1,
    EnumClass = 2,
    EnumEntry = 3,
    AnnotationClass = 4,
    Object = 5,
    CompanionObject = 6,
}

/// Flags encoding for Kotlin declarations.
/// Layout (for classes):
///   bit 0:     HAS_ANNOTATIONS
///   bits 1-3:  VISIBILITY (0=Internal,1=Private,2=Protected,3=Public,4=PrivateToThis,5=Local)
///   bits 4-5:  MODALITY (0=Final,1=Open,2=Abstract,3=Sealed)
///   bits 6-8:  CLASS_KIND (0=Class,1=Interface,2=EnumClass,3=EnumEntry,4=AnnotationClass,5=Object,6=CompanionObject)
///   bit 9:     IS_INNER
///   bit 10:    IS_DATA
///   bit 11:    IS_EXTERNAL_CLASS
///   bit 12:    IS_EXPECT_CLASS
///   bit 13:    IS_VALUE_CLASS
///   bit 14:    IS_FUN_INTERFACE
///   bit 15:    HAS_ENUM_ENTRIES
#[derive(Debug, Clone, Copy, Default)]
pub struct KFlags(pub i32);

impl KFlags {
    pub fn has_annotations(self) -> bool {
        self.0 & 1 != 0
    }

    pub fn visibility(self) -> Visibility {
        match (self.0 >> 1) & 0x07 {
            0 => Visibility::Internal,
            1 => Visibility::Private,
            2 => Visibility::Protected,
            3 => Visibility::Public,
            4 => Visibility::PrivateToThis,
            5 => Visibility::Local,
            _ => Visibility::Public,
        }
    }

    pub fn modality(self) -> Modality {
        match (self.0 >> 4) & 0x03 {
            0 => Modality::Final,
            1 => Modality::Open,
            2 => Modality::Abstract,
            3 => Modality::Sealed,
            _ => Modality::Final,
        }
    }

    pub fn class_kind(self) -> ClassKind {
        match (self.0 >> 6) & 0x07 {
            0 => ClassKind::Class,
            1 => ClassKind::Interface,
            2 => ClassKind::EnumClass,
            3 => ClassKind::EnumEntry,
            4 => ClassKind::AnnotationClass,
            5 => ClassKind::Object,
            6 => ClassKind::CompanionObject,
            _ => ClassKind::Class,
        }
    }

    // Class-specific flags (bits 9+)
    pub fn is_inner_class(self) -> bool {
        (self.0 >> 9) & 1 != 0
    }
    pub fn is_data_class(self) -> bool {
        (self.0 >> 10) & 1 != 0
    }
    pub fn is_external_class(self) -> bool {
        (self.0 >> 11) & 1 != 0
    }
    pub fn is_expect_class(self) -> bool {
        (self.0 >> 12) & 1 != 0
    }
    pub fn is_value_class(self) -> bool {
        (self.0 >> 13) & 1 != 0
    }
    pub fn is_fun_interface(self) -> bool {
        (self.0 >> 14) & 1 != 0
    }
}

/// Function flags layout:
///   bit 0:     HAS_ANNOTATIONS
///   bits 1-3:  VISIBILITY
///   bits 4-5:  MODALITY
///   bits 6-7:  MEMBER_KIND (0=Declaration,1=FakeOverride,2=Delegation,3=Synthesized)
///   bit 8:     IS_OPERATOR
///   bit 9:     IS_INFIX
///   bit 10:    IS_INLINE
///   bit 11:    IS_TAILREC
///   bit 12:    IS_EXTERNAL
///   bit 13:    IS_SUSPEND
///   bit 14:    IS_EXPECT
#[derive(Debug, Clone, Copy, Default)]
pub struct KFunctionFlags(pub i32);

impl KFunctionFlags {
    pub fn visibility(self) -> Visibility {
        match (self.0 >> 1) & 0x07 {
            0 => Visibility::Internal,
            1 => Visibility::Private,
            2 => Visibility::Protected,
            3 => Visibility::Public,
            4 => Visibility::PrivateToThis,
            5 => Visibility::Local,
            _ => Visibility::Public,
        }
    }

    pub fn modality(self) -> Modality {
        match (self.0 >> 4) & 0x03 {
            0 => Modality::Final,
            1 => Modality::Open,
            2 => Modality::Abstract,
            3 => Modality::Sealed,
            _ => Modality::Final,
        }
    }

    pub fn is_operator(self) -> bool {
        (self.0 >> 8) & 1 != 0
    }
    pub fn is_infix(self) -> bool {
        (self.0 >> 9) & 1 != 0
    }
    pub fn is_inline(self) -> bool {
        (self.0 >> 10) & 1 != 0
    }
    pub fn is_tailrec(self) -> bool {
        (self.0 >> 11) & 1 != 0
    }
    pub fn is_external(self) -> bool {
        (self.0 >> 12) & 1 != 0
    }
    pub fn is_suspend(self) -> bool {
        (self.0 >> 13) & 1 != 0
    }
    pub fn is_expect(self) -> bool {
        (self.0 >> 14) & 1 != 0
    }
}

/// Property flags layout:
///   bit 0:     HAS_ANNOTATIONS
///   bits 1-3:  VISIBILITY
///   bits 4-5:  MODALITY
///   bits 6-7:  MEMBER_KIND
///   bit 8:     IS_VAR
///   bit 9:     HAS_GETTER
///   bit 10:    HAS_SETTER
///   bit 11:    IS_CONST
///   bit 12:    IS_LATEINIT
///   bit 13:    HAS_CONSTANT
///   bit 14:    IS_EXTERNAL
///   bit 15:    IS_DELEGATED
///   bit 16:    IS_EXPECT
#[derive(Debug, Clone, Copy, Default)]
pub struct KPropertyFlags(pub i32);

impl KPropertyFlags {
    pub fn visibility(self) -> Visibility {
        match (self.0 >> 1) & 0x07 {
            0 => Visibility::Internal,
            1 => Visibility::Private,
            2 => Visibility::Protected,
            3 => Visibility::Public,
            4 => Visibility::PrivateToThis,
            5 => Visibility::Local,
            _ => Visibility::Public,
        }
    }

    pub fn modality(self) -> Modality {
        match (self.0 >> 4) & 0x03 {
            0 => Modality::Final,
            1 => Modality::Open,
            2 => Modality::Abstract,
            3 => Modality::Sealed,
            _ => Modality::Final,
        }
    }

    pub fn is_var(self) -> bool {
        (self.0 >> 8) & 1 != 0
    }
    pub fn is_const(self) -> bool {
        (self.0 >> 11) & 1 != 0
    }
    pub fn is_lateinit(self) -> bool {
        (self.0 >> 12) & 1 != 0
    }
    pub fn has_constant(self) -> bool {
        (self.0 >> 13) & 1 != 0
    }
    pub fn is_delegated(self) -> bool {
        (self.0 >> 15) & 1 != 0
    }
}

/// Constructor flags:
///   bit 0:     HAS_ANNOTATIONS
///   bits 1-3:  VISIBILITY
///   bit 4:     IS_SECONDARY
#[derive(Debug, Clone, Copy, Default)]
pub struct KConstructorFlags(pub i32);

impl KConstructorFlags {
    pub fn visibility(self) -> Visibility {
        match (self.0 >> 1) & 0x07 {
            0 => Visibility::Internal,
            1 => Visibility::Private,
            2 => Visibility::Protected,
            3 => Visibility::Public,
            4 => Visibility::PrivateToThis,
            5 => Visibility::Local,
            _ => Visibility::Public,
        }
    }

    pub fn is_secondary(self) -> bool {
        (self.0 >> 4) & 1 != 0
    }
}

/// ValueParameter flags:
///   bit 0:     HAS_ANNOTATIONS
///   bit 1:     DECLARES_DEFAULT_VALUE
///   bit 2:     IS_CROSSINLINE
///   bit 3:     IS_NOINLINE
#[derive(Debug, Clone, Copy, Default)]
pub struct KValueParamFlags(pub i32);

impl KValueParamFlags {
    pub fn declares_default_value(self) -> bool {
        (self.0 >> 1) & 1 != 0
    }
    pub fn is_crossinline(self) -> bool {
        (self.0 >> 2) & 1 != 0
    }
    pub fn is_noinline(self) -> bool {
        (self.0 >> 3) & 1 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Internal,
    Private,
    Protected,
    Public,
    PrivateToThis,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    Final,
    Open,
    Abstract,
    Sealed,
}

#[derive(Debug, Clone)]
pub struct KClass {
    pub flags: KFlags,
    pub fq_name: Option<String>,
    pub companion_object_name: Option<String>,
    pub type_parameters: Vec<KTypeParameter>,
    pub supertypes: Vec<KType>,
    pub constructors: Vec<KConstructor>,
    pub functions: Vec<KFunction>,
    pub properties: Vec<KProperty>,
    pub enum_entries: Vec<KEnumEntry>,
    pub nested_class_names: Vec<String>,
    pub sealed_subclass_names: Vec<String>,
    pub type_table: Option<KTypeTable>,
}

#[derive(Debug, Clone)]
pub struct KPackage {
    pub functions: Vec<KFunction>,
    pub properties: Vec<KProperty>,
    pub type_table: Option<KTypeTable>,
}

#[derive(Debug, Clone)]
pub struct KFunction {
    pub flags: KFunctionFlags,
    pub name: String,
    pub return_type: Option<KType>,
    pub receiver_type: Option<KType>,
    pub type_parameters: Vec<KTypeParameter>,
    pub value_parameters: Vec<KValueParameter>,
    pub jvm_signature: Option<JvmMemberSignature>,
}

#[derive(Debug, Clone)]
pub struct KProperty {
    pub flags: KPropertyFlags,
    pub name: String,
    pub return_type: Option<KType>,
    pub receiver_type: Option<KType>,
    pub type_parameters: Vec<KTypeParameter>,
    pub getter_flags: i32,
    pub setter_flags: i32,
    pub jvm_signature: Option<JvmPropertySignature>,
}

#[derive(Debug, Clone)]
pub struct KConstructor {
    pub flags: KConstructorFlags,
    pub value_parameters: Vec<KValueParameter>,
    pub jvm_signature: Option<JvmMemberSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JvmMemberSignature {
    pub name: Option<String>,
    pub descriptor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct JvmPropertySignature {
    pub field: Option<JvmMemberSignature>,
    pub synthetic_method: Option<JvmMemberSignature>,
    pub getter: Option<JvmMemberSignature>,
    pub setter: Option<JvmMemberSignature>,
    pub delegate_method: Option<JvmMemberSignature>,
}

#[derive(Debug, Clone)]
pub struct KEnumEntry {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct KValueParameter {
    pub flags: KValueParamFlags,
    pub name: String,
    pub type_: Option<KType>,
    pub vararg_element_type: Option<KType>,
}

#[derive(Debug, Clone)]
pub struct KTypeParameter {
    pub id: i32,
    pub name: String,
    pub reified: bool,
    pub variance: Variance,
    pub upper_bounds: Vec<KType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variance {
    In = 0,
    Out = 1,
    Inv = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    In = 0,
    Out = 1,
    Inv = 2,
    Star = 3,
}

#[derive(Debug, Clone)]
pub struct KType {
    pub flags: KFlags,
    pub nullable: bool,
    pub class_name: Option<String>,
    pub type_parameter_id: Option<i32>,
    pub type_parameter_name: Option<String>,
    pub arguments: Vec<KTypeArgument>,
    pub outer_type: Option<Box<KType>>,
    pub abbreviated_type: Option<Box<KType>>,
}

#[derive(Debug, Clone)]
pub struct KTypeArgument {
    pub projection: Projection,
    pub type_: Option<KType>,
}

#[derive(Debug, Clone)]
pub struct KTypeTable {
    pub types: Vec<KType>,
}

// ── String table / name resolution ────────────────────────────────────────

/// Predefined strings in Kotlin metadata (indices 0..43).
const PREDEFINED: &[&str] = &[
    "kotlin/Any",
    "kotlin/Nothing",
    "kotlin/Unit",
    "kotlin/Throwable",
    "kotlin/Number",
    "kotlin/Byte",
    "kotlin/Double",
    "kotlin/Float",
    "kotlin/Int",
    "kotlin/Long",
    "kotlin/Short",
    "kotlin/Boolean",
    "kotlin/Char",
    "kotlin/CharSequence",
    "kotlin/String",
    "kotlin/Comparable",
    "kotlin/Enum",
    "kotlin/Array",
    "kotlin/ByteArray",
    "kotlin/DoubleArray",
    "kotlin/FloatArray",
    "kotlin/IntArray",
    "kotlin/LongArray",
    "kotlin/ShortArray",
    "kotlin/BooleanArray",
    "kotlin/CharArray",
    "kotlin/Cloneable",
    "kotlin/Annotation",
    "kotlin/collections/Iterable",
    "kotlin/collections/MutableIterable",
    "kotlin/collections/Collection",
    "kotlin/collections/MutableCollection",
    "kotlin/collections/List",
    "kotlin/collections/MutableList",
    "kotlin/collections/Set",
    "kotlin/collections/MutableSet",
    "kotlin/collections/Map",
    "kotlin/collections/MutableMap",
    "kotlin/collections/Map.Entry",
    "kotlin/collections/MutableMap.MutableEntry",
    "kotlin/collections/Iterator",
    "kotlin/collections/MutableIterator",
    "kotlin/collections/ListIterator",
    "kotlin/collections/MutableListIterator",
];

/// StringTableTypes record — describes how to resolve each name index.
#[derive(Debug, Clone)]
struct StringRecord {
    range: i32,
    predefined_index: Option<i32>,
    operation: i32, // 0=NONE, 1=INTERNAL_TO_CLASS_ID, 2=DESC_TO_CLASS_ID
    substring_index: Vec<i32>,
    replace_char: Vec<i32>,
    string: Option<String>,
}

/// Name resolver built from StringTableTypes + d2.
pub struct NameResolver {
    records: Vec<StringRecord>,
    d2: Vec<String>,
}

impl NameResolver {
    /// Build from the StringTableTypes protobuf bytes and the d2 string array.
    pub fn new(table_bytes: &[u8], d2: Vec<String>) -> Self {
        let records = parse_string_table_types(table_bytes);
        NameResolver { records, d2 }
    }

    /// Resolve a name index to a string.
    pub fn resolve(&self, idx: i32) -> String {
        let idx = idx as usize;

        // Find which record covers this index
        let mut record_offset = 0usize;
        let mut target_record: Option<&StringRecord> = None;
        for rec in &self.records {
            let range = rec.range.max(1) as usize;
            if idx >= record_offset && idx < record_offset + range {
                target_record = Some(rec);
                break;
            }
            record_offset += range;
        }

        // Get the raw string
        let mut s = if let Some(rec) = target_record {
            if let Some(ref st) = rec.string {
                st.clone()
            } else if let Some(pi) = rec.predefined_index {
                PREDEFINED.get(pi as usize).unwrap_or(&"").to_string()
            } else {
                self.d2.get(idx).cloned().unwrap_or_default()
            }
        } else {
            self.d2.get(idx).cloned().unwrap_or_default()
        };

        // Apply transformations
        if let Some(rec) = target_record {
            // Substring
            if rec.substring_index.len() >= 2 {
                let start = rec.substring_index[0] as usize;
                let end = rec.substring_index[1] as usize;
                if end <= s.len() && start <= end {
                    s = s[start..end].to_string();
                }
            }
            // Replace char
            if rec.replace_char.len() >= 2 {
                let from = char::from_u32(rec.replace_char[0] as u32).unwrap_or('\0');
                let to = char::from_u32(rec.replace_char[1] as u32).unwrap_or('\0');
                s = s.replace(from, &to.to_string());
            }
            // Operation
            match rec.operation {
                1 => {
                    // INTERNAL_TO_CLASS_ID
                    s = s.replace('$', ".");
                }
                2 => {
                    // DESC_TO_CLASS_ID
                    if s.len() >= 2 {
                        s = s[1..s.len() - 1].to_string(); // strip L...;
                    }
                    s = s.replace('$', ".");
                }
                _ => {}
            }
        }

        s
    }
}

fn parse_string_table_types(data: &[u8]) -> Vec<StringRecord> {
    let mut reader = ProtoReader::new(data);
    let mut records = Vec::new();

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::LengthDelimited) => {
                // Record message
                let Some(bytes) = reader.read_bytes() else {
                    break;
                };
                records.push(parse_string_record(bytes));
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    records
}

fn parse_string_record(data: &[u8]) -> StringRecord {
    let mut reader = ProtoReader::new(data);
    let mut rec = StringRecord {
        range: 1,
        predefined_index: None,
        operation: 0,
        substring_index: Vec::new(),
        replace_char: Vec::new(),
        string: None,
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                rec.range = reader.read_int32().unwrap_or(1);
            }
            (2, WireType::Varint) => {
                rec.predefined_index = reader.read_int32();
            }
            (3, WireType::Varint) => {
                rec.operation = reader.read_int32().unwrap_or(0);
            }
            (4, WireType::Varint) => {
                if let Some(v) = reader.read_int32() {
                    rec.substring_index.push(v);
                }
            }
            (4, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    rec.substring_index
                        .extend(ProtoReader::read_packed_int32(bytes));
                }
            }
            (5, WireType::Varint) => {
                if let Some(v) = reader.read_int32() {
                    rec.replace_char.push(v);
                }
            }
            (5, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    rec.replace_char
                        .extend(ProtoReader::read_packed_int32(bytes));
                }
            }
            (6, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    rec.string = Some(String::from_utf8_lossy(bytes).to_string());
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    rec
}

// ── Top-level parse entry point ───────────────────────────────────────────

/// Extract Kotlin metadata from a class file's annotations.
/// Returns `None` if the class is not a Kotlin class.
pub fn parse_kotlin_metadata(annotations: &[Annotation]) -> Option<KotlinMetadata> {
    // Find @kotlin/Metadata annotation
    let meta_ann = annotations.iter().find(|a| {
        a.type_descriptor == "Lkotlin/Metadata;" || a.type_descriptor == "kotlin/Metadata"
    })?;

    // Extract fields
    let mut k: i32 = 1;
    let mut d1: Vec<String> = Vec::new();
    let mut d2: Vec<String> = Vec::new();
    let mut metadata_version = Vec::new();
    let mut extra_int = 0;
    let mut extra_string = None;
    let mut package_name = None;

    for (name, value) in &meta_ann.elements {
        match name.as_str() {
            "k" => {
                if let ElementValue::Int(v) = value {
                    k = *v;
                }
            }
            "d1" => {
                if let ElementValue::Array(arr) = value {
                    for ev in arr {
                        if let ElementValue::String(s) = ev {
                            d1.push(s.clone());
                        }
                    }
                }
            }
            "d2" => {
                if let ElementValue::Array(arr) = value {
                    for ev in arr {
                        if let ElementValue::String(s) = ev {
                            d2.push(s.clone());
                        }
                    }
                }
            }
            "mv" => {
                if let ElementValue::Array(values) = value {
                    metadata_version.extend(values.iter().filter_map(|value| match value {
                        ElementValue::Int(value) => Some(*value),
                        _ => None,
                    }));
                }
            }
            "xi" => {
                if let ElementValue::Int(value) = value {
                    extra_int = *value;
                }
            }
            "xs" => {
                if let ElementValue::String(value) = value {
                    extra_string = Some(value.clone());
                }
            }
            "pn" => {
                if let ElementValue::String(value) = value {
                    package_name = Some(value.clone());
                }
            }
            _ => {}
        }
    }

    let kind = match k {
        1 => MetadataKind::Class,
        2 => MetadataKind::FileFacade,
        3 => MetadataKind::SyntheticClass,
        4 => MetadataKind::MultiFileFacade,
        5 => MetadataKind::MultiFilePart,
        _ => return None,
    };

    // Decode d1 → raw protobuf bytes
    let raw_bytes = protobuf::decode_bit_encoding(&d1);
    if raw_bytes.is_empty() {
        return Some(KotlinMetadata {
            kind,
            metadata_version,
            extra_int,
            extra_string,
            package_name,
            multi_file_parts: if kind == MetadataKind::MultiFileFacade {
                d1.clone()
            } else {
                Vec::new()
            },
            class: None,
            package: None,
            lambda_function: None,
        });
    }

    // Parse StringTableTypes (length-delimited at the start)
    let mut reader = ProtoReader::new(&raw_bytes);
    let table_bytes = reader.read_bytes().unwrap_or(&[]);
    let resolver = NameResolver::new(table_bytes, d2);

    // Parse the main message from the remaining bytes
    let remaining = &raw_bytes[reader.position()..];

    let mut class = None;
    let mut package = None;
    let mut lambda_function = None;

    match kind {
        MetadataKind::Class => {
            class = Some(parse_class(remaining, &resolver));
        }
        MetadataKind::FileFacade | MetadataKind::MultiFilePart => {
            package = Some(parse_package(remaining, &resolver));
        }
        MetadataKind::SyntheticClass => {
            lambda_function = parse_lambda_function(remaining, &resolver);
        }
        MetadataKind::MultiFileFacade => {
            // d1 contains class names directly, no protobuf
        }
    }

    Some(KotlinMetadata {
        kind,
        metadata_version,
        extra_int,
        extra_string,
        package_name,
        multi_file_parts: if kind == MetadataKind::MultiFileFacade {
            d1
        } else {
            Vec::new()
        },
        class,
        package,
        lambda_function,
    })
}

// ── Class parsing ─────────────────────────────────────────────────────────

fn parse_class(data: &[u8], resolver: &NameResolver) -> KClass {
    let mut reader = ProtoReader::new(data);
    let mut cls = KClass {
        flags: KFlags(6), // default
        fq_name: None,
        companion_object_name: None,
        type_parameters: Vec::new(),
        supertypes: Vec::new(),
        constructors: Vec::new(),
        functions: Vec::new(),
        properties: Vec::new(),
        enum_entries: Vec::new(),
        nested_class_names: Vec::new(),
        sealed_subclass_names: Vec::new(),
        type_table: None,
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                cls.flags = KFlags(reader.read_int32().unwrap_or(6));
            }
            (3, WireType::Varint) => {
                cls.fq_name = reader.read_int32().map(|i| resolver.resolve(i));
            }
            (4, WireType::Varint) => {
                cls.companion_object_name = reader.read_int32().map(|i| resolver.resolve(i));
            }
            (5, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    cls.type_parameters
                        .push(parse_type_parameter(bytes, resolver));
                }
            }
            (6, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    cls.supertypes.push(parse_type(bytes, resolver));
                }
            }
            (7, WireType::Varint) => {
                if let Some(i) = reader.read_int32() {
                    cls.nested_class_names.push(resolver.resolve(i));
                }
            }
            (8, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    cls.constructors.push(parse_constructor(bytes, resolver));
                }
            }
            (9, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    cls.functions.push(parse_function(bytes, resolver));
                }
            }
            (10, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    cls.properties.push(parse_property(bytes, resolver));
                }
            }
            (13, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    cls.enum_entries.push(parse_enum_entry(bytes, resolver));
                }
            }
            (16, WireType::Varint) => {
                if let Some(i) = reader.read_int32() {
                    cls.sealed_subclass_names.push(resolver.resolve(i));
                }
            }
            (30, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    cls.type_table = Some(parse_type_table(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    cls
}

// ── Package parsing ───────────────────────────────────────────────────────

fn parse_package(data: &[u8], resolver: &NameResolver) -> KPackage {
    let mut reader = ProtoReader::new(data);
    let mut pkg = KPackage {
        functions: Vec::new(),
        properties: Vec::new(),
        type_table: None,
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (3, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    pkg.functions.push(parse_function(bytes, resolver));
                }
            }
            (4, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    pkg.properties.push(parse_property(bytes, resolver));
                }
            }
            (30, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    pkg.type_table = Some(parse_type_table(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    pkg
}

// ── Lambda function (k=3) ─────────────────────────────────────────────────

fn parse_lambda_function(data: &[u8], resolver: &NameResolver) -> Option<KFunction> {
    // For synthetic classes (k=3), d1 contains a ProtoBuf.Function directly
    // after the StringTableTypes, or wrapped in a Package-like message.
    let func = parse_function(data, resolver);
    if func.name.is_empty() {
        // Try as package wrapper
        let pkg = parse_package(data, resolver);
        pkg.functions.into_iter().next()
    } else {
        Some(func)
    }
}

// ── Function parsing ──────────────────────────────────────────────────────

fn parse_function(data: &[u8], resolver: &NameResolver) -> KFunction {
    let mut reader = ProtoReader::new(data);
    let mut func = KFunction {
        flags: KFunctionFlags(6), // default: public, final
        name: String::new(),
        return_type: None,
        receiver_type: None,
        type_parameters: Vec::new(),
        value_parameters: Vec::new(),
        jvm_signature: None,
    };
    let mut has_flags = false;

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (9, WireType::Varint) => {
                func.flags = KFunctionFlags(reader.read_int32().unwrap_or(6));
                has_flags = true;
            }
            (1, WireType::Varint) => {
                // oldFlags (pre-1.7)
                if !has_flags {
                    func.flags = KFunctionFlags(reader.read_int32().unwrap_or(6));
                    has_flags = true;
                } else {
                    reader.read_varint();
                }
            }
            (2, WireType::Varint) => {
                func.name = reader
                    .read_int32()
                    .map(|i| resolver.resolve(i))
                    .unwrap_or_default();
            }
            (3, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    func.return_type = Some(parse_type(bytes, resolver));
                }
            }
            (4, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    func.type_parameters
                        .push(parse_type_parameter(bytes, resolver));
                }
            }
            (5, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    func.receiver_type = Some(parse_type(bytes, resolver));
                }
            }
            (6, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    func.value_parameters
                        .push(parse_value_parameter(bytes, resolver));
                }
            }
            (100, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    func.jvm_signature = Some(parse_jvm_member_signature(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    func
}

// ── Property parsing ──────────────────────────────────────────────────────

fn parse_property(data: &[u8], resolver: &NameResolver) -> KProperty {
    let mut reader = ProtoReader::new(data);
    let mut prop = KProperty {
        flags: KPropertyFlags(6), // default: public, final
        name: String::new(),
        return_type: None,
        receiver_type: None,
        type_parameters: Vec::new(),
        getter_flags: 0,
        setter_flags: 0,
        jvm_signature: None,
    };
    let mut has_flags = false;

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (11, WireType::Varint) => {
                prop.flags = KPropertyFlags(reader.read_int32().unwrap_or(6));
                has_flags = true;
            }
            (1, WireType::Varint) => {
                if !has_flags {
                    prop.flags = KPropertyFlags(reader.read_int32().unwrap_or(6));
                    has_flags = true;
                } else {
                    reader.read_varint();
                }
            }
            (2, WireType::Varint) => {
                prop.name = reader
                    .read_int32()
                    .map(|i| resolver.resolve(i))
                    .unwrap_or_default();
            }
            (3, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    prop.return_type = Some(parse_type(bytes, resolver));
                }
            }
            (4, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    prop.type_parameters
                        .push(parse_type_parameter(bytes, resolver));
                }
            }
            (5, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    prop.receiver_type = Some(parse_type(bytes, resolver));
                }
            }
            (7, WireType::Varint) => {
                prop.getter_flags = reader.read_int32().unwrap_or(0);
            }
            (8, WireType::Varint) => {
                prop.setter_flags = reader.read_int32().unwrap_or(0);
            }
            (100, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    prop.jvm_signature = Some(parse_jvm_property_signature(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    prop
}

// ── Constructor parsing ───────────────────────────────────────────────────

fn parse_constructor(data: &[u8], resolver: &NameResolver) -> KConstructor {
    let mut reader = ProtoReader::new(data);
    let mut ctor = KConstructor {
        flags: KConstructorFlags(6),
        value_parameters: Vec::new(),
        jvm_signature: None,
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                ctor.flags = KConstructorFlags(reader.read_int32().unwrap_or(6));
            }
            (2, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    ctor.value_parameters
                        .push(parse_value_parameter(bytes, resolver));
                }
            }
            (100, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    ctor.jvm_signature = Some(parse_jvm_member_signature(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    ctor
}

fn parse_jvm_member_signature(data: &[u8], resolver: &NameResolver) -> JvmMemberSignature {
    let mut reader = ProtoReader::new(data);
    let mut signature = JvmMemberSignature {
        name: None,
        descriptor: None,
    };
    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                signature.name = reader.read_int32().map(|i| resolver.resolve(i));
            }
            (2, WireType::Varint) => {
                signature.descriptor = reader.read_int32().map(|i| resolver.resolve(i));
            }
            _ => {
                reader.skip(wt);
            }
        }
    }
    signature
}

fn parse_jvm_property_signature(data: &[u8], resolver: &NameResolver) -> JvmPropertySignature {
    let mut reader = ProtoReader::new(data);
    let mut signature = JvmPropertySignature::default();
    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        if wt != WireType::LengthDelimited {
            reader.skip(wt);
            continue;
        }
        let Some(bytes) = reader.read_bytes() else {
            break;
        };
        let member = Some(parse_jvm_member_signature(bytes, resolver));
        match field {
            1 => signature.field = member,
            2 => signature.synthetic_method = member,
            3 => signature.getter = member,
            4 => signature.setter = member,
            5 => signature.delegate_method = member,
            _ => {}
        }
    }
    signature
}

// ── ValueParameter parsing ────────────────────────────────────────────────

fn parse_value_parameter(data: &[u8], resolver: &NameResolver) -> KValueParameter {
    let mut reader = ProtoReader::new(data);
    let mut vp = KValueParameter {
        flags: KValueParamFlags(0),
        name: String::new(),
        type_: None,
        vararg_element_type: None,
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                vp.flags = KValueParamFlags(reader.read_int32().unwrap_or(0));
            }
            (2, WireType::Varint) => {
                vp.name = reader
                    .read_int32()
                    .map(|i| resolver.resolve(i))
                    .unwrap_or_default();
            }
            (3, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    vp.type_ = Some(parse_type(bytes, resolver));
                }
            }
            (4, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    vp.vararg_element_type = Some(parse_type(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    vp
}

// ── TypeParameter parsing ─────────────────────────────────────────────────

fn parse_type_parameter(data: &[u8], resolver: &NameResolver) -> KTypeParameter {
    let mut reader = ProtoReader::new(data);
    let mut tp = KTypeParameter {
        id: 0,
        name: String::new(),
        reified: false,
        variance: Variance::Inv,
        upper_bounds: Vec::new(),
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                tp.id = reader.read_int32().unwrap_or(0);
            }
            (2, WireType::Varint) => {
                tp.name = reader
                    .read_int32()
                    .map(|i| resolver.resolve(i))
                    .unwrap_or_default();
            }
            (3, WireType::Varint) => {
                tp.reified = reader.read_bool().unwrap_or(false);
            }
            (4, WireType::Varint) => {
                tp.variance = match reader.read_int32().unwrap_or(2) {
                    0 => Variance::In,
                    1 => Variance::Out,
                    _ => Variance::Inv,
                };
            }
            (5, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    tp.upper_bounds.push(parse_type(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    tp
}

// ── Type parsing ──────────────────────────────────────────────────────────

fn parse_type(data: &[u8], resolver: &NameResolver) -> KType {
    let mut reader = ProtoReader::new(data);
    let mut ty = KType {
        flags: KFlags(0),
        nullable: false,
        class_name: None,
        type_parameter_id: None,
        type_parameter_name: None,
        arguments: Vec::new(),
        outer_type: None,
        abbreviated_type: None,
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                ty.flags = KFlags(reader.read_int32().unwrap_or(0));
            }
            (2, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    ty.arguments.push(parse_type_argument(bytes, resolver));
                }
            }
            (3, WireType::Varint) => {
                ty.nullable = reader.read_bool().unwrap_or(false);
            }
            (6, WireType::Varint) => {
                ty.class_name = reader.read_int32().map(|i| resolver.resolve(i));
            }
            (7, WireType::Varint) => {
                ty.type_parameter_id = reader.read_int32();
            }
            (9, WireType::Varint) => {
                ty.type_parameter_name = reader.read_int32().map(|i| resolver.resolve(i));
            }
            (10, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    ty.outer_type = Some(Box::new(parse_type(bytes, resolver)));
                }
            }
            (12, WireType::Varint) => {
                // ProtoBuf.Type.typeAliasName.  Type aliases still need their
                // source-level name when rendered; treating them as unnamed
                // types degrades aliases such as kotlin.collections.RandomAccess
                // to Any.
                ty.class_name = reader.read_int32().map(|i| resolver.resolve(i));
            }
            (13, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    ty.abbreviated_type = Some(Box::new(parse_type(bytes, resolver)));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    ty
}

// ── TypeArgument parsing ──────────────────────────────────────────────────

fn parse_type_argument(data: &[u8], resolver: &NameResolver) -> KTypeArgument {
    let mut reader = ProtoReader::new(data);
    let mut arg = KTypeArgument {
        projection: Projection::Inv,
        type_: None,
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                arg.projection = match reader.read_int32().unwrap_or(2) {
                    0 => Projection::In,
                    1 => Projection::Out,
                    2 => Projection::Inv,
                    3 => Projection::Star,
                    _ => Projection::Inv,
                };
            }
            (2, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    arg.type_ = Some(parse_type(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    arg
}

// ── EnumEntry parsing ─────────────────────────────────────────────────────

fn parse_enum_entry(data: &[u8], resolver: &NameResolver) -> KEnumEntry {
    let mut reader = ProtoReader::new(data);
    let mut entry = KEnumEntry {
        name: String::new(),
    };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::Varint) => {
                entry.name = reader
                    .read_int32()
                    .map(|i| resolver.resolve(i))
                    .unwrap_or_default();
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    entry
}

// ── TypeTable parsing ─────────────────────────────────────────────────────

fn parse_type_table(data: &[u8], resolver: &NameResolver) -> KTypeTable {
    let mut reader = ProtoReader::new(data);
    let mut table = KTypeTable { types: Vec::new() };

    while !reader.is_empty() {
        let Some((field, wt)) = reader.read_tag() else {
            break;
        };
        match (field, wt) {
            (1, WireType::LengthDelimited) => {
                if let Some(bytes) = reader.read_bytes() {
                    table.types.push(parse_type(bytes, resolver));
                }
            }
            _ => {
                reader.skip(wt);
            }
        }
    }

    table
}

#[cfg(test)]
mod tests {
    use super::{parse_type, NameResolver};

    #[test]
    fn type_alias_name_is_preserved_as_a_renderable_type() {
        let resolver = NameResolver::new(&[], vec!["kotlin/collections/RandomAccess".into()]);
        let ty = parse_type(&[0x60, 0x00], &resolver);
        assert_eq!(
            ty.class_name.as_deref(),
            Some("kotlin/collections/RandomAccess")
        );
    }
}
