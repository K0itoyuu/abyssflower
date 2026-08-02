/// Class-level code generator.
/// Renders a parsed ClassFile + its structured IR into a complete Java source file.
use crate::cfg::{builder as cfg_builder, DomTree};
use crate::classfile::attribute::{Annotation, Attribute, BootstrapMethod, ElementValue};
use crate::classfile::constant_pool::{ConstantPool, CpEntry};
use crate::classfile::member::{flags, Field, Method};
use crate::classfile::ClassFile;
use crate::codegen::expr_writer::{simple_name, IndentWriter};
use crate::codegen::render_context::RenderContext;
use crate::codegen::stmt_writer::render_method_body;
use crate::ir::recovery::recover;
use crate::ir::LambdaBootstrap;
use crate::types::descriptor::{parse_field_descriptor, MethodDescriptor};
use crate::types::java_type::JavaType;
use crate::types::signature::{
    parse_class_signature, parse_field_signature, parse_method_signature, GenericType,
};
use std::collections::{BTreeSet, HashMap};

// ── Annotation rendering ──────────────────────────────────────────────────

/// Render a single `ElementValue` to its Java source form.
fn render_element_value(v: &ElementValue) -> String {
    match v {
        ElementValue::Byte(i) => format!("(byte){}", i),
        ElementValue::Char(i) => format!("'{}'", char::from_u32(*i as u32).unwrap_or('?')),
        ElementValue::Double(d) => format!("{}d", d),
        ElementValue::Float(f) => format!("{}f", f),
        ElementValue::Int(i) => i.to_string(),
        ElementValue::Long(l) => format!("{}L", l),
        ElementValue::Short(i) => format!("(short){}", i),
        ElementValue::Boolean(b) => b.to_string(),
        ElementValue::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        ElementValue::ClassInfo(c) => {
            let name = c.trim_start_matches('L').trim_end_matches(';');
            format!("{}.class", name.replace('/', "."))
        }
        ElementValue::EnumConst {
            type_name,
            const_name,
        } => {
            let ty = type_name
                .trim_start_matches('L')
                .trim_end_matches(';')
                .replace('/', ".");
            let simple = ty.rsplit('.').next().unwrap_or(&ty);
            format!("{}.{}", simple, const_name)
        }
        ElementValue::Annotation(ann) => render_annotation(ann),
        ElementValue::Array(elems) => {
            if elems.len() == 1 {
                render_element_value(&elems[0])
            } else {
                let inner: Vec<String> = elems.iter().map(render_element_value).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }
}

/// Render a full annotation, e.g. `@Mixin(Entity.class)` or
/// `@Inject(method = "tick", at = @At("HEAD"))`.
fn render_annotation(ann: &Annotation) -> String {
    let name = ann
        .type_descriptor
        .trim_start_matches('L')
        .trim_end_matches(';')
        .replace('/', ".");
    // Use simple name for readability (imports handle the rest)
    let simple = name.rsplit('.').next().unwrap_or(&name);
    if ann.elements.is_empty() {
        return format!("@{}", simple);
    }
    // If there is exactly one element named "value", omit the key.
    if ann.elements.len() == 1 && ann.elements[0].0 == "value" {
        return format!("@{}({})", simple, render_element_value(&ann.elements[0].1));
    }
    let pairs: Vec<String> = ann
        .elements
        .iter()
        .map(|(k, v)| format!("{} = {}", k, render_element_value(v)))
        .collect();
    format!("@{}({})", simple, pairs.join(", "))
}

/// Write all annotations (both visible and invisible) for a slice of attributes.
fn write_annotations(attrs: &[Attribute], w: &mut IndentWriter) {
    for attr in attrs {
        let anns: &[Annotation] = match attr {
            Attribute::RuntimeVisibleAnnotations(a) => a,
            Attribute::RuntimeInvisibleAnnotations(a) => a,
            _ => continue,
        };
        for ann in anns {
            w.line(&render_annotation(ann));
        }
    }
}

// ── import collection ─────────────────────────────────────────────────────

/// Return a sorted, de-duped set of fully-qualified class names that should
/// be imported.  We scan the constant pool for Class entries and keep only
/// those in a different package from `this_class`, not in `java.lang`, and
/// not array types.
fn collect_imports(cf: &ClassFile) -> BTreeSet<String> {
    let this_pkg = cf.this_class.rsplit_once('/').map(|(p, _)| p).unwrap_or("");

    let mut imports = BTreeSet::new();

    for entry in cf.constant_pool.entries() {
        let name = match entry {
            CpEntry::Class(n) => n.as_str(),
            CpEntry::Fieldref(mr) | CpEntry::Methodref(mr) | CpEntry::InterfaceMethodref(mr) => {
                mr.class_name.as_str()
            }
            _ => continue,
        };
        // Skip arrays, primitives, the class itself
        if name.starts_with('[') || !name.contains('/') {
            continue;
        }
        if name == cf.this_class {
            continue;
        }
        // Skip java.lang.*
        if name.starts_with("java/lang/") && !name["java/lang/".len()..].contains('/') {
            continue;
        }
        // Skip java.lang.invoke.* — these are implementation details of invokedynamic
        // (StringConcatFactory, MethodHandles, LambdaMetafactory, etc.) that we desugar
        // into readable Java source.  Importing them just creates noise.
        if name.starts_with("java/lang/invoke/") {
            continue;
        }
        // Skip same-package types
        let pkg = name.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        if pkg == this_pkg {
            continue;
        }

        imports.insert(name.replace('/', "."));
    }

    // Annotation types live in the constant pool as UTF-8 descriptors, not
    // Class entries, so they need collecting separately — otherwise a class
    // using @Mixin / @Inject / @At renders those names unqualified with no
    // matching import.
    let add_ann = |anns: &[Annotation], imports: &mut BTreeSet<String>| {
        collect_annotation_imports(anns, this_pkg, &cf.this_class, imports);
    };
    for attrs in std::iter::once(&cf.attributes)
        .chain(cf.fields.iter().map(|f| &f.attributes))
        .chain(cf.methods.iter().map(|m| &m.attributes))
    {
        for attr in attrs {
            match attr {
                Attribute::RuntimeVisibleAnnotations(a)
                | Attribute::RuntimeInvisibleAnnotations(a) => add_ann(a, &mut imports),
                Attribute::RuntimeVisibleParameterAnnotations(groups)
                | Attribute::RuntimeInvisibleParameterAnnotations(groups) => {
                    for g in groups {
                        add_ann(g, &mut imports);
                    }
                }
                _ => {}
            }
        }
    }

    imports
}

/// Walk an annotation tree, adding every annotation type (including nested
/// annotations and those inside array values) to the import set.
fn collect_annotation_imports(
    anns: &[Annotation],
    this_pkg: &str,
    this_class: &str,
    imports: &mut BTreeSet<String>,
) {
    fn visit_value(
        v: &ElementValue,
        this_pkg: &str,
        this_class: &str,
        imports: &mut BTreeSet<String>,
    ) {
        match v {
            ElementValue::Annotation(a) => {
                collect_annotation_imports(std::slice::from_ref(a), this_pkg, this_class, imports)
            }
            ElementValue::Array(elems) => {
                for e in elems {
                    visit_value(e, this_pkg, this_class, imports);
                }
            }
            _ => {}
        }
    }

    for ann in anns {
        let binary = ann
            .type_descriptor
            .trim_start_matches('L')
            .trim_end_matches(';');
        if binary.contains('/') && binary != this_class {
            let pkg = binary.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            let is_java_lang =
                binary.starts_with("java/lang/") && !binary["java/lang/".len()..].contains('/');
            if pkg != this_pkg && !is_java_lang {
                imports.insert(binary.replace('/', "."));
            }
        }
        for (_, v) in &ann.elements {
            visit_value(v, this_pkg, this_class, imports);
        }
    }
}

// ── public entry ──────────────────────────────────────────────────────────

/// Render a full class file to Java source code.
pub fn render_class(cf: &ClassFile) -> String {
    let mut w = IndentWriter::new(4);
    let lambda_bootstrap = build_lambda_bootstrap(cf);

    // ── package declaration ────────────────────────────────────────────
    let this = &cf.this_class;
    if let Some(slash) = this.rfind('/') {
        let pkg = this[..slash].replace('/', ".");
        w.line(&format!("package {};", pkg));
        w.line("");
    }

    // ── imports ────────────────────────────────────────────────────────
    let imports = collect_imports(cf);
    if !imports.is_empty() {
        for imp in &imports {
            w.line(&format!("import {};", imp));
        }
        w.line("");
    }

    // ── class-level annotations ────────────────────────────────────────
    write_annotations(&cf.attributes, &mut w);

    // ── class declaration ──────────────────────────────────────────────
    let decl = build_class_declaration(cf);
    w.line(&format!("{} {{", decl));
    w.indent();

    if cf.is_enum() {
        let enum_consts: Vec<&Field> = cf.fields.iter().filter(|f| f.is_enum()).collect();
        if !enum_consts.is_empty() {
            w.line("");
            let names: Vec<&str> = enum_consts.iter().map(|f| f.name.as_str()).collect();
            w.line(&format!("{};", names.join(",\n    ")));
        }

        let extra_fields: Vec<&Field> = cf
            .fields
            .iter()
            .filter(|f| !f.is_enum() && !should_skip_field(f))
            .collect();
        for f in extra_fields {
            w.line("");
            render_field(f, cf, &mut w);
        }
    } else {
        let visible_fields: Vec<&Field> =
            cf.fields.iter().filter(|f| !should_skip_field(f)).collect();
        if !visible_fields.is_empty() {
            w.line("");
        }
        for f in visible_fields {
            render_field(f, cf, &mut w);
        }
    }

    for m in &cf.methods {
        w.line("");
        render_method(m, cf, &lambda_bootstrap, &mut w);
    }

    w.dedent();
    w.line("}");
    w.finish()
}

#[derive(Clone)]
struct SyntheticLambdaTarget {
    bootstrap_index: u16,
    impl_name: String,
    impl_desc: String,
    sam_param_count: usize,
}

pub(crate) fn build_lambda_bootstrap(cf: &ClassFile) -> HashMap<u16, LambdaBootstrap> {
    // First collect every LambdaMetafactory target without rendering any method
    // body. Rendering is a separate fixed-point pass so nested lambdas can see
    // the complete set of bootstrap targets.
    let mut bootstrap_map: HashMap<u16, LambdaBootstrap> = HashMap::new();
    let mut synthetic_targets = Vec::new();
    let bootstrap_methods: Vec<BootstrapMethod> = cf
        .attributes
        .iter()
        .filter_map(|a| {
            if let Attribute::BootstrapMethods(bms) = a {
                Some(bms)
            } else {
                None
            }
        })
        .flat_map(|bms| bms.iter().cloned())
        .collect();

    for (idx, bm) in bootstrap_methods.iter().enumerate() {
        let is_lambda_factory = match cf.constant_pool.get(bm.bootstrap_method_ref) {
            Ok(CpEntry::MethodHandle { reference, .. }) => match reference.as_ref() {
                CpEntry::Methodref(mr) | CpEntry::InterfaceMethodref(mr) => {
                    mr.class_name.contains("LambdaMetafactory")
                        && (mr.name == "metafactory" || mr.name == "altMetafactory")
                }
                _ => false,
            },
            _ => false,
        };
        if !is_lambda_factory {
            continue;
        }

        // arg[0] = SAM method type  (e.g. "()Ljava/lang/Object;")
        // arg[1] = impl MethodHandle
        // arg[2] = instantiated method type
        let sam_type_idx = match bm.arguments.first() {
            Some(&i) => i,
            None => continue,
        };
        let impl_handle_idx = match bm.arguments.get(1) {
            Some(&i) => i,
            None => continue,
        };

        // Get SAM param count from the MethodType descriptor
        let sam_param_count = match cf.constant_pool.get(sam_type_idx) {
            Ok(CpEntry::MethodType(desc)) => MethodDescriptor::parse(desc)
                .map(|md| md.params.len())
                .unwrap_or(0),
            _ => 0,
        };

        // Keep the MethodHandle kind and owner. They are required to distinguish
        // receiver::method, Owner::method and Owner::new.
        let (reference_kind, impl_owner, impl_name, impl_desc) =
            match cf.constant_pool.get(impl_handle_idx) {
                Ok(CpEntry::MethodHandle {
                    reference_kind,
                    reference,
                }) => match reference.as_ref() {
                    CpEntry::Methodref(mr) | CpEntry::InterfaceMethodref(mr) => (
                        *reference_kind,
                        mr.class_name.clone(),
                        mr.name.clone(),
                        mr.descriptor.clone(),
                    ),
                    _ => continue,
                },
                _ => continue,
            };

        if impl_name.starts_with("lambda$") {
            synthetic_targets.push(SyntheticLambdaTarget {
                bootstrap_index: idx as u16,
                impl_name,
                impl_desc,
                sam_param_count,
            });
        } else {
            bootstrap_map.insert(
                idx as u16,
                LambdaBootstrap::MethodReference {
                    reference_kind,
                    owner: impl_owner,
                    name: impl_name,
                    descriptor: impl_desc,
                    sam_parameter_count: sam_param_count,
                },
            );
        }
    }

    // Each pass can resolve one additional nesting level. Stable output stops
    // early; the bound also prevents malformed cyclic bootstrap data looping.
    for _ in 0..=synthetic_targets.len() {
        let previous = bootstrap_map.clone();
        for target in &synthetic_targets {
            let Some(method) = cf
                .methods
                .iter()
                .find(|m| m.name == target.impl_name && m.descriptor == target.impl_desc)
            else {
                continue;
            };
            let Some(code) = method.code() else {
                continue;
            };

            let raw_body = decompile_lambda_body(method, code, cf, &previous);
            let impl_md =
                MethodDescriptor::parse(&target.impl_desc).unwrap_or_else(|_| MethodDescriptor {
                    params: vec![],
                    return_type: JavaType::VOID,
                });
            let captured_count = impl_md.params.len().saturating_sub(target.sam_param_count);
            let impl_lvt = extract_param_names_from_lvt(method, !method.is_static());
            let lambda_params: Vec<String> = impl_md
                .params
                .iter()
                .enumerate()
                .skip(captured_count)
                .map(|(i, ty)| {
                    let name = impl_lvt
                        .get(i)
                        .and_then(|name| name.clone())
                        .unwrap_or_else(|| format!("p{}", i - captured_count));
                    format!("{} {}", ty, name)
                })
                .collect();
            let params_str = lambda_params.join(", ");
            let body_trimmed = raw_body.trim();
            let lambda_expr = if body_trimmed.contains('\n') {
                format!("({}) -> {{\n{}\n}}", params_str, body_trimmed)
            } else {
                let expr = body_trimmed.trim_end_matches(';');
                let expr = expr.trim_start_matches("return ").trim();
                format!("({}) -> {}", params_str, expr)
            };
            bootstrap_map.insert(target.bootstrap_index, LambdaBootstrap::Lambda(lambda_expr));
        }
        if bootstrap_map == previous {
            break;
        }
    }
    bootstrap_map
}

// ── class declaration line ─────────────────────────────────────────────────

fn build_class_declaration(cf: &ClassFile) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if cf.access_flags & flags::PUBLIC != 0 {
        parts.push("public");
    }
    if cf.access_flags & flags::ABSTRACT != 0 && !cf.is_interface() {
        parts.push("abstract");
    }
    if cf.access_flags & flags::FINAL != 0 && !cf.is_enum() {
        parts.push("final");
    }

    let kind = if cf.is_annotation() {
        "@interface"
    } else if cf.is_interface() {
        "interface"
    } else if cf.is_enum() {
        "enum"
    } else {
        "class"
    };

    let simple = cf.this_class.rsplit('/').next().unwrap_or(&cf.this_class);

    // Generic signature if available — validate against bytecode before using
    let mut extends_str = String::new();
    let mut implements_str = String::new();
    let mut type_params_str = String::new();

    let sig_valid = cf.signature().and_then(|sig_str| {
        parse_class_signature(sig_str)
            .map(|sig| {
                // Validate: each superinterface's erased type must match bytecode interfaces[]
                let ifaces_ok = sig
                    .superinterfaces
                    .iter()
                    .zip(cf.interfaces.iter())
                    .all(|(st, bi)| signature_erased_matches(st, bi));
                // Validate: superclass erased type must match bytecode super_class
                let super_ok = cf
                    .super_class
                    .as_deref()
                    .map(|bs| signature_erased_matches(&sig.superclass, bs))
                    .unwrap_or(true);
                (ifaces_ok && super_ok, sig)
            })
            .ok()
    });

    if let Some((true, sig)) = sig_valid {
        if !sig.type_params.is_empty() {
            type_params_str = format!(
                "<{}>",
                sig.type_params
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let sup = sig.superclass.to_string();
        if sup != "java.lang.Object" && !cf.is_enum() {
            extends_str = format!(" extends {}", sup);
        }
        if !sig.superinterfaces.is_empty() {
            let kw = if cf.is_interface() {
                " extends "
            } else {
                " implements "
            };
            implements_str = format!(
                "{}{}",
                kw,
                sig.superinterfaces
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    } else {
        if let Some(sup) = &cf.super_class {
            if sup != "java/lang/Object" && !cf.is_enum() {
                extends_str = format!(" extends {}", simple_name(sup));
            }
        }
        if !cf.interfaces.is_empty() {
            let kw = if cf.is_interface() {
                " extends "
            } else {
                " implements "
            };
            implements_str = format!(
                "{}{}",
                kw,
                cf.interfaces
                    .iter()
                    .map(|i| simple_name(i))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    format!(
        "{} {} {}{}{}{}",
        parts.join(" "),
        kind,
        simple,
        type_params_str,
        extends_str,
        implements_str
    )
}

// ── signature validation ────────────────────────────────────────────────────

/// Check that the erased form of a generic type matches a bytecode binary name.
/// We erase by taking the raw class name from the signature (ignoring type args).
/// If they match, the signature is trustworthy; if not, fall back to bytecode.
fn signature_erased_matches(sig_ty: &GenericType, bytecode_binary: &str) -> bool {
    let erased = match sig_ty {
        GenericType::Class { class_name, .. } => class_name.replace('.', "/"),
        GenericType::Base(jt) => {
            // For primitives, check the descriptor char
            return jt.to_string() == bytecode_binary.replace('/', ".");
        }
        GenericType::TypeVar(_) => return true, // type var — trust it
        GenericType::Array { element, dims } => {
            // array: prepend [
            let inner = match element.as_ref() {
                GenericType::Class { class_name, .. } => format!(
                    "{}{}",
                    "[".repeat(*dims as usize),
                    class_name.replace('.', "/")
                ),
                _ => return true,
            };
            inner
        }
    };
    erased == bytecode_binary
}

// ── field rendering ────────────────────────────────────────────────────────

/// Returns true if a field should be hidden from the output.
/// We hide compiler-generated synthetic fields that aren't part of the
/// original source (e.g. `this$0`, `$SWITCH_TABLE$…`, `$VALUES`).
fn should_skip_field(f: &Field) -> bool {
    if f.is_synthetic() {
        return true;
    }
    // Inner-class outer-this reference
    if f.name.starts_with("this$") {
        return true;
    }
    // javac enum switch table cache
    if f.name.starts_with("$SWITCH_TABLE$") {
        return true;
    }
    // Enum values array (we render enum constants separately)
    // Only skip if it's a static final field named $VALUES or ENUM$VALUES
    if (f.name == "$VALUES" || f.name == "ENUM$VALUES") && f.is_static() && f.is_final() {
        return true;
    }
    false
}

fn render_field(f: &Field, cf: &ClassFile, w: &mut IndentWriter) {
    write_annotations(&f.attributes, w);
    let mut mods: Vec<&str> = Vec::new();
    if f.access_flags & flags::PUBLIC != 0 {
        mods.push("public");
    }
    if f.access_flags & flags::PROTECTED != 0 {
        mods.push("protected");
    }
    if f.access_flags & flags::PRIVATE != 0 {
        mods.push("private");
    }
    if f.is_static() {
        mods.push("static");
    }
    if f.is_final() {
        mods.push("final");
    }
    if f.is_volatile() {
        mods.push("volatile");
    }
    if f.is_transient() {
        mods.push("transient");
    }

    let type_str = field_type_str(f);
    let mods_str = mods.join(" ");
    let separator = if mods_str.is_empty() { "" } else { " " };

    let const_val = field_constant_value_with_pool(f, &cf.constant_pool);
    if let Some(val) = const_val {
        w.line(&format!(
            "{}{}{} {} = {};",
            mods_str, separator, type_str, f.name, val
        ));
    } else {
        w.line(&format!(
            "{}{}{} {};",
            mods_str, separator, type_str, f.name
        ));
    }
}

fn field_type_str(f: &Field) -> String {
    // Use Signature attribute only if its erased type matches the bytecode descriptor.
    for attr in &f.attributes {
        if let Attribute::Signature(sig) = attr {
            if let Ok(fs) = parse_field_signature(sig) {
                // Erased type from signature must match bytecode descriptor's class name
                let sig_ok = field_sig_matches_descriptor(&fs.ty, &f.descriptor);
                if sig_ok {
                    return fs.ty.to_string();
                }
            }
        }
    }
    parse_field_descriptor(&f.descriptor)
        .map(|(t, _)| t.to_string())
        .unwrap_or_else(|_| f.descriptor.clone())
}

/// Check that the erased class name from a field signature matches the bytecode descriptor.
/// Descriptor looks like `Ljava/util/List;` for an object field.
fn field_sig_matches_descriptor(sig_ty: &GenericType, descriptor: &str) -> bool {
    let desc_class = if descriptor.starts_with('L') && descriptor.ends_with(';') {
        &descriptor[1..descriptor.len() - 1]
    } else {
        return true; // primitive or array — skip check
    };
    signature_erased_matches(sig_ty, desc_class)
}

/// Resolve a field's ConstantValue attribute using the class-level constant pool.
fn field_constant_value_with_pool(f: &Field, pool: &ConstantPool) -> Option<String> {
    for attr in &f.attributes {
        if let Attribute::ConstantValue(cv) = attr {
            match pool.get(cv.constant_value_index) {
                Ok(CpEntry::Integer(v)) => {
                    // Narrow int constants (boolean, byte, char, short) share
                    // this entry.  Use the field descriptor to format correctly.
                    let s = match f.descriptor.as_str() {
                        "Z" => if *v != 0 { "true" } else { "false" }.to_string(),
                        "C" => {
                            let c = char::from_u32(*v as u32).unwrap_or('?');
                            format!("'{}'", c.escape_default())
                        }
                        "B" | "S" => format!("{}", *v as i16),
                        _ => format!("{}", v),
                    };
                    return Some(s);
                }
                Ok(CpEntry::Long(v)) => return Some(format!("{}L", v)),
                Ok(CpEntry::Float(v)) => {
                    if v.is_infinite() {
                        return Some(if *v > 0.0 {
                            "Float.POSITIVE_INFINITY".into()
                        } else {
                            "Float.NEGATIVE_INFINITY".into()
                        });
                    }
                    if v.is_nan() {
                        return Some("Float.NaN".into());
                    }
                    return Some(format!("{}f", v));
                }
                Ok(CpEntry::Double(v)) => {
                    if v.is_infinite() {
                        return Some(if *v > 0.0 {
                            "Double.POSITIVE_INFINITY".into()
                        } else {
                            "Double.NEGATIVE_INFINITY".into()
                        });
                    }
                    if v.is_nan() {
                        return Some("Double.NaN".into());
                    }
                    return Some(format!("{}d", v));
                }
                Ok(CpEntry::String(s)) => {
                    let escaped = s
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t");
                    return Some(format!("\"{}\"", escaped));
                }
                _ => return Some("/* const */".into()),
            }
        }
    }
    None
}

// ── method rendering ───────────────────────────────────────────────────────

fn render_method(
    m: &Method,
    cf: &ClassFile,
    lambda_bootstrap: &HashMap<u16, LambdaBootstrap>,
    w: &mut IndentWriter,
) {
    // Skip bridge/synthetic methods
    if m.is_bridge() || (m.is_synthetic() && !m.name.starts_with('<')) {
        return;
    }

    // Skip trivial default constructor
    if m.is_constructor() && is_default_constructor(m, cf) {
        return;
    }

    // For enums: skip compiler-generated boilerplate
    if cf.is_enum() {
        // Skip values() — always compiler-generated
        if m.name == "values" && m.descriptor.starts_with("()[L") {
            return;
        }
        // Skip valueOf(String) — always compiler-generated
        if m.name == "valueOf" && m.descriptor.starts_with("(Ljava/lang/String;)") {
            return;
        }
        // Skip the private (String, int) constructor that calls super(Enum)
        if m.is_constructor() && m.descriptor == "(Ljava/lang/String;I)V" {
            return;
        }
        // Skip <clinit> — the static initializer that sets up $VALUES is all compiler noise
        if m.is_static_init() {
            return;
        }
    } else {
        // For non-enum classes: skip empty <clinit>
        if m.is_static_init() {
            if let Some(code) = m.code() {
                if code.instructions.is_empty() {
                    return;
                }
            }
        }
    }

    // Skip lambda implementation methods (lambda$name$N)
    if m.name.starts_with("lambda$") {
        return;
    }

    // ── method annotations ─────────────────────────────────────────────
    write_annotations(&m.attributes, w);

    let decl = build_method_declaration(m, cf);

    let code_opt = m.code();
    if m.is_abstract() || m.is_native() || code_opt.is_none() {
        w.line(&format!("{};", decl));
        return;
    }

    w.line(&format!("{} {{", decl));
    w.indent();

    let code = code_opt.unwrap();
    let body = decompile_method_body(m, code, cf, lambda_bootstrap);
    w.push_str(&body);

    w.dedent();
    w.line("}");
}

fn build_method_declaration(m: &Method, cf: &ClassFile) -> String {
    let mut mods: Vec<&str> = Vec::new();
    if m.access_flags & flags::PUBLIC != 0 {
        mods.push("public");
    }
    if m.access_flags & flags::PROTECTED != 0 {
        mods.push("protected");
    }
    if m.access_flags & flags::PRIVATE != 0 {
        mods.push("private");
    }
    if m.is_static() {
        mods.push("static");
    }
    if m.is_final() {
        mods.push("final");
    }
    if m.is_abstract() {
        mods.push("abstract");
    }
    if m.is_native() {
        mods.push("native");
    }
    if m.is_synchronized() {
        mods.push("synchronized");
    }

    let simple_class = cf.this_class.rsplit('/').next().unwrap_or(&cf.this_class);

    // Handle generic signature
    let (type_params, params_str, ret_str, throws_str) = if let Some(sig_str) = method_signature(m)
    {
        match parse_method_signature(&sig_str) {
            Ok(ms) => {
                let tp = if ms.type_params.is_empty() {
                    String::new()
                } else {
                    format!(
                        "<{}> ",
                        ms.type_params
                            .iter()
                            .map(|p| p.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let params = format_generic_params(&ms.params, m);
                let ret = ms.return_type.to_string();
                let thr = if ms.throws.is_empty() {
                    String::new()
                } else {
                    format!(
                        " throws {}",
                        ms.throws
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                (tp, params, ret, thr)
            }
            Err(_) => fallback_method_parts(m),
        }
    } else {
        fallback_method_parts(m)
    };

    let mods_str = mods.join(" ");
    let sep = if mods_str.is_empty() { "" } else { " " };

    if m.is_constructor() {
        format!(
            "{}{}{}{}{}{}",
            mods_str, sep, type_params, simple_class, params_str, throws_str
        )
    } else if m.is_static_init() {
        "static".into()
    } else {
        format!(
            "{}{}{}{} {}{}{}",
            mods_str, sep, type_params, ret_str, m.name, params_str, throws_str
        )
    }
}

fn method_signature(m: &Method) -> Option<String> {
    m.attributes.iter().find_map(|a| {
        if let Attribute::Signature(s) = a {
            Some(s.clone())
        } else {
            None
        }
    })
}

fn format_generic_params(params: &[crate::types::signature::GenericType], m: &Method) -> String {
    // Try to match parameter names from MethodParameters attribute
    let names: Vec<Option<String>> = m
        .attributes
        .iter()
        .find_map(|a| {
            if let Attribute::MethodParameters(ps) = a {
                Some(ps)
            } else {
                None
            }
        })
        .map(|ps| ps.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();

    let parts: Vec<String> = params
        .iter()
        .enumerate()
        .map(|(i, ty)| {
            let name = names
                .get(i)
                .and_then(|n| n.as_deref())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("param{}", i));
            format!("{} {}", ty, name)
        })
        .collect();
    format!("({})", parts.join(", "))
}

fn fallback_method_parts(m: &Method) -> (String, String, String, String) {
    let md = MethodDescriptor::parse(&m.descriptor).unwrap_or_else(|_| MethodDescriptor {
        params: vec![],
        return_type: JavaType::VOID,
    });

    // LVT start_pc=0 entries are parameters — prefer those over "param<i>" placeholders.
    let lvt_param_names = extract_param_names_from_lvt(m, !m.is_static());
    let mp_names: Vec<Option<String>> = m
        .attributes
        .iter()
        .find_map(|a| {
            if let Attribute::MethodParameters(ps) = a {
                Some(ps)
            } else {
                None
            }
        })
        .map(|ps| ps.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();

    let parts: Vec<String> = md
        .params
        .iter()
        .enumerate()
        .map(|(i, ty)| {
            let name = lvt_param_names
                .get(i)
                .and_then(|name| name.clone())
                .or_else(|| {
                    mp_names
                        .get(i)
                        .and_then(|n| n.as_deref())
                        .map(|s| sanitize_java_identifier(s, &format!("param{i}")))
                })
                .unwrap_or_else(|| format!("param{}", i));
            format!("{} {}", ty, name)
        })
        .collect();

    let params_str = format!("({})", parts.join(", "));
    let ret_str = md.return_type.to_string();

    let throws_str = m
        .attributes
        .iter()
        .find_map(|a| {
            if let Attribute::Exceptions(ex) = a {
                if !ex.exception_names.is_empty() {
                    return Some(format!(
                        " throws {}",
                        ex.exception_names
                            .iter()
                            .map(|n| simple_name(n))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            None
        })
        .unwrap_or_default();

    (String::new(), params_str, ret_str, throws_str)
}

/// Extract parameter names from LVT by matching descriptor parameter slots.
/// Missing LVT entries stay as `None` so later names cannot shift left.
fn extract_param_names_from_lvt(m: &Method, has_this: bool) -> Vec<Option<String>> {
    let descriptor = match MethodDescriptor::parse(&m.descriptor) {
        Ok(descriptor) => descriptor,
        Err(_) => return Vec::new(),
    };
    let lvt = m.code().and_then(|code| {
        code.attributes.iter().find_map(|a| {
            if let Attribute::LocalVariableTable(entries) = a {
                Some(entries)
            } else {
                None
            }
        })
    });

    let mut slot = if has_this { 1u16 } else { 0u16 };
    descriptor
        .params
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            let fallback = format!("param{index}");
            let name = lvt.and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.start_pc == 0 && entry.index == slot)
                    .map(|entry| sanitize_java_identifier(&entry.name, &fallback))
            });
            slot += ty.stack_size() as u16;
            name
        })
        .collect()
}

fn sanitize_java_identifier(name: &str, fallback: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "abstract",
        "assert",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "false",
        "final",
        "finally",
        "float",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "native",
        "new",
        "null",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "true",
        "try",
        "void",
        "volatile",
        "while",
        "_",
    ];

    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return fallback.to_string();
    };
    if !(first == '_' || first == '$' || first.is_alphabetic())
        || !chars.all(|ch| ch == '_' || ch == '$' || ch.is_alphanumeric())
    {
        return fallback.to_string();
    }
    if KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The display name for each parameter, in declaration order.
/// Mirrors the resolution order used when rendering the method signature:
/// LocalVariableTable → MethodParameters → `param<i>` placeholder.
fn method_param_display_names(m: &Method) -> Vec<String> {
    let md = MethodDescriptor::parse(&m.descriptor).unwrap_or_else(|_| MethodDescriptor {
        params: vec![],
        return_type: JavaType::VOID,
    });
    let lvt_names = extract_param_names_from_lvt(m, !m.is_static());
    let mp_names: Vec<Option<String>> = m
        .attributes
        .iter()
        .find_map(|a| {
            if let Attribute::MethodParameters(ps) = a {
                Some(ps)
            } else {
                None
            }
        })
        .map(|ps| ps.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();

    (0..md.params.len())
        .map(|i| {
            lvt_names
                .get(i)
                .and_then(|name| name.clone())
                .or_else(|| {
                    mp_names
                        .get(i)
                        .and_then(|n| n.as_deref())
                        .map(|s| sanitize_java_identifier(s, &format!("param{i}")))
                })
                .unwrap_or_else(|| format!("param{}", i))
        })
        .collect()
}

// ── method body decompilation ─────────────────────────────────────────────

/// Returns true if this constructor is the implicit default:
/// - no parameters
/// - body only contains `super()` + `return`  (≤ 5 instructions)
/// - same accessibility as the class
/// - class has exactly one constructor
fn is_default_constructor(m: &Method, cf: &ClassFile) -> bool {
    use crate::classfile::opcodes::opc;

    // Must have no parameters (descriptor == "()V")
    if m.descriptor != "()V" {
        return false;
    }

    // Class must have exactly one constructor
    let ctor_count = cf.methods.iter().filter(|x| x.is_constructor()).count();
    if ctor_count != 1 {
        return false;
    }

    // Must not declare checked exceptions
    if m.attributes
        .iter()
        .any(|a| matches!(a, Attribute::Exceptions(_)))
    {
        return false;
    }

    // Body must be trivial: aload_0, invokespecial Object.<init>, return
    if let Some(code) = m.code() {
        let ops: Vec<u8> = code.instructions.iter().map(|i| i.opcode).collect();
        // Typical pattern: [aload_0, invokespecial, return]
        // or: [aload_0, invokespecial, return] with possible nop
        let meaningful: Vec<u8> = ops.iter().copied().filter(|&o| o != opc::nop).collect();
        if meaningful.len() > 3 {
            return false;
        }
        // If body has any store/field operations, it's not trivial
        let has_field_ops = ops.iter().any(|&o| {
            matches!(
                o,
                opc::putfield
                    | opc::putstatic
                    | opc::istore
                    | opc::istore_0
                    | opc::istore_1
                    | opc::istore_2
                    | opc::istore_3
                    | opc::astore
                    | opc::astore_0
                    | opc::astore_1
                    | opc::astore_2
                    | opc::astore_3
                    | opc::lstore
                    | opc::fstore
                    | opc::dstore
            )
        });
        if has_field_ops {
            return false;
        }
    }

    true
}

fn decompile_method_body(
    m: &Method,
    code: &crate::classfile::attribute::CodeAttribute,
    cf: &ClassFile,
    lambda_bootstrap: &HashMap<u16, LambdaBootstrap>,
) -> String {
    let cfg = cfg_builder::build(code);
    let dom = DomTree::compute(&cfg);
    let (arena, root) = recover(&cfg, &dom, code);
    let is_void_or_ctor = m.is_constructor() || m.descriptor.ends_with(")V");
    // Seed parameter types so boolean/byte/short/char params keep their real
    // type through the simulator (fixes `flag == 0` → `!flag`), and seed the
    // same names used in the signature so the body matches the declaration.
    let param_names = method_param_display_names(m);
    let context = RenderContext::for_method(code, cf, m.is_static(), &m.descriptor, &param_names)
        .with_lambda_bootstrap(lambda_bootstrap);
    render_method_body(&arena, root, &context, 2, is_void_or_ctor)
}

/// Decompile a lambda implementation method body to a raw string (no lambda syntax).
/// The bootstrap map builder will wrap this with `(params) -> `.
fn decompile_lambda_body(
    m: &Method,
    code: &crate::classfile::attribute::CodeAttribute,
    cf: &ClassFile,
    lambda_bootstrap: &HashMap<u16, LambdaBootstrap>,
) -> String {
    let cfg = cfg_builder::build(code);
    let dom = DomTree::compute(&cfg);
    let (arena, root) = recover(&cfg, &dom, code);
    // Always suppress trailing return; lambda bodies are expressions or blocks
    let context = RenderContext::for_method(code, cf, m.is_static(), &m.descriptor, &[])
        .with_lambda_bootstrap(lambda_bootstrap);
    render_method_body(&arena, root, &context, 0, true)
}
