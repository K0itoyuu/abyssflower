/// Kotlin source writer — generates Kotlin source from ClassFile + KotlinMetadata.
///
/// Handles: data class, object, companion object, sealed class, enum class,
/// val/var properties, fun with receiver, suspend/inline/infix modifiers,
/// nullable types, when expressions, lambda syntax.

use crate::classfile::attribute::Attribute;
use crate::classfile::ClassFile;
use super::body_writer::decompile_kotlin_body;
use super::metadata::*;
use super::types::*;

/// Check if a class file is a Kotlin class (has @kotlin/Metadata).
pub fn is_kotlin_class(cf: &ClassFile) -> bool {
    get_kotlin_annotations(cf).is_some()
}

/// Get RuntimeVisibleAnnotations from a class file.
fn get_kotlin_annotations(cf: &ClassFile) -> Option<&[crate::classfile::attribute::Annotation]> {
    for attr in &cf.attributes {
        if let Attribute::RuntimeVisibleAnnotations(anns) = attr {
            if anns.iter().any(|a| a.type_descriptor == "Lkotlin/Metadata;") {
                return Some(anns);
            }
        }
    }
    None
}

/// Main entry: render a Kotlin class file to Kotlin source.
pub fn render_kotlin_class(cf: &ClassFile) -> String {
    let anns = match get_kotlin_annotations(cf) {
        Some(a) => a,
        None => return String::from("// Not a Kotlin class\n"),
    };

    let metadata = match parse_kotlin_metadata(anns) {
        Some(m) => m,
        None => return String::from("// Failed to parse Kotlin metadata\n"),
    };

    let mut out = String::new();

    // Package declaration
    if let Some(pkg) = extract_package(&cf.this_class) {
        out.push_str(&format!("package {}\n\n", pkg.replace('/', ".")));
    }

    match metadata.kind {
        MetadataKind::Class => {
            if let Some(ref cls) = metadata.class {
                render_class_decl(&mut out, cf, cls);
            }
        }
        MetadataKind::FileFacade | MetadataKind::MultiFilePart => {
            if let Some(ref pkg) = metadata.package {
                render_file_facade(&mut out, cf, pkg);
            }
        }
        MetadataKind::SyntheticClass => {
            // Lambda / synthetic — usually not decompiled standalone
            out.push_str("// Synthetic class (lambda)\n");
        }
        MetadataKind::MultiFileFacade => {
            out.push_str("// Multi-file facade\n");
        }
    }

    out
}

// ── Class declaration rendering ───────────────────────────────────────────

fn render_class_decl(out: &mut String, cf: &ClassFile, cls: &KClass) {
    let kind = cls.flags.class_kind();

    // Annotations (skip @kotlin/Metadata and JetBrains nullability)
    render_class_annotations(out, cf);

    // Class header
    let mut header = String::new();

    // Visibility
    let vis = cls.flags.visibility();
    match vis {
        Visibility::Public => {} // default in Kotlin, omit
        Visibility::Private => header.push_str("private "),
        Visibility::Protected => header.push_str("protected "),
        Visibility::Internal => header.push_str("internal "),
        _ => {}
    }

    // Modality
    let modality = cls.flags.modality();
    match modality {
        Modality::Abstract => {
            if kind != ClassKind::Interface && kind != ClassKind::AnnotationClass {
                header.push_str("abstract ");
            }
        }
        Modality::Open => header.push_str("open "),
        Modality::Sealed => header.push_str("sealed "),
        Modality::Final => {} // default
    }

    // Special class modifiers
    if cls.flags.is_data_class() { header.push_str("data "); }
    if cls.flags.is_inner_class() { header.push_str("inner "); }
    if cls.flags.is_value_class() { header.push_str("value "); }
    if cls.flags.is_fun_interface() { header.push_str("fun "); }

    // Class keyword
    match kind {
        ClassKind::Class => header.push_str("class "),
        ClassKind::Interface => header.push_str("interface "),
        ClassKind::EnumClass => header.push_str("enum class "),
        ClassKind::Object => header.push_str("object "),
        ClassKind::CompanionObject => header.push_str("companion object "),
        ClassKind::AnnotationClass => header.push_str("annotation class "),
        ClassKind::EnumEntry => header.push_str("/* enum entry */ "),
    }

    // Class name
    let simple = cf.simple_name();
    // Strip $Companion suffix for companion objects
    let name = if kind == ClassKind::CompanionObject {
        simple.rsplit('$').next().unwrap_or(simple)
    } else {
        simple.rsplit('$').next().unwrap_or(simple)
    };
    header.push_str(name);

    // Type parameters
    if !cls.type_parameters.is_empty() {
        header.push_str(&render_type_params_decl(&cls.type_parameters));
    }

    // Primary constructor (for data classes and regular classes)
    if cls.flags.is_data_class() || has_primary_constructor(cls) {
        if let Some(primary) = cls.constructors.first() {
            header.push_str(&render_primary_constructor(primary, cls, cf));
        }
    }

    // Supertypes
    let supers = render_supertypes(cls, &cls.type_parameters);
    if !supers.is_empty() {
        header.push_str(" : ");
        header.push_str(&supers);
    }

    out.push_str(&header);

    // Body
    let visible_funcs: Vec<_> = cls.functions.iter()
        .filter(|f| {
            if cls.flags.is_data_class() {
                let name = f.name.as_str();
                !(name == "copy" || name == "toString" || name == "hashCode"
                    || name == "equals" || name.starts_with("component"))
            } else { true }
        })
        .collect();

    let visible_props: Vec<_> = cls.properties.iter()
        .filter(|p| {
            if cls.flags.is_data_class() {
                let in_ctor = cls.constructors.first()
                    .map(|c| c.value_parameters.iter().any(|vp| vp.name == p.name))
                    .unwrap_or(false);
                !in_ctor
            } else { true }
        })
        .collect();

    let has_body = !visible_funcs.is_empty()
        || !visible_props.is_empty()
        || !cls.enum_entries.is_empty()
        || cls.companion_object_name.is_some();

    if has_body {
        out.push_str(" {\n");
        render_class_body(out, cf, cls);
        out.push_str("}\n");
    } else {
        out.push('\n');
    }
}

fn has_primary_constructor(cls: &KClass) -> bool {
    // A primary constructor has no specific flag in metadata;
    // typically it's the first one if it's not secondary
    !cls.constructors.is_empty()
        && cls.flags.class_kind() != ClassKind::Interface
        && cls.flags.class_kind() != ClassKind::Object
}

fn render_primary_constructor(ctor: &KConstructor, cls: &KClass, _cf: &ClassFile) -> String {
    if ctor.value_parameters.is_empty() {
        return String::new();
    }

    let vis = ctor.flags.visibility();
    let vis_str = match vis {
        Visibility::Public => "",
        Visibility::Private => "private constructor",
        Visibility::Protected => "protected constructor",
        Visibility::Internal => "internal constructor",
        _ => "",
    };

    let params: Vec<String> = ctor.value_parameters.iter().map(|p| {
        let mut s = String::new();

        // For data classes, params are val/var
        if cls.flags.is_data_class() {
            // Check if there's a matching property
            let is_var = cls.properties.iter().any(|prop| prop.name == p.name && prop.flags.is_var());
            if is_var {
                s.push_str("var ");
            } else {
                s.push_str("val ");
            }
        }

        s.push_str(&p.name);
        s.push_str(": ");
        if let Some(ref ty) = p.type_ {
            s.push_str(&render_kotlin_type(ty, &cls.type_parameters));
        } else {
            s.push_str("Any?");
        }

        if p.flags.declares_default_value() {
            s.push_str(" = ...");
        }

        s
    }).collect();

    if vis_str.is_empty() {
        format!("({})", params.join(", "))
    } else {
        format!(" {}({})", vis_str, params.join(", "))
    }
}

fn render_supertypes(cls: &KClass, type_params: &[KTypeParameter]) -> String {
    let kind = cls.flags.class_kind();
    let supers: Vec<String> = cls.supertypes.iter()
        .filter(|t| {
            // Filter out kotlin/Any
            if t.class_name.as_deref() == Some("kotlin/Any") { return false; }
            // Filter out kotlin/Enum for enum classes
            if kind == ClassKind::EnumClass && t.class_name.as_deref() == Some("kotlin/Enum") {
                return false;
            }
            true
        })
        .map(|t| render_kotlin_type(t, type_params))
        .collect();
    supers.join(", ")
}

// ── Class body rendering ──────────────────────────────────────────────────

fn render_class_body(out: &mut String, cf: &ClassFile, cls: &KClass) {
    let tp = &cls.type_parameters;
    let in_interface = cls.flags.class_kind() == ClassKind::Interface;

    // Enum entries
    if !cls.enum_entries.is_empty() {
        let entries: Vec<&str> = cls.enum_entries.iter()
            .map(|e| e.name.as_str())
            .collect();
        out.push_str("    ");
        out.push_str(&entries.join(",\n    "));
        if cls.properties.is_empty() && cls.functions.is_empty() {
            out.push('\n');
        } else {
            out.push_str(";\n\n");
        }
    }

    // Companion object placeholder
    if let Some(ref companion_name) = cls.companion_object_name {
        out.push_str(&format!("    companion object {} {{}}\n\n", companion_name));
    }

    // Properties
    for prop in &cls.properties {
        // Skip properties already declared in primary constructor (data class)
        if cls.flags.is_data_class() {
            let in_ctor = cls.constructors.first()
                .map(|c| c.value_parameters.iter().any(|p| p.name == prop.name))
                .unwrap_or(false);
            if in_ctor { continue; }
        }
        render_property(out, prop, tp, in_interface, cf);
    }

    // Functions
    for func in &cls.functions {
        // Skip auto-generated data class methods
        if cls.flags.is_data_class() {
            let name = func.name.as_str();
            if name == "copy" || name == "toString" || name == "hashCode" || name == "equals"
                || name.starts_with("component") {
                continue;
            }
        }
        render_function(out, func, tp, in_interface, cf);
    }
}

// ── Property rendering ────────────────────────────────────────────────────

fn render_property(out: &mut String, prop: &KProperty, type_params: &[KTypeParameter], in_interface: bool, cf: &ClassFile) {
    out.push_str("    ");

    // Visibility
    let vis = prop.flags.visibility();
    match vis {
        Visibility::Private => out.push_str("private "),
        Visibility::Protected => out.push_str("protected "),
        Visibility::Internal => out.push_str("internal "),
        _ => {}
    }

    // Modifiers
    if prop.flags.is_const() { out.push_str("const "); }
    if prop.flags.is_lateinit() { out.push_str("lateinit "); }

    // Modality (suppress abstract in interfaces)
    if !in_interface {
        match prop.flags.modality() {
            Modality::Abstract => out.push_str("abstract "),
            Modality::Open => out.push_str("open "),
            Modality::Sealed => out.push_str("sealed "),
            Modality::Final => {}
        }
    } else if prop.flags.modality() == Modality::Open {
        out.push_str("open ");
    }

    // val/var
    if prop.flags.is_var() {
        out.push_str("var ");
    } else {
        out.push_str("val ");
    }

    // Type parameters
    if !prop.type_parameters.is_empty() {
        out.push_str(&render_type_params_decl(&prop.type_parameters));
        out.push(' ');
    }

    // Extension receiver
    if let Some(ref recv) = prop.receiver_type {
        out.push_str(&render_kotlin_type(recv, type_params));
        out.push('.');
    }

    // Name
    out.push_str(&prop.name);

    // Type
    out.push_str(": ");
    if let Some(ref ty) = prop.return_type {
        out.push_str(&render_kotlin_type(ty, type_params));
    } else {
        out.push_str("Any?");
    }

    // Try to get initial value from ConstantValue attribute or field
    if let Some(init_val) = get_field_initial_value(cf, &prop.name) {
        out.push_str(&format!(" = {}", init_val));
        out.push('\n');
        return; // Properties with initializers don't need custom getter shown
    }

    // Try to render custom getter body
    if let Some(getter_body) = find_property_getter_body(cf, &prop.name) {
        // Only show custom getter if it's not a simple field access
        let trimmed = getter_body.trim();
        if !trimmed.is_empty() && !is_simple_field_getter(trimmed, &prop.name) {
            out.push('\n');
            out.push_str("        get() {\n");
            out.push_str(&getter_body);
            out.push_str("        }\n");
            return;
        }
    }

    out.push('\n');
}

// ── Function rendering ────────────────────────────────────────────────────

fn render_function(out: &mut String, func: &KFunction, type_params: &[KTypeParameter], in_interface: bool, cf: &ClassFile) {
    out.push_str("    ");

    // Visibility
    let vis = func.flags.visibility();
    match vis {
        Visibility::Private => out.push_str("private "),
        Visibility::Protected => out.push_str("protected "),
        Visibility::Internal => out.push_str("internal "),
        _ => {}
    }

    // Modality (suppress abstract in interfaces — it's implicit)
    if !in_interface {
        match func.flags.modality() {
            Modality::Abstract => out.push_str("abstract "),
            Modality::Open => out.push_str("open "),
            Modality::Final => {} // default
            Modality::Sealed => {}
        }
    } else if func.flags.modality() == Modality::Open {
        out.push_str("open ");
    }

    // Modifiers
    if func.flags.is_inline() { out.push_str("inline "); }
    if func.flags.is_infix() { out.push_str("infix "); }
    if func.flags.is_operator() { out.push_str("operator "); }
    if func.flags.is_suspend() { out.push_str("suspend "); }
    if func.flags.is_tailrec() { out.push_str("tailrec "); }
    if func.flags.is_external() { out.push_str("external "); }

    out.push_str("fun ");

    // Function type parameters
    if !func.type_parameters.is_empty() {
        out.push_str(&render_type_params_decl(&func.type_parameters));
        out.push(' ');
    }

    // Extension receiver
    if let Some(ref recv) = func.receiver_type {
        out.push_str(&render_kotlin_type(recv, type_params));
        out.push('.');
    }

    // Name
    out.push_str(&func.name);

    // Parameters — extract defaults from $default method
    let defaults = extract_default_values(cf, &func.name);
    out.push('(');
    let params: Vec<String> = func.value_parameters.iter().enumerate().map(|(idx, p)| {
        let mut s = String::new();

        if p.flags.is_crossinline() { s.push_str("crossinline "); }
        if p.flags.is_noinline() { s.push_str("noinline "); }

        if p.vararg_element_type.is_some() {
            s.push_str("vararg ");
        }

        s.push_str(&p.name);
        s.push_str(": ");

        if let Some(ref vararg_ty) = p.vararg_element_type {
            s.push_str(&render_kotlin_type(vararg_ty, type_params));
        } else if let Some(ref ty) = p.type_ {
            s.push_str(&render_kotlin_type(ty, type_params));
        } else {
            s.push_str("Any?");
        }

        if p.flags.declares_default_value() {
            if let Some(val) = defaults.get(&idx) {
                s.push_str(&format!(" = {}", val));
            } else {
                s.push_str(" = ...");
            }
        }

        s
    }).collect();
    out.push_str(&params.join(", "));
    out.push(')');

    // Return type
    if let Some(ref ret) = func.return_type {
        let ret_str = render_kotlin_type(ret, type_params);
        if ret_str != "Unit" {
            out.push_str(": ");
            out.push_str(&ret_str);
        }
    }

    // Body placeholder (abstract functions don't have body)
    if func.flags.modality() == Modality::Abstract || in_interface && func.flags.modality() != Modality::Open {
        out.push('\n');
    } else {
        // Try to find and decompile the method body from bytecode
        let body = find_and_decompile(cf, &func.name);
        if let Some(body_text) = body {
            if body_text.trim().is_empty() {
                out.push_str(" {}\n");
            } else {
                out.push_str(" {\n");
                out.push_str(&body_text);
                out.push_str("    }\n");
            }
        } else {
            out.push_str(" {}\n");
        }
    }
}

// ── File facade rendering ─────────────────────────────────────────────────

fn render_file_facade(out: &mut String, _cf: &ClassFile, pkg: &KPackage) {
    let tp: &[KTypeParameter] = &[];

    for prop in &pkg.properties {
        render_top_level_property(out, prop, tp);
    }

    if !pkg.properties.is_empty() && !pkg.functions.is_empty() {
        out.push('\n');
    }

    for func in &pkg.functions {
        render_top_level_function(out, func, tp, _cf);
    }
}

fn render_top_level_property(out: &mut String, prop: &KProperty, type_params: &[KTypeParameter]) {
    // Visibility
    let vis = prop.flags.visibility();
    match vis {
        Visibility::Private => out.push_str("private "),
        Visibility::Protected => out.push_str("protected "),
        Visibility::Internal => out.push_str("internal "),
        _ => {}
    }

    if prop.flags.is_const() { out.push_str("const "); }
    if prop.flags.is_lateinit() { out.push_str("lateinit "); }

    if prop.flags.is_var() {
        out.push_str("var ");
    } else {
        out.push_str("val ");
    }

    if let Some(ref recv) = prop.receiver_type {
        out.push_str(&render_kotlin_type(recv, type_params));
        out.push('.');
    }

    out.push_str(&prop.name);
    out.push_str(": ");
    if let Some(ref ty) = prop.return_type {
        out.push_str(&render_kotlin_type(ty, type_params));
    } else {
        out.push_str("Any?");
    }
    out.push('\n');
}

fn render_top_level_function(out: &mut String, func: &KFunction, type_params: &[KTypeParameter], cf: &ClassFile) {
    // Visibility
    let vis = func.flags.visibility();
    match vis {
        Visibility::Private => out.push_str("private "),
        Visibility::Protected => out.push_str("protected "),
        Visibility::Internal => out.push_str("internal "),
        _ => {}
    }

    // Modifiers
    if func.flags.is_inline() { out.push_str("inline "); }
    if func.flags.is_infix() { out.push_str("infix "); }
    if func.flags.is_operator() { out.push_str("operator "); }
    if func.flags.is_suspend() { out.push_str("suspend "); }
    if func.flags.is_tailrec() { out.push_str("tailrec "); }
    if func.flags.is_external() { out.push_str("external "); }

    out.push_str("fun ");

    if !func.type_parameters.is_empty() {
        out.push_str(&render_type_params_decl(&func.type_parameters));
        out.push(' ');
    }

    if let Some(ref recv) = func.receiver_type {
        out.push_str(&render_kotlin_type(recv, type_params));
        out.push('.');
    }

    out.push_str(&func.name);
    let defaults = extract_default_values(cf, &func.name);
    out.push('(');
    let params: Vec<String> = func.value_parameters.iter().enumerate().map(|(idx, p)| {
        let mut s = String::new();
        if p.flags.is_crossinline() { s.push_str("crossinline "); }
        if p.flags.is_noinline() { s.push_str("noinline "); }
        if p.vararg_element_type.is_some() { s.push_str("vararg "); }
        s.push_str(&p.name);
        s.push_str(": ");
        if let Some(ref vararg_ty) = p.vararg_element_type {
            s.push_str(&render_kotlin_type(vararg_ty, type_params));
        } else if let Some(ref ty) = p.type_ {
            s.push_str(&render_kotlin_type(ty, type_params));
        } else {
            s.push_str("Any?");
        }
        if p.flags.declares_default_value() {
            if let Some(val) = defaults.get(&idx) {
                s.push_str(&format!(" = {}", val));
            } else {
                s.push_str(" = ...");
            }
        }
        s
    }).collect();
    out.push_str(&params.join(", "));
    out.push(')');

    if let Some(ref ret) = func.return_type {
        let ret_str = render_kotlin_type(ret, type_params);
        if ret_str != "Unit" {
            out.push_str(": ");
            out.push_str(&ret_str);
        }
    }

    // Top-level function body decompilation
    let body = find_and_decompile(cf, &func.name);
    if let Some(body_text) = body {
        if body_text.trim().is_empty() {
            out.push_str(" {}\n");
        } else {
            out.push_str(" {\n");
            out.push_str(&body_text);
            out.push_str("}\n");
        }
    } else {
        out.push_str(" {}\n");
    }
}

// ── Body decompilation helper ─────────────────────────────────────────────

/// Find a method by name in the class file and decompile its body.
/// Returns None if no matching method found or method has no Code attribute.
fn find_and_decompile(cf: &ClassFile, func_name: &str) -> Option<String> {
    for m in &cf.methods {
        if m.name == func_name && m.code().is_some() {
            let body = decompile_kotlin_body(m, cf, 2)?;
            return Some(body);
        }
    }
    None
}

/// Extract default parameter values from the $default synthetic method.
/// Returns a map of parameter index → default value string.
fn extract_default_values(cf: &ClassFile, func_name: &str) -> std::collections::HashMap<usize, String> {
    use crate::classfile::constant_pool::CpEntry;
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;

    let mut defaults = std::collections::HashMap::new();
    let default_method_name = format!("{}$default", func_name);

    for m in &cf.methods {
        if m.name != default_method_name { continue; }
        let code = match m.code() {
            Some(c) => c,
            None => continue,
        };

        let insns = &code.instructions;
        // Scan for pattern: iload(mask) + iconst(bit) + iand + ifeq + value + store
        let mut i = 0;
        while i + 4 < insns.len() {
            // Look for iand (0x7e)
            if insns[i + 2].opcode != 0x7e { i += 1; continue; }
            // Check ifeq after iand
            if insns[i + 3].opcode != opc::ifeq { i += 1; continue; }

            // Get the bit value from insns[i+1]
            let bit = match insns[i + 1].opcode {
                opc::iconst_0 => 0i32, // shouldn't happen
                opc::iconst_1 => 1,
                opc::iconst_2 => 2,
                opc::iconst_3 => 3,
                opc::iconst_4 => 4,
                opc::iconst_5 => 5,
                _ => match &insns[i + 1].kind {
                    InsnKind::BytePush { value } => *value as i32,
                    InsnKind::ShortPush { value } => *value as i32,
                    _ => { i += 1; continue; }
                }
            };

            // Determine parameter index from bit (bit 0 = param 0, bit 1 = param 1, etc.)
            let param_idx = bit.trailing_zeros() as usize;

            // The value instruction is at i+4 (first instruction after ifeq)
            if i + 4 >= insns.len() { break; }
            let val_insn = &insns[i + 4];
            let default_val = match val_insn.opcode {
                opc::iconst_m1 => Some("-1".into()),
                opc::iconst_0 => Some("0".into()),
                opc::iconst_1 => Some("1".into()),
                opc::iconst_2 => Some("2".into()),
                opc::iconst_3 => Some("3".into()),
                opc::iconst_4 => Some("4".into()),
                opc::iconst_5 => Some("5".into()),
                opc::lconst_0 => Some("0L".into()),
                opc::lconst_1 => Some("1L".into()),
                opc::fconst_0 => Some("0.0f".into()),
                opc::fconst_1 => Some("1.0f".into()),
                opc::dconst_0 => Some("0.0".into()),
                opc::dconst_1 => Some("1.0".into()),
                opc::aconst_null => Some("null".into()),
                opc::ldc | opc::ldc_w | opc::ldc2_w => {
                    let cp_idx = match &val_insn.kind {
                        InsnKind::Ldc { index } => *index,
                        InsnKind::Cp { index } => *index,
                        _ => { i += 1; continue; }
                    };
                    if let Ok(entry) = cf.constant_pool.get(cp_idx) {
                        match entry {
                            CpEntry::Integer(v) => Some(v.to_string()),
                            CpEntry::Long(v) => Some(format!("{}L", v)),
                            CpEntry::Float(v) => Some(format!("{}f", v)),
                            CpEntry::Double(v) => Some(v.to_string()),
                            CpEntry::String(s) => Some(format!("\"{}\"", s.replace('"', "\\\""))),
                            _ => None,
                        }
                    } else { None }
                }
                _ => match &val_insn.kind {
                    InsnKind::BytePush { value } => Some(value.to_string()),
                    InsnKind::ShortPush { value } => Some(value.to_string()),
                    _ => None,
                }
            };

            if let Some(val) = default_val {
                defaults.insert(param_idx, val);
            }
            i += 5; // Skip past this pattern
        }
        break;
    }
    defaults
}

/// Find a property getter method and decompile its body.
/// Kotlin generates getter names like: getPropertyName, isPropertyName, or just propertyName.
fn find_property_getter_body(cf: &ClassFile, prop_name: &str) -> Option<String> {
    // Try getter name patterns
    let getter_names = [
        format!("get{}{}", prop_name[..1].to_uppercase(), &prop_name[1..]),
        format!("is{}{}", prop_name[..1].to_uppercase(), &prop_name[1..]),
        prop_name.to_string(),
    ];

    for getter_name in &getter_names {
        for m in &cf.methods {
            if m.name == *getter_name && m.code().is_some() && !m.is_static() {
                // Skip if this is a simple backing field getter (just returns this.field)
                let body = decompile_kotlin_body(m, cf, 3)?;
                return Some(body);
            }
        }
    }
    None
}

/// Check if a getter body is just a simple field access (return this.fieldName)
fn is_simple_field_getter(body: &str, prop_name: &str) -> bool {
    let trimmed = body.trim();
    // Match "return this.propName" or "return this.propName$delegate"
    trimmed == format!("return this.{}", prop_name)
        || trimmed.starts_with(&format!("return this.{}", prop_name))
        // Also match backing field patterns
        || trimmed == format!("return {}", prop_name)
}

/// Get the initial value of a field from the ConstantValue attribute or static initializer.
fn get_field_initial_value(cf: &ClassFile, prop_name: &str) -> Option<String> {
    use crate::classfile::attribute::Attribute;
    use crate::classfile::constant_pool::CpEntry;
    use crate::classfile::opcodes::opc;

    // First try ConstantValue attribute (for const val)
    for field in &cf.fields {
        if field.name == prop_name {
            for attr in &field.attributes {
                if let Attribute::ConstantValue(cv) = attr {
                    if let Ok(entry) = cf.constant_pool.get(cv.constant_value_index) {
                        return match entry {
                            CpEntry::Integer(v) => Some(v.to_string()),
                            CpEntry::Long(v) => Some(format!("{}L", v)),
                            CpEntry::Float(v) => Some(format!("{}f", v)),
                            CpEntry::Double(v) => Some(v.to_string()),
                            CpEntry::String(s) => Some(format!("\"{}\"", s.replace('"', "\\\""))),
                            _ => None,
                        };
                    }
                }
            }
            break;
        }
    }

    // Scan <clinit> for static fields, <init> for instance fields
    let is_static_field = cf.fields.iter()
        .any(|f| f.name == prop_name && f.access_flags & 0x0008 != 0); // ACC_STATIC

    let init_method_name = if is_static_field { "<clinit>" } else { "<init>" };

    for m in &cf.methods {
        if m.name != init_method_name { continue; }
        let code = match m.code() {
            Some(c) => c,
            None => continue,
        };

        // Look for simple pattern: ldc/const + putstatic/putfield matching our field name
        let insns = &code.instructions;
        for (i, insn) in insns.iter().enumerate() {
            let is_put = if is_static_field {
                insn.opcode == opc::putstatic
            } else {
                insn.opcode == opc::putfield
            };
            if !is_put { continue; }

            // Check if this puts to our field
            if let crate::classfile::instruction::InsnKind::Cp { index } = &insn.kind {
                if let Ok(entry) = cf.constant_pool.get(*index) {
                    let field_name = match entry {
                        CpEntry::Fieldref(mr) => &mr.name,
                        _ => continue,
                    };
                    if field_name != prop_name { continue; }

                    // Look at the previous instruction for the value
                    if i == 0 { continue; }
                    let prev = &insns[i - 1];
                    let val = match prev.opcode {
                        opc::iconst_m1 => Some("-1".into()),
                        opc::iconst_0 => Some("0".into()),
                        opc::iconst_1 => Some("1".into()),
                        opc::iconst_2 => Some("2".into()),
                        opc::iconst_3 => Some("3".into()),
                        opc::iconst_4 => Some("4".into()),
                        opc::iconst_5 => Some("5".into()),
                        opc::lconst_0 => Some("0L".into()),
                        opc::lconst_1 => Some("1L".into()),
                        opc::fconst_0 => Some("0.0f".into()),
                        opc::fconst_1 => Some("1.0f".into()),
                        opc::fconst_2 => Some("2.0f".into()),
                        opc::dconst_0 => Some("0.0".into()),
                        opc::dconst_1 => Some("1.0".into()),
                        opc::ldc | opc::ldc_w | opc::ldc2_w => {
                            let cp_idx = match &prev.kind {
                                crate::classfile::instruction::InsnKind::Ldc { index } => *index,
                                crate::classfile::instruction::InsnKind::Cp { index } => *index,
                                _ => continue,
                            };
                            if let Ok(cp_entry) = cf.constant_pool.get(cp_idx) {
                                match cp_entry {
                                    CpEntry::Integer(v) => Some(v.to_string()),
                                    CpEntry::Long(v) => Some(format!("{}L", v)),
                                    CpEntry::Float(v) => Some(format!("{}f", v)),
                                    CpEntry::Double(v) => Some(v.to_string()),
                                    CpEntry::String(s) => Some(format!("\"{}\"", s.replace('"', "\\\""))),
                                    _ => None,
                                }
                            } else { None }
                        }
                        _ => {
                            match &prev.kind {
                                crate::classfile::instruction::InsnKind::BytePush { value } =>
                                    Some(value.to_string()),
                                crate::classfile::instruction::InsnKind::ShortPush { value } =>
                                    Some(value.to_string()),
                                _ => None,
                            }
                        }
                    };
                    if val.is_some() { return val; }

                    // Also check for `new ClassName; dup; invokespecial <init>; [checkcast;] putfield`
                    // pattern (constructor call initializer)
                    // With checkcast: i-4=new, i-3=dup, i-2=invokespecial, i-1=checkcast
                    // Without: i-3=new, i-2=dup, i-1=invokespecial
                    let (new_idx, init_idx_off) = if i >= 4
                        && insns[i - 1].opcode == 0xc0 // checkcast
                        && insns[i - 2].opcode == 0xb7 // invokespecial
                        && insns[i - 3].opcode == 0x59 // dup
                        && insns[i - 4].opcode == 0xbb // new
                    {
                        (i - 4, i - 2)
                    } else if i >= 3
                        && insns[i - 1].opcode == 0xb7 // invokespecial
                        && insns[i - 2].opcode == 0x59 // dup
                        && insns[i - 3].opcode == 0xbb // new
                    {
                        (i - 3, i - 1)
                    } else {
                        (0, 0) // no match
                    };

                    if new_idx > 0 || (i >= 3 && insns[i-3].opcode == 0xbb) {
                        let actual_new = if new_idx > 0 { new_idx } else { i - 3 };
                        let actual_init = if init_idx_off > 0 { init_idx_off } else { i - 1 };
                        if insns[actual_new].opcode == 0xbb {
                            if let crate::classfile::instruction::InsnKind::Cp { index } = &insns[actual_new].kind {
                                if let Ok(entry) = cf.constant_pool.get(*index) {
                                    let class_name = match entry {
                                        CpEntry::Class(name) => name.clone(),
                                        _ => continue,
                                    };
                                    let short = class_name.rsplit('/').next()
                                        .unwrap_or(&class_name);
                                    if let crate::classfile::instruction::InsnKind::Invoke { index: iidx, .. } = &insns[actual_init].kind {
                                        if let Ok(CpEntry::Methodref(mr)) = cf.constant_pool.get(*iidx) {
                                            let args = mr.descriptor.trim_start_matches('(')
                                                .split(')')
                                                .next()
                                                .unwrap_or("");
                                            if args.is_empty() {
                                                return Some(format!("{}()", short));
                                            } else {
                                                return Some(format!("{}(...)", short));
                                            }
                                        }
                                    }
                                    return Some(format!("{}()", short));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

// ── Annotation rendering ──────────────────────────────────────────────────

fn render_class_annotations(out: &mut String, cf: &ClassFile) {
    for attr in &cf.attributes {
        if let Attribute::RuntimeVisibleAnnotations(anns) = attr {
            for ann in anns {
                // Skip kotlin internal annotations
                if ann.type_descriptor.starts_with("Lkotlin/")
                    || ann.type_descriptor.starts_with("Lorg/jetbrains/annotations/")
                {
                    continue;
                }
                let name = ann.type_descriptor.trim_start_matches('L')
                    .trim_end_matches(';')
                    .rsplit('/')
                    .next()
                    .unwrap_or(&ann.type_descriptor);
                out.push_str(&format!("@{}\n", name));
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn extract_package(class_name: &str) -> Option<String> {
    let last_slash = class_name.rfind('/')?;
    Some(class_name[..last_slash].to_string())
}
