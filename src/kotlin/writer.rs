use super::body_writer::{
    decompile_kotlin_body, decompile_kotlin_body_with_bindings, decompile_kotlin_function_object,
    decompile_kotlin_suspend_lambda, kotlinize_lambda_bootstrap, kt_render_expr,
    split_top_level_arguments,
};
use super::metadata::*;
use super::types::*;
/// Kotlin source writer — generates Kotlin source from ClassFile + KotlinMetadata.
///
/// Handles: data class, object, companion object, sealed class, enum class,
/// val/var properties, fun with receiver, suspend/inline/infix modifiers,
/// nullable types, when expressions, lambda syntax.
use crate::classfile::attribute::Attribute;
use crate::classfile::constant_pool::CpEntry;
use crate::classfile::instruction::InsnKind;
use crate::classfile::opcodes::opc;
use crate::classfile::ClassFile;
use crate::codegen::class_writer::build_lambda_bootstrap;
use crate::codegen::render_context::RenderContext;
use crate::ir::{ConstExpr, ConstValue, Expr, FieldDir, LambdaBootstrap};
use crate::types::descriptor::MethodDescriptor;
use crate::types::java_type::{JavaType, TypeKind};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct KotlinSourceUnit {
    pub class_name: String,
    pub source: String,
}

/// Check if a class file is a Kotlin class (has @kotlin/Metadata).
pub fn is_kotlin_class(cf: &ClassFile) -> bool {
    get_kotlin_annotations(cf).is_some()
}

/// Get RuntimeVisibleAnnotations from a class file.
fn get_kotlin_annotations(cf: &ClassFile) -> Option<&[crate::classfile::attribute::Annotation]> {
    for attr in &cf.attributes {
        if let Attribute::RuntimeVisibleAnnotations(anns) = attr {
            if anns
                .iter()
                .any(|a| a.type_descriptor == "Lkotlin/Metadata;")
            {
                return Some(anns);
            }
        }
    }
    None
}

/// Main entry: render a Kotlin class file to Kotlin source.
pub fn render_kotlin_class(cf: &ClassFile) -> String {
    render_kotlin_class_with_backing(cf, None, &[])
}

fn render_kotlin_class_with_backing(
    cf: &ClassFile,
    backing: Option<&ClassFile>,
    related: &[ClassFile],
) -> String {
    try_render_kotlin_class_with_backing(cf, backing, related).unwrap_or_else(|| {
        if is_kotlin_class(cf) {
            String::from("// Failed to parse Kotlin metadata\n")
        } else {
            String::from("// Not a Kotlin class\n")
        }
    })
}

/// Render related Kotlin class files as source-level units. Synthetic classes
/// are consumed as implementation details; nested/companion declarations are
/// inserted into their host and multi-file parts are merged into the facade.
pub fn render_kotlin_group(classes: &[ClassFile]) -> Vec<KotlinSourceUnit> {
    struct Entry<'a> {
        class: &'a ClassFile,
        metadata: KotlinMetadata,
    }

    let entries: Vec<Entry<'_>> = classes
        .iter()
        .filter_map(|class| {
            let annotations = get_kotlin_annotations(class)?;
            Some(Entry {
                class,
                metadata: parse_kotlin_metadata(annotations)?,
            })
        })
        .collect();
    let available: std::collections::HashSet<&str> = entries
        .iter()
        .map(|entry| entry.class.this_class.as_str())
        .collect();
    let mut units = BTreeMap::<String, String>::new();
    let mut nested = Vec::<(String, String, Option<String>)>::new();
    let mut multi_parts = HashMap::<String, Vec<String>>::new();

    for entry in &entries {
        match entry.metadata.kind {
            MetadataKind::SyntheticClass => continue,
            MetadataKind::MultiFileFacade => {
                units
                    .entry(entry.class.this_class.clone())
                    .or_insert_with(|| package_prefix(entry.class, &entry.metadata));
            }
            MetadataKind::MultiFilePart => {
                let facade = multi_file_facade_name(entry.class, &entry.metadata);
                let body = source_body(&render_kotlin_class_with_backing(
                    entry.class,
                    None,
                    classes,
                ));
                multi_parts.entry(facade).or_default().push(body);
            }
            MetadataKind::Class => {
                let immediate_host = immediate_kotlin_host(entry.class);
                let backing = immediate_host.as_ref().and_then(|host| {
                    entries
                        .iter()
                        .find(|candidate| candidate.class.this_class == *host)
                        .map(|candidate| candidate.class)
                });
                let source = render_kotlin_class_with_backing(entry.class, backing, classes);
                if let Some(host) = immediate_host {
                    if available.contains(host.as_str()) {
                        let enum_entry = entry
                            .metadata
                            .class
                            .as_ref()
                            .filter(|class| class.flags.class_kind() == ClassKind::EnumEntry)
                            .map(|_| {
                                entry
                                    .class
                                    .simple_name()
                                    .rsplit('$')
                                    .next()
                                    .unwrap_or(entry.class.simple_name())
                                    .to_string()
                            });
                        nested.push((host, source, enum_entry));
                        continue;
                    }
                    // Local/anonymous/inlined implementation classes can have
                    // several synthetic `$...` owners that are not emitted as
                    // metadata units. If a real outer source unit exists, the
                    // implementation class belongs to that unit and must not
                    // become a standalone Kotlin file.
                    if nearest_available_host(&host, &available).is_some() {
                        continue;
                    }
                }
                units.insert(entry.class.this_class.clone(), source);
            }
            MetadataKind::FileFacade => {
                units.insert(
                    entry.class.this_class.clone(),
                    render_kotlin_class_with_backing(entry.class, None, classes),
                );
            }
        }
    }

    for (facade, parts) in multi_parts {
        let source = units.entry(facade.clone()).or_insert_with(|| {
            let package = facade.rsplit_once('/').map(|(package, _)| package);
            package
                .map(|package| format!("package {}\n\n", kotlin_package_name(package)))
                .unwrap_or_default()
        });
        for part in parts {
            if !source.ends_with('\n') {
                source.push('\n');
            }
            source.push_str(&part);
        }
    }

    for (host, child_source, enum_entry) in nested {
        let Some(host_source) = units.get_mut(&host) else {
            continue;
        };
        let child_body = source_body(&child_source);
        if let Some(entry_name) = enum_entry {
            merge_enum_entry_declaration(host_source, &entry_name, &child_body);
            continue;
        }
        let is_companion = child_body.trim_start().starts_with("companion object ");
        if is_companion {
            *host_source = host_source
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    !(trimmed.starts_with("companion object ") && trimmed.ends_with("{}"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            host_source.push('\n');
        }
        insert_nested_declaration(host_source, &child_body);
    }

    units
        .into_iter()
        .map(|(class_name, source)| KotlinSourceUnit { class_name, source })
        .collect()
}

fn package_prefix(class: &ClassFile, metadata: &KotlinMetadata) -> String {
    metadata
        .package_name
        .clone()
        .or_else(|| extract_package(&class.this_class))
        .map(|package| format!("package {}\n\n", kotlin_package_name(&package)))
        .unwrap_or_default()
}

fn source_body(source: &str) -> String {
    source
        .split_once("\n\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_else(|| source.to_string())
}

fn immediate_kotlin_host(class: &ClassFile) -> Option<String> {
    for attribute in &class.attributes {
        if let Attribute::InnerClasses(classes) = attribute {
            if let Some(info) = classes
                .iter()
                .find(|info| info.inner_class_name.as_deref() == Some(class.this_class.as_str()))
            {
                if let Some(outer) = &info.outer_class_name {
                    return Some(outer.clone());
                }
            }
        }
    }
    class
        .this_class
        .rsplit_once('$')
        .map(|(host, _)| host.to_string())
}

fn nearest_available_host<'a>(
    name: &str,
    available: &std::collections::HashSet<&'a str>,
) -> Option<&'a str> {
    let mut candidate = name;
    loop {
        if let Some(host) = available.get(candidate) {
            return Some(*host);
        }
        candidate = candidate.rsplit_once('$')?.0;
    }
}

fn multi_file_facade_name(class: &ClassFile, metadata: &KotlinMetadata) -> String {
    let facade = metadata.extra_string.as_deref().unwrap_or_else(|| {
        class
            .this_class
            .rsplit('/')
            .next()
            .unwrap_or(&class.this_class)
    });
    if facade.contains('/') {
        facade.to_string()
    } else if let Some((package, _)) = class.this_class.rsplit_once('/') {
        format!("{}/{}", package, facade)
    } else {
        facade.to_string()
    }
}

fn insert_nested_declaration(host: &mut String, child: &str) {
    let declaration = child
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("    {}", line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(close) = host.rfind('}') {
        host.insert_str(close, &format!("\n{}\n", declaration));
    } else {
        host.push_str(&declaration);
    }
}

fn merge_enum_entry_declaration(host: &mut String, entry_name: &str, child: &str) {
    let Some(open) = child.find('{') else {
        return;
    };
    let Some(close) = child.rfind('}') else {
        return;
    };
    if close <= open {
        return;
    }
    let inner = child[open + 1..close].trim_matches(|ch| ch == '\r' || ch == '\n');
    let nonempty = inner
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let common_indent = nonempty
        .iter()
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);

    let mut merged = Vec::new();
    let mut replaced = false;
    for line in host.lines() {
        let trimmed = line.trim();
        let delimiter = if trimmed == format!("{entry_name},") {
            Some(',')
        } else if trimmed == format!("{entry_name};") {
            Some(';')
        } else {
            None
        };
        if let Some(delimiter) = delimiter {
            let leading = &line[..line.len() - line.trim_start().len()];
            merged.push(format!("{leading}{entry_name} {{"));
            for inner_line in inner.lines() {
                if inner_line.trim().is_empty() {
                    merged.push(String::new());
                } else {
                    merged.push(format!(
                        "{leading}    {}",
                        &inner_line[common_indent.min(inner_line.len())..]
                    ));
                }
            }
            merged.push(format!("{leading}}}{delimiter}"));
            replaced = true;
        } else {
            merged.push(line.to_string());
        }
    }
    if replaced {
        *host = merged.join("\n");
        host.push('\n');
    }
}

/// Render a Kotlin class, returning `None` when metadata is absent or invalid.
pub fn try_render_kotlin_class(cf: &ClassFile) -> Option<String> {
    try_render_kotlin_class_with_backing(cf, None, &[])
}

fn try_render_kotlin_class_with_backing(
    cf: &ClassFile,
    backing: Option<&ClassFile>,
    related: &[ClassFile],
) -> Option<String> {
    let anns = get_kotlin_annotations(cf)?;
    let metadata = parse_kotlin_metadata(anns)?;

    let mut out = String::new();

    // Package declaration
    let inferred_package = extract_package(&cf.this_class);
    if let Some(pkg) = metadata
        .package_name
        .as_deref()
        .or(inferred_package.as_deref())
    {
        out.push_str(&format!("package {}\n\n", kotlin_package_name(pkg)));
    }

    match metadata.kind {
        MetadataKind::Class => {
            if let Some(ref cls) = metadata.class {
                render_class_decl(&mut out, cf, cls, backing, related);
            }
        }
        MetadataKind::FileFacade | MetadataKind::MultiFilePart => {
            if let Some(ref pkg) = metadata.package {
                render_file_facade(&mut out, cf, pkg, related);
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

    Some(out)
}

// ── Class declaration rendering ───────────────────────────────────────────

fn render_class_decl(
    out: &mut String,
    cf: &ClassFile,
    cls: &KClass,
    backing: Option<&ClassFile>,
    related: &[ClassFile],
) {
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
    if cls.flags.is_data_class() {
        header.push_str("data ");
    }
    if cls.flags.is_inner_class() {
        header.push_str("inner ");
    }
    if cls.flags.is_value_class() {
        header.push_str("value ");
    }
    if cls.flags.is_fun_interface() {
        header.push_str("fun ");
    }

    // Class keyword
    match kind {
        ClassKind::Class => header.push_str("class "),
        ClassKind::Interface => header.push_str("interface "),
        ClassKind::EnumClass => header.push_str("enum class "),
        ClassKind::Object => header.push_str("object "),
        ClassKind::CompanionObject => header.push_str("companion object "),
        ClassKind::AnnotationClass => header.push_str("annotation class "),
        ClassKind::EnumEntry => header.push_str("object "),
    }

    // Class name
    let simple = cf.simple_name();
    // Strip $Companion suffix for companion objects
    let name = simple.rsplit('$').next().unwrap_or(simple);
    header.push_str(&kotlin_identifier(name));

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
    let visible_funcs: Vec<_> = cls
        .functions
        .iter()
        .filter(|f| {
            if cls.flags.is_data_class() {
                let name = f.name.as_str();
                !(name == "copy"
                    || name == "toString"
                    || name == "hashCode"
                    || name == "equals"
                    || name.starts_with("component"))
            } else {
                true
            }
        })
        .collect();

    let visible_props: Vec<_> = cls
        .properties
        .iter()
        .filter(|p| {
            if cls.flags.is_data_class() || cls.flags.class_kind() == ClassKind::AnnotationClass {
                let in_ctor = cls
                    .constructors
                    .first()
                    .map(|c| c.value_parameters.iter().any(|vp| vp.name == p.name))
                    .unwrap_or(false);
                !in_ctor
            } else {
                true
            }
        })
        .collect();

    let has_body = !visible_funcs.is_empty()
        || !visible_props.is_empty()
        || !cls.enum_entries.is_empty()
        || cls.companion_object_name.is_some();

    if has_body {
        out.push_str(" {\n");
        render_class_body(out, cf, cls, backing, related);
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

fn render_primary_constructor(ctor: &KConstructor, cls: &KClass, cf: &ClassFile) -> String {
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

    let defaults = extract_constructor_default_values(cf, ctor);
    let params: Vec<String> = ctor
        .value_parameters
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let mut s = String::new();

            // Data and annotation constructor parameters declare properties.
            if cls.flags.is_data_class() || cls.flags.class_kind() == ClassKind::AnnotationClass {
                // Check if there's a matching property
                let is_var = cls
                    .properties
                    .iter()
                    .any(|prop| prop.name == p.name && prop.flags.is_var());
                if cls.flags.class_kind() != ClassKind::AnnotationClass && is_var {
                    s.push_str("var ");
                } else {
                    s.push_str("val ");
                }
            }

            s.push_str(&kotlin_identifier(&p.name));
            s.push_str(": ");
            if let Some(ref ty) = p.type_ {
                s.push_str(&render_kotlin_type(ty, &cls.type_parameters));
            } else {
                s.push_str("Any?");
            }

            if p.flags.declares_default_value() {
                if let Some(value) = defaults.get(&index) {
                    s.push_str(" = ");
                    s.push_str(&normalize_default_value(value, p));
                } else {
                    s.push_str(" = TODO(\"unrecovered default value\")");
                }
            }

            s
        })
        .collect();

    if vis_str.is_empty() {
        format!("({})", params.join(", "))
    } else {
        format!(" {}({})", vis_str, params.join(", "))
    }
}

fn render_supertypes(cls: &KClass, type_params: &[KTypeParameter]) -> String {
    let kind = cls.flags.class_kind();
    let supers: Vec<String> = cls
        .supertypes
        .iter()
        .filter(|t| {
            // Filter out kotlin/Any
            if t.class_name.as_deref() == Some("kotlin/Any") {
                return false;
            }
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

fn render_class_body(
    out: &mut String,
    cf: &ClassFile,
    cls: &KClass,
    backing: Option<&ClassFile>,
    related: &[ClassFile],
) {
    let tp = &cls.type_parameters;
    let in_interface = cls.flags.class_kind() == ClassKind::Interface;

    // Enum entries
    if !cls.enum_entries.is_empty() {
        let entries: Vec<&str> = cls.enum_entries.iter().map(|e| e.name.as_str()).collect();
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
        if cls.flags.is_data_class() || cls.flags.class_kind() == ClassKind::AnnotationClass {
            let in_ctor = cls
                .constructors
                .first()
                .map(|c| c.value_parameters.iter().any(|p| p.name == prop.name))
                .unwrap_or(false);
            if in_ctor {
                continue;
            }
        }
        render_property(out, prop, tp, in_interface, cf, backing, related);
    }

    // Functions
    for func in &cls.functions {
        // Skip auto-generated data class methods
        if cls.flags.is_data_class() {
            let name = func.name.as_str();
            if name == "copy"
                || name == "toString"
                || name == "hashCode"
                || name == "equals"
                || name.starts_with("component")
            {
                continue;
            }
        }
        render_function(out, func, tp, in_interface, cf);
    }
}

// ── Property rendering ────────────────────────────────────────────────────

fn render_property(
    out: &mut String,
    prop: &KProperty,
    type_params: &[KTypeParameter],
    in_interface: bool,
    cf: &ClassFile,
    backing: Option<&ClassFile>,
    related: &[ClassFile],
) {
    let init_val = get_field_initial_value(cf, backing, prop, related);
    let inferred_anonymous_type = init_val
        .as_deref()
        .is_some_and(|value| value.trim_start().starts_with("object"))
        && prop
            .return_type
            .as_ref()
            .is_some_and(kotlin_type_is_anonymous);
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
    if prop.flags.is_const() && init_val.is_some() && !prop.flags.is_delegated() {
        out.push_str("const ");
    }
    if prop.flags.is_lateinit() {
        out.push_str("lateinit ");
    }

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
    out.push_str(&kotlin_identifier(&prop.name));

    // Anonymous object types cannot be named in Kotlin source.  Their
    // property type is inferred from the object expression.
    if !inferred_anonymous_type {
        out.push_str(": ");
        if let Some(ref ty) = prop.return_type {
            out.push_str(&render_kotlin_type(ty, type_params));
        } else {
            out.push_str("Any?");
        }
    }

    // Try to get initial value from ConstantValue attribute or field
    if let Some(init_val) = init_val {
        let operator = if prop.flags.is_delegated() {
            " by "
        } else {
            " = "
        };
        out.push_str(operator);
        out.push_str(&init_val);
        out.push('\n');
        return; // Properties with initializers don't need custom getter shown
    }

    if prop.flags.is_delegated() {
        out.push_str("\n        get() = TODO(\"unrecovered property delegate\")\n");
        return;
    }

    // Try to render custom getter body
    if let Some(getter_body) = find_property_getter_body(cf, prop, 3) {
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

    if in_interface || prop.flags.modality() == Modality::Abstract || prop.flags.is_lateinit() {
        out.push('\n');
    } else if prop.receiver_type.is_some() {
        out.push_str("\n        get() = TODO(\"unrecovered property getter\")\n");
    } else {
        out.push_str(" = TODO(\"unrecovered property initializer\")\n");
    }
}

// ── Function rendering ────────────────────────────────────────────────────

fn render_function(
    out: &mut String,
    func: &KFunction,
    type_params: &[KTypeParameter],
    in_interface: bool,
    cf: &ClassFile,
) {
    if let Some(context_types) = kotlin_context_receiver_types(cf, func) {
        if !context_types.is_empty() {
            out.push_str("    context(");
            out.push_str(
                &context_types
                    .iter()
                    .map(kotlin_type_name_from_java)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push_str(")\n");
        }
    }
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
    if func.flags.is_inline() {
        out.push_str("inline ");
    }
    if func.flags.is_infix() {
        out.push_str("infix ");
    }
    if func.flags.is_operator() {
        out.push_str("operator ");
    }
    if func.flags.is_suspend() {
        out.push_str("suspend ");
    }
    if func.flags.is_tailrec() {
        out.push_str("tailrec ");
    }
    if func.flags.is_external() {
        out.push_str("external ");
    }

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
    out.push_str(&kotlin_identifier(&func.name));

    // Parameters — extract defaults from $default method
    let defaults = extract_function_default_values(cf, func);
    out.push('(');
    let params: Vec<String> = func
        .value_parameters
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let mut s = String::new();

            if p.flags.is_crossinline() {
                s.push_str("crossinline ");
            }
            if p.flags.is_noinline() {
                s.push_str("noinline ");
            }

            if p.vararg_element_type.is_some() {
                s.push_str("vararg ");
            }

            s.push_str(&kotlin_identifier(&p.name));
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
                    s.push_str(&format!(" = {}", normalize_default_value(val, p)));
                } else {
                    s.push_str(" = TODO(\"unrecovered default value\")");
                }
            }

            s
        })
        .collect();
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
    if func.flags.modality() == Modality::Abstract
        || in_interface && func.flags.modality() != Modality::Open
    {
        out.push('\n');
    } else {
        // Try to find and decompile the method body from bytecode
        let body = find_and_decompile(cf, func);
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

fn render_file_facade(out: &mut String, cf: &ClassFile, pkg: &KPackage, related: &[ClassFile]) {
    let tp: &[KTypeParameter] = &[];

    for prop in &pkg.properties {
        render_top_level_property(out, prop, tp, cf, related);
    }

    if !pkg.properties.is_empty() && !pkg.functions.is_empty() {
        out.push('\n');
    }

    for func in &pkg.functions {
        render_top_level_function(out, func, tp, cf);
    }
}

fn render_top_level_property(
    out: &mut String,
    prop: &KProperty,
    type_params: &[KTypeParameter],
    cf: &ClassFile,
    related: &[ClassFile],
) {
    let init_val = get_field_initial_value(cf, None, prop, related);
    // Visibility
    let vis = prop.flags.visibility();
    match vis {
        Visibility::Private => out.push_str("private "),
        Visibility::Protected => out.push_str("protected "),
        Visibility::Internal => out.push_str("internal "),
        _ => {}
    }

    if prop.flags.is_const() && init_val.is_some() && !prop.flags.is_delegated() {
        out.push_str("const ");
    }
    if prop.flags.is_lateinit() {
        out.push_str("lateinit ");
    }

    if prop.flags.is_var() {
        out.push_str("var ");
    } else {
        out.push_str("val ");
    }

    if let Some(ref recv) = prop.receiver_type {
        out.push_str(&render_kotlin_type(recv, type_params));
        out.push('.');
    }

    out.push_str(&kotlin_identifier(&prop.name));
    out.push_str(": ");
    if let Some(ref ty) = prop.return_type {
        out.push_str(&render_kotlin_type(ty, type_params));
    } else {
        out.push_str("Any?");
    }
    if let Some(init_val) = init_val {
        let operator = if prop.flags.is_delegated() {
            " by "
        } else {
            " = "
        };
        out.push_str(operator);
        out.push_str(&init_val);
        out.push('\n');
    } else if prop.flags.is_delegated() {
        out.push_str("\n    get() = TODO(\"unrecovered property delegate\")\n");
    } else if prop.flags.is_lateinit() {
        out.push('\n');
    } else if let Some(getter_body) = find_property_getter_body(cf, prop, 2) {
        let trimmed = getter_body.trim();
        if !trimmed.is_empty() && !is_simple_field_getter(trimmed, &prop.name) {
            out.push_str("\n    get() {\n");
            out.push_str(&getter_body);
            out.push_str("    }\n");
        } else {
            out.push('\n');
        }
    } else if prop.receiver_type.is_some() {
        out.push_str("\n    get() = TODO(\"unrecovered property getter\")\n");
    } else {
        out.push_str(" = TODO(\"unrecovered property initializer\")\n");
    }
}

fn render_top_level_function(
    out: &mut String,
    func: &KFunction,
    type_params: &[KTypeParameter],
    cf: &ClassFile,
) {
    if let Some(context_types) = kotlin_context_receiver_types(cf, func) {
        if !context_types.is_empty() {
            out.push_str("context(");
            out.push_str(
                &context_types
                    .iter()
                    .map(kotlin_type_name_from_java)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push_str(")\n");
        }
    }
    // Visibility
    let vis = func.flags.visibility();
    match vis {
        Visibility::Private => out.push_str("private "),
        Visibility::Protected => out.push_str("protected "),
        Visibility::Internal => out.push_str("internal "),
        _ => {}
    }

    // Modifiers
    if func.flags.is_inline() {
        out.push_str("inline ");
    }
    if func.flags.is_infix() {
        out.push_str("infix ");
    }
    if func.flags.is_operator() {
        out.push_str("operator ");
    }
    if func.flags.is_suspend() {
        out.push_str("suspend ");
    }
    if func.flags.is_tailrec() {
        out.push_str("tailrec ");
    }
    if func.flags.is_external() {
        out.push_str("external ");
    }

    out.push_str("fun ");

    if !func.type_parameters.is_empty() {
        out.push_str(&render_type_params_decl(&func.type_parameters));
        out.push(' ');
    }

    if let Some(ref recv) = func.receiver_type {
        out.push_str(&render_kotlin_type(recv, type_params));
        out.push('.');
    }

    out.push_str(&kotlin_identifier(&func.name));
    let defaults = extract_function_default_values(cf, func);
    out.push('(');
    let params: Vec<String> = func
        .value_parameters
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let mut s = String::new();
            if p.flags.is_crossinline() {
                s.push_str("crossinline ");
            }
            if p.flags.is_noinline() {
                s.push_str("noinline ");
            }
            if p.vararg_element_type.is_some() {
                s.push_str("vararg ");
            }
            s.push_str(&kotlin_identifier(&p.name));
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
                    s.push_str(&format!(" = {}", normalize_default_value(val, p)));
                } else {
                    s.push_str(" = TODO(\"unrecovered default value\")");
                }
            }
            s
        })
        .collect();
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
    let body = find_and_decompile(cf, func);
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
fn find_and_decompile(cf: &ClassFile, func: &KFunction) -> Option<String> {
    let method = find_kotlin_function_method(cf, func)?;
    let descriptor = MethodDescriptor::parse(&method.descriptor).ok();
    let parameter_count = descriptor
        .as_ref()
        .map_or(func.value_parameters.len(), |descriptor| {
            descriptor.params.len()
        });
    let mut parameter_names = vec![String::new(); parameter_count];
    let represented_count = func.value_parameters.len()
        + usize::from(func.receiver_type.is_some())
        + usize::from(func.flags.is_suspend());
    let mut next = descriptor
        .as_ref()
        .map_or(0, |descriptor| descriptor.params.len())
        .saturating_sub(represented_count);
    let context_types = descriptor
        .as_ref()
        .map(|descriptor| descriptor.params.iter().take(next).collect::<Vec<_>>())
        .unwrap_or_default();
    for (index, name) in parameter_names.iter_mut().take(next).enumerate() {
        *name = context_types
            .get(index)
            .map(|ty| format!("this@{}", kotlin_type_label_from_java(ty)))
            .unwrap_or_else(|| format!("$context_receiver_{index}"));
    }
    if func.receiver_type.is_some() && next < parameter_names.len() {
        parameter_names[next] = "$kotlin$extension$this".into();
        next += 1;
    }
    for parameter in &func.value_parameters {
        if next >= parameter_names.len() {
            break;
        }
        parameter_names[next] = parameter.name.clone();
        next += 1;
    }
    let dispatch_receiver_label = (func.receiver_type.is_some() && !method.is_static())
        .then(|| kotlin_dispatch_receiver_label(&cf.this_class));
    crate::kotlin::body_writer::decompile_kotlin_body_with_bindings(
        method,
        cf,
        2,
        &parameter_names,
        dispatch_receiver_label.as_deref(),
    )
}

fn find_kotlin_function_method<'a>(
    cf: &'a ClassFile,
    func: &KFunction,
) -> Option<&'a crate::classfile::member::Method> {
    find_kotlin_function_member(cf, func, true)
}

fn find_kotlin_function_member<'a>(
    cf: &'a ClassFile,
    func: &KFunction,
    require_code: bool,
) -> Option<&'a crate::classfile::member::Method> {
    let signature = func.jvm_signature.as_ref();
    let expected_name = signature
        .and_then(|signature| signature.name.as_deref())
        .unwrap_or(&func.name);
    if let Some(descriptor) = signature.and_then(|signature| signature.descriptor.as_deref()) {
        return cf.methods.iter().find(|method| {
            method.name == expected_name
                && method.descriptor == descriptor
                && (!require_code || method.code().is_some())
        });
    }

    let expected_count = func.value_parameters.len()
        + usize::from(func.receiver_type.is_some())
        + usize::from(func.flags.is_suspend());
    cf.methods
        .iter()
        .filter(|method| method.name == expected_name && (!require_code || method.code().is_some()))
        .filter_map(|method| {
            let descriptor = MethodDescriptor::parse(&method.descriptor).ok()?;
            (descriptor.params.len() >= expected_count).then(|| {
                (
                    kotlin_function_match_score(func, &descriptor.params),
                    descriptor.params.len(),
                    method,
                )
            })
        })
        .max_by_key(|(score, parameter_count, _)| (*score, usize::MAX - *parameter_count))
        .map(|(_, _, method)| method)
        .or_else(|| {
            cf.methods.iter().find(|method| {
                method.name == expected_name && (!require_code || method.code().is_some())
            })
        })
}

fn kotlin_context_receiver_types(cf: &ClassFile, func: &KFunction) -> Option<Vec<JavaType>> {
    let method = find_kotlin_function_member(cf, func, false)?;
    let descriptor = MethodDescriptor::parse(&method.descriptor).ok()?;
    let represented_count = func.value_parameters.len()
        + usize::from(func.receiver_type.is_some())
        + usize::from(func.flags.is_suspend());
    let hidden_count = descriptor.params.len().saturating_sub(represented_count);
    Some(descriptor.params.into_iter().take(hidden_count).collect())
}

fn kotlin_type_label_from_java(ty: &JavaType) -> String {
    kotlin_type_name_from_java(ty)
        .trim_end_matches('?')
        .rsplit('.')
        .next()
        .unwrap_or("Context")
        .to_string()
}

fn kotlin_type_name_from_java(ty: &JavaType) -> String {
    if ty.array_dim > 0 {
        let mut element = ty.clone();
        element.array_dim -= 1;
        return format!("Array<{}>", kotlin_type_name_from_java(&element));
    }
    match ty.kind {
        TypeKind::Boolean => "Boolean".into(),
        TypeKind::Byte => "Byte".into(),
        TypeKind::Char => "Char".into(),
        TypeKind::Double => "Double".into(),
        TypeKind::Float => "Float".into(),
        TypeKind::Int | TypeKind::ByteChar | TypeKind::ShortChar => "Int".into(),
        TypeKind::Long => "Long".into(),
        TypeKind::Short => "Short".into(),
        TypeKind::Object | TypeKind::GenVar => ty
            .class_name
            .as_deref()
            .map(|name| name.rsplit('/').next().unwrap_or(name).replace('$', "."))
            .unwrap_or_else(|| "Any".into()),
        _ => "Any".into(),
    }
}

fn kotlin_function_match_score(func: &KFunction, params: &[JavaType]) -> usize {
    let mut expected = Vec::with_capacity(params.len());
    if let Some(receiver) = func.receiver_type.as_ref() {
        expected.push(Some(receiver));
    }
    expected.extend(
        func.value_parameters
            .iter()
            .map(|parameter| parameter.type_.as_ref()),
    );
    if func.flags.is_suspend() {
        expected.push(None);
    }

    if params.len() < expected.len() {
        return 0;
    }
    let params = &params[params.len() - expected.len()..];
    expected
        .into_iter()
        .zip(params)
        .map(|(expected, actual)| {
            expected.map_or(0, |expected| kotlin_type_match_score(expected, actual))
        })
        .sum()
}

fn kotlin_type_match_score(expected: &KType, actual: &JavaType) -> usize {
    if expected.type_parameter_id.is_some() || expected.type_parameter_name.is_some() {
        return usize::from(matches!(actual.kind, TypeKind::Object) || actual.array_dim > 0);
    }

    let Some(name) = expected.class_name.as_deref() else {
        return 0;
    };
    let primitive = match name {
        "kotlin/Boolean" if !expected.nullable => Some(TypeKind::Boolean),
        "kotlin/Byte" if !expected.nullable => Some(TypeKind::Byte),
        "kotlin/Char" if !expected.nullable => Some(TypeKind::Char),
        "kotlin/Double" if !expected.nullable => Some(TypeKind::Double),
        "kotlin/Float" if !expected.nullable => Some(TypeKind::Float),
        "kotlin/Int" if !expected.nullable => Some(TypeKind::Int),
        "kotlin/Long" if !expected.nullable => Some(TypeKind::Long),
        "kotlin/Short" if !expected.nullable => Some(TypeKind::Short),
        _ => None,
    };
    if let Some(kind) = primitive {
        return if actual.array_dim == 0 && actual.kind == kind {
            8
        } else {
            0
        };
    }

    if actual.array_dim > 0 {
        return usize::from(name == "kotlin/Array");
    }
    if actual.kind != TypeKind::Object {
        return 0;
    }
    let Some(actual_name) = actual.class_name.as_deref() else {
        return 1;
    };
    let expected_name = match name {
        "kotlin/Any" => "java/lang/Object",
        "kotlin/String" => "java/lang/String",
        "kotlin/CharSequence" => "java/lang/CharSequence",
        "kotlin/Throwable" => "java/lang/Throwable",
        "kotlin/Boolean" => "java/lang/Boolean",
        "kotlin/Byte" => "java/lang/Byte",
        "kotlin/Char" => "java/lang/Character",
        "kotlin/Double" => "java/lang/Double",
        "kotlin/Float" => "java/lang/Float",
        "kotlin/Int" => "java/lang/Integer",
        "kotlin/Long" => "java/lang/Long",
        "kotlin/Short" => "java/lang/Short",
        _ => name,
    };
    if actual_name == expected_name
        || name
            .strip_prefix("kotlin/Function")
            .is_some_and(|arity| actual_name == format!("kotlin/jvm/functions/Function{arity}"))
    {
        8
    } else {
        1
    }
}

/// Extract default parameter values by simulating each mask-controlled branch
/// of a `$default` method or synthetic default constructor.
fn extract_function_default_values(cf: &ClassFile, func: &KFunction) -> HashMap<usize, String> {
    let jvm_name = func
        .jvm_signature
        .as_ref()
        .and_then(|signature| signature.name.as_deref())
        .unwrap_or(&func.name);
    let default_name = format!("{jvm_name}$default");
    let expected_descriptor = func
        .jvm_signature
        .as_ref()
        .and_then(|signature| signature.descriptor.as_deref());
    let original = find_kotlin_function_member(cf, func, false);
    let expected =
        expected_descriptor.and_then(|descriptor| MethodDescriptor::parse(descriptor).ok());

    cf.methods
        .iter()
        .filter(|method| {
            if method.name != default_name || method.code().is_none() {
                return false;
            }
            let Ok(candidate) = MethodDescriptor::parse(&method.descriptor) else {
                return false;
            };
            let Some(original) = original else {
                return expected.as_ref().is_none_or(|expected| {
                    candidate.params.len() >= expected.params.len()
                        && candidate.params[..expected.params.len()] == expected.params
                });
            };
            let Ok(original_descriptor) = MethodDescriptor::parse(&original.descriptor) else {
                return false;
            };
            let receiver_offset = usize::from(!original.is_static());
            candidate.params.len() > receiver_offset + original_descriptor.params.len()
                && candidate.params
                    [receiver_offset..receiver_offset + original_descriptor.params.len()]
                    == original_descriptor.params
        })
        .min_by_key(|method| {
            MethodDescriptor::parse(&method.descriptor)
                .map(|descriptor| descriptor.params.len())
                .unwrap_or(usize::MAX)
        })
        .map(|method| {
            let candidate = MethodDescriptor::parse(&method.descriptor).ok();
            let receiver_offset = original.is_some_and(|method| !method.is_static()) as usize;
            let represented_count = func.value_parameters.len()
                + usize::from(func.receiver_type.is_some())
                + usize::from(func.flags.is_suspend());
            let hidden_parameter_count = original
                .and_then(|method| MethodDescriptor::parse(&method.descriptor).ok())
                .map_or(0, |descriptor| {
                    descriptor.params.len().saturating_sub(represented_count)
                });
            let mut names = Vec::new();
            if receiver_offset != 0 {
                names.push("$this$default".into());
            }
            names.extend(
                (0..hidden_parameter_count).map(|index| format!("$context_receiver_{index}")),
            );
            if func.receiver_type.is_some() {
                names.push("$this$receiver".into());
            }
            names.extend(
                func.value_parameters
                    .iter()
                    .map(|parameter| parameter.name.clone()),
            );
            if func.flags.is_suspend() {
                names.push("$continuation".into());
            }
            names.resize(
                candidate.map_or(names.len(), |descriptor| descriptor.params.len()),
                "$synthetic".into(),
            );
            extract_mask_default_values(cf, method, &names, hidden_parameter_count)
        })
        .unwrap_or_default()
}

fn extract_constructor_default_values(
    cf: &ClassFile,
    constructor: &KConstructor,
) -> HashMap<usize, String> {
    let expected = constructor
        .jvm_signature
        .as_ref()
        .and_then(|signature| signature.descriptor.as_deref())
        .and_then(|descriptor| MethodDescriptor::parse(descriptor).ok());

    let mask_count = constructor.value_parameters.len().div_ceil(32).max(1);
    cf.methods
        .iter()
        .filter(|method| {
            if method.name != "<init>" || method.code().is_none() {
                return false;
            }
            let Ok(candidate) = MethodDescriptor::parse(&method.descriptor) else {
                return false;
            };
            if candidate
                .params
                .last()
                .and_then(|ty| ty.class_name.as_deref())
                != Some("kotlin/jvm/internal/DefaultConstructorMarker")
            {
                return false;
            }
            if candidate.params.len() < constructor.value_parameters.len() + mask_count + 1 {
                return false;
            }
            let marker_index = candidate.params.len() - 1;
            candidate.params[marker_index - mask_count..marker_index]
                .iter()
                .all(|ty| *ty == JavaType::INT)
        })
        .min_by_key(|method| {
            let candidate = MethodDescriptor::parse(&method.descriptor).ok();
            let exact_prefix = expected.as_ref().is_some_and(|expected| {
                candidate.as_ref().is_some_and(|candidate| {
                    candidate.params.len() > expected.params.len()
                        && candidate.params[..expected.params.len()] == expected.params
                })
            });
            (
                !exact_prefix,
                candidate.map_or(usize::MAX, |candidate| candidate.params.len()),
            )
        })
        .map(|method| {
            let count = MethodDescriptor::parse(&method.descriptor)
                .map(|descriptor| descriptor.params.len())
                .unwrap_or(constructor.value_parameters.len());
            let mut names = constructor
                .value_parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>();
            names.resize(count, "$synthetic".into());
            extract_mask_default_values(cf, method, &names, 0)
        })
        .unwrap_or_default()
}

fn extract_mask_default_values(
    cf: &ClassFile,
    method: &crate::classfile::member::Method,
    parameter_names: &[String],
    parameter_index_offset: usize,
) -> HashMap<usize, String> {
    use crate::classfile::opcodes::opc;

    let Some(code) = method.code() else {
        return HashMap::new();
    };
    let mut defaults = HashMap::new();
    let mut lambda_bootstrap = build_lambda_bootstrap(cf);
    kotlinize_lambda_bootstrap(cf, &mut lambda_bootstrap);
    let context = RenderContext::for_method(
        code,
        cf,
        method.is_static(),
        &method.descriptor,
        parameter_names,
    )
    .with_lambda_bootstrap(&lambda_bootstrap);
    let instructions = &code.instructions;

    for index in 0..instructions.len().saturating_sub(3) {
        if instructions[index + 2].opcode != opc::iand
            || instructions[index + 3].opcode != opc::ifeq
        {
            continue;
        }
        let Some(bit) =
            instruction_int_constant(cf, &instructions[index + 1]).map(|value| value as u32)
        else {
            continue;
        };
        if bit == 0 || bit.count_ones() != 1 {
            continue;
        }
        let Some(target) = instructions[index + 3]
            .kind
            .branch_targets(
                instructions[index + 3].offset,
                instructions[index + 3].opcode,
            )
            .first()
            .copied()
        else {
            continue;
        };
        let Some(target_index) = instructions
            .iter()
            .position(|instruction| instruction.offset == target)
        else {
            continue;
        };
        if target_index <= index + 4 {
            continue;
        }
        let result = context.simulate(&instructions[index + 4..target_index], vec![]);
        let value = result.stmts.iter().rev().find_map(|stmt| match stmt {
            Expr::Assign { rhs, .. } => Some(kt_render_expr(rhs)),
            _ => None,
        });
        if let Some(value) = value.filter(|value| is_complete_kotlin_expression(value)) {
            let bit_index = bit.trailing_zeros() as usize;
            if let Some(parameter_index) = bit_index.checked_sub(parameter_index_offset) {
                defaults.insert(parameter_index, value);
            }
        }
    }
    defaults
}

fn instruction_int_constant(
    cf: &ClassFile,
    instruction: &crate::classfile::instruction::Instruction,
) -> Option<i32> {
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;
    match instruction.opcode {
        opc::iconst_m1 => Some(-1),
        opc::iconst_0 => Some(0),
        opc::iconst_1 => Some(1),
        opc::iconst_2 => Some(2),
        opc::iconst_3 => Some(3),
        opc::iconst_4 => Some(4),
        opc::iconst_5 => Some(5),
        _ => match instruction.kind {
            InsnKind::BytePush { value } => Some(value as i32),
            InsnKind::ShortPush { value } => Some(value as i32),
            InsnKind::Ldc { index } => match cf.constant_pool.get(index).ok()? {
                CpEntry::Integer(value) => Some(*value),
                _ => None,
            },
            _ => None,
        },
    }
}

fn normalize_default_value(value: &str, parameter: &KValueParameter) -> String {
    if parameter
        .type_
        .as_ref()
        .and_then(|ty| ty.class_name.as_deref())
        == Some("kotlin/Boolean")
    {
        match value {
            "0" => return "false".into(),
            "1" => return "true".into(),
            _ => {}
        }
    }
    value.to_string()
}

/// Find a property getter method and decompile its body.
/// Kotlin generates getter names like: getPropertyName, isPropertyName, or just propertyName.
fn find_property_getter_body(cf: &ClassFile, prop: &KProperty, indent: usize) -> Option<String> {
    if let Some(signature) = prop
        .jvm_signature
        .as_ref()
        .and_then(|signature| signature.getter.as_ref())
    {
        let getter_names = property_getter_names(&prop.name);
        let method = cf.methods.iter().find(|method| {
            signature.name.as_deref().map_or_else(
                || getter_names.contains(&method.name),
                |name| method.name == name,
            ) && signature
                .descriptor
                .as_deref()
                .is_none_or(|descriptor| method.descriptor == descriptor)
                && method.code().is_some()
        })?;
        return decompile_property_accessor_body(method, cf, prop, indent);
    }
    let prop_name = &prop.name;
    // Try getter name patterns
    let getter_names = property_getter_names(prop_name);

    for getter_name in &getter_names {
        for m in &cf.methods {
            if m.name == *getter_name && m.code().is_some() && !m.is_static() {
                // Skip if this is a simple backing field getter (just returns this.field)
                let body = decompile_property_accessor_body(m, cf, prop, indent)?;
                return Some(body);
            }
        }
    }
    None
}

fn decompile_property_accessor_body(
    method: &crate::classfile::member::Method,
    cf: &ClassFile,
    prop: &KProperty,
    indent: usize,
) -> Option<String> {
    if prop.receiver_type.is_none() {
        return decompile_kotlin_body(method, cf, indent);
    }

    let descriptor = MethodDescriptor::parse(&method.descriptor).ok()?;
    let mut parameter_names = vec![String::new(); descriptor.params.len()];
    if !parameter_names.is_empty() {
        parameter_names[0] = "$kotlin$extension$this".into();
    }
    let dispatch_receiver_label =
        (!method.is_static()).then(|| kotlin_dispatch_receiver_label(&cf.this_class));
    decompile_kotlin_body_with_bindings(
        method,
        cf,
        indent,
        &parameter_names,
        dispatch_receiver_label.as_deref(),
    )
}

fn property_getter_names(prop_name: &str) -> Vec<String> {
    let mut chars = prop_name.chars();
    let Some(first) = chars.next() else {
        return vec![String::new()];
    };
    let capitalized = format!("{}{}", first.to_uppercase(), chars.as_str());
    vec![
        format!("get{}", capitalized),
        format!("is{}", capitalized),
        prop_name.to_string(),
    ]
}

/// Check if a getter body is just a simple field access (return this.fieldName)
fn is_simple_field_getter(body: &str, prop_name: &str) -> bool {
    let trimmed = body.trim();
    let direct = format!("return this.{}", prop_name);
    let delegated = format!("return this.{}$delegate", prop_name);
    trimmed == direct
        || trimmed == format!("return {}", prop_name)
        || trimmed == delegated
        || trimmed
            .strip_prefix(&delegated)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with(" as "))
}

/// Get the initial value of a field from ConstantValue or its initializer method.
fn get_field_initial_value(
    cf: &ClassFile,
    backing: Option<&ClassFile>,
    prop: &KProperty,
    related: &[ClassFile],
) -> Option<String> {
    use crate::classfile::attribute::Attribute;
    let prop_name = prop
        .jvm_signature
        .as_ref()
        .and_then(|signature| signature.field.as_ref())
        .and_then(|signature| signature.name.as_deref())
        .unwrap_or(&prop.name);
    let prop_descriptor = prop
        .jvm_signature
        .as_ref()
        .and_then(|signature| signature.field.as_ref())
        .and_then(|signature| signature.descriptor.as_deref());
    let storage = backing
        .filter(|class| class.fields.iter().any(|field| field.name == prop_name))
        .unwrap_or(cf);
    let field = storage.fields.iter().find(|field| {
        field.name == prop_name
            && prop_descriptor.is_none_or(|descriptor| field.descriptor == descriptor)
    });
    let Some(field) = field else {
        let computed = prop
            .flags
            .is_delegated()
            .then(|| recover_computed_delegate(storage, &prop.name))
            .flatten();
        return computed;
    };

    // First try ConstantValue attribute (for const val)
    for attr in &field.attributes {
        if let Attribute::ConstantValue(value) = attr {
            if let Ok(entry) = storage.constant_pool.get(value.constant_value_index) {
                return render_constant_value(entry, &field.descriptor);
            }
        }
    }

    let is_static_field = field.is_static();
    let init_method_name = if is_static_field {
        "<clinit>"
    } else {
        "<init>"
    };

    let mut lambda_bootstrap = build_lambda_bootstrap(storage);
    kotlinize_lambda_bootstrap(storage, &mut lambda_bootstrap);
    for m in &storage.methods {
        if m.name != init_method_name {
            continue;
        }
        let code = match m.code() {
            Some(c) => c,
            None => continue,
        };

        let context = RenderContext::for_method(code, storage, m.is_static(), &m.descriptor, &[])
            .with_lambda_bootstrap(&lambda_bootstrap);
        let result = context.simulate(&code.instructions, vec![]);
        if let Some(value) = recover_field_value(
            &result.stmts,
            &storage.this_class,
            prop_name,
            &m.descriptor,
            m.is_static(),
            related,
        ) {
            return Some(value);
        }

        if let Some(value) = recover_run_catching_class_probe(storage, m, prop_name) {
            return Some(value);
        }
    }

    // Kotlin deliberately omits writes for explicit JVM-default initializers
    // such as `var count = 0` and `var value: T? = null`.  Only use that
    // default when the raw initializer bytecode contains no write to this
    // exact field.  If a write exists but expression recovery failed, keeping
    // the property unresolved is safer than silently replacing its value.
    if !prop.flags.is_lateinit()
        && !prop.flags.is_delegated()
        && !initializer_writes_field(
            storage,
            init_method_name,
            &storage.this_class,
            prop_name,
            &field.descriptor,
            is_static_field,
        )
    {
        return jvm_default_field_value(
            &field.descriptor,
            prop.return_type.as_ref().is_some_and(|ty| ty.nullable),
        );
    }

    None
}

fn recover_computed_delegate(cf: &ClassFile, prop_name: &str) -> Option<String> {
    let getter_name = property_getter_names(prop_name).into_iter().next()?;
    let delegate_name = format!("{getter_name}$delegate");
    let method = cf
        .methods
        .iter()
        .find(|method| method.name == delegate_name && method.code().is_some())?;
    let descriptor = MethodDescriptor::parse(&method.descriptor).ok()?;
    let parameter_names = descriptor
        .params
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index == 0 {
                "this".to_string()
            } else {
                format!("p{index}")
            }
        })
        .collect::<Vec<_>>();
    let body = decompile_kotlin_body_with_bindings(method, cf, 0, &parameter_names, None)?;
    let expression = body.trim().strip_prefix("return ")?;
    (!expression.is_empty()
        && !expression.contains('\n')
        && !expression.contains("TODO(")
        && !expression.contains("opaque")
        && !expression.contains("/*"))
    .then(|| expression.to_string())
}

fn initializer_writes_field(
    cf: &ClassFile,
    initializer_name: &str,
    owner: &str,
    field_name: &str,
    descriptor: &str,
    is_static: bool,
) -> bool {
    let expected_opcode = if is_static {
        opc::putstatic
    } else {
        opc::putfield
    };

    cf.methods
        .iter()
        .filter(|method| method.name == initializer_name)
        .filter_map(|method| method.code())
        .flat_map(|code| &code.instructions)
        .any(|instruction| {
            if instruction.opcode != expected_opcode {
                return false;
            }
            let InsnKind::Cp { index } = instruction.kind else {
                // A malformed/unrecognised field write must disable the
                // optimistic default-value fallback.
                return true;
            };
            match cf.constant_pool.get(index) {
                Ok(CpEntry::Fieldref(reference)) => {
                    reference.class_name == owner
                        && reference.name == field_name
                        && reference.descriptor == descriptor
                }
                Err(_) => true,
                _ => false,
            }
        })
}

fn jvm_default_field_value(descriptor: &str, nullable: bool) -> Option<String> {
    match descriptor {
        "Z" => Some("false".to_string()),
        "B" | "S" | "I" => Some("0".to_string()),
        "J" => Some("0L".to_string()),
        "F" => Some("0.0f".to_string()),
        "D" => Some("0.0".to_string()),
        "C" => Some("'\\u0000'".to_string()),
        descriptor if nullable && (descriptor.starts_with('L') || descriptor.starts_with('[')) => {
            Some("null".to_string())
        }
        _ => None,
    }
}

fn render_constant_value(
    entry: &crate::classfile::constant_pool::CpEntry,
    descriptor: &str,
) -> Option<String> {
    use crate::classfile::constant_pool::CpEntry;
    match entry {
        CpEntry::Integer(value) if descriptor == "Z" => Some((value != &0).to_string()),
        CpEntry::Integer(value) if descriptor == "C" => {
            char::from_u32(*value as u32).map(|value| format!("'{}'", value.escape_default()))
        }
        CpEntry::Integer(value) => Some(value.to_string()),
        CpEntry::Long(value) => Some(format!("{}L", value)),
        CpEntry::Float(value) => Some(format!("{}f", value)),
        CpEntry::Double(value) => Some(value.to_string()),
        CpEntry::String(value) => Some(format!("\"{}\"", value.escape_default())),
        _ => None,
    }
}

fn recover_field_value(
    stmts: &[Expr],
    owner: &str,
    field_name: &str,
    method_descriptor: &str,
    is_static: bool,
    related: &[ClassFile],
) -> Option<String> {
    let mut locals = HashMap::<u16, Expr>::new();
    let mut recovered = None;

    for stmt in stmts {
        match stmt {
            Expr::Assign { lhs, rhs } => {
                if let Expr::LocalVar(local) = lhs.as_ref() {
                    let mut value = rhs.as_ref().clone();
                    substitute_initializer_locals(&mut value, &locals, &mut HashSet::new());
                    locals.insert(local.slot, value);
                }
            }
            Expr::ArrayStore {
                array,
                index,
                value,
            } => {
                let Expr::LocalVar(local) = array.as_ref() else {
                    continue;
                };
                let mut value = value.as_ref().clone();
                substitute_initializer_locals(&mut value, &locals, &mut HashSet::new());
                let Some(target) = locals.get_mut(&local.slot) else {
                    continue;
                };
                let Expr::NewArray { initializer, .. } = target else {
                    continue;
                };
                let Some(index) = initializer_index(index) else {
                    continue;
                };
                let values = initializer.get_or_insert_with(Vec::new);
                while values.len() <= index {
                    values.push(Expr::Const(ConstExpr {
                        value: ConstValue::Null,
                        ty: crate::types::java_type::JavaType::UNKNOWN,
                    }));
                }
                values[index] = value;
            }
            Expr::Field {
                dir: FieldDir::Put,
                owner: target_owner,
                name,
                value: Some(value),
                ..
            } if target_owner == owner && name == field_name => {
                let mut value = value.as_ref().clone();
                substitute_initializer_locals(&mut value, &locals, &mut HashSet::new());
                recovered = Some(value);
            }
            _ => {}
        }
    }

    let mut value = recovered?;
    replace_suspend_lambda_constructors(&mut value, related);
    let allowed_locals = initializer_parameter_slots(method_descriptor, is_static);
    if !initializer_expr_is_safe(&value, &allowed_locals) {
        return None;
    }
    let rendered = kt_render_expr(&value);
    is_complete_kotlin_expression(&rendered).then_some(rendered)
}

fn recover_run_catching_class_probe(
    cf: &ClassFile,
    method: &crate::classfile::member::Method,
    field_name: &str,
) -> Option<String> {
    let code = method.code()?;
    if !code
        .exception_table
        .iter()
        .any(|handler| handler.catch_type.as_deref() == Some("java/lang/Throwable"))
    {
        return None;
    }

    let mut has_result_constructor = false;
    let mut has_result_failure_test = false;
    let mut has_create_failure = false;
    let mut class_name = None;
    let mut booleans = Vec::new();

    for (index, instruction) in code.instructions.iter().enumerate() {
        let InsnKind::Invoke {
            index: cp_index, ..
        } = instruction.kind
        else {
            continue;
        };
        let reference = match cf.constant_pool.get(cp_index).ok()? {
            CpEntry::Methodref(reference) | CpEntry::InterfaceMethodref(reference) => reference,
            _ => continue,
        };
        match (reference.class_name.as_str(), reference.name.as_str()) {
            ("kotlin/Result", "constructor-impl") => has_result_constructor = true,
            ("kotlin/Result", "isFailure-impl") => has_result_failure_test = true,
            ("kotlin/ResultKt", "createFailure") => has_create_failure = true,
            ("java/lang/Class", "forName") => {
                let previous = code.instructions.get(index.checked_sub(1)?)?;
                let InsnKind::Ldc { index } = previous.kind else {
                    continue;
                };
                if let Ok(CpEntry::String(value)) = cf.constant_pool.get(index) {
                    class_name = Some(value.clone());
                }
            }
            ("java/lang/Boolean", "valueOf") => {
                let previous = code.instructions.get(index.checked_sub(1)?)?;
                if let Some(value) = instruction_int_constant(cf, previous) {
                    booleans.push(value != 0);
                }
            }
            _ => {}
        }
    }

    let writes_target = code.instructions.iter().any(|instruction| {
        let InsnKind::Cp { index } = instruction.kind else {
            return false;
        };
        matches!(
            cf.constant_pool.get(index),
            Ok(CpEntry::Fieldref(reference))
                if reference.class_name == cf.this_class && reference.name == field_name
        )
    });
    let [success, fallback] = booleans.as_slice() else {
        return None;
    };
    if !writes_target || !has_result_constructor || !has_result_failure_test || !has_create_failure
    {
        return None;
    }

    Some(format!(
        "runCatching {{ Class.forName(\"{}\"); {} }}.getOrDefault({})",
        class_name?.escape_default(),
        success,
        fallback
    ))
}

fn is_complete_kotlin_expression(rendered: &str) -> bool {
    !rendered.is_empty() && !rendered.contains("opaque") && !rendered.contains("/*")
}

fn initializer_index(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Const(ConstExpr {
            value: ConstValue::Int(value),
            ..
        }) if *value >= 0 => Some(*value as usize),
        _ => None,
    }
}

fn initializer_parameter_slots(descriptor: &str, is_static: bool) -> HashSet<u16> {
    let mut slots = HashSet::new();
    if !is_static {
        slots.insert(0);
    }
    let Ok(descriptor) = MethodDescriptor::parse(descriptor) else {
        return slots;
    };
    let mut slot = if is_static { 0 } else { 1 };
    for ty in descriptor.params {
        slots.insert(slot);
        slot += if ty.is_wide() { 2 } else { 1 };
    }
    slots
}

fn replace_suspend_lambda_constructors(expr: &mut Expr, related: &[ClassFile]) {
    if let Expr::New { args, .. } = expr {
        for arg in args {
            replace_suspend_lambda_constructors(arg, related);
        }
    }

    if let Expr::New {
        class_name,
        args,
        descriptor,
    } = expr
    {
        if class_name.contains("$$inlined$computedOn") {
            if let Some(body) = decompile_computed_on_delegate(class_name, related) {
                if args.len() == 3 {
                    *expr = Expr::InvokeDynamic {
                        name: "computedOn".into(),
                        descriptor: "()Lkotlin/properties/ReadWriteProperty;".into(),
                        bootstrap_index: u16::MAX,
                        args: args.clone(),
                        concat_recipe: None,
                        lambda_body: Some(LambdaBootstrap::KotlinLambda {
                            body,
                            capture_count: 3,
                        }),
                    };
                    return;
                }
            }
        }

        if let Some(class) = related.iter().find(|class| class.this_class == *class_name) {
            if class.super_class.as_deref() == Some("kotlin/coroutines/jvm/internal/SuspendLambda")
            {
                if let Some((body, capture_count)) = decompile_kotlin_suspend_lambda(class, related)
                {
                    let captures = MethodDescriptor::parse(descriptor)
                        .ok()
                        .map(|constructor| {
                            args.iter()
                                .zip(constructor.params.iter())
                                .filter(|(_, ty)| {
                                    ty.class_name.as_deref()
                                        != Some("kotlin/coroutines/Continuation")
                                })
                                .map(|(arg, _)| arg.clone())
                                .take(capture_count)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if captures.len() != capture_count {
                        return;
                    }
                    *expr = Expr::InvokeDynamic {
                        name: "suspendLambda".into(),
                        descriptor: "()Lkotlin/jvm/functions/Function3;".into(),
                        bootstrap_index: u16::MAX,
                        args: captures,
                        concat_recipe: None,
                        lambda_body: Some(LambdaBootstrap::KotlinLambda {
                            body,
                            capture_count,
                        }),
                    };
                    return;
                }
            }
            if let Some((body, capture_count)) = decompile_kotlin_function_object(class) {
                let captures = args.clone();
                *expr = Expr::InvokeDynamic {
                    name: "kotlinFunctionObject".into(),
                    descriptor: "()Lkotlin/jvm/functions/Function;".into(),
                    bootstrap_index: u16::MAX,
                    args: captures,
                    concat_recipe: None,
                    lambda_body: Some(LambdaBootstrap::KotlinLambda {
                        body,
                        capture_count,
                    }),
                };
                return;
            }
            if let Some((body, capture_count)) =
                decompile_kotlin_anonymous_object(class, descriptor, related)
            {
                if args.len() != capture_count {
                    return;
                }
                let captures = args
                    .iter()
                    .cloned()
                    .map(label_anonymous_outer_this)
                    .collect();
                *expr = Expr::InvokeDynamic {
                    name: "anonymousObject".into(),
                    descriptor: descriptor.clone(),
                    bootstrap_index: u16::MAX,
                    args: captures,
                    concat_recipe: None,
                    lambda_body: Some(LambdaBootstrap::KotlinLambda {
                        body,
                        capture_count,
                    }),
                };
                return;
            }
        }
    }

    match expr {
        Expr::BinOp(_, left, right) => {
            replace_suspend_lambda_constructors(left, related);
            replace_suspend_lambda_constructors(right, related);
        }
        Expr::UnOp(_, value)
        | Expr::Cast(_, _, value)
        | Expr::InstanceOf(value, _)
        | Expr::ArrayLength(value)
        | Expr::Throw(value) => replace_suspend_lambda_constructors(value, related),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            if let crate::ir::TernaryCondition::Expression(cond) = cond {
                replace_suspend_lambda_constructors(cond, related);
            }
            replace_suspend_lambda_constructors(then_expr, related);
            replace_suspend_lambda_constructors(else_expr, related);
        }
        Expr::SwitchExpression { selector, arms } => {
            replace_suspend_lambda_constructors(selector, related);
            for (_, value) in arms {
                replace_suspend_lambda_constructors(value, related);
            }
        }
        Expr::Field { object, value, .. } => {
            if let Some(object) = object {
                replace_suspend_lambda_constructors(object, related);
            }
            if let Some(value) = value {
                replace_suspend_lambda_constructors(value, related);
            }
        }
        Expr::Invoke { object, args, .. } => {
            if let Some(object) = object {
                replace_suspend_lambda_constructors(object, related);
            }
            for arg in args {
                replace_suspend_lambda_constructors(arg, related);
            }
        }
        Expr::InvokeDynamic { args, .. } => {
            for arg in args {
                replace_suspend_lambda_constructors(arg, related);
            }
        }
        Expr::New { .. } => {}
        Expr::ArrayLoad { array, index, .. } => {
            replace_suspend_lambda_constructors(array, related);
            replace_suspend_lambda_constructors(index, related);
        }
        Expr::ArrayStore {
            array,
            index,
            value,
        } => {
            replace_suspend_lambda_constructors(array, related);
            replace_suspend_lambda_constructors(index, related);
            replace_suspend_lambda_constructors(value, related);
        }
        Expr::NewArray {
            dimensions,
            initializer,
            ..
        } => {
            for dimension in dimensions {
                replace_suspend_lambda_constructors(dimension, related);
            }
            if let Some(values) = initializer {
                for value in values {
                    replace_suspend_lambda_constructors(value, related);
                }
            }
        }
        Expr::Assign { lhs, rhs } => {
            replace_suspend_lambda_constructors(lhs, related);
            replace_suspend_lambda_constructors(rhs, related);
        }
        Expr::Monitor { object, .. } => replace_suspend_lambda_constructors(object, related),
        Expr::Return(Some(value)) => replace_suspend_lambda_constructors(value, related),
        Expr::Const(_)
        | Expr::LocalVar(_)
        | Expr::Null
        | Expr::This(_)
        | Expr::IInc { .. }
        | Expr::Return(None)
        | Expr::Opaque { .. } => {}
    }
}

fn kotlin_type_is_anonymous(ty: &KType) -> bool {
    ty.class_name
        .as_deref()
        .and_then(|name| name.rsplit('$').next())
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
}

fn label_anonymous_outer_this(expr: Expr) -> Expr {
    match expr {
        Expr::This(owner) => Expr::LocalVar(crate::ir::LocalVarExpr {
            slot: 0,
            ty: JavaType::object(&owner),
            name: Some(format!(
                "this@{}",
                owner.rsplit(['/', '$']).next().unwrap_or(&owner)
            )),
        }),
        Expr::LocalVar(mut local)
            if local.slot == 0 && local.name.as_deref().is_some_and(|name| name == "this") =>
        {
            if let Some(owner) = local.ty.class_name.as_deref() {
                local.name = Some(format!(
                    "this@{}",
                    owner.rsplit(['/', '$']).next().unwrap_or(owner)
                ));
            }
            Expr::LocalVar(local)
        }
        other => other,
    }
}

fn decompile_kotlin_anonymous_object(
    class: &ClassFile,
    constructor_descriptor: &str,
    related: &[ClassFile],
) -> Option<(String, usize)> {
    let suffix = class.this_class.rsplit('$').next()?;
    if suffix.is_empty() || !suffix.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let metadata = parse_kotlin_metadata(get_kotlin_annotations(class)?)?;
    if metadata.kind != MetadataKind::Class {
        return None;
    }
    let cls = metadata.class.as_ref()?;
    let constructor = class.methods.iter().find(|method| {
        method.name == "<init>"
            && method.descriptor == constructor_descriptor
            && method.code().is_some()
    })?;
    let descriptor = MethodDescriptor::parse(constructor_descriptor).ok()?;
    let capture_count = descriptor.params.len();
    let parameter_names = (0..capture_count)
        .map(|index| format!("$kotlin$capture${index}"))
        .collect::<Vec<_>>();
    let super_arguments = recover_anonymous_super_arguments(class, constructor, &parameter_names)?;

    let mut supertypes = cls
        .supertypes
        .iter()
        .filter(|ty| ty.class_name.as_deref() != Some("kotlin/Any"))
        .map(|ty| render_kotlin_type(ty, &cls.type_parameters))
        .collect::<Vec<_>>();
    if class.super_class.as_deref() != Some("java/lang/Object") {
        let supertype = supertypes.first_mut()?;
        supertype.push('(');
        supertype.push_str(&super_arguments);
        supertype.push(')');
    }

    let mut body = String::new();
    render_class_body(&mut body, class, cls, None, related);
    for (index, field) in class
        .fields
        .iter()
        .filter(|field| !field.is_static() && field.is_synthetic())
        .enumerate()
    {
        body = body.replace(
            &format!("this.{}", kotlin_identifier(&field.name)),
            &format!("__abyss_capture_{index}"),
        );
    }
    if body.contains("opaque") || body.contains("TODO(") || body.contains("/*") {
        return None;
    }
    body = body
        .lines()
        .map(|line| {
            let line = line
                .replacen("    open val ", "    override val ", 1)
                .replacen("    open var ", "    override var ", 1)
                .replacen("    open fun ", "    override fun ", 1)
                .replacen("    open operator fun ", "    override operator fun ", 1);
            format!("    {line}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let header = if supertypes.is_empty() {
        "object".to_string()
    } else {
        format!("object : {}", supertypes.join(", "))
    };
    let source = if body.trim().is_empty() {
        format!("{header} {{}}")
    } else {
        format!("{header} {{\n{body}\n    }}")
    };
    Some((source, capture_count))
}

fn recover_anonymous_super_arguments(
    class: &ClassFile,
    constructor: &crate::classfile::member::Method,
    parameter_names: &[String],
) -> Option<String> {
    if class.super_class.as_deref() == Some("java/lang/Object") {
        return Some(String::new());
    }
    let code = constructor.code()?;
    let super_name = class.super_class.as_deref()?;
    let super_index = code.instructions.iter().position(|instruction| {
        let InsnKind::Invoke { index, .. } = instruction.kind else {
            return false;
        };
        matches!(
            class.constant_pool.get(index),
            Ok(CpEntry::Methodref(reference))
                if reference.class_name == super_name && reference.name == "<init>"
        )
    })?;
    let context =
        RenderContext::for_method(code, class, false, &constructor.descriptor, parameter_names);
    let result = context.simulate(&code.instructions[..=super_index], vec![]);
    let (args, descriptor) = result.stmts.iter().find_map(|statement| match statement {
        Expr::Invoke {
            name,
            args,
            descriptor,
            ..
        } if name == "super" => Some((args.as_slice(), descriptor.as_str())),
        _ => None,
    })?;
    let args = source_constructor_arguments(args, descriptor);
    let parameter_types = MethodDescriptor::parse(descriptor).ok()?.params;
    Some(
        args.iter()
            .enumerate()
            .map(|(index, arg)| {
                if parameter_types.get(index) == Some(&JavaType::BOOLEAN) {
                    if let Expr::Const(ConstExpr {
                        value: ConstValue::Int(value),
                        ..
                    }) = arg
                    {
                        return (*value != 0).to_string();
                    }
                }
                kt_render_expr(arg)
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn source_constructor_arguments<'a>(args: &'a [Expr], descriptor: &str) -> Vec<&'a Expr> {
    let Ok(descriptor) = MethodDescriptor::parse(descriptor) else {
        return args.iter().collect();
    };
    let marker_index = descriptor.params.len().checked_sub(1);
    if marker_index
        .and_then(|index| descriptor.params.get(index))
        .and_then(|ty| ty.class_name.as_deref())
        != Some("kotlin/jvm/internal/DefaultConstructorMarker")
    {
        return args.iter().collect();
    }
    let marker_index = marker_index.unwrap_or_default();
    let Some(mask) = marker_index
        .checked_sub(1)
        .and_then(|index| args.get(index))
        .and_then(|arg| match arg {
            Expr::Const(ConstExpr {
                value: ConstValue::Int(value),
                ..
            }) => Some(*value as u32),
            _ => None,
        })
    else {
        return args.iter().collect();
    };
    let source_count = marker_index.saturating_sub(1);
    let last_required = (0..source_count)
        .rev()
        .find(|index| mask & (1u32 << index) == 0);
    last_required.map_or_else(Vec::new, |last| args[..=last].iter().collect())
}

fn decompile_computed_on_delegate(class_name: &str, related: &[ClassFile]) -> Option<String> {
    let consumer_name = format!("{class_name}$1");
    let consumer = related
        .iter()
        .find(|class| class.this_class == consumer_name)?;
    let method = consumer.methods.iter().find(|method| {
        method.name == "accept"
            && !method.is_bridge()
            && !method.is_synthetic()
            && method.code().is_some()
    })?;
    let code = method.code()?;
    let current_name = crate::codegen::stmt_writer::lvt_entries(code)
        .into_iter()
        .filter(|entry| entry.slot == 2 && !entry.name.starts_with('$'))
        .min_by_key(|entry| entry.start_pc)
        .map(|entry| kotlin_identifier(&entry.name))
        .unwrap_or_else(|| "current".into());
    let body = decompile_kotlin_body_with_bindings(method, consumer, 0, &["event".into()], None)?;
    let setter = "access$setValue$p`(";
    let mut lines = Vec::new();
    let mut returned = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.contains("access$getValue$p`(")
            || (trimmed.starts_with("val ") && trimmed.contains("this.`this$0`"))
        {
            continue;
        }
        if trimmed.contains(setter) {
            let open = trimmed.find('(')?;
            let close = trimmed.rfind(')')?;
            let arguments = split_top_level_arguments(&trimmed[open + 1..close]);
            let value = arguments.get(1)?.trim();
            lines.push(format!("return@__abyss_computed {value}"));
            returned = true;
            continue;
        }
        lines.push(line.to_string());
    }
    if !returned {
        return None;
    }
    let accumulator = format!(
        "__abyss_computed@ {{ event, {current_name} ->\n{}\n}}",
        lines
            .iter()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    Some(format!(
        "EventListenerKt.computedOn(__abyss_capture_1 as EventListener, __abyss_capture_0, (__abyss_capture_2).toShort(), {accumulator})"
    ))
}

fn substitute_initializer_locals(
    expr: &mut Expr,
    locals: &HashMap<u16, Expr>,
    visiting: &mut HashSet<u16>,
) {
    if let Expr::LocalVar(local) = expr {
        let slot = local.slot;
        if visiting.insert(slot) {
            if let Some(value) = locals.get(&slot) {
                *expr = value.clone();
                substitute_initializer_locals(expr, locals, visiting);
            }
            visiting.remove(&slot);
        }
        return;
    }

    match expr {
        Expr::BinOp(_, left, right) => {
            substitute_initializer_locals(left, locals, visiting);
            substitute_initializer_locals(right, locals, visiting);
        }
        Expr::UnOp(_, value)
        | Expr::Cast(_, _, value)
        | Expr::InstanceOf(value, _)
        | Expr::ArrayLength(value)
        | Expr::Throw(value) => substitute_initializer_locals(value, locals, visiting),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            if let crate::ir::TernaryCondition::Expression(cond) = cond {
                substitute_initializer_locals(cond, locals, visiting);
            }
            substitute_initializer_locals(then_expr, locals, visiting);
            substitute_initializer_locals(else_expr, locals, visiting);
        }
        Expr::SwitchExpression { selector, arms } => {
            substitute_initializer_locals(selector, locals, visiting);
            for (_, value) in arms {
                substitute_initializer_locals(value, locals, visiting);
            }
        }
        Expr::Field { object, value, .. } => {
            if let Some(object) = object {
                substitute_initializer_locals(object, locals, visiting);
            }
            if let Some(value) = value {
                substitute_initializer_locals(value, locals, visiting);
            }
        }
        Expr::Invoke { object, args, .. } => {
            if let Some(object) = object {
                substitute_initializer_locals(object, locals, visiting);
            }
            for arg in args {
                substitute_initializer_locals(arg, locals, visiting);
            }
        }
        Expr::InvokeDynamic { args, .. } | Expr::New { args, .. } => {
            for arg in args {
                substitute_initializer_locals(arg, locals, visiting);
            }
        }
        Expr::ArrayLoad { array, index, .. } => {
            substitute_initializer_locals(array, locals, visiting);
            substitute_initializer_locals(index, locals, visiting);
        }
        Expr::ArrayStore {
            array,
            index,
            value,
        } => {
            substitute_initializer_locals(array, locals, visiting);
            substitute_initializer_locals(index, locals, visiting);
            substitute_initializer_locals(value, locals, visiting);
        }
        Expr::NewArray {
            dimensions,
            initializer,
            ..
        } => {
            for dimension in dimensions {
                substitute_initializer_locals(dimension, locals, visiting);
            }
            if let Some(values) = initializer {
                for value in values {
                    substitute_initializer_locals(value, locals, visiting);
                }
            }
        }
        Expr::Assign { lhs, rhs } => {
            substitute_initializer_locals(lhs, locals, visiting);
            substitute_initializer_locals(rhs, locals, visiting);
        }
        Expr::Monitor { object, .. } => {
            substitute_initializer_locals(object, locals, visiting);
        }
        Expr::Return(Some(value)) => substitute_initializer_locals(value, locals, visiting),
        Expr::Const(_)
        | Expr::LocalVar(_)
        | Expr::Null
        | Expr::This(_)
        | Expr::IInc { .. }
        | Expr::Return(None)
        | Expr::Opaque { .. } => {}
    }
}

fn initializer_expr_is_safe(expr: &Expr, allowed_locals: &HashSet<u16>) -> bool {
    match expr {
        Expr::Opaque { .. }
        | Expr::Assign { .. }
        | Expr::IInc { .. }
        | Expr::Monitor { .. }
        | Expr::Throw(_)
        | Expr::Return(_)
        | Expr::ArrayStore { .. }
        | Expr::Field {
            dir: FieldDir::Put, ..
        } => false,
        Expr::LocalVar(local) => allowed_locals.contains(&local.slot) && local.name.is_some(),
        Expr::Const(_) | Expr::Null | Expr::This(_) => true,
        Expr::BinOp(_, left, right) => {
            initializer_expr_is_safe(left, allowed_locals)
                && initializer_expr_is_safe(right, allowed_locals)
        }
        Expr::UnOp(_, value)
        | Expr::Cast(_, _, value)
        | Expr::InstanceOf(value, _)
        | Expr::ArrayLength(value) => initializer_expr_is_safe(value, allowed_locals),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            (match cond {
                crate::ir::TernaryCondition::Rendered(_) => true,
                crate::ir::TernaryCondition::Expression(cond) => {
                    initializer_expr_is_safe(cond, allowed_locals)
                }
            }) && initializer_expr_is_safe(then_expr, allowed_locals)
                && initializer_expr_is_safe(else_expr, allowed_locals)
        }
        Expr::SwitchExpression { selector, arms } => {
            initializer_expr_is_safe(selector, allowed_locals)
                && arms
                    .iter()
                    .all(|(_, value)| initializer_expr_is_safe(value, allowed_locals))
        }
        Expr::Field { object, value, .. } => {
            object
                .as_deref()
                .is_none_or(|value| initializer_expr_is_safe(value, allowed_locals))
                && value
                    .as_deref()
                    .is_none_or(|value| initializer_expr_is_safe(value, allowed_locals))
        }
        Expr::Invoke { object, args, .. } => {
            object
                .as_deref()
                .is_none_or(|value| initializer_expr_is_safe(value, allowed_locals))
                && args
                    .iter()
                    .all(|value| initializer_expr_is_safe(value, allowed_locals))
        }
        Expr::InvokeDynamic {
            name,
            args,
            lambda_body,
            ..
        } => {
            (lambda_body.is_some() || name == "makeConcat" || name == "makeConcatWithConstants")
                && args
                    .iter()
                    .all(|value| initializer_expr_is_safe(value, allowed_locals))
        }
        Expr::ArrayLoad { array, index, .. } => {
            initializer_expr_is_safe(array, allowed_locals)
                && initializer_expr_is_safe(index, allowed_locals)
        }
        Expr::NewArray {
            dimensions,
            initializer,
            ..
        } => {
            dimensions
                .iter()
                .all(|value| initializer_expr_is_safe(value, allowed_locals))
                && initializer.as_ref().is_none_or(|values| {
                    values
                        .iter()
                        .all(|value| initializer_expr_is_safe(value, allowed_locals))
                })
        }
        Expr::New {
            class_name, args, ..
        } => {
            source_named_class(class_name)
                && args
                    .iter()
                    .all(|value| initializer_expr_is_safe(value, allowed_locals))
        }
    }
}

fn source_named_class(class_name: &str) -> bool {
    class_name.split('$').skip(1).all(|segment| {
        segment
            .chars()
            .next()
            .is_some_and(|first| first == '_' || first.is_alphabetic())
    })
}

// ── Annotation rendering ──────────────────────────────────────────────────

fn render_class_annotations(out: &mut String, cf: &ClassFile) {
    for attr in &cf.attributes {
        if let Attribute::RuntimeVisibleAnnotations(anns) = attr {
            for ann in anns {
                // Skip kotlin internal annotations
                if ann.type_descriptor.starts_with("Lkotlin/")
                    || ann
                        .type_descriptor
                        .starts_with("Lorg/jetbrains/annotations/")
                {
                    continue;
                }
                let name = ann
                    .type_descriptor
                    .trim_start_matches('L')
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

fn kotlin_dispatch_receiver_label(class_name: &str) -> String {
    class_name
        .rsplit(['/', '$'])
        .next()
        .unwrap_or(class_name)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        is_complete_kotlin_expression, is_simple_field_getter, kotlin_dispatch_receiver_label,
        kotlin_type_is_anonymous, kotlin_type_match_score, label_anonymous_outer_this,
        merge_enum_entry_declaration, source_constructor_arguments, source_named_class,
    };
    use crate::ir::{ConstExpr, ConstValue, Expr, LocalVarExpr};
    use crate::kotlin::metadata::{KFlags, KType};
    use crate::types::JavaType;

    fn class_type(name: &str) -> KType {
        KType {
            flags: KFlags(0),
            nullable: false,
            class_name: Some(name.into()),
            type_parameter_id: None,
            type_parameter_name: None,
            arguments: Vec::new(),
            outer_type: None,
            abbreviated_type: None,
        }
    }

    #[test]
    fn dispatch_receiver_label_uses_innermost_declaration_name() {
        assert_eq!(kotlin_dispatch_receiver_label("pkg/Outer$Inner"), "Inner");
        assert_eq!(kotlin_dispatch_receiver_label("pkg/TopLevel"), "TopLevel");
    }

    #[test]
    fn metadata_type_matching_distinguishes_overload_receivers_and_primitives() {
        let block_pos = class_type("net/minecraft/core/BlockPos");
        assert!(
            kotlin_type_match_score(&block_pos, &JavaType::object("net/minecraft/core/BlockPos"))
                > kotlin_type_match_score(
                    &block_pos,
                    &JavaType::object("net/minecraft/world/phys/Vec3")
                )
        );

        let int_type = class_type("kotlin/Int");
        assert!(
            kotlin_type_match_score(&int_type, &JavaType::INT)
                > kotlin_type_match_score(&int_type, &JavaType::DOUBLE)
        );
    }

    #[test]
    fn named_nested_classes_are_safe_initializer_types() {
        assert!(source_named_class("pkg/Outer$Named"));
        assert!(source_named_class("pkg/Outer$Named$Nested"));
        assert!(!source_named_class("pkg/Outer$1"));
        assert!(!source_named_class("pkg/Outer$$inlined"));
    }

    #[test]
    fn anonymous_object_type_and_outer_this_are_source_level() {
        let anonymous = class_type("pkg/Outer$value$1");
        assert!(kotlin_type_is_anonymous(&anonymous));

        let labeled = label_anonymous_outer_this(Expr::LocalVar(LocalVarExpr {
            slot: 0,
            ty: JavaType::object("pkg/Outer$Nested"),
            name: Some("this".into()),
        }));
        assert!(matches!(
            labeled,
            Expr::LocalVar(LocalVarExpr { name: Some(name), .. }) if name == "this@Nested"
        ));
    }

    #[test]
    fn anonymous_super_call_drops_trailing_defaulted_arguments() {
        let int = |value| {
            Expr::Const(ConstExpr {
                value: ConstValue::Int(value),
                ty: JavaType::INT,
            })
        };
        let args = vec![int(1), int(2), Expr::Null, int(4), Expr::Null];
        let visible = source_constructor_arguments(
            &args,
            "(IZLjava/util/List;ILkotlin/jvm/internal/DefaultConstructorMarker;)V",
        );
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn initializer_completeness_accepts_literal_ellipsis() {
        assert!(is_complete_kotlin_expression("logger.info(\"Loading...\")"));
        assert!(!is_complete_kotlin_expression("/* unresolved */"));
        assert!(!is_complete_kotlin_expression("opaque"));
    }

    #[test]
    fn simple_getter_matching_observes_identifier_boundaries() {
        assert!(is_simple_field_getter("return this.bind", "bind"));
        assert!(is_simple_field_getter("return this.bind$delegate", "bind"));
        assert!(!is_simple_field_getter(
            "return this.bindValue.get()",
            "bind"
        ));
    }

    #[test]
    fn specialized_enum_entry_body_is_merged_into_entry_list() {
        let mut host = "enum class Mode {\n    FIRST,\n    SECOND;\n}\n".to_string();
        let child = "object FIRST : Mode {\n    fun value(): Int {\n        return 1\n    }\n}";
        merge_enum_entry_declaration(&mut host, "FIRST", child);
        assert!(host.contains(
            "    FIRST {\n        fun value(): Int {\n            return 1\n        }\n    },"
        ));
        assert!(!host.contains("object FIRST"));
    }
}
