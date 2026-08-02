/// Kotlin method body renderer.
///
/// Reuses the existing IR pipeline (CFG → recovery → StmtArena) but renders
/// Kotlin syntax: val/var, when, is, string templates, ?. and ?: operators.
use crate::cfg::{builder as cfg_builder, DomTree};
use crate::classfile::attribute::CodeAttribute;
use crate::classfile::constant_pool::ConstantPool;
use crate::classfile::constant_pool::CpEntry;
use crate::classfile::instruction::{InsnKind, Instruction};
use crate::classfile::member::Method;
use crate::classfile::opcodes::opc;
use crate::classfile::ClassFile;
use crate::codegen::class_writer::build_lambda_bootstrap;
use crate::codegen::expr_writer::IndentWriter;
use crate::codegen::render_context::RenderContext;
use crate::ir::expr::*;
use crate::ir::recovery::recover_with_branch_convergence;
use crate::ir::stmt::*;
use crate::ir::StmtArena;
use crate::kotlin::types::{kotlin_class_name, kotlin_identifier};
use crate::types::descriptor::MethodDescriptor;

// ── Public entry point ────────────────────────────────────────────────────

/// Decompile a method body to Kotlin source text.
/// Returns the body content (without the surrounding braces).
pub fn decompile_kotlin_body(m: &Method, cf: &ClassFile, indent: usize) -> Option<String> {
    decompile_kotlin_body_with_bindings(m, cf, indent, &[], None)
}

/// Decompile while preserving source parameter/receiver bindings from
/// Kotlin metadata. Empty entries denote synthetic JVM parameters.
pub fn decompile_kotlin_body_with_bindings(
    m: &Method,
    cf: &ClassFile,
    indent: usize,
    parameter_names: &[String],
    dispatch_receiver_label: Option<&str>,
) -> Option<String> {
    let original_code = m.code()?;
    let normalized_code = normalize_coroutine_state_machine(m, original_code, cf);
    let is_coroutine_state_machine = normalized_code.is_some();
    let code = normalized_code.as_ref().unwrap_or(original_code);
    let cfg = cfg_builder::build(code);
    let mut lambda_bootstrap = build_lambda_bootstrap(cf);
    kotlinize_lambda_bootstrap(cf, &mut lambda_bootstrap);
    let mut context =
        RenderContext::for_method(code, cf, m.is_static(), &m.descriptor, parameter_names);
    if let Some(label) = dispatch_receiver_label {
        context = context.with_dispatch_receiver_label(label);
    }
    let context = context
        .with_lambda_bootstrap(&lambda_bootstrap)
        .with_coroutine_state_machine(is_coroutine_state_machine)
        .with_cfg_dataflow(&cfg);

    // Try to handle simple null-check expression methods first (?.  ?:)
    if let Some(result) = try_render_null_check_method(&context, indent) {
        if fast_path_result_is_complete(&result, &context) {
            return Some(result);
        }
    }

    // Try to handle simple if/else expression methods (if (cond) a else b)
    if let Some(result) = try_render_if_expression_method(&context, indent) {
        if fast_path_result_is_complete(&result, &context) {
            return Some(result);
        }
    }

    // Try to handle when-expression methods (switch where each arm returns a value)
    if let Some(result) = try_render_when_expression_method(&context, indent) {
        if fast_path_result_is_complete(&result, &context) {
            return Some(result);
        }
    }

    if let Some(result) = try_render_merged_terminal_return_method(&context, indent) {
        if fast_path_result_is_complete(&result, &context) {
            return Some(result);
        }
    }

    let dom = DomTree::compute(&cfg);
    let (arena, root) = recover_with_branch_convergence(&cfg, &dom, code);

    let is_void_or_ctor = m.is_constructor() || m.descriptor.ends_with(")V");
    Some(render_kotlin_method_body(
        &arena,
        root,
        &context,
        indent,
        is_void_or_ctor,
    ))
}

fn fast_path_result_is_complete(result: &str, context: &RenderContext<'_>) -> bool {
    !result.contains("opaque")
        && !result.contains("/* ? */")
        && !contains_unbound_synthetic_local(result)
        && !context
            .hoisted_locals
            .iter()
            .any(|(_, _, name)| source_contains_identifier(result, &kotlin_identifier(name)))
}

fn source_contains_identifier(source: &str, identifier: &str) -> bool {
    source.match_indices(identifier).any(|(start, _)| {
        let before = source[..start].chars().next_back();
        let end = start + identifier.len();
        let after = source[end..].chars().next();
        before.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_' && ch != '$')
            && after.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_' && ch != '$')
    })
}

fn try_render_merged_terminal_return_method(
    context: &RenderContext<'_>,
    indent: usize,
) -> Option<String> {
    use crate::classfile::opcodes::opc;

    let returns = context
        .block_instructions
        .iter()
        .flat_map(|(block, instructions)| {
            instructions.iter().filter_map(|instruction| {
                matches!(
                    instruction.opcode,
                    opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn | opc::areturn
                )
                .then_some((*block, instruction.offset))
            })
        })
        .collect::<Vec<_>>();
    let [(return_block, _)] = returns.as_slice() else {
        return None;
    };

    for (block, instructions) in &context.block_instructions {
        let entry = context.block_entry(*block);
        let result = context.simulate_state(instructions, &entry);
        if result
            .stmts
            .iter()
            .any(|statement| !matches!(statement, Expr::Return(_) | Expr::Throw(_)))
        {
            return None;
        }
    }

    let instructions = context.block_instructions.get(return_block)?;
    let entry = context.block_entry(*return_block);
    let result = context.simulate_state(instructions, &entry);
    let expression = result
        .stmts
        .into_iter()
        .find_map(|statement| match statement {
            Expr::Return(Some(value)) => Some(*value),
            _ => None,
        })?;
    if !expression_contains_switch(&expression) {
        return None;
    }

    let mut writer = IndentWriter::new(4);
    for _ in 0..indent {
        writer.indent();
    }
    writer.line(&format!("return {}", kt_render_expr(&expression)));
    Some(writer.finish())
}

fn expression_contains_switch(expression: &Expr) -> bool {
    match expression {
        Expr::SwitchExpression { .. } => true,
        Expr::Ternary {
            then_expr,
            else_expr,
            ..
        }
        | Expr::BinOp(_, then_expr, else_expr)
        | Expr::Assign {
            lhs: then_expr,
            rhs: else_expr,
        } => expression_contains_switch(then_expr) || expression_contains_switch(else_expr),
        Expr::UnOp(_, value)
        | Expr::Cast(_, _, value)
        | Expr::InstanceOf(value, _)
        | Expr::ArrayLength(value)
        | Expr::Throw(value) => expression_contains_switch(value),
        _ => false,
    }
}

fn contains_unbound_synthetic_local(result: &str) -> bool {
    let bytes = result.as_bytes();
    let mut index = 0;
    while index + 3 < bytes.len() {
        if bytes[index..].starts_with(b"var")
            && (index == 0 || !is_identifier_byte(bytes[index - 1]))
            && bytes[index + 3].is_ascii_digit()
        {
            let mut end = index + 4;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end == bytes.len() || !is_identifier_byte(bytes[end]) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

pub(super) fn kotlinize_lambda_bootstrap(
    class: &ClassFile,
    bootstraps: &mut std::collections::HashMap<u16, LambdaBootstrap>,
) {
    for bootstrap in bootstraps.values_mut() {
        let LambdaBootstrap::MethodReference {
            name,
            descriptor,
            sam_parameter_count,
            ..
        } = bootstrap
        else {
            continue;
        };
        if !name.contains("$lambda$") {
            continue;
        }
        let Some(method) = class
            .methods
            .iter()
            .find(|method| method.name == *name && method.descriptor == *descriptor)
        else {
            continue;
        };
        let Some(code) = method.code() else {
            continue;
        };
        let Ok(method_descriptor) = MethodDescriptor::parse(descriptor) else {
            continue;
        };
        let captured = method_descriptor
            .params
            .len()
            .saturating_sub(*sam_parameter_count);
        let lvt = crate::codegen::stmt_writer::lvt_entries(code);
        let mut slot = if method.is_static() { 0 } else { 1 };
        let mut parameter_slots = Vec::new();
        for ty in &method_descriptor.params {
            parameter_slots.push(slot);
            slot += if ty.is_wide() { 2 } else { 1 };
        }
        let parameters = method_descriptor
            .params
            .iter()
            .enumerate()
            .skip(captured)
            .map(|(index, ty)| {
                let name = lvt
                    .iter()
                    .find(|entry| entry.slot == parameter_slots[index])
                    .map(|entry| kotlin_identifier(&entry.name))
                    .unwrap_or_else(|| format!("p{}", index - captured));
                format!("{}: {}", name, kt_type_name_from_java(ty))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let parameter_names = (0..method_descriptor.params.len())
            .map(|index| {
                if index < captured {
                    format!("$kotlin$capture${index}")
                } else {
                    lvt.iter()
                        .find(|entry| entry.slot == parameter_slots[index])
                        .map(|entry| entry.name.clone())
                        .unwrap_or_else(|| format!("p{}", index - captured))
                }
            })
            .collect::<Vec<_>>();
        let Some(raw_body) = decompile_kotlin_lambda_method(method, class, &parameter_names) else {
            continue;
        };
        let trimmed = raw_body.trim();
        let expression = trimmed
            .strip_prefix("return ")
            .filter(|_| !trimmed.contains('\n'));
        let lambda = if let Some(expression) = expression {
            if parameters.is_empty() {
                format!("{{ {} }}", expression)
            } else {
                format!("{{ {} -> {} }}", parameters, expression)
            }
        } else {
            let body = relabel_lambda_returns(trimmed, "__abyss_lambda", false);
            if parameters.is_empty() {
                format!("__abyss_lambda@ {{\n{}\n}}", body)
            } else {
                format!("__abyss_lambda@ {{ {} ->\n{}\n}}", parameters, body)
            }
        };
        *bootstrap = LambdaBootstrap::KotlinLambda {
            body: lambda,
            capture_count: captured,
        };
    }
}

fn decompile_kotlin_lambda_method(
    method: &Method,
    class: &ClassFile,
    parameter_names: &[String],
) -> Option<String> {
    let code = method.code()?;
    let cfg = cfg_builder::build(code);
    let context = RenderContext::for_method(
        code,
        class,
        method.is_static(),
        &method.descriptor,
        parameter_names,
    )
    .with_cfg_dataflow(&cfg);
    let dom = DomTree::compute(&cfg);
    let (arena, root) = recover_with_branch_convergence(&cfg, &dom, code);
    Some(render_kotlin_method_body(
        &arena,
        root,
        &context,
        0,
        method.descriptor.ends_with(")V"),
    ))
}

/// Recover the source lambda represented by a Kotlin `SuspendLambda` class.
/// These classes are instantiated directly from `<clinit>` for inline
/// suspend-handler helpers, so they do not have a LambdaMetafactory bootstrap.
pub(super) fn decompile_kotlin_suspend_lambda(
    class: &ClassFile,
    related: &[ClassFile],
) -> Option<(String, usize)> {
    if class.super_class.as_deref() != Some("kotlin/coroutines/jvm/internal/SuspendLambda") {
        return None;
    }
    let method = class.methods.iter().find(|method| {
        method.name == "invokeSuspend"
            && method.descriptor == "(Ljava/lang/Object;)Ljava/lang/Object;"
    })?;
    let mut body =
        decompile_kotlin_body_with_bindings(method, class, 0, &["$result".into()], None)?;
    if has_pathological_body_repetition(&body) {
        return None;
    }
    let capture_fields = class
        .fields
        .iter()
        .filter(|field| !field.is_static() && !is_coroutine_state_field(&field.name))
        .collect::<Vec<_>>();
    for (index, field) in capture_fields.iter().enumerate() {
        body = body.replace(
            &format!("this.{}", kotlin_identifier(&field.name)),
            &format!("__abyss_capture_{index}"),
        );
    }
    let body = replace_kotlin_function_object_constructors(body, related);
    if body.contains("opaque") || body.contains("TODO(") || body.contains("/*") {
        return None;
    }

    let body = relabel_lambda_returns(&body, "__abyss_suspend", true);
    if body.is_empty() {
        Some(("{}".into(), capture_fields.len()))
    } else {
        Some((
            format!("__abyss_suspend@ {{\n{}\n}}", indent_text(&body)),
            capture_fields.len(),
        ))
    }
}

fn is_coroutine_state_field(name: &str) -> bool {
    name == "label"
        || name == "result"
        || ["L$", "I$", "J$", "F$", "D$", "Z$", "B$", "S$", "C$"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn relabel_lambda_returns(body: &str, label: &str, drop_unit_return: bool) -> String {
    body.lines()
        .filter_map(|line| {
            let indent = &line[..line.len() - line.trim_start().len()];
            let trimmed = line.trim();
            if trimmed.is_empty()
                || drop_unit_return && matches!(trimmed, "return Unit" | "return Unit.INSTANCE")
            {
                return None;
            }
            if trimmed == "return" {
                Some(format!("{indent}return@{label}"))
            } else if let Some(value) = trimmed.strip_prefix("return ") {
                Some(format!("{indent}return@{label} {value}"))
            } else {
                Some(line.to_owned())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn indent_text(body: &str) -> String {
    body.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_pathological_body_repetition(body: &str) -> bool {
    if body.len() > 256 * 1024 {
        return true;
    }
    let mut counts = std::collections::HashMap::<&str, usize>::new();
    body.lines().any(|line| {
        let line = line.trim();
        if line.len() < 16 || matches!(line, "else {" | "} else {") {
            return false;
        }
        let count = counts.entry(line).or_default();
        *count += 1;
        *count > 64
    })
}

/// Recover a Kotlin function object that is instantiated directly instead of
/// through LambdaMetafactory. Inline functions commonly leave these classes in
/// bytecode, with constructor parameters stored in synthetic capture fields.
pub(super) fn decompile_kotlin_function_object(class: &ClassFile) -> Option<(String, usize)> {
    if class.super_class.as_deref() == Some("kotlin/coroutines/jvm/internal/SuspendLambda") {
        return None;
    }
    let function_interface = class
        .interfaces
        .iter()
        .find(|interface| interface.starts_with("kotlin/jvm/functions/Function"));
    let sam = class.interfaces.iter().find_map(|interface| {
        let method = match interface.as_str() {
            "java/util/Comparator" => "compare",
            "kotlinx/coroutines/CoroutineExceptionHandler" => "handleException",
            "net/ccbluex/liquidbounce/features/command/AutoCompletionProvider" => "autocomplete",
            "net/ccbluex/liquidbounce/features/command/Parameter$Verificator" => "verifyAndParse",
            _ => return None,
        };
        Some((interface.as_str(), method))
    });
    let method_name = if function_interface.is_some() {
        "invoke"
    } else {
        sam?.1
    };
    let method = class.methods.iter().find(|method| {
        method.name == method_name
            && !method.is_bridge()
            && !method.is_synthetic()
            && method.code().is_some()
    })?;
    let descriptor = MethodDescriptor::parse(&method.descriptor).ok()?;
    let code = method.code()?;
    let lvt = crate::codegen::stmt_writer::lvt_entries(code);
    let mut slot = 1u16;
    let mut parameter_names = Vec::with_capacity(descriptor.params.len());
    for (index, ty) in descriptor.params.iter().enumerate() {
        let name = lvt
            .iter()
            .find(|entry| entry.slot == slot)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| format!("p{index}"));
        parameter_names.push(name);
        slot += if ty.is_wide() { 2 } else { 1 };
    }

    let constructor = class
        .methods
        .iter()
        .find(|method| method.is_constructor())?;
    let capture_fields = class
        .fields
        .iter()
        .filter(|field| !field.is_static())
        .collect::<Vec<_>>();
    let constructor_parameter_count = MethodDescriptor::parse(&constructor.descriptor)
        .ok()?
        .params
        .len();
    let inherited_receiver = class.super_class.as_deref()
        == Some("kotlin/jvm/internal/FunctionReferenceImpl")
        && constructor_parameter_count > capture_fields.len();
    let capture_count =
        (capture_fields.len() + usize::from(inherited_receiver)).min(constructor_parameter_count);

    let mut body = decompile_kotlin_body_with_bindings(method, class, 0, &parameter_names, None)?;
    let capture_offset = usize::from(inherited_receiver);
    if inherited_receiver {
        body = body
            .replace("this.receiver", "__abyss_capture_0")
            .replace("this.`receiver`", "__abyss_capture_0");
    }
    for (index, field) in capture_fields.iter().enumerate() {
        let field = kotlin_identifier(&field.name);
        body = body.replace(
            &format!("this.{field}"),
            &format!("__abyss_capture_{}", index + capture_offset),
        );
    }
    if body.contains("opaque")
        || body.contains("TODO(")
        || body.contains("/*")
        || source_contains_identifier(&body, "this")
    {
        return None;
    }

    let body = relabel_lambda_returns(&body, "__abyss_lambda", false);
    let parameters = parameter_names
        .iter()
        .map(|name| kotlin_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let header = if parameters.is_empty() {
        "__abyss_lambda@ {".to_string()
    } else {
        format!("__abyss_lambda@ {{ {parameters} ->")
    };
    let lambda = if body.is_empty() {
        format!("{header} }}")
    } else {
        format!("{header}\n{}\n}}", indent_text(&body))
    };
    let lambda = if let Some((interface, _)) = sam {
        format!("{}({lambda})", kotlin_class_name(interface))
    } else {
        lambda
    };
    Some((lambda, capture_count))
}

fn replace_kotlin_function_object_constructors(mut body: String, related: &[ClassFile]) -> String {
    // A function object's body may recursively instantiate the same class.
    // Revisiting it would expand recursive source exponentially.
    for class in related {
        let Some((lambda, capture_count)) = decompile_kotlin_function_object(class) else {
            continue;
        };
        let constructor = format!("{}(", kotlin_class_name(&class.this_class));
        let (next, _) =
            replace_balanced_constructor_calls(&body, &constructor, &lambda, capture_count);
        body = next;
    }
    body
}

fn replace_balanced_constructor_calls(
    source: &str,
    constructor: &str,
    lambda: &str,
    capture_count: usize,
) -> (String, bool) {
    let mut output = String::with_capacity(source.len());
    let mut rest = source;
    let mut replaced = false;
    while let Some(start) = rest.find(constructor) {
        output.push_str(&rest[..start]);
        let invocation = &rest[start..];
        let open = constructor.len() - 1;
        let Some(close) = matching_delimiter(invocation, open, b'(', b')') else {
            output.push_str(&rest[start..start + constructor.len()]);
            rest = &rest[start + constructor.len()..];
            continue;
        };
        let args = split_top_level_arguments(&invocation[open + 1..close]);
        if args.len() != capture_count {
            output.push_str(&invocation[..=close]);
            rest = &invocation[close + 1..];
            continue;
        }
        let mut captured = lambda.to_owned();
        for (index, argument) in args.iter().enumerate().rev() {
            captured = captured.replace(&format!("__abyss_capture_{index}"), argument.trim());
        }
        output.push('(');
        output.push_str(&captured);
        output.push(')');
        rest = &invocation[close + 1..];
        replaced = true;
    }
    output.push_str(rest);
    (output, replaced)
}

fn matching_delimiter(source: &str, open: usize, opening: u8, closing: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&opening) {
        return None;
    }
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate().skip(open) {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' && active != b'`' {
                escaped = true;
            } else if byte == active {
                quote = None;
            }
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
        } else if byte == opening {
            depth += 1;
        } else if byte == closing {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

pub(super) fn split_top_level_arguments(arguments: &str) -> Vec<&str> {
    if arguments.trim().is_empty() {
        return Vec::new();
    }
    let bytes = arguments.as_bytes();
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut delimiters = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' && active != b'`' {
                escaped = true;
            } else if byte == active {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => quote = Some(byte),
            b'(' | b'[' | b'{' => delimiters.push(byte),
            b')' | b']' | b'}' => {
                delimiters.pop();
            }
            b',' if delimiters.is_empty() => {
                result.push(&arguments[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    result.push(&arguments[start..]);
    result
}

fn normalize_coroutine_state_machine(
    method: &Method,
    code: &CodeAttribute,
    class: &ClassFile,
) -> Option<CodeAttribute> {
    let descriptor = MethodDescriptor::parse(&method.descriptor).ok()?;
    let is_suspend_lambda = method.name == "invokeSuspend"
        && class.super_class.as_deref() == Some("kotlin/coroutines/jvm/internal/SuspendLambda");
    if !is_suspend_lambda
        && !descriptor
            .params
            .iter()
            .any(|ty| ty.class_name.as_deref() == Some("kotlin/coroutines/Continuation"))
    {
        return None;
    }

    let switch_index = code.instructions.iter().position(|instruction| {
        matches!(instruction.kind, InsnKind::TableSwitch { low, high, .. } if low <= 0 && high >= 0)
    })?;
    let initial_target = match &code.instructions[switch_index].kind {
        InsnKind::TableSwitch { low, offsets, .. } => {
            let case = usize::try_from(-*low).ok()?;
            let relative = *offsets.get(case)?;
            (i64::from(code.instructions[switch_index].offset) + i64::from(relative)) as u32
        }
        _ => return None,
    };
    let sentinel_invoke = code.instructions[..switch_index]
        .iter()
        .position(|instruction| {
            invocation_member(instruction, class).is_some_and(|member| {
                member.class_name == "kotlin/coroutines/intrinsics/IntrinsicsKt"
                    && member.name == "getCOROUTINE_SUSPENDED"
            })
        })?;
    let sentinel_slot = code.instructions[sentinel_invoke + 1..switch_index]
        .iter()
        .find_map(local_store_slot);

    let mut normalized = code.clone();
    normalized.exception_table = merge_coroutine_exception_ranges(&code.exception_table);
    for instruction in &mut normalized.instructions[..switch_index] {
        make_nop(instruction);
    }
    let switch_offset = normalized.instructions[switch_index].offset;
    normalized.instructions[switch_index].opcode = opc::goto_w;
    normalized.instructions[switch_index].wide = false;
    normalized.instructions[switch_index].kind = InsnKind::Branch {
        offset: initial_target as i64 as i32 - switch_offset as i32,
    };

    let original = code.instructions.clone();
    for invoke_index in 0..original.len() {
        let Some(member) = invocation_member(&original[invoke_index], class) else {
            continue;
        };
        let descriptor_has_continuation = MethodDescriptor::parse(&member.descriptor)
            .ok()
            .is_some_and(|descriptor| {
                descriptor
                    .params
                    .iter()
                    .any(|ty| ty.class_name.as_deref() == Some("kotlin/coroutines/Continuation"))
            });
        let Some(branch_index) = (invoke_index + 1..original.len().min(invoke_index + 8))
            .find(|index| original[*index].opcode == opc::if_acmpne)
        else {
            continue;
        };
        let compares_sentinel = sentinel_slot.is_some_and(|slot| {
            original[..branch_index]
                .iter()
                .rev()
                .take(3)
                .any(|instruction| local_load_slot(instruction) == Some(slot))
        });
        if !descriptor_has_continuation && !compares_sentinel {
            continue;
        }
        let Some(target) = original[branch_index]
            .kind
            .branch_targets(original[branch_index].offset, original[branch_index].opcode)
            .first()
            .copied()
        else {
            continue;
        };
        let Some(target_index) = original
            .iter()
            .position(|instruction| instruction.offset == target)
        else {
            continue;
        };
        if target_index <= branch_index
            || !original[branch_index + 1..target_index]
                .iter()
                .any(|instruction| instruction.opcode == opc::areturn)
        {
            continue;
        }
        for instruction in &mut normalized.instructions[invoke_index + 1..target_index] {
            make_nop(instruction);
        }
        let branch = &mut normalized.instructions[branch_index];
        // Preserve the original three-byte instruction footprint.  Using
        // goto_w here changes the computed end offset from +3 to +5 even
        // though the surrounding instruction offsets are unchanged, which
        // prevents the CFG builder from splitting after this jump.
        branch.opcode = opc::goto;
        branch.wide = false;
        branch.kind = InsnKind::Branch {
            offset: target as i64 as i32 - branch.offset as i32,
        };
    }
    Some(normalized)
}

fn merge_coroutine_exception_ranges(
    ranges: &[crate::classfile::attribute::ExceptionHandler],
) -> Vec<crate::classfile::attribute::ExceptionHandler> {
    let mut merged = Vec::<crate::classfile::attribute::ExceptionHandler>::new();
    for range in ranges {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            existing.handler_pc == range.handler_pc && existing.catch_type == range.catch_type
        }) {
            existing.start_pc = existing.start_pc.min(range.start_pc);
            existing.end_pc = existing.end_pc.max(range.end_pc);
        } else {
            merged.push(range.clone());
        }
    }
    merged.sort_by_key(|range| (range.start_pc, range.handler_pc));
    merged
}

fn invocation_member<'a>(
    instruction: &Instruction,
    class: &'a ClassFile,
) -> Option<&'a crate::classfile::constant_pool::MemberRef> {
    let InsnKind::Invoke { index, .. } = instruction.kind else {
        return None;
    };
    match class.constant_pool.get(index).ok()? {
        CpEntry::Methodref(member) | CpEntry::InterfaceMethodref(member) => Some(member),
        _ => None,
    }
}

fn make_nop(instruction: &mut Instruction) {
    instruction.opcode = opc::nop;
    instruction.wide = false;
    instruction.kind = InsnKind::NoOperand;
}

fn local_store_slot(instruction: &Instruction) -> Option<u16> {
    match instruction.kind {
        InsnKind::LocalVar { index }
            if matches!(
                instruction.opcode,
                opc::istore | opc::lstore | opc::fstore | opc::dstore | opc::astore
            ) =>
        {
            Some(index)
        }
        _ => match instruction.opcode {
            opc::istore_0 | opc::lstore_0 | opc::fstore_0 | opc::dstore_0 | opc::astore_0 => {
                Some(0)
            }
            opc::istore_1 | opc::lstore_1 | opc::fstore_1 | opc::dstore_1 | opc::astore_1 => {
                Some(1)
            }
            opc::istore_2 | opc::lstore_2 | opc::fstore_2 | opc::dstore_2 | opc::astore_2 => {
                Some(2)
            }
            opc::istore_3 | opc::lstore_3 | opc::fstore_3 | opc::dstore_3 | opc::astore_3 => {
                Some(3)
            }
            _ => None,
        },
    }
}

fn local_load_slot(instruction: &Instruction) -> Option<u16> {
    match instruction.kind {
        InsnKind::LocalVar { index }
            if matches!(
                instruction.opcode,
                opc::iload | opc::lload | opc::fload | opc::dload | opc::aload
            ) =>
        {
            Some(index)
        }
        _ => match instruction.opcode {
            opc::iload_0 | opc::lload_0 | opc::fload_0 | opc::dload_0 | opc::aload_0 => Some(0),
            opc::iload_1 | opc::lload_1 | opc::fload_1 | opc::dload_1 | opc::aload_1 => Some(1),
            opc::iload_2 | opc::lload_2 | opc::fload_2 | opc::dload_2 | opc::aload_2 => Some(2),
            opc::iload_3 | opc::lload_3 | opc::fload_3 | opc::dload_3 | opc::aload_3 => Some(3),
            _ => None,
        },
    }
}

/// Try to detect and render simple null-check expression methods directly.
/// These are methods whose entire body is `expr?.method() ?: default` or similar.
/// We simulate the whole instruction stream and detect the null-check pattern.
fn try_render_null_check_method(context: &RenderContext<'_>, indent: usize) -> Option<String> {
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;

    let code = context.code;
    let insns = &code.instructions;
    if insns.len() < 4 || insns.len() > 40 {
        return None;
    }

    // Look for the null-check pattern: ..., dup, ifnull/ifnonnull, ..., goto, ..., return
    // Find first ifnull/ifnonnull
    let null_check_idx = insns
        .iter()
        .position(|i| i.opcode == opc::ifnull || i.opcode == opc::ifnonnull)?;

    // Must have a dup before the null check (the value being tested)
    if null_check_idx == 0 {
        return None;
    }
    let prev = &insns[null_check_idx - 1];
    if prev.opcode != opc::dup {
        return None;
    }

    // Get branch target
    let branch_offset = match &insns[null_check_idx].kind {
        InsnKind::Branch { offset } => *offset,
        _ => return None,
    };
    let branch_target = (insns[null_check_idx].offset as i64 + branch_offset as i64) as u32;

    // Find the goto that ends the non-null path
    let goto_idx_opt = insns[null_check_idx + 1..]
        .iter()
        .position(|i| i.opcode == opc::goto)
        .map(|p| null_check_idx + 1 + p);

    // If no goto, try the compound null-check pattern:
    // The non-null path ends with areturn directly, and the null path has a default value
    if goto_idx_opt.is_none() {
        return try_render_compound_null_check(
            insns,
            null_check_idx,
            branch_target,
            context,
            indent,
        );
    }
    let goto_idx = goto_idx_opt.unwrap();

    let goto_target = match &insns[goto_idx].kind {
        InsnKind::Branch { offset } => (insns[goto_idx].offset as i64 + *offset as i64) as u32,
        _ => return None,
    };

    // Find the branch target instruction index
    let null_path_start = insns.iter().position(|i| i.offset == branch_target)?;
    // Find the merge point
    let merge_idx = insns.iter().position(|i| i.offset == goto_target)?;

    // Now simulate:
    // 1. Instructions before dup (to get the subject expression)
    // 2. Non-null path (between ifnull and goto)
    // 3. Null path (between branch target and merge)

    // Simulate everything before the dup to get the subject
    let pre_dup = &insns[..null_check_idx - 1]; // everything before dup
    let pre_result = context.simulate(pre_dup, vec![]);
    let subject = pre_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr))?;

    // Determine which path is null vs non-null based on the opcode
    let (nonnull_range, null_range) = if insns[null_check_idx].opcode == opc::ifnull {
        // ifnull: jump to null path, fall through to non-null path
        let null_end = merge_idx.min(insns.len());
        (null_check_idx + 1..goto_idx, null_path_start..null_end)
    } else {
        // ifnonnull: jump to non-null path, fall through to null path
        let null_end = insns
            .iter()
            .position(|i| i.opcode == opc::goto && i.offset > insns[null_check_idx].offset)
            .unwrap_or(goto_idx);
        (
            null_path_start..merge_idx.min(insns.len()),
            null_check_idx + 1..null_end,
        )
    };
    // This fast path only supports a forward diamond. Loops and crossed
    // branches can place the target after the merge in linear bytecode order;
    // slicing such a range used to panic. Let the CFG-based renderer handle it.
    if nonnull_range.start > nonnull_range.end
        || null_range.start > null_range.end
        || nonnull_range.end > insns.len()
        || null_range.end > insns.len()
    {
        return None;
    }
    let nonnull_insns = &insns[nonnull_range];
    let null_insns = &insns[null_range];

    // Simulate non-null path (it operates on the duped subject, so pre-feed it)
    let nonnull_input = pre_result.stack_out.clone();
    let nonnull_result = context.simulate(nonnull_insns, nonnull_input);
    let nonnull_expr = nonnull_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr));

    // Simulate null path (starts after pop of the null value)
    let null_insns_filtered: Vec<_> = null_insns
        .iter()
        .filter(|i| i.opcode != opc::pop && i.opcode != opc::pop2)
        .cloned()
        .collect();
    let null_result = context.simulate(&null_insns_filtered, vec![]);
    let null_expr = null_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr));

    // Check for a second null-check (for the ?: part after ?.)
    // Look for another ifnull/ifnonnull after the merge point
    let second_check = insns[merge_idx..]
        .iter()
        .position(|i| i.opcode == opc::ifnull || i.opcode == opc::ifnonnull);

    let mut w = IndentWriter::new(4);
    for _ in 0..indent {
        w.indent();
    }

    kt_emit_stmts_scoped(&pre_result.stmts, &context.lvt, &mut w);

    if let (Some(nonnull_str), Some(null_str)) = (nonnull_expr, null_expr) {
        if null_str == "null" {
            // Pattern: subject?.method()
            let safe_call = render_safe_call_branch(&subject, &nonnull_result, indent)?;

            if second_check.is_some() {
                // There's a ?: default after the ?.
                // Simulate the final return to get the default value
                let after_merge = &insns[merge_idx..];
                let return_idx = after_merge.iter().rposition(|i| {
                    matches!(
                        i.opcode,
                        opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn | opc::areturn
                    )
                });
                if return_idx.is_some() {
                    // Find the default value (usually the last const before the second merge)
                    let second_null_insns: Vec<_> = after_merge
                        .iter()
                        .filter(|i| {
                            i.opcode != opc::dup
                                && i.opcode != opc::pop
                                && !matches!(
                                    i.opcode,
                                    opc::ifnull
                                        | opc::ifnonnull
                                        | opc::goto
                                        | opc::ireturn
                                        | opc::lreturn
                                        | opc::freturn
                                        | opc::dreturn
                                        | opc::areturn
                                )
                        })
                        .cloned()
                        .collect();
                    let default_result = context.simulate(&second_null_insns, vec![]);
                    if let Some(default_val) = default_result.stack_out.last() {
                        let default_str = kt_render_expr(&default_val.expr);
                        w.line(&format!("return {} ?: {}", safe_call, default_str));
                        return Some(w.finish());
                    }
                }
                w.line(&format!("return {}", safe_call));
            } else {
                w.line(&format!("return {}", safe_call));
            }
        } else if null_str == "0" || null_str == "false" || null_str.starts_with('"') {
            // Pattern: subject?.method() ?: default
            let safe_call = render_safe_call_branch(&subject, &nonnull_result, indent)?;
            w.line(&format!("return {} ?: {}", safe_call, null_str));
        } else {
            // Generic if (x != null) a else b
            w.line(&format!(
                "return if ({} != null) {} else {}",
                subject, nonnull_str, null_str
            ));
        }
        return Some(w.finish());
    }

    None
}

fn render_safe_call_branch(
    subject: &str,
    result: &crate::ir::stack_sim::SimResult,
    indent: usize,
) -> Option<String> {
    let terminal = result.stack_out.last()?;
    let terminal_text = kt_render_expr(&terminal.expr);
    if result.stmts.is_empty() {
        if let Some(member) = terminal_text.strip_prefix(&format!("{}.", subject)) {
            return Some(format!("{}?.{}", kotlin_safe_receiver(subject), member));
        }
    }

    if terminal_text.contains("opaque") || !result.errors.is_empty() {
        return None;
    }
    let mut body = IndentWriter::new(4);
    for _ in 0..=indent {
        body.indent();
    }
    kt_emit_stmts_scoped(&result.stmts, &[], &mut body);
    body.line(&terminal_text);
    Some(format!(
        "{}?.let {{\n{}{}}}",
        kotlin_safe_receiver(subject),
        body.finish(),
        " ".repeat(indent * 4)
    ))
}

/// Handle compound null-check where non-null path has no goto (ends with areturn directly).
/// Pattern: name?.let { transform } ?: "default"
fn try_render_compound_null_check(
    insns: &[crate::classfile::instruction::Instruction],
    null_check_idx: usize,
    branch_target: u32,
    context: &RenderContext<'_>,
    indent: usize,
) -> Option<String> {
    use crate::classfile::opcodes::opc;

    // Find where the null path starts
    let null_path_start = insns.iter().position(|i| i.offset == branch_target)?;

    // The null path should be short: pop + ldc + areturn
    let null_path = &insns[null_path_start..];
    let return_in_null = null_path.iter().position(|i| {
        matches!(
            i.opcode,
            opc::areturn | opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn
        )
    })?;

    // Simulate null path (skip pop) to get the default value
    let null_insns: Vec<_> = null_path[..return_in_null]
        .iter()
        .filter(|i| i.opcode != opc::pop && i.opcode != opc::pop2)
        .cloned()
        .collect();
    let null_result = context.simulate(&null_insns, vec![]);
    let default_val = null_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr))?;

    // Simulate the non-null path
    let nonnull_path = &insns[null_check_idx + 1..null_path_start];
    let nonnull_end = nonnull_path
        .iter()
        .rposition(|i| {
            matches!(
                i.opcode,
                opc::areturn | opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn
            )
        })
        .unwrap_or(nonnull_path.len());
    let nonnull_insns = &nonnull_path[..nonnull_end];

    // Filter out nops, pops, and secondary null checks
    let filtered: Vec<_> = nonnull_insns
        .iter()
        .filter(|i| {
            i.opcode != 0x00 // nop
            && i.opcode != opc::pop && i.opcode != opc::pop2
            && i.opcode != opc::ifnonnull && i.opcode != opc::ifnull
        })
        .cloned()
        .collect();

    // Get the subject (first instruction(s) before dup)
    let pre_dup = &insns[..null_check_idx - 1];
    let pre_result = context.simulate(pre_dup, vec![]);
    let subject = pre_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr))?;

    // Simulate non-null path with the subject on stack
    let nonnull_result = context.simulate(&filtered, pre_result.stack_out.clone());
    let nonnull_expr = nonnull_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr));

    let mut w = IndentWriter::new(4);
    for _ in 0..indent {
        w.indent();
    }

    if let Some(expr) = nonnull_expr {
        let safe_expr = if expr == subject {
            subject.clone()
        } else if let Some(member) = expr.strip_prefix(&format!("{}.", subject)) {
            format!("{}?.{}", kotlin_safe_receiver(&subject), member)
        } else {
            format!("{}?.let {{ {} }}", kotlin_safe_receiver(&subject), expr)
        };
        w.line(&format!("return {} ?: {}", safe_expr, default_val));
    } else {
        w.line(&format!("return {} ?: {}", subject, default_val));
    }

    Some(w.finish())
}

fn kotlin_safe_receiver(receiver: &str) -> String {
    if receiver.contains(" as ") || receiver.starts_with("if (") || receiver.starts_with("when (") {
        format!("({receiver})")
    } else {
        receiver.to_owned()
    }
}

/// Try to detect and render a simple if/else expression method.
/// Pattern: condition + ifXX branch + then_value + goto + else_value + return
fn try_render_if_expression_method(context: &RenderContext<'_>, indent: usize) -> Option<String> {
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;

    let insns = &context.code.instructions;
    if insns.len() < 5 || insns.len() > 20 {
        return None;
    }

    // Find the conditional branch (not ifnull/ifnonnull — those are handled by null-check)
    let branch_idx = insns.iter().position(|i| {
        matches!(
            i.opcode,
            opc::ifeq
                | opc::ifne
                | opc::iflt
                | opc::ifge
                | opc::ifgt
                | opc::ifle
                | opc::if_icmpeq
                | opc::if_icmpne
                | opc::if_icmplt
                | opc::if_icmpge
                | opc::if_icmpgt
                | opc::if_icmple
        )
    })?;

    // Must have a goto in the then-path
    let goto_idx = insns[branch_idx + 1..]
        .iter()
        .position(|i| i.opcode == opc::goto)?;
    let goto_idx = branch_idx + 1 + goto_idx;

    // Must end with a return
    let last = insns.last()?;
    if !matches!(
        last.opcode,
        opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn | opc::areturn
    ) {
        return None;
    }

    let branch_target = match &insns[branch_idx].kind {
        InsnKind::Branch { offset } => (insns[branch_idx].offset as i64 + *offset as i64) as u32,
        _ => return None,
    };

    let goto_target = match &insns[goto_idx].kind {
        InsnKind::Branch { offset } => (insns[goto_idx].offset as i64 + *offset as i64) as u32,
        _ => return None,
    };

    // Find instruction indices for the branch target and goto target
    let else_start = insns.iter().position(|i| i.offset == branch_target)?;
    let _merge_idx = insns.iter().position(|i| i.offset == goto_target)?;

    // Build the condition string
    let branch_op = insns[branch_idx].opcode;
    let pre_branch = &insns[..branch_idx];
    let cond_result = context.simulate(pre_branch, vec![]);
    let cond_str = build_kotlin_condition(branch_op, &cond_result.stack_out, true); // negated for if-expression

    // Simulate then path (between branch and goto)
    let then_insns = &insns[branch_idx + 1..goto_idx];
    let then_result = context.simulate(then_insns, vec![]);
    let then_expr = then_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr))?;

    // Simulate else path (between branch target and return)
    let return_idx = insns.len() - 1;
    let else_insns = &insns[else_start..return_idx];
    let else_result = context.simulate(else_insns, vec![]);
    let else_expr = else_result
        .stack_out
        .last()
        .map(|s| kt_render_expr(&s.expr))?;

    // Render
    let mut w = IndentWriter::new(4);
    for _ in 0..indent {
        w.indent();
    }
    w.line(&format!(
        "return if ({}) {} else {}",
        cond_str, then_expr, else_expr
    ));
    Some(w.finish())
}

/// Try to detect and render a when-expression method where each case arm returns a value.
/// Pattern: instructions before switch + tableswitch/lookupswitch + each case arm = value + return
fn try_render_when_expression_method(context: &RenderContext<'_>, indent: usize) -> Option<String> {
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;

    let insns = &context.code.instructions;
    if insns.len() < 5 {
        return None;
    }

    // Find the tableswitch or lookupswitch instruction
    let switch_idx = insns
        .iter()
        .position(|i| matches!(i.opcode, opc::tableswitch | opc::lookupswitch))?;

    // This fast path is only valid when the switch is the method's outermost
    // value expression. A switch nested in a preceding if/loop used to be
    // mistaken for the whole method, dropping all control flow before it.
    // Any branch or return before the switch proves that CFG recovery owns it.
    if insns[..switch_idx].iter().any(|instruction| {
        !instruction
            .kind
            .branch_targets(instruction.offset, instruction.opcode)
            .is_empty()
            || matches!(
                instruction.opcode,
                opc::ireturn
                    | opc::lreturn
                    | opc::freturn
                    | opc::dreturn
                    | opc::areturn
                    | opc::r#return
                    | opc::athrow
            )
    }) {
        return None;
    }

    // Simulate instructions before the switch to get the switch expression
    let pre_switch = &insns[..switch_idx];
    let pre_result = context.simulate(pre_switch, vec![]);

    // Get the switch expression from the stack
    let switch_expr = pre_result.stack_out.last()?;

    // Detect if this is a Kotlin enum when-mapping pattern
    let (subject_str, case_map) =
        if let Some((subj, map)) = detect_kotlin_enum_when(&switch_expr.expr, context.class) {
            (subj, Some(map))
        } else {
            (kt_render_expr(&switch_expr.expr), None)
        };

    // Parse switch targets
    let switch_insn = &insns[switch_idx];
    let base_offset = switch_insn.offset as i64;

    let (cases, default_offset): (Vec<(i32, u32)>, u32) = match &switch_insn.kind {
        InsnKind::TableSwitch {
            low,
            offsets,
            default_offset,
            ..
        } => {
            let cases: Vec<(i32, u32)> = offsets
                .iter()
                .enumerate()
                .map(|(i, &off)| (*low + i as i32, (base_offset + off as i64) as u32))
                .collect();
            (cases, (base_offset + *default_offset as i64) as u32)
        }
        InsnKind::LookupSwitch {
            pairs,
            default_offset,
        } => {
            let cases: Vec<(i32, u32)> = pairs
                .iter()
                .map(|&(val, off)| (val, (base_offset + off as i64) as u32))
                .collect();
            (cases, (base_offset + *default_offset as i64) as u32)
        }
        _ => return None,
    };

    // For each case, simulate the instructions at that offset to get the return value
    // Each case arm should be short: 1-3 instructions ending in a return
    let mut arms: Vec<(String, String)> = Vec::new(); // (case_label, value_expr)

    for (case_val, target_offset) in &cases {
        let arm_start = insns.iter().position(|i| i.offset == *target_offset)?;
        // Find the end of this arm (next return or goto to merge point)
        let arm_end = insns[arm_start..].iter().position(|i| {
            matches!(
                i.opcode,
                opc::areturn
                    | opc::ireturn
                    | opc::lreturn
                    | opc::freturn
                    | opc::dreturn
                    | opc::goto
                    | opc::goto_w
            )
        })?;
        let arm_insns = &insns[arm_start..arm_start + arm_end];

        if arm_insns.len() > 5 {
            return None;
        } // Too complex, bail

        let arm_result = context.simulate(arm_insns, vec![]);
        let value_expr = arm_result
            .stack_out
            .last()
            .map(|s| kt_render_expr(&s.expr))
            .unwrap_or_else(|| "/* ? */".into());

        let case_label = if let Some(ref map) = case_map {
            map.get(case_val)
                .cloned()
                .unwrap_or_else(|| case_val.to_string())
        } else {
            case_val.to_string()
        };

        arms.push((case_label, value_expr));
    }

    // Also handle default arm if it exists and is reachable
    let default_arm_start = insns.iter().position(|i| i.offset == default_offset);
    let default_value = if let Some(start) = default_arm_start {
        let arm_end = insns[start..].iter().position(|i| {
            matches!(
                i.opcode,
                opc::areturn
                    | opc::ireturn
                    | opc::lreturn
                    | opc::freturn
                    | opc::dreturn
                    | opc::athrow
                    | opc::goto
                    | opc::goto_w
            )
        });
        if let Some(end) = arm_end {
            let arm_insns = &insns[start..start + end];
            if arm_insns.len() <= 5 {
                let arm_result = context.simulate(arm_insns, vec![]);
                arm_result.stack_out.last().map(|s| kt_render_expr(&s.expr))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Render the when expression
    let mut w = IndentWriter::new(4);
    for _ in 0..indent {
        w.indent();
    }

    // Simplify class-qualified enum references (e.g., Direction.SOUTH → SOUTH within the same class)
    let this_simple = kotlin_class_name(context.this_class);

    w.line(&format!("return when ({}) {{", subject_str));
    w.indent();
    for (label, value) in &arms {
        // Strip enum class prefix from value if it matches the enclosing class
        let simplified_value = value
            .strip_prefix(&format!("{}.", this_simple))
            .unwrap_or(value);
        w.line(&format!("{} -> {}", label, simplified_value));
    }
    if let Some(ref def_val) = default_value {
        // Suppress NoWhenBranchMatchedException (compiler-generated exhaustiveness check)
        if !def_val.contains("NoWhenBranchMatchedException") {
            w.line(&format!("else -> {}", def_val));
        }
    }
    w.dedent();
    w.line("}");

    Some(w.finish())
}

/// Render a Kotlin method body from the structured IR.
fn render_kotlin_method_body(
    arena: &StmtArena,
    root: StmtId,
    context: &RenderContext<'_>,
    indent: usize,
    suppress_trailing_return: bool,
) -> String {
    let mut w = IndentWriter::new(4);
    for _ in 0..indent {
        w.indent();
    }

    for value_loop in &context.value_producing_loops {
        w.line(&format!(
            "var {}: {}? = null",
            value_loop.name,
            kt_type_name_from_java(&value_loop.result_type)
        ));
    }

    for (slot, ty, name) in &context.hoisted_locals {
        let escaped_name = kotlin_identifier(name);
        let declaration = if ty.is_reference() {
            format!(
                "lateinit var {}: {}",
                escaped_name,
                kt_type_name_from_java(ty)
            )
        } else {
            format!(
                "var {}: {} = {}",
                escaped_name,
                kt_type_name_from_java(ty),
                kt_default_value(ty)
            )
        };
        w.line(&declaration);
        context.declared_slots.borrow_mut().insert(*slot);
        context
            .declared_local_names
            .borrow_mut()
            .insert(*slot, escaped_name);
    }

    render_stmt(arena, root, context, &mut w);
    let mut out = w.finish();

    if suppress_trailing_return {
        let marker = "return";
        if let Some(last_pos) = out.rfind(marker) {
            // Only strip bare "return" (no value), not "return expr"
            let after = &out[last_pos + marker.len()..];
            let is_bare = after.starts_with('\n') || after.trim().is_empty();
            if is_bare {
                let line_start = out[..last_pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
                let prefix = &out[line_start..last_pos];
                if prefix.chars().all(|c| c == ' ') {
                    let line_end = out[last_pos..]
                        .find('\n')
                        .map(|p| last_pos + p + 1)
                        .unwrap_or(out.len());
                    out.replace_range(line_start..line_end, "");
                }
            }
        }
    }
    out
}

fn kt_default_value(ty: &crate::types::java_type::JavaType) -> &'static str {
    use crate::types::java_type::JavaType;

    if *ty == JavaType::BOOLEAN {
        "false"
    } else if *ty == JavaType::CHAR {
        "'\\u0000'"
    } else if *ty == JavaType::LONG {
        "0L"
    } else if *ty == JavaType::FLOAT {
        "0f"
    } else if *ty == JavaType::DOUBLE {
        "0.0"
    } else {
        "0"
    }
}

// ── Kotlin expression renderer ────────────────────────────────────────────

/// Render an expression to Kotlin source.
pub(super) fn kt_render_expr(expr: &Expr) -> String {
    kt_render_expr_prec(expr, 15)
}

fn kt_render_expr_prec(expr: &Expr, parent_prec: u8) -> String {
    let s = kt_render_expr_inner(expr);
    let my_prec = expr.precedence();
    if my_prec > parent_prec {
        format!("({})", s)
    } else {
        s
    }
}

fn kt_render_expr_inner(expr: &Expr) -> String {
    match expr {
        Expr::Null => "null".into(),
        Expr::This(_) => "this".into(),

        Expr::Const(c) => kt_render_const(c),

        Expr::LocalVar(lv) => kt_local_var_name(lv),

        Expr::BinOp(op, lhs, rhs) => {
            if let Some(rendered) = render_nested_three_way_comparison(*op, lhs, rhs) {
                return rendered;
            }
            if matches!(
                op,
                BinOp::LCmp | BinOp::FCmpL | BinOp::FCmpG | BinOp::DCmpL | BinOp::DCmpG
            ) {
                return format!(
                    "{}.compareTo({})",
                    kt_render_expr_prec(lhs, 0),
                    kt_render_expr(rhs)
                );
            }
            let prec = op.precedence();
            format!(
                "{} {} {}",
                kt_render_expr_prec(lhs, prec),
                kotlin_binop_symbol(*op),
                kt_render_expr_prec(rhs, prec)
            )
        }

        Expr::UnOp(op, operand) => {
            format!("{}{}", op.symbol(), kt_render_expr_prec(operand, 1))
        }

        Expr::Cast(kind, ty, inner) => {
            match kind {
                CastKind::CheckCast => {
                    // Kotlin uses "as" for casts
                    format!(
                        "{} as {}",
                        kt_render_expr_prec(inner, 1),
                        kt_type_name_from_java(ty)
                    )
                }
                _ => {
                    // Primitive conversions: use .toInt(), .toLong(), etc.
                    let method = kt_cast_method(*kind);
                    format!("{}.{}()", kt_render_expr_prec(inner, 0), method)
                }
            }
        }

        // Kotlin uses "is" instead of "instanceof"
        Expr::InstanceOf(obj, ty) => {
            format!(
                "{} is {}",
                kt_render_expr_prec(obj, 6),
                kt_type_name_from_java(ty)
            )
        }

        Expr::Field {
            dir: FieldDir::Get,
            owner,
            name,
            object,
            ..
        } => match object {
            Some(obj) => format!(
                "{}.{}",
                kt_render_expr_prec(obj, 0),
                kotlin_identifier(name)
            ),
            None => format!("{}.{}", kotlin_class_name(owner), kotlin_identifier(name)),
        },

        Expr::Field {
            dir: FieldDir::Put,
            owner,
            name,
            object,
            value,
            ..
        } => {
            if owner.contains('$')
                && (name == "label"
                    || name.starts_with("L$")
                    || name.starts_with("I$")
                    || name.starts_with("J$")
                    || name.starts_with("F$")
                    || name.starts_with("D$"))
            {
                return String::new();
            }
            let lhs = match object {
                Some(obj) => format!(
                    "{}.{}",
                    kt_render_expr_prec(obj, 0),
                    kotlin_identifier(name)
                ),
                None => format!("{}.{}", kotlin_class_name(owner), kotlin_identifier(name)),
            };
            let rhs = value
                .as_ref()
                .map(|v| kt_render_expr(v))
                .unwrap_or_default();
            format!("{} = {}", lhs, rhs)
        }

        Expr::Invoke {
            kind,
            owner,
            name,
            descriptor,
            object,
            args,
            ..
        } => kt_render_invoke(kind, owner, name, descriptor, object.as_deref(), args),

        Expr::InvokeDynamic {
            name,
            args,
            lambda_body,
            ..
        } => {
            // String concatenation → Kotlin string template
            if (name == "makeConcatWithConstants" || name == "makeConcat") && !args.is_empty() {
                return kt_render_string_template(args);
            }

            // Lambda support
            if let Some(lambda) = lambda_body {
                return match lambda {
                    crate::ir::LambdaBootstrap::Lambda(body) => body.clone(),
                    crate::ir::LambdaBootstrap::KotlinLambda {
                        body,
                        capture_count,
                    } => substitute_kotlin_lambda_captures(body, *capture_count, args),
                    crate::ir::LambdaBootstrap::MethodReference {
                        reference_kind,
                        owner,
                        name,
                        ..
                    } => match *reference_kind {
                        8 => format!("::{}", kotlin_class_name(owner)),
                        6 => format!("{}::{}", kotlin_class_name(owner), kotlin_identifier(name)),
                        5 | 7 | 9 if !args.is_empty() => {
                            format!(
                                "{}::{}",
                                kt_render_expr_prec(&args[0], 0),
                                kotlin_identifier(name)
                            )
                        }
                        5 | 7 | 9 => {
                            format!("{}::{}", kotlin_class_name(owner), kotlin_identifier(name))
                        }
                        _ => format!("{}::{}", kotlin_class_name(owner), kotlin_identifier(name)),
                    },
                };
            }

            let args_str = args
                .iter()
                .map(kt_render_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({})", name, args_str)
        }

        Expr::ArrayLoad { array, index, .. } => {
            format!(
                "{}[{}]",
                kt_render_expr_prec(array, 0),
                kt_render_expr(index)
            )
        }

        Expr::ArrayStore {
            array,
            index,
            value,
        } => {
            format!(
                "{}[{}] = {}",
                kt_render_expr_prec(array, 0),
                kt_render_expr(index),
                kt_render_expr(value)
            )
        }

        Expr::ArrayLength(arr) => {
            format!("{}.size", kt_render_expr_prec(arr, 0))
        }

        Expr::NewArray {
            kind,
            type_,
            dimensions,
            initializer,
        } => {
            if let Some(values) = initializer {
                let values = values
                    .iter()
                    .map(kt_render_expr)
                    .collect::<Vec<_>>()
                    .join(", ");
                return match kind {
                    NewKind::PrimitiveArray { atype } => {
                        let ctor = match atype {
                            4 => "booleanArrayOf",
                            5 => "charArrayOf",
                            6 => "floatArrayOf",
                            7 => "doubleArrayOf",
                            8 => "byteArrayOf",
                            9 => "shortArrayOf",
                            10 => "intArrayOf",
                            11 => "longArrayOf",
                            _ => "arrayOf",
                        };
                        format!("{}({})", ctor, values)
                    }
                    NewKind::RefArray => format!("arrayOf({})", values),
                    _ => format!("arrayOf({})", values),
                };
            }
            let size = dimensions.first().map(kt_render_expr).unwrap_or_default();
            match kind {
                NewKind::PrimitiveArray { atype } => {
                    let ctor = match atype {
                        4 => "BooleanArray",
                        5 => "CharArray",
                        6 => "FloatArray",
                        7 => "DoubleArray",
                        8 => "ByteArray",
                        9 => "ShortArray",
                        10 => "IntArray",
                        11 => "LongArray",
                        _ => "IntArray",
                    };
                    format!("{}({})", ctor, size)
                }
                NewKind::RefArray => {
                    format!("arrayOfNulls<{}>({})", kt_type_name_from_java(type_), size)
                }
                NewKind::MultiArray { .. } => {
                    let dims = dimensions
                        .iter()
                        .map(kt_render_expr)
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("Array({})", dims)
                }
                NewKind::Object => format!("{}()", kt_type_name_from_java(type_)),
            }
        }

        Expr::New {
            class_name,
            args,
            descriptor,
        } => {
            let args_str = kt_render_constructor_args(descriptor, args).join(", ");
            format!("{}({})", kotlin_class_name(class_name), args_str)
        }

        Expr::Assign { lhs, rhs } => {
            format!("{} = {}", kt_render_expr_prec(lhs, 14), kt_render_expr(rhs))
        }

        Expr::IInc { slot, delta, name } => {
            let fallback = format!("var{}", slot);
            let var = kotlin_identifier(name.as_deref().unwrap_or(&fallback));
            if *delta == 1 {
                format!("{}++", var)
            } else if *delta == -1 {
                format!("{}--", var)
            } else if *delta > 0 {
                format!("{} += {}", var, delta)
            } else {
                format!("{} -= {}", var, -delta)
            }
        }

        Expr::Monitor { .. } => String::new(),

        Expr::Throw(exc) => format!("throw {}", kt_render_expr(exc)),

        Expr::Return(Some(val)) => format!("return {}", kt_render_expr(val)),
        Expr::Return(None) => "return".into(),

        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            let cond = match cond {
                TernaryCondition::Rendered(cond) => cond.clone(),
                TernaryCondition::Expression(cond) => kt_render_expr(cond),
            };
            format!(
                "if ({}) {} else {}",
                cond,
                kt_render_expr(then_expr),
                kt_render_expr(else_expr)
            )
        }

        Expr::SwitchExpression { selector, arms } => {
            let mut rendered_arms = arms
                .iter()
                .map(|(value, expr)| match value {
                    Some(value) => format!("{value} -> {}", kt_render_expr(expr)),
                    None => format!("else -> {}", kt_render_expr(expr)),
                })
                .collect::<Vec<_>>();
            if !arms.iter().any(|(value, _)| value.is_none()) {
                rendered_arms.push("else -> throw NoWhenBranchMatchedException()".into());
            }
            format!(
                "when ({}) {{ {} }}",
                kt_render_expr(selector),
                rendered_arms.join("; ")
            )
        }

        Expr::Opaque { opcode, offset } => format!("/* opaque 0x{:02x} @{} */", opcode, offset),
    }
}

fn kotlin_binop_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Xor => "xor",
        BinOp::Shl => "shl",
        BinOp::Shr => "shr",
        BinOp::Ushr => "ushr",
        _ => op.symbol(),
    }
}

fn render_nested_three_way_comparison(op: BinOp, lhs: &Expr, rhs: &Expr) -> Option<String> {
    if !matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    ) || int_constant(rhs) != Some(0)
    {
        return None;
    }
    let Expr::BinOp(
        BinOp::LCmp | BinOp::FCmpL | BinOp::FCmpG | BinOp::DCmpL | BinOp::DCmpG,
        compared_lhs,
        compared_rhs,
    ) = lhs
    else {
        return None;
    };
    Some(format!(
        "{} {} {}",
        kt_render_expr_prec(compared_lhs, 6),
        op.symbol(),
        kt_render_expr_prec(compared_rhs, 6)
    ))
}

fn kt_local_var_name(lv: &crate::ir::LocalVarExpr) -> String {
    let Some(name) = lv.name.as_deref() else {
        return format!("var{}", lv.slot);
    };
    if name == "$kotlin$extension$this" {
        "this".into()
    } else if let Some(label) = name.strip_prefix("this@") {
        format!("this@{}", kotlin_identifier(label))
    } else if let Some(index) = name.strip_prefix("$kotlin$capture$") {
        format!("__abyss_capture_{index}")
    } else if let Some(label) = name.strip_prefix("$kotlin$dispatch$this@") {
        format!("this@{}", kotlin_identifier(label))
    } else if lv.slot == 0 && name == "this" {
        "this".into()
    } else if name.starts_with("$i$f$") || name.starts_with("$i$a$") {
        format!("var{}", lv.slot)
    } else {
        kotlin_identifier(name)
    }
}

fn substitute_kotlin_lambda_captures(body: &str, capture_count: usize, args: &[Expr]) -> String {
    let mut rendered = body.to_owned();
    for index in (0..capture_count.min(args.len())).rev() {
        // Captures are substituted after the lambda body has already been
        // rendered. Group them so a cast, lambda, or compound expression keeps
        // its precedence when the placeholder is used as a callee/receiver.
        let argument = kt_render_expr(&args[index]);
        let argument = if matches!(
            &args[index],
            Expr::BinOp(..)
                | Expr::UnOp(..)
                | Expr::Cast(..)
                | Expr::InstanceOf(..)
                | Expr::Ternary { .. }
                | Expr::SwitchExpression { .. }
                | Expr::InvokeDynamic { .. }
                | Expr::Assign { .. }
        ) {
            format!("({argument})")
        } else {
            argument
        };
        rendered = rendered.replace(&format!("__abyss_capture_{index}"), &argument);
    }
    rendered
}

// ── Helper functions ──────────────────────────────────────────────────────

fn kt_render_const(c: &ConstExpr) -> String {
    use crate::types::java_type::JavaType;
    match &c.value {
        ConstValue::Int(v) => {
            if c.ty == JavaType::BOOLEAN {
                if *v == 0 {
                    "false".into()
                } else {
                    "true".into()
                }
            } else if c.ty == JavaType::CHAR {
                render_kotlin_char_literal(*v)
            } else {
                v.to_string()
            }
        }
        ConstValue::Long(v) => format!("{}L", v),
        ConstValue::Float(v) if v.is_nan() => "Float.NaN".into(),
        ConstValue::Float(v) if *v == f32::INFINITY => "Float.POSITIVE_INFINITY".into(),
        ConstValue::Float(v) if *v == f32::NEG_INFINITY => "Float.NEGATIVE_INFINITY".into(),
        ConstValue::Float(v) if *v == f32::MAX => "Float.MAX_VALUE".into(),
        ConstValue::Float(v) if *v == -f32::MAX => "-Float.MAX_VALUE".into(),
        ConstValue::Float(v) => format!("{}f", v),
        ConstValue::Double(v) if v.is_nan() => "Double.NaN".into(),
        ConstValue::Double(v) if *v == f64::INFINITY => "Double.POSITIVE_INFINITY".into(),
        ConstValue::Double(v) if *v == f64::NEG_INFINITY => "Double.NEGATIVE_INFINITY".into(),
        ConstValue::Double(v) if *v == f64::MAX => "Double.MAX_VALUE".into(),
        ConstValue::Double(v) if *v == -f64::MAX => "-Double.MAX_VALUE".into(),
        ConstValue::Double(v) => v.to_string(),
        ConstValue::StringRef(s) => format!("\"{}\"", s.replace('"', "\\\"").replace('\n', "\\n")),
        ConstValue::ClassRef(s) => format!("{}::class.java", kotlin_class_name(s)),
        ConstValue::Null => "null".into(),
    }
}

fn render_kotlin_char_literal(value: i32) -> String {
    if !(0..=u16::MAX as i32).contains(&value) {
        return format!("({value}).toChar()");
    }
    let value = value as u16;
    let escaped = match value {
        0x08 => "\\b".into(),
        0x09 => "\\t".into(),
        0x0a => "\\n".into(),
        0x0c => "\\u000C".into(),
        0x0d => "\\r".into(),
        0x27 => "\\'".into(),
        0x5c => "\\\\".into(),
        0x20..=0x7e => char::from_u32(u32::from(value)).unwrap().to_string(),
        _ => format!("\\u{value:04X}"),
    };
    format!("'{escaped}'")
}

fn kt_render_constructor_args(descriptor: &str, args: &[Expr]) -> Vec<String> {
    let Ok(method) = MethodDescriptor::parse(descriptor) else {
        return args.iter().map(kt_render_expr).collect();
    };
    let has_marker = method.params.last().and_then(|ty| ty.class_name.as_deref())
        == Some("kotlin/jvm/internal/DefaultConstructorMarker");
    let visible_count = if has_marker {
        default_constructor_original_parameter_count(&method.params)
            .unwrap_or_else(|| method.params.len().saturating_sub(1))
    } else {
        method.params.len()
    };

    args.iter()
        .zip(method.params.iter())
        .take(visible_count)
        .map(|(arg, ty)| {
            if *ty == crate::types::java_type::JavaType::BOOLEAN {
                if let Some(value) = int_constant(arg) {
                    return (value != 0).to_string();
                }
            }
            kt_render_expr(arg)
        })
        .collect()
}

fn default_constructor_original_parameter_count(
    params: &[crate::types::java_type::JavaType],
) -> Option<usize> {
    if params.len() < 3 {
        return None;
    }
    for mask_count in 1..params.len() - 1 {
        let original_count = params.len() - mask_count - 1;
        if original_count.div_ceil(32) != mask_count {
            continue;
        }
        if params[original_count..original_count + mask_count]
            .iter()
            .all(|ty| *ty == crate::types::java_type::JavaType::INT)
        {
            return Some(original_count);
        }
    }
    None
}

/// Map a Java type to Kotlin type name.
fn kt_type_name_from_java(ty: &crate::types::java_type::JavaType) -> String {
    if ty.array_dim > 0 {
        let mut element = ty.clone();
        element.array_dim = 0;
        let primitive_array = match element.kind {
            crate::types::java_type::TypeKind::Boolean => Some("BooleanArray"),
            crate::types::java_type::TypeKind::Byte => Some("ByteArray"),
            crate::types::java_type::TypeKind::Char => Some("CharArray"),
            crate::types::java_type::TypeKind::Short => Some("ShortArray"),
            crate::types::java_type::TypeKind::Int => Some("IntArray"),
            crate::types::java_type::TypeKind::Long => Some("LongArray"),
            crate::types::java_type::TypeKind::Float => Some("FloatArray"),
            crate::types::java_type::TypeKind::Double => Some("DoubleArray"),
            _ => None,
        };
        let mut rendered = primitive_array
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Array<{}>", kt_type_name_from_java(&element)));
        for _ in 1..ty.array_dim {
            rendered = format!("Array<{rendered}>");
        }
        return rendered;
    }
    let s = ty.to_string();
    match s.as_str() {
        "int" => "Int".into(),
        "long" => "Long".into(),
        "float" => "Float".into(),
        "double" => "Double".into(),
        "boolean" => "Boolean".into(),
        "byte" => "Byte".into(),
        "short" => "Short".into(),
        "char" => "Char".into(),
        "void" => "Unit".into(),
        "null" => "Any".into(),
        "java.lang.Object" | "Object" => "Any".into(),
        _ => ty
            .class_name
            .as_deref()
            .map(kotlin_class_name)
            .unwrap_or_else(|| s.rsplit('.').next().unwrap_or(&s).to_string()),
    }
}

/// Primitive cast → Kotlin conversion method.
fn kt_cast_method(kind: CastKind) -> &'static str {
    match kind {
        CastKind::I2L => "toLong",
        CastKind::I2F => "toFloat",
        CastKind::I2D => "toDouble",
        CastKind::L2I => "toInt",
        CastKind::L2F => "toFloat",
        CastKind::L2D => "toDouble",
        CastKind::F2I => "toInt",
        CastKind::F2L => "toLong",
        CastKind::F2D => "toDouble",
        CastKind::D2I => "toInt",
        CastKind::D2L => "toLong",
        CastKind::D2F => "toFloat",
        CastKind::I2B => "toByte",
        CastKind::I2C => "toChar",
        CastKind::I2S => "toShort",
        CastKind::CheckCast => "",
    }
}

/// Render method invocation with Kotlin idioms.
fn kt_render_invoke(
    kind: &InvokeKind,
    owner: &str,
    name: &str,
    descriptor: &str,
    object: Option<&Expr>,
    args: &[Expr],
) -> String {
    // Detect StringBuilder chain: new StringBuilder().append(...).append(...).toString()
    if name == "toString" && args.is_empty() {
        if let Some(obj) = object {
            if let Some(template) = try_render_stringbuilder_chain(obj) {
                return template;
            }
        }
    }

    let descriptor_params = MethodDescriptor::parse(descriptor)
        .map(|method| method.params)
        .unwrap_or_default();
    let visible_args: Vec<&Expr> = args
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            descriptor_params
                .get(*index)
                .and_then(|ty| ty.class_name.as_deref())
                != Some("kotlin/coroutines/Continuation")
        })
        .map(|(_, arg)| arg)
        .collect();
    let args_str = visible_args
        .iter()
        .map(|arg| kt_render_expr(arg))
        .collect::<Vec<_>>()
        .join(", ");

    if name.ends_with("$default") {
        if let Some(rendered) = render_default_invoke(kind, owner, name, &visible_args) {
            return rendered;
        }
    }

    // Map common Java patterns to Kotlin equivalents
    match (owner, name) {
        ("java/io/PrintStream", "println") => format!("println({})", args_str),
        ("java/io/PrintStream", "print") => format!("print({})", args_str),
        // Suppress Kotlin compiler null-check intrinsics
        ("kotlin/jvm/internal/Intrinsics", "checkNotNullParameter") => String::new(),
        ("kotlin/jvm/internal/Intrinsics", "checkNotNullExpressionValue") => {
            // Return just the first argument (the checked expression)
            args.first().map(kt_render_expr).unwrap_or_default()
        }
        ("kotlin/jvm/internal/Intrinsics", "checkNotNull") => args
            .first()
            .map(|arg| format!("{}!!", kt_render_expr(arg)))
            .unwrap_or_default(),
        ("kotlin/ResultKt", "throwOnFailure") => String::new(),
        // Range expressions: rangeTo → ..
        (_, "rangeTo") if visible_args.len() == 1 => {
            if let Some(obj) = object {
                return format!(
                    "{}..{}",
                    kt_render_expr(obj),
                    kt_render_expr(visible_args[0])
                );
            }
            args_str
        }
        (_, "downTo") if visible_args.len() == 1 => {
            if let Some(obj) = object {
                return format!(
                    "{} downTo {}",
                    kt_render_expr(obj),
                    kt_render_expr(visible_args[0])
                );
            }
            args_str
        }
        (_, "step") if visible_args.len() == 1 => {
            if let Some(obj) = object {
                return format!(
                    "{} step {}",
                    kt_render_expr(obj),
                    kt_render_expr(visible_args[0])
                );
            }
            args_str
        }
        (_, "until") if visible_args.len() == 1 => {
            if let Some(obj) = object {
                return format!(
                    "{} until {}",
                    kt_render_expr(obj),
                    kt_render_expr(visible_args[0])
                );
            }
            args_str
        }
        ("java/lang/String", "length") if args.is_empty() => {
            if let Some(obj) = object {
                format!("{}.length", kt_render_expr_prec(obj, 0))
            } else {
                format!("\"\".\"{}\".length", name)
            }
        }
        ("java/lang/String", "equals") => {
            if let Some(obj) = object {
                format!("{} == {}", kt_render_expr_prec(obj, 7), args_str)
            } else {
                format!("{}.equals({})", kotlin_class_name(owner), args_str)
            }
        }
        ("java/lang/Object", "equals") => {
            if let Some(obj) = object {
                format!("{} == {}", kt_render_expr_prec(obj, 7), args_str)
            } else {
                format!("{}.equals({})", kotlin_class_name(owner), args_str)
            }
        }
        ("java/lang/Integer", "valueOf") => args_str,
        ("java/lang/Long", "valueOf") => args_str,
        ("java/lang/Float", "valueOf") => args_str,
        ("java/lang/Double", "valueOf") => args_str,
        ("java/lang/Boolean", "valueOf") => args
            .first()
            .and_then(int_constant)
            .map(|value| (value != 0).to_string())
            .or_else(|| args.first().map(|arg| kt_render_expr_prec(arg, 0)))
            .unwrap_or(args_str),
        ("java/lang/Integer", "intValue")
        | ("java/lang/Long", "longValue")
        | ("java/lang/Float", "floatValue")
        | ("java/lang/Double", "doubleValue")
        | ("java/lang/Boolean", "booleanValue") => {
            if let Some(obj) = object {
                kt_render_expr_prec(obj, 0)
            } else {
                format!("{}.{}()", kotlin_class_name(owner), kotlin_identifier(name))
            }
        }
        _ => {
            // Kotlin property access patterns
            if visible_args.is_empty() {
                match name {
                    "size" | "length" => {
                        if let Some(obj) = object {
                            return format!("{}.size", kt_render_expr_prec(obj, 0));
                        }
                    }
                    "iterator" => {
                        if let Some(obj) = object {
                            return format!("{}.iterator()", kt_render_expr_prec(obj, 0));
                        }
                    }
                    _ => {}
                }
            }
            // Function type invoke: transform.invoke(arg) → transform(arg)
            if name == "invoke" {
                if let Some(obj) = object {
                    let callee = kt_render_expr_prec(obj, 0);
                    let callee = if matches!(obj, Expr::Cast(..)) {
                        format!("({callee})")
                    } else {
                        callee
                    };
                    return format!("{}({})", callee, args_str);
                }
            }
            // .get(index) → [index]
            if name == "get" && visible_args.len() == 1 {
                if let Some(obj) = object {
                    return format!("{}[{}]", kt_render_expr_prec(obj, 0), args_str);
                }
            }
            // .set(index, value) → [index] = value
            if name == "set" && visible_args.len() == 2 {
                if let Some(obj) = object {
                    return format!(
                        "{}[{}] = {}",
                        kt_render_expr_prec(obj, 0),
                        kt_render_expr(visible_args[0]),
                        kt_render_expr(visible_args[1])
                    );
                }
            }
            match (kind, object) {
                (InvokeKind::Static, _) => format!(
                    "{}.{}({})",
                    kotlin_class_name(owner),
                    kotlin_identifier(name),
                    args_str
                ),
                (InvokeKind::Special, Some(obj)) if name == "<init>" => {
                    format!("{}.{}({})", kt_render_expr_prec(obj, 0), name, args_str)
                }
                (_, Some(obj)) => format!(
                    "{}.{}({})",
                    kt_render_expr_prec(obj, 0),
                    kotlin_identifier(name),
                    args_str
                ),
                (_, None) => format!(
                    "{}.{}({})",
                    kotlin_class_name(owner),
                    kotlin_identifier(name),
                    args_str
                ),
            }
        }
    }
}

fn render_default_invoke(
    kind: &InvokeKind,
    owner: &str,
    name: &str,
    args: &[&Expr],
) -> Option<String> {
    let marker_index = args.len().checked_sub(1)?;
    if !matches!(args[marker_index], Expr::Null) {
        return None;
    }
    let mut mask_start = marker_index;
    while mask_start > 0 && int_constant(args[mask_start - 1]).is_some() {
        mask_start -= 1;
    }
    if mask_start == marker_index {
        return None;
    }
    let masks: Vec<i32> = args[mask_start..marker_index]
        .iter()
        .filter_map(|arg| int_constant(arg))
        .collect();
    let mut original = args[..mask_start].to_vec();
    let receiver = if matches!(kind, InvokeKind::Static) && !original.is_empty() {
        Some(original.remove(0))
    } else {
        None
    };
    let kept = original
        .into_iter()
        .enumerate()
        .filter(|(index, _)| {
            let mask = masks.get(index / 32).copied().unwrap_or(0);
            mask & (1_i32 << (index % 32)) == 0
        })
        .map(|(_, arg)| kt_render_expr(arg))
        .collect::<Vec<_>>()
        .join(", ");
    let source_name = name.trim_end_matches("$default");
    let target = receiver
        .map(|receiver| kt_render_expr_prec(receiver, 0))
        .unwrap_or_else(|| kotlin_class_name(owner));
    Some(format!(
        "{}.{}({})",
        target,
        kotlin_identifier(source_name),
        kept
    ))
}

fn int_constant(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Const(ConstExpr {
            value: ConstValue::Int(value),
            ..
        }) => Some(*value),
        _ => None,
    }
}

/// Render string concatenation as Kotlin string template.
fn kt_render_string_template(args: &[Expr]) -> String {
    if args.len() == 1 {
        // Single arg — just convert toString
        return format!("{}.toString()", kt_render_expr(&args[0]));
    }

    let mut template = String::from("\"");
    for arg in args {
        match arg {
            Expr::Const(c) => match &c.value {
                ConstValue::StringRef(s) => template.push_str(s),
                _ => {
                    template.push_str("${");
                    template.push_str(&kt_render_expr(arg));
                    template.push('}');
                }
            },
            Expr::Invoke {
                kind: InvokeKind::Static,
                owner,
                name,
                args: vargs,
                ..
            } if name == "valueOf" && owner == "java/lang/String" && vargs.len() == 1 => {
                template.push_str("${");
                template.push_str(&kt_render_expr(&vargs[0]));
                template.push('}');
            }
            Expr::LocalVar(lv) => {
                let var_name = kt_local_var_name(lv);
                // Simple variable names can use $name, complex need ${expr}
                if var_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    template.push('$');
                    template.push_str(&var_name);
                } else {
                    template.push_str("${");
                    template.push_str(&var_name);
                    template.push('}');
                }
            }
            _ => {
                template.push_str("${");
                template.push_str(&kt_render_expr(arg));
                template.push('}');
            }
        }
    }
    template.push('"');
    template
}

/// Try to detect a StringBuilder chain and render it as a Kotlin string template.
/// Pattern: new StringBuilder().append(a).append(b)...
fn try_render_stringbuilder_chain(expr: &Expr) -> Option<String> {
    let mut parts: Vec<&Expr> = Vec::new();
    collect_append_args(expr, &mut parts)?;

    if parts.is_empty() {
        return None;
    }

    // Check if all parts are string literals — then just concatenate
    let all_strings = parts
        .iter()
        .all(|p| matches!(p, Expr::Const(c) if matches!(&c.value, ConstValue::StringRef(_))));
    if all_strings && parts.len() == 1 {
        if let Expr::Const(c) = parts[0] {
            if let ConstValue::StringRef(s) = &c.value {
                return Some(format!(
                    "\"{}\"",
                    s.replace('"', "\\\"").replace('\n', "\\n")
                ));
            }
        }
    }

    // Build a string template
    let mut template = String::from("\"");
    for part in &parts {
        match part {
            Expr::Const(c) => {
                match &c.value {
                    ConstValue::StringRef(s) => {
                        template.push_str(&s.replace('"', "\\\"").replace('\n', "\\n"));
                    }
                    ConstValue::Int(v) => {
                        // Check if it could be a char
                        if *v >= 32 && *v < 127 {
                            template.push(*v as u8 as char);
                        } else {
                            template.push_str("${");
                            template.push_str(&v.to_string());
                            template.push('}');
                        }
                    }
                    _ => {
                        template.push_str("${");
                        template.push_str(&kt_render_expr(part));
                        template.push('}');
                    }
                }
            }
            Expr::LocalVar(lv) => {
                let var_name = kt_local_var_name(lv);
                if var_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    template.push('$');
                    template.push_str(&var_name);
                } else {
                    template.push_str("${");
                    template.push_str(&var_name);
                    template.push('}');
                }
            }
            Expr::Field {
                name,
                object: Some(obj),
                ..
            } => {
                // this.name → ${this.name} or $name
                let obj_str = kt_render_expr(obj);
                if obj_str == "this" {
                    template.push_str("${this.");
                    template.push_str(&kotlin_identifier(name));
                    template.push('}');
                } else {
                    template.push_str("${");
                    template.push_str(&obj_str);
                    template.push('.');
                    template.push_str(&kotlin_identifier(name));
                    template.push('}');
                }
            }
            _ => {
                template.push_str("${");
                template.push_str(&kt_render_expr(part));
                template.push('}');
            }
        }
    }
    template.push('"');
    Some(template)
}

/// Recursively collect arguments from a StringBuilder.append() chain.
/// Returns None if the expression is not a StringBuilder chain.
fn collect_append_args<'a>(expr: &'a Expr, parts: &mut Vec<&'a Expr>) -> Option<()> {
    match expr {
        Expr::Invoke {
            name,
            object: Some(obj),
            args,
            owner,
            ..
        } if name == "append"
            && args.len() == 1
            && (owner == "java/lang/StringBuilder" || owner == "java/lang/StringBuffer") =>
        {
            collect_append_args(obj, parts)?;
            parts.push(&args[0]);
            Some(())
        }
        // Base case: new StringBuilder() or new StringBuilder("")
        Expr::New {
            class_name, args, ..
        } if class_name == "java/lang/StringBuilder" || class_name == "java/lang/StringBuffer" => {
            if args.len() == 1 {
                // new StringBuilder("initial")
                parts.push(&args[0]);
            }
            Some(())
        }
        // Invoke on <init> pattern from the IR
        Expr::Invoke {
            name,
            owner,
            args,
            kind: InvokeKind::Special,
            ..
        } if name == "<init>"
            && (owner == "java/lang/StringBuilder" || owner == "java/lang/StringBuffer") =>
        {
            if args.len() == 1 {
                parts.push(&args[0]);
            }
            Some(())
        }
        _ => None,
    }
}

// ── Statement rendering ───────────────────────────────────────────────────

use crate::codegen::stmt_writer::LvtEntry;

fn render_stmt(arena: &StmtArena, id: StmtId, context: &RenderContext<'_>, w: &mut IndentWriter) {
    match arena.get(id) {
        Stmt::Exit => {}

        Stmt::Block(b) => {
            let entry = context.block_entry(b.block_id);
            let result = context.simulate_state(&b.instructions, &entry);
            kt_emit_stmts(&result.stmts, context, w);
        }

        Stmt::Seq(s) => {
            let children = s.children.clone();
            for child in children {
                render_stmt(arena, child, context, w);
            }
        }

        Stmt::If(s) => {
            render_if(arena, s.clone(), context, w);
        }

        Stmt::Loop(s) => {
            render_loop(arena, s.clone(), context, w);
        }

        Stmt::BreakIf(s) => {
            emit_branch_prefix(s.cond_block, &s.cond_insns, context, w);
            let condition =
                extract_branch_condition(s.cond_block, &s.cond_insns, context, s.negated);
            w.line(&format!("if ({condition}) {{"));
            w.indent();
            w.line("break");
            w.dedent();
            w.line("}");
        }

        Stmt::Switch(s) => {
            render_when(arena, s.clone(), context, w);
        }

        Stmt::TryCatch(s) => {
            render_try_catch(arena, s.clone(), context, w);
        }

        Stmt::Synchronized(s) => {
            w.line("synchronized(/* monitor */) {");
            w.indent();
            let body = s.body;
            render_stmt(arena, body, context, w);
            w.dedent();
            w.line("}");
        }
    }
}

/// Emit statements with Kotlin val/var declarations and destructuring.
fn kt_emit_stmts(stmts: &[Expr], context: &RenderContext<'_>, w: &mut IndentWriter) {
    let mut declared = context.declared_slots.borrow_mut();
    let mut declared_names = context.declared_local_names.borrow_mut();
    kt_emit_stmts_with_state(
        stmts,
        &context.lvt,
        &mut declared,
        &mut declared_names,
        &context.local_assignment_counts,
        &context.completed_value_producing_loops.borrow(),
        w,
    );
}

fn kt_emit_stmts_scoped(stmts: &[Expr], lvt: &[LvtEntry], w: &mut IndentWriter) {
    let mut declared = std::collections::HashSet::<u16>::new();
    let mut declared_names = std::collections::HashMap::<u16, String>::new();
    let mut assign_count = std::collections::HashMap::<u16, usize>::new();
    for expr in stmts {
        if let Expr::Assign { lhs, .. } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                *assign_count.entry(lv.slot).or_insert(0) += 1;
            }
        }
    }
    kt_emit_stmts_with_state(
        stmts,
        lvt,
        &mut declared,
        &mut declared_names,
        &assign_count,
        &[],
        w,
    );
}

fn kt_emit_stmts_with_state(
    stmts: &[Expr],
    lvt: &[LvtEntry],
    declared: &mut std::collections::HashSet<u16>,
    declared_names: &mut std::collections::HashMap<u16, String>,
    assign_count: &std::collections::HashMap<u16, usize>,
    completed_value_loops: &[crate::codegen::render_context::ValueProducingLoop],
    w: &mut IndentWriter,
) {
    let mut suppressed_inline_markers = std::collections::HashSet::<u16>::new();

    let mut i = 0;
    while i < stmts.len() {
        // Try to detect destructuring: consecutive componentN() calls on same source
        if let Some((destructuring_line, consumed)) =
            try_detect_destructuring(&stmts[i..], lvt, declared)
        {
            w.line(&destructuring_line);
            i += consumed;
            continue;
        }

        let expr = &stmts[i];
        let line = if let Expr::Assign { lhs, rhs } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                let slot = lv.slot;
                let local_identity = kt_local_var_name(lv);
                let already_declared = declared.contains(&slot)
                    && declared_names
                        .get(&slot)
                        .is_none_or(|name| name == &local_identity);
                if !already_declared {
                    if let Some(name) = lv.name.as_deref() {
                        // Suppress compiler-internal variables
                        if (name.starts_with("$i$f$") || name.starts_with("$i$a$"))
                            && matches!(rhs.as_ref(), Expr::Const(c) if matches!(c.value, ConstValue::Int(0)))
                            && suppressed_inline_markers.insert(slot)
                        {
                            i += 1;
                            continue;
                        }
                        declared.insert(slot);
                        declared_names.insert(slot, local_identity);
                        let rhs_str = kt_render_expr_concat_with_loops(rhs, completed_value_loops);
                        let keyword = if assign_count.get(&slot).copied().unwrap_or(0) > 1 {
                            "var"
                        } else {
                            "val"
                        };
                        format!("{} {} = {}", keyword, kt_local_var_name(lv), rhs_str)
                    } else {
                        let rhs_str = kt_render_expr_concat_with_loops(rhs, completed_value_loops);
                        declared.insert(slot);
                        declared_names.insert(slot, local_identity);
                        let keyword = if assign_count.get(&slot).copied().unwrap_or(0) > 1 {
                            "var"
                        } else {
                            "val"
                        };
                        format!("{} var{} = {}", keyword, slot, rhs_str)
                    }
                } else {
                    let rhs_str = kt_render_expr_concat_with_loops(rhs, completed_value_loops);
                    format!("{} = {}", kt_render_expr(lhs), rhs_str)
                }
            } else {
                kt_render_expr_concat_with_loops(expr, completed_value_loops)
            }
        } else {
            kt_render_expr_concat_with_loops(expr, completed_value_loops)
        };
        if !line.is_empty() {
            w.line(&line);
        }
        i += 1;
    }
}

/// Try to detect destructuring declaration pattern:
/// val a = source.component1()
/// val b = source.component2()
/// → val (a, b) = source
fn try_detect_destructuring(
    stmts: &[Expr],
    lvt: &[LvtEntry],
    declared: &mut std::collections::HashSet<u16>,
) -> Option<(String, usize)> {
    if stmts.len() < 2 {
        return None;
    }

    // Check if first stmt is var = source.component1()
    let source_expr = match &stmts[0] {
        Expr::Assign { rhs, .. } => extract_component_source(rhs, 1)?,
        _ => return None,
    };

    // Count consecutive componentN() calls on the same source
    let mut names = Vec::new();
    let mut count = 0;
    for (idx, stmt) in stmts.iter().enumerate() {
        let expected_n = idx + 1;
        if let Expr::Assign { lhs, rhs } = stmt {
            if let Some(src) = extract_component_source(rhs, expected_n as u32) {
                if src == source_expr {
                    // Get the variable name
                    if let Expr::LocalVar(lv) = lhs.as_ref() {
                        let name = if let Some(entry) = lvt.iter().find(|e| e.slot == lv.slot) {
                            declared.insert(lv.slot);
                            kotlin_identifier(&entry.name)
                        } else {
                            format!("var{}", lv.slot)
                        };
                        names.push(name);
                        count += 1;
                        continue;
                    }
                }
            }
        }
        break;
    }

    if count >= 2 {
        let destructured = format!("val ({}) = {}", names.join(", "), source_expr);
        Some((destructured, count))
    } else {
        None
    }
}

/// Extract the source expression from a componentN() call.
/// Returns the rendered source expression if this is a componentN call with the expected index.
fn extract_component_source(expr: &Expr, expected_n: u32) -> Option<String> {
    // Handle (expr as Type).componentN() which comes from box/unbox
    // or direct expr.componentN()
    match expr {
        Expr::Invoke {
            name,
            object: Some(obj),
            args,
            ..
        } if name.starts_with("component") && args.is_empty() => {
            let n: u32 = name.strip_prefix("component")?.parse().ok()?;
            if n == expected_n {
                // Strip boxing casts for readability
                let source = strip_boxing_cast(obj);
                Some(kt_render_expr(source))
            } else {
                None
            }
        }
        // Handle (source.componentN() as Number).intValue() pattern
        Expr::Invoke {
            name: unbox_name,
            object: Some(inner),
            args,
            ..
        } if is_unbox_method(unbox_name) && args.is_empty() => {
            if let Expr::Cast(CastKind::CheckCast, _, inner2) = inner.as_ref() {
                return extract_component_source(inner2, expected_n);
            }
            None
        }
        _ => None,
    }
}

fn strip_boxing_cast(expr: &Expr) -> &Expr {
    if let Expr::Cast(CastKind::CheckCast, _, inner) = expr {
        return strip_boxing_cast(inner);
    }
    expr
}

fn is_unbox_method(name: &str) -> bool {
    matches!(
        name,
        "intValue"
            | "longValue"
            | "floatValue"
            | "doubleValue"
            | "booleanValue"
            | "byteValue"
            | "shortValue"
            | "charValue"
    )
}

/// Render expression, converting string concat to Kotlin string templates.
fn kt_render_expr_concat(expr: &Expr) -> String {
    if let Expr::InvokeDynamic { name, args, .. } = expr {
        if (name == "makeConcatWithConstants" || name == "makeConcat") && !args.is_empty() {
            return kt_render_string_template(args);
        }
    }
    kt_render_expr(expr)
}

fn kt_render_expr_concat_with_loops(
    expr: &Expr,
    completed: &[crate::codegen::render_context::ValueProducingLoop],
) -> String {
    let rewritten = rewrite_value_producing_loop_expr(expr, completed);
    kt_render_expr_concat(&rewritten)
}

fn rewrite_value_producing_loop_expr(
    expr: &Expr,
    completed: &[crate::codegen::render_context::ValueProducingLoop],
) -> Expr {
    if let Some(value_loop) = completed
        .iter()
        .rev()
        .find(|value_loop| ternary_is_loop_merge(expr, value_loop))
    {
        return Expr::LocalVar(crate::ir::LocalVarExpr {
            slot: u16::MAX,
            ty: value_loop.result_type.clone(),
            name: Some(value_loop.name.clone()),
        });
    }

    match expr {
        Expr::BinOp(op, left, right) => Expr::BinOp(
            *op,
            Box::new(rewrite_value_producing_loop_expr(left, completed)),
            Box::new(rewrite_value_producing_loop_expr(right, completed)),
        ),
        Expr::UnOp(op, value) => Expr::UnOp(
            *op,
            Box::new(rewrite_value_producing_loop_expr(value, completed)),
        ),
        Expr::Cast(kind, ty, value) => {
            let rewritten = rewrite_value_producing_loop_expr(value, completed);
            if *kind == CastKind::CheckCast
                && completed.iter().any(|value_loop| {
                    value_loop.result_type == *ty
                        && matches!(&rewritten, Expr::LocalVar(local)
                            if local.name.as_deref() == Some(value_loop.name.as_str()))
                })
            {
                rewritten
            } else {
                Expr::Cast(*kind, ty.clone(), Box::new(rewritten))
            }
        }
        Expr::InstanceOf(value, ty) => Expr::InstanceOf(
            Box::new(rewrite_value_producing_loop_expr(value, completed)),
            ty.clone(),
        ),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => Expr::Ternary {
            cond: cond.clone(),
            then_expr: Box::new(rewrite_value_producing_loop_expr(then_expr, completed)),
            else_expr: Box::new(rewrite_value_producing_loop_expr(else_expr, completed)),
        },
        Expr::Assign { lhs, rhs } => Expr::Assign {
            lhs: Box::new(rewrite_value_producing_loop_expr(lhs, completed)),
            rhs: Box::new(rewrite_value_producing_loop_expr(rhs, completed)),
        },
        Expr::Return(value) => Expr::Return(
            value
                .as_ref()
                .map(|value| Box::new(rewrite_value_producing_loop_expr(value, completed))),
        ),
        Expr::Invoke {
            kind,
            owner,
            name,
            descriptor,
            object,
            args,
        } => Expr::Invoke {
            kind: *kind,
            owner: owner.clone(),
            name: name.clone(),
            descriptor: descriptor.clone(),
            object: object
                .as_ref()
                .map(|value| Box::new(rewrite_value_producing_loop_expr(value, completed))),
            args: args
                .iter()
                .map(|value| rewrite_value_producing_loop_expr(value, completed))
                .collect(),
        },
        _ => expr.clone(),
    }
}

fn ternary_is_loop_merge(
    expr: &Expr,
    value_loop: &crate::codegen::render_context::ValueProducingLoop,
) -> bool {
    let Expr::Ternary {
        cond,
        then_expr,
        else_expr,
    } = expr
    else {
        return false;
    };
    let _ = cond;
    (matches!(then_expr.as_ref(), Expr::Null)
        && expr_is_local_slot(else_expr, value_loop.element_slot))
        || (matches!(else_expr.as_ref(), Expr::Null)
            && expr_is_local_slot(then_expr, value_loop.element_slot))
}

fn expr_is_local_slot(expr: &Expr, slot: u16) -> bool {
    match expr {
        Expr::LocalVar(local) => local.slot == slot,
        Expr::Cast(_, _, inner) => expr_is_local_slot(inner, slot),
        _ => false,
    }
}

mod control_flow;
use control_flow::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::render_context::ValueProducingLoop;
    use crate::types::JavaType;

    fn value_loop(name: &str, element_slot: u16) -> ValueProducingLoop {
        ValueProducingLoop {
            header_block: 1,
            body_blocks: vec![1, 2],
            predicate_block: 2,
            predicate_negated: false,
            iterator_slot: 5,
            element_slot,
            result_type: JavaType::object("pkg/Item"),
            success_value: Expr::Null,
            name: name.into(),
        }
    }

    fn loop_merge(element_slot: u16) -> Expr {
        Expr::Ternary {
            cond: TernaryCondition::Rendered("iterator.hasNext()".into()),
            then_expr: Box::new(Expr::Null),
            else_expr: Box::new(Expr::LocalVar(crate::ir::LocalVarExpr {
                slot: element_slot,
                ty: JavaType::object("java/lang/Object"),
                name: None,
            })),
        }
    }

    #[test]
    fn value_loop_merge_prefers_most_recent_reused_slot() {
        let first = value_loop("__abyss_loop_result_1", 6);
        let second = value_loop("__abyss_loop_result_2", 6);

        let rewritten = rewrite_value_producing_loop_expr(&loop_merge(6), &[first, second.clone()]);

        assert!(matches!(
            rewritten,
            Expr::LocalVar(local)
                if local.name.as_deref() == Some(second.name.as_str())
                    && local.ty == second.result_type
        ));
    }

    #[test]
    fn value_loop_merge_removes_same_type_non_null_cast() {
        let value_loop = value_loop("__abyss_loop_result_1", 6);
        let expression = Expr::Cast(
            CastKind::CheckCast,
            value_loop.result_type.clone(),
            Box::new(loop_merge(6)),
        );

        let rewritten =
            rewrite_value_producing_loop_expr(&expression, std::slice::from_ref(&value_loop));

        assert!(matches!(
            rewritten,
            Expr::LocalVar(ref local)
                if local.name.as_deref() == Some(value_loop.name.as_str())
        ));
        assert_eq!(kt_render_expr(&rewritten), value_loop.name);
    }

    #[test]
    fn value_loop_merge_preserves_different_type_cast() {
        let value_loop = value_loop("__abyss_loop_result_1", 6);
        let expression = Expr::Cast(
            CastKind::CheckCast,
            JavaType::object("pkg/Other"),
            Box::new(loop_merge(6)),
        );

        let rewritten = rewrite_value_producing_loop_expr(&expression, &[value_loop]);

        assert!(matches!(rewritten, Expr::Cast(CastKind::CheckCast, _, _)));
    }

    #[test]
    fn kotlin_type_renderer_preserves_array_shape() {
        assert_eq!(
            kt_type_name_from_java(&JavaType::object("pkg/Mode").array_of()),
            "Array<Mode>"
        );
        assert_eq!(
            kt_type_name_from_java(&JavaType::INT.with_dims(2)),
            "Array<IntArray>"
        );
    }

    #[test]
    fn kotlin_char_literals_escape_non_printable_and_delimiters() {
        assert_eq!(render_kotlin_char_literal(0), "'\\u0000'");
        assert_eq!(render_kotlin_char_literal(39), "'\\\''");
        assert_eq!(render_kotlin_char_literal(92), "'\\\\'");
        assert_eq!(render_kotlin_char_literal(65), "'A'");
    }

    #[test]
    fn context_receiver_local_renders_as_labeled_this() {
        let local = crate::ir::LocalVarExpr {
            slot: 0,
            ty: JavaType::object("pkg/EventListener"),
            name: Some("this@EventListener".into()),
        };
        assert_eq!(kt_local_var_name(&local), "this@EventListener");
    }

    #[test]
    fn nested_three_way_comparison_does_not_leak_raw_opcode_marker() {
        let local = |slot: u16, name: &str| {
            Expr::LocalVar(crate::ir::LocalVarExpr {
                slot,
                ty: JavaType::FLOAT,
                name: Some(name.into()),
            })
        };
        let comparison = Expr::BinOp(
            BinOp::Ne,
            Box::new(Expr::BinOp(
                BinOp::FCmpG,
                Box::new(local(0, "left")),
                Box::new(local(1, "right")),
            )),
            Box::new(Expr::Const(ConstExpr {
                value: ConstValue::Int(0),
                ty: JavaType::INT,
            })),
        );
        assert_eq!(kt_render_expr(&comparison), "left != right");
    }

    #[test]
    fn function_object_replacement_parses_nested_capture_arguments() {
        let source = "click(Owner.lambda$1(make(1, 2), `name,with,commas`))";
        let lambda = "__abyss_lambda@ { use(__abyss_capture_0, __abyss_capture_1) }";
        let (rendered, replaced) =
            replace_balanced_constructor_calls(source, "Owner.lambda$1(", lambda, 2);
        assert!(replaced);
        assert_eq!(
            rendered,
            "click((__abyss_lambda@ { use(make(1, 2), `name,with,commas`) }))"
        );
    }

    #[test]
    fn pathological_coroutine_path_repetition_is_rejected() {
        let repeated = std::iter::repeat_n("handler.createEventHook()", 65)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(has_pathological_body_repetition(&repeated));
        assert!(!has_pathological_body_repetition(
            "if (ready) {\n    handler.createEventHook()\n}"
        ));
    }

    #[test]
    fn multiline_lambda_returns_are_labeled() {
        assert_eq!(
            relabel_lambda_returns("if (stop) {\n    return\n}\nreturn value", "lambda", false),
            "if (stop) {\n    return@lambda\n}\nreturn@lambda value"
        );
        assert_eq!(
            relabel_lambda_returns("work()\nreturn Unit.INSTANCE", "suspend", true),
            "work()"
        );
    }

    #[test]
    fn kotlin_operators_and_synthetic_increment_names_are_legal() {
        let local = |slot: u16, name: &str| {
            Expr::LocalVar(crate::ir::LocalVarExpr {
                slot,
                ty: JavaType::INT,
                name: Some(name.into()),
            })
        };
        let shifted = Expr::BinOp(
            BinOp::Shl,
            Box::new(local(0, "mask")),
            Box::new(local(1, "bits")),
        );
        assert_eq!(kt_render_expr(&shifted), "mask shl bits");
        assert_eq!(
            kt_render_expr(&Expr::IInc {
                slot: 2,
                delta: 1,
                name: Some("x$iv".into()),
            }),
            "`x$iv`++"
        );
    }

    #[test]
    fn string_templates_escape_synthetic_local_names() {
        let local = Expr::LocalVar(crate::ir::LocalVarExpr {
            slot: 1,
            ty: JavaType::object("java/lang/String"),
            name: Some("endpoint$iv".into()),
        });
        assert_eq!(
            kt_render_string_template(&[
                Expr::Const(ConstExpr {
                    value: ConstValue::StringRef("url=".into()),
                    ty: JavaType::object("java/lang/String"),
                }),
                local,
            ]),
            "\"url=${`endpoint$iv`}\""
        );
    }

    #[test]
    fn null_locals_safe_receivers_and_extreme_floats_are_legal_kotlin() {
        assert_eq!(kt_type_name_from_java(&JavaType::NULL), "Any");
        assert_eq!(kotlin_safe_receiver("value as Pair?"), "(value as Pair?)");
        assert_eq!(
            kt_render_const(&ConstExpr {
                value: ConstValue::Float(f32::MAX),
                ty: JavaType::FLOAT,
            }),
            "Float.MAX_VALUE"
        );
        assert_eq!(
            kt_local_var_name(&crate::ir::LocalVarExpr {
                slot: 0,
                ty: JavaType::object("java/lang/Object"),
                name: Some("this".into()),
            }),
            "this"
        );
    }

    #[test]
    fn lambda_capture_substitution_groups_cast_callees() {
        let capture = Expr::Cast(
            CastKind::CheckCast,
            JavaType::object("kotlin/jvm/functions/Function1"),
            Box::new(Expr::LocalVar(crate::ir::LocalVarExpr {
                slot: 0,
                ty: JavaType::object("java/lang/Object"),
                name: Some("callback".into()),
            })),
        );

        assert_eq!(
            substitute_kotlin_lambda_captures("__abyss_capture_0(p0)", 1, &[capture]),
            "(callback as Function1)(p0)"
        );
    }

    #[test]
    fn boxed_number_receiver_cast_is_grouped_before_primitive_conversion() {
        let boxed_value = Expr::Invoke {
            kind: InvokeKind::Virtual,
            owner: "java/lang/Integer".into(),
            name: "intValue".into(),
            descriptor: "()I".into(),
            object: Some(Box::new(Expr::Cast(
                CastKind::CheckCast,
                JavaType::object("java/lang/Integer"),
                Box::new(Expr::LocalVar(crate::ir::LocalVarExpr {
                    slot: 0,
                    ty: JavaType::object("java/lang/Object"),
                    name: Some("value".into()),
                })),
            ))),
            args: Vec::new(),
        };
        let converted = Expr::Cast(CastKind::I2F, JavaType::FLOAT, Box::new(boxed_value));

        assert_eq!(kt_render_expr(&converted), "(value as Integer).toFloat()");
    }

    #[test]
    fn boxed_boolean_instance_check_is_grouped_before_outer_cast() {
        let checked = Expr::InstanceOf(
            Box::new(Expr::LocalVar(crate::ir::LocalVarExpr {
                slot: 0,
                ty: JavaType::object("java/lang/Object"),
                name: Some("value".into()),
            })),
            JavaType::object("pkg/Thing"),
        );
        let boxed = Expr::Invoke {
            kind: InvokeKind::Static,
            owner: "java/lang/Boolean".into(),
            name: "valueOf".into(),
            descriptor: "(Z)Ljava/lang/Boolean;".into(),
            object: None,
            args: vec![checked],
        };
        let cast = Expr::Cast(
            CastKind::CheckCast,
            JavaType::object("java/lang/Comparable"),
            Box::new(boxed),
        );

        assert_eq!(kt_render_expr(&cast), "(value is Thing) as Comparable");
    }
}
