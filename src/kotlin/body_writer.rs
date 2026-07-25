/// Kotlin method body renderer.
///
/// Reuses the existing IR pipeline (CFG → recovery → StmtArena) but renders
/// Kotlin syntax: val/var, when, is, string templates, ?. and ?: operators.

use crate::cfg::{builder as cfg_builder, DomTree};
use crate::classfile::attribute::CodeAttribute;
use crate::classfile::constant_pool::ConstantPool;
use crate::classfile::member::Method;
use crate::classfile::ClassFile;
use crate::codegen::expr_writer::{simple_name, IndentWriter};
use crate::codegen::stmt_writer::lvt_entries;
use crate::ir::expr::*;
use crate::ir::recovery::recover;
use crate::ir::stack_sim::simulate_block;
use crate::ir::stmt::*;
use crate::ir::StmtArena;

// ── Public entry point ────────────────────────────────────────────────────

/// Decompile a method body to Kotlin source text.
/// Returns the body content (without the surrounding braces).
pub fn decompile_kotlin_body(m: &Method, cf: &ClassFile, indent: usize) -> Option<String> {
    let code = m.code()?;

    // Try to handle simple null-check expression methods first (?.  ?:)
    if let Some(result) = try_render_null_check_method(code, &cf.constant_pool, m.is_static(), &cf.this_class, indent) {
        return Some(result);
    }

    // Try to handle simple if/else expression methods (if (cond) a else b)
    if let Some(result) = try_render_if_expression_method(code, &cf.constant_pool, m.is_static(), &cf.this_class, indent) {
        return Some(result);
    }

    // Try to handle when-expression methods (switch where each arm returns a value)
    if let Some(result) = try_render_when_expression_method(code, &cf.constant_pool, m.is_static(), &cf.this_class, cf, indent) {
        return Some(result);
    }

    let cfg = cfg_builder::build(code);
    let dom = DomTree::compute(&cfg);
    let (arena, root) = recover(&cfg, &dom, code);

    let is_void_or_ctor = m.is_constructor() || m.descriptor.ends_with(")V");
    Some(render_kotlin_method_body(
        &arena, root, code, &cf.constant_pool,
        m.is_static(), &cf.this_class, indent,
        is_void_or_ctor, cf,
    ))
}

/// Try to detect and render simple null-check expression methods directly.
/// These are methods whose entire body is `expr?.method() ?: default` or similar.
/// We simulate the whole instruction stream and detect the null-check pattern.
fn try_render_null_check_method(
    code: &CodeAttribute,
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    indent: usize,
) -> Option<String> {
    use crate::classfile::opcodes::opc;
    use crate::classfile::instruction::InsnKind;

    let insns = &code.instructions;
    if insns.len() < 4 || insns.len() > 40 { return None; }

    // Look for the null-check pattern: ..., dup, ifnull/ifnonnull, ..., goto, ..., return
    // Find first ifnull/ifnonnull
    let null_check_idx = insns.iter().position(|i|
        i.opcode == opc::ifnull || i.opcode == opc::ifnonnull
    )?;

    // Must have a dup before the null check (the value being tested)
    if null_check_idx == 0 { return None; }
    let prev = &insns[null_check_idx - 1];
    if prev.opcode != opc::dup { return None; }

    // Get branch target
    let branch_offset = match &insns[null_check_idx].kind {
        InsnKind::Branch { offset } => *offset,
        _ => return None,
    };
    let branch_target = (insns[null_check_idx].offset as i64 + branch_offset as i64) as u32;

    // Find the goto that ends the non-null path
    let goto_idx_opt = insns[null_check_idx + 1..].iter().position(|i| i.opcode == opc::goto)
        .map(|p| null_check_idx + 1 + p);

    // If no goto, try the compound null-check pattern:
    // The non-null path ends with areturn directly, and the null path has a default value
    if goto_idx_opt.is_none() {
        let entries = lvt_entries(code);
        let local_names: Vec<(u16, String)> = entries.iter().map(|e| (e.slot, e.name.clone())).collect();
        return try_render_compound_null_check(insns, null_check_idx, branch_target, pool, is_static, this_class, indent, &local_names);
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

    // Get LVT names
    let entries = lvt_entries(code);
    let names: Vec<(u16, String)> = entries.iter().map(|e| (e.slot, e.name.clone())).collect();

    // Simulate everything before the dup to get the subject
    let pre_dup = &insns[..null_check_idx - 1]; // everything before dup
    let pre_result = simulate_block(pre_dup, pool, vec![], is_static, this_class, &names);
    let subject = pre_result.stack_out.last().map(|s| kt_render_expr(&s.expr))?;

    // Determine which path is null vs non-null based on the opcode
    let (nonnull_insns, null_insns) = if insns[null_check_idx].opcode == opc::ifnull {
        // ifnull: jump to null path, fall through to non-null path
        let nonnull = &insns[null_check_idx + 1..goto_idx];
        let null_end = merge_idx.min(insns.len());
        let null_path = &insns[null_path_start..null_end];
        (nonnull, null_path)
    } else {
        // ifnonnull: jump to non-null path, fall through to null path
        let null_end = insns.iter().position(|i| i.opcode == opc::goto && i.offset > insns[null_check_idx].offset)
            .unwrap_or(goto_idx);
        let null_path = &insns[null_check_idx + 1..null_end];
        let nonnull = &insns[null_path_start..merge_idx.min(insns.len())];
        (nonnull, null_path)
    };

    // Simulate non-null path (it operates on the duped subject, so pre-feed it)
    let nonnull_input = pre_result.stack_out.clone();
    let nonnull_result = simulate_block(nonnull_insns, pool, nonnull_input, is_static, this_class, &names);
    let nonnull_expr = nonnull_result.stack_out.last().map(|s| kt_render_expr(&s.expr));

    // Simulate null path (starts after pop of the null value)
    let null_insns_filtered: Vec<_> = null_insns.iter()
        .filter(|i| i.opcode != opc::pop && i.opcode != opc::pop2)
        .cloned()
        .collect();
    let null_result = simulate_block(&null_insns_filtered, pool, vec![], is_static, this_class, &names);
    let null_expr = null_result.stack_out.last().map(|s| kt_render_expr(&s.expr));

    // Check for a second null-check (for the ?: part after ?.)
    // Look for another ifnull/ifnonnull after the merge point
    let second_check = insns[merge_idx..].iter().position(|i|
        i.opcode == opc::ifnull || i.opcode == opc::ifnonnull
    );

    let mut w = IndentWriter::new(4);
    for _ in 0..indent { w.indent(); }

    if let (Some(nonnull_str), Some(null_str)) = (nonnull_expr, null_expr) {
        if null_str == "null" {
            // Pattern: subject?.method()
            // The nonnull_str should be something like "subject.method()"
            // Replace the subject reference with safe-call notation
            let safe_call = format!("{}?.{}", subject,
                nonnull_str.strip_prefix(&format!("{}.", subject))
                    .or_else(|| nonnull_str.strip_prefix(&subject))
                    .unwrap_or(&nonnull_str));

            if second_check.is_some() {
                // There's a ?: default after the ?.
                // Simulate the final return to get the default value
                let after_merge = &insns[merge_idx..];
                let return_idx = after_merge.iter().rposition(|i|
                    matches!(i.opcode, opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn | opc::areturn)
                );
                if return_idx.is_some() {
                    // Find the default value (usually the last const before the second merge)
                    let second_null_insns: Vec<_> = after_merge.iter()
                        .filter(|i| i.opcode != opc::dup && i.opcode != opc::pop
                            && !matches!(i.opcode, opc::ifnull | opc::ifnonnull | opc::goto
                                | opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn | opc::areturn))
                        .cloned()
                        .collect();
                    let default_result = simulate_block(&second_null_insns, pool, vec![], is_static, this_class, &names);
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
            let safe_call = if nonnull_str.contains(&subject) {
                format!("{}?.{}", subject,
                    nonnull_str.strip_prefix(&format!("{}.", subject))
                        .unwrap_or(&nonnull_str))
            } else {
                nonnull_str.clone()
            };
            w.line(&format!("return {} ?: {}", safe_call, null_str));
        } else {
            // Generic if (x != null) a else b
            w.line(&format!("return if ({} != null) {} else {}", subject, nonnull_str, null_str));
        }
        return Some(w.finish());
    }

    None
}

/// Handle compound null-check where non-null path has no goto (ends with areturn directly).
/// Pattern: name?.let { transform } ?: "default"
fn try_render_compound_null_check(
    insns: &[crate::classfile::instruction::Instruction],
    null_check_idx: usize,
    branch_target: u32,
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    indent: usize,
    names: &[(u16, String)],
) -> Option<String> {
    use crate::classfile::opcodes::opc;

    // Find where the null path starts
    let null_path_start = insns.iter().position(|i| i.offset == branch_target)?;

    // The null path should be short: pop + ldc + areturn
    let null_path = &insns[null_path_start..];
    let return_in_null = null_path.iter().position(|i|
        matches!(i.opcode, opc::areturn | opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn)
    )?;

    // Simulate null path (skip pop) to get the default value
    let null_insns: Vec<_> = null_path[..return_in_null].iter()
        .filter(|i| i.opcode != opc::pop && i.opcode != opc::pop2)
        .cloned()
        .collect();
    let null_result = simulate_block(&null_insns, pool, vec![], is_static, this_class, &names);
    let default_val = null_result.stack_out.last().map(|s| kt_render_expr(&s.expr))?;

    // Simulate the non-null path
    let nonnull_path = &insns[null_check_idx + 1..null_path_start];
    let nonnull_end = nonnull_path.iter().rposition(|i|
        matches!(i.opcode, opc::areturn | opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn)
    ).unwrap_or(nonnull_path.len());
    let nonnull_insns = &nonnull_path[..nonnull_end];

    // Filter out nops, pops, and secondary null checks
    let filtered: Vec<_> = nonnull_insns.iter()
        .filter(|i| i.opcode != 0x00 // nop
            && i.opcode != opc::pop && i.opcode != opc::pop2
            && i.opcode != opc::ifnonnull && i.opcode != opc::ifnull)
        .cloned()
        .collect();

    // Get the subject (first instruction(s) before dup)
    let pre_dup = &insns[..null_check_idx - 1];
    let pre_result = simulate_block(pre_dup, pool, vec![], is_static, this_class, &names);
    let subject = pre_result.stack_out.last().map(|s| kt_render_expr(&s.expr))?;

    // Simulate non-null path with the subject on stack
    let nonnull_result = simulate_block(&filtered, pool, pre_result.stack_out.clone(), is_static, this_class, &names);
    let nonnull_expr = nonnull_result.stack_out.last().map(|s| kt_render_expr(&s.expr));

    let mut w = IndentWriter::new(4);
    for _ in 0..indent { w.indent(); }

    if let Some(expr) = nonnull_expr {
        let safe_expr = if expr.contains(&subject) {
            expr.replacen(&subject, &format!("{}?", subject), 1)
        } else {
            format!("{}?.let {{ {} }}", subject, expr)
        };
        w.line(&format!("return {} ?: {}", safe_expr, default_val));
    } else {
        w.line(&format!("return {} ?: {}", subject, default_val));
    }

    Some(w.finish())
}

/// Try to detect and render a simple if/else expression method.
/// Pattern: condition + ifXX branch + then_value + goto + else_value + return
fn try_render_if_expression_method(
    code: &CodeAttribute,
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    indent: usize,
) -> Option<String> {
    use crate::classfile::opcodes::opc;
    use crate::classfile::instruction::InsnKind;

    let insns = &code.instructions;
    if insns.len() < 5 || insns.len() > 20 { return None; }

    // Find the conditional branch (not ifnull/ifnonnull — those are handled by null-check)
    let branch_idx = insns.iter().position(|i| {
        matches!(i.opcode,
            opc::ifeq | opc::ifne | opc::iflt | opc::ifge | opc::ifgt | opc::ifle |
            opc::if_icmpeq | opc::if_icmpne | opc::if_icmplt |
            opc::if_icmpge | opc::if_icmpgt | opc::if_icmple
        )
    })?;

    // Must have a goto in the then-path
    let goto_idx = insns[branch_idx + 1..].iter().position(|i| i.opcode == opc::goto)?;
    let goto_idx = branch_idx + 1 + goto_idx;

    // Must end with a return
    let last = insns.last()?;
    if !matches!(last.opcode, opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn | opc::areturn) {
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

    let entries = lvt_entries(code);
    let names: Vec<(u16, String)> = entries.iter().map(|e| (e.slot, e.name.clone())).collect();

    // Build the condition string
    let branch_op = insns[branch_idx].opcode;
    let pre_branch = &insns[..branch_idx];
    let cond_result = simulate_block(pre_branch, pool, vec![], is_static, this_class, &names);
    let cond_str = build_kotlin_condition(branch_op, &cond_result.stack_out, true); // negated for if-expression

    // Simulate then path (between branch and goto)
    let then_insns = &insns[branch_idx + 1..goto_idx];
    let then_result = simulate_block(then_insns, pool, vec![], is_static, this_class, &names);
    let then_expr = then_result.stack_out.last()
        .map(|s| kt_render_expr(&s.expr))?;

    // Simulate else path (between branch target and return)
    let return_idx = insns.len() - 1;
    let else_insns = &insns[else_start..return_idx];
    let else_result = simulate_block(else_insns, pool, vec![], is_static, this_class, &names);
    let else_expr = else_result.stack_out.last()
        .map(|s| kt_render_expr(&s.expr))?;

    // Render
    let mut w = IndentWriter::new(4);
    for _ in 0..indent { w.indent(); }
    w.line(&format!("return if ({}) {} else {}", cond_str, then_expr, else_expr));
    Some(w.finish())
}

/// Try to detect and render a when-expression method where each case arm returns a value.
/// Pattern: instructions before switch + tableswitch/lookupswitch + each case arm = value + return
fn try_render_when_expression_method(
    code: &CodeAttribute,
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    cf: &ClassFile,
    indent: usize,
) -> Option<String> {
    use crate::classfile::opcodes::opc;
    use crate::classfile::instruction::InsnKind;

    let insns = &code.instructions;
    if insns.len() < 5 { return None; }

    // Find the tableswitch or lookupswitch instruction
    let switch_idx = insns.iter().position(|i| matches!(i.opcode, opc::tableswitch | opc::lookupswitch))?;

    let entries = lvt_entries(code);
    let names: Vec<(u16, String)> = entries.iter().map(|e| (e.slot, e.name.clone())).collect();

    // Simulate instructions before the switch to get the switch expression
    let pre_switch = &insns[..switch_idx];
    let pre_result = simulate_block(pre_switch, pool, vec![], is_static, this_class, &names);

    // Get the switch expression from the stack
    let switch_expr = pre_result.stack_out.last()?;

    // Detect if this is a Kotlin enum when-mapping pattern
    let (subject_str, case_map) = if let Some((subj, map)) = detect_kotlin_enum_when(&switch_expr.expr, cf) {
        (subj, Some(map))
    } else {
        (kt_render_expr(&switch_expr.expr), None)
    };

    // Parse switch targets
    let switch_insn = &insns[switch_idx];
    let base_offset = switch_insn.offset as i64;

    let (cases, default_offset): (Vec<(i32, u32)>, u32) = match &switch_insn.kind {
        InsnKind::TableSwitch { low, offsets, default_offset, .. } => {
            let cases: Vec<(i32, u32)> = offsets.iter().enumerate()
                .map(|(i, &off)| (*low + i as i32, (base_offset + off as i64) as u32))
                .collect();
            (cases, (base_offset + *default_offset as i64) as u32)
        }
        InsnKind::LookupSwitch { pairs, default_offset } => {
            let cases: Vec<(i32, u32)> = pairs.iter()
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
        let arm_end = insns[arm_start..].iter().position(|i|
            matches!(i.opcode, opc::areturn | opc::ireturn | opc::lreturn | opc::freturn | opc::dreturn
                | opc::goto | opc::goto_w)
        )?;
        let arm_insns = &insns[arm_start..arm_start + arm_end];

        if arm_insns.len() > 5 { return None; } // Too complex, bail

        let arm_result = simulate_block(arm_insns, pool, vec![], is_static, this_class, &names);
        let value_expr = arm_result.stack_out.last()
            .map(|s| kt_render_expr(&s.expr))
            .unwrap_or_else(|| "/* ? */".into());

        let case_label = if let Some(ref map) = case_map {
            map.get(case_val).cloned().unwrap_or_else(|| case_val.to_string())
        } else {
            case_val.to_string()
        };

        arms.push((case_label, value_expr));
    }

    // Also handle default arm if it exists and is reachable
    let default_arm_start = insns.iter().position(|i| i.offset == default_offset);
    let default_value = if let Some(start) = default_arm_start {
        let arm_end = insns[start..].iter().position(|i|
            matches!(i.opcode, opc::areturn | opc::ireturn | opc::lreturn | opc::freturn
                | opc::dreturn | opc::athrow | opc::goto | opc::goto_w)
        );
        if let Some(end) = arm_end {
            let arm_insns = &insns[start..start + end];
            if arm_insns.len() <= 5 {
                let arm_result = simulate_block(arm_insns, pool, vec![], is_static, this_class, &names);
                arm_result.stack_out.last().map(|s| kt_render_expr(&s.expr))
            } else { None }
        } else { None }
    } else { None };

    // Render the when expression
    let mut w = IndentWriter::new(4);
    for _ in 0..indent { w.indent(); }

    // Simplify class-qualified enum references (e.g., Direction.SOUTH → SOUTH within the same class)
    let this_simple = simple_name(this_class);

    w.line(&format!("return when ({}) {{", subject_str));
    w.indent();
    for (label, value) in &arms {
        // Strip enum class prefix from value if it matches the enclosing class
        let simplified_value = value.strip_prefix(&format!("{}.", this_simple))
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
    code: &CodeAttribute,
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    indent: usize,
    suppress_trailing_return: bool,
    cf: &ClassFile,
) -> String {
    let mut w = IndentWriter::new(4);
    for _ in 0..indent { w.indent(); }

    let entries = lvt_entries(code);
    let names: Vec<(u16, String)> = entries.iter().map(|e| (e.slot, e.name.clone())).collect();
    render_stmt(arena, root, code, pool, is_static, this_class, &names, &entries, cf, &mut w);
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
                    let line_end = out[last_pos..].find('\n')
                        .map(|p| last_pos + p + 1)
                        .unwrap_or(out.len());
                    out.replace_range(line_start..line_end, "");
                }
            }
        }
    }
    out
}

// ── Kotlin expression renderer ────────────────────────────────────────────

/// Render an expression to Kotlin source.
fn kt_render_expr(expr: &Expr) -> String {
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

        Expr::LocalVar(lv) => {
            if let Some(name) = &lv.name {
                // $this$functionName → this (extension function receiver)
                if name.starts_with("$this$") {
                    "this".into()
                } else if name.starts_with("$i$f$") || name.starts_with("$i$a$") {
                    // Inline function metadata variables — suppress
                    return String::new();
                } else {
                    name.clone()
                }
            } else {
                format!("var{}", lv.slot)
            }
        }

        Expr::BinOp(op, lhs, rhs) => {
            let prec = op.precedence();
            format!("{} {} {}",
                kt_render_expr_prec(lhs, prec),
                op.symbol(),
                kt_render_expr_prec(rhs, prec))
        }

        Expr::UnOp(op, operand) => {
            format!("{}{}", op.symbol(), kt_render_expr_prec(operand, 1))
        }

        Expr::Cast(kind, ty, inner) => {
            match kind {
                CastKind::CheckCast => {
                    // Kotlin uses "as" for casts
                    format!("{} as {}", kt_render_expr_prec(inner, 1), kt_type_name_from_java(ty))
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
            format!("{} is {}",
                kt_render_expr_prec(obj, 6),
                kt_type_name_from_java(ty))
        }

        Expr::Field { dir: FieldDir::Get, owner, name, object, .. } => {
            match object {
                Some(obj) => format!("{}.{}", kt_render_expr_prec(obj, 0), name),
                None => format!("{}.{}", simple_name(owner), name),
            }
        }

        Expr::Field { dir: FieldDir::Put, owner, name, object, value, .. } => {
            let lhs = match object {
                Some(obj) => format!("{}.{}", kt_render_expr_prec(obj, 0), name),
                None => format!("{}.{}", simple_name(owner), name),
            };
            let rhs = value.as_ref()
                .map(|v| kt_render_expr(v))
                .unwrap_or_default();
            format!("{} = {}", lhs, rhs)
        }

        Expr::Invoke { kind, owner, name, object, args, .. } => {
            kt_render_invoke(kind, owner, name, object.as_deref(), args)
        }

        Expr::InvokeDynamic { name, args, bootstrap_index, .. } => {
            // String concatenation → Kotlin string template
            if name == "makeConcatWithConstants" || name == "makeConcat" {
                if !args.is_empty() {
                    return kt_render_string_template(args);
                }
            }

            // Lambda support
            let lambda_body = crate::codegen::class_writer::LAMBDA_BOOTSTRAP.with(|cache| {
                cache.borrow().get(bootstrap_index).cloned()
            });
            if let Some(body) = lambda_body {
                return body;
            }

            let args_str = args.iter().map(|a| kt_render_expr(a)).collect::<Vec<_>>().join(", ");
            format!("{}({})", name, args_str)
        }

        Expr::ArrayLoad { array, index, .. } => {
            format!("{}[{}]", kt_render_expr_prec(array, 0), kt_render_expr(index))
        }

        Expr::ArrayStore { array, index, value } => {
            format!("{}[{}] = {}",
                kt_render_expr_prec(array, 0),
                kt_render_expr(index),
                kt_render_expr(value))
        }

        Expr::ArrayLength(arr) => {
            format!("{}.size", kt_render_expr_prec(arr, 0))
        }

        Expr::NewArray { kind, type_, dimensions, initializer } => {
            let size = dimensions.first().map(|d| kt_render_expr(d)).unwrap_or_default();
            match kind {
                NewKind::PrimitiveArray { atype } => {
                    let ctor = match atype {
                        4 => "BooleanArray", 5 => "CharArray",
                        6 => "FloatArray", 7 => "DoubleArray",
                        8 => "ByteArray", 9 => "ShortArray",
                        10 => "IntArray", 11 => "LongArray",
                        _ => "IntArray",
                    };
                    format!("{}({})", ctor, size)
                }
                NewKind::RefArray => {
                    format!("arrayOfNulls<{}>({})", kt_type_name_from_java(type_), size)
                }
                NewKind::MultiArray { .. } => {
                    let dims = dimensions.iter().map(|d| kt_render_expr(d)).collect::<Vec<_>>().join(", ");
                    format!("Array({})", dims)
                }
                NewKind::Object => format!("{}()", kt_type_name_from_java(type_)),
            }
        }

        Expr::New { class_name, args, .. } => {
            let args_str = args.iter().map(|a| kt_render_expr(a)).collect::<Vec<_>>().join(", ");
            format!("{}({})", simple_name(class_name), args_str)
        }

        Expr::Assign { lhs, rhs } => {
            format!("{} = {}", kt_render_expr_prec(lhs, 14), kt_render_expr(rhs))
        }

        Expr::IInc { slot, delta, name } => {
            let fallback = format!("var{}", slot);
            let var = name.as_deref().unwrap_or(&fallback);
            if *delta == 1 { format!("{}++", var) }
            else if *delta == -1 { format!("{}--", var) }
            else if *delta > 0 { format!("{} += {}", var, delta) }
            else { format!("{} -= {}", var, -delta) }
        }

        Expr::Monitor { .. } => String::new(),

        Expr::Throw(exc) => format!("throw {}", kt_render_expr(exc)),

        Expr::Return(Some(val)) => format!("return {}", kt_render_expr(val)),
        Expr::Return(None) => "return".into(),

        Expr::Ternary { cond, then_expr, else_expr } =>
            format!("if ({}) {} else {}",
                cond, kt_render_expr(then_expr), kt_render_expr(else_expr)),

        Expr::Opaque { opcode, offset } =>
            format!("/* opaque 0x{:02x} @{} */", opcode, offset),
    }
}

// ── Helper functions ──────────────────────────────────────────────────────

fn kt_render_const(c: &ConstExpr) -> String {
    use crate::types::java_type::JavaType;
    match &c.value {
        ConstValue::Int(v) => {
            if c.ty == JavaType::BOOLEAN {
                if *v == 0 { "false".into() } else { "true".into() }
            } else if c.ty == JavaType::CHAR && *v >= 32 && *v < 127 {
                format!("'{}'", *v as u8 as char)
            } else {
                v.to_string()
            }
        }
        ConstValue::Long(v) => format!("{}L", v),
        ConstValue::Float(v) => format!("{}f", v),
        ConstValue::Double(v) => v.to_string(),
        ConstValue::StringRef(s) => format!("\"{}\"", s.replace('"', "\\\"").replace('\n', "\\n")),
        ConstValue::ClassRef(s) => format!("{}::class.java", simple_name(s)),
        ConstValue::Null => "null".into(),
    }
}

/// Map a Java type to Kotlin type name.
fn kt_type_name_from_java(ty: &crate::types::java_type::JavaType) -> String {
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
        _ => {
            // java.lang.String → String, etc.
            let name = s.rsplit('.').next().unwrap_or(&s);
            name.replace('$', ".")
        }
    }
}

/// Primitive cast → Kotlin conversion method.
fn kt_cast_method(kind: CastKind) -> &'static str {
    match kind {
        CastKind::I2L => "toLong", CastKind::I2F => "toFloat", CastKind::I2D => "toDouble",
        CastKind::L2I => "toInt", CastKind::L2F => "toFloat", CastKind::L2D => "toDouble",
        CastKind::F2I => "toInt", CastKind::F2L => "toLong", CastKind::F2D => "toDouble",
        CastKind::D2I => "toInt", CastKind::D2L => "toLong", CastKind::D2F => "toFloat",
        CastKind::I2B => "toByte", CastKind::I2C => "toChar", CastKind::I2S => "toShort",
        CastKind::CheckCast => "",
    }
}

/// Render method invocation with Kotlin idioms.
fn kt_render_invoke(kind: &InvokeKind, owner: &str, name: &str,
                    object: Option<&Expr>, args: &[Expr]) -> String {
    // Detect StringBuilder chain: new StringBuilder().append(...).append(...).toString()
    if name == "toString" && args.is_empty() {
        if let Some(obj) = object {
            if let Some(template) = try_render_stringbuilder_chain(obj) {
                return template;
            }
        }
    }

    let args_str = args.iter().map(|a| kt_render_expr(a)).collect::<Vec<_>>().join(", ");

    // Map common Java patterns to Kotlin equivalents
    match (owner, name) {
        ("java/io/PrintStream", "println") => format!("println({})", args_str),
        ("java/io/PrintStream", "print") => format!("print({})", args_str),
        // Suppress Kotlin compiler null-check intrinsics
        ("kotlin/jvm/internal/Intrinsics", "checkNotNullParameter") => return String::new(),
        ("kotlin/jvm/internal/Intrinsics", "checkNotNullExpressionValue") => {
            // Return just the first argument (the checked expression)
            if !args.is_empty() {
                return kt_render_expr(&args[0]);
            }
            return String::new();
        }
        ("kotlin/jvm/internal/Intrinsics", "checkNotNull") => {
            if !args.is_empty() {
                return format!("{}!!", kt_render_expr(&args[0]));
            }
            return String::new();
        }
        // Range expressions: rangeTo → ..
        (_, "rangeTo") if args.len() == 1 => {
            if let Some(obj) = object {
                return format!("{}..{}", kt_render_expr(obj), kt_render_expr(&args[0]));
            }
            args_str
        }
        (_, "downTo") if args.len() == 1 => {
            if let Some(obj) = object {
                return format!("{} downTo {}", kt_render_expr(obj), kt_render_expr(&args[0]));
            }
            args_str
        }
        (_, "step") if args.len() == 1 => {
            if let Some(obj) = object {
                return format!("{} step {}", kt_render_expr(obj), kt_render_expr(&args[0]));
            }
            args_str
        }
        (_, "until") if args.len() == 1 => {
            if let Some(obj) = object {
                return format!("{} until {}", kt_render_expr(obj), kt_render_expr(&args[0]));
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
                format!("{}.equals({})", simple_name(owner), args_str)
            }
        }
        ("java/lang/Object", "equals") => {
            if let Some(obj) = object {
                format!("{} == {}", kt_render_expr_prec(obj, 7), args_str)
            } else {
                format!("{}.equals({})", simple_name(owner), args_str)
            }
        }
        ("java/lang/Integer", "valueOf") => args_str,
        ("java/lang/Long", "valueOf") => args_str,
        ("java/lang/Float", "valueOf") => args_str,
        ("java/lang/Double", "valueOf") => args_str,
        ("java/lang/Boolean", "valueOf") => args_str,
        ("java/lang/Integer", "intValue") |
        ("java/lang/Long", "longValue") |
        ("java/lang/Float", "floatValue") |
        ("java/lang/Double", "doubleValue") |
        ("java/lang/Boolean", "booleanValue") => {
            if let Some(obj) = object {
                kt_render_expr(obj)
            } else {
                format!("{}.{}()", simple_name(owner), name)
            }
        }
        _ => {
            // Kotlin property access patterns
            if args.is_empty() {
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
                    return format!("{}({})", kt_render_expr_prec(obj, 0), args_str);
                }
            }
            // .get(index) → [index]
            if name == "get" && args.len() == 1 {
                if let Some(obj) = object {
                    return format!("{}[{}]", kt_render_expr_prec(obj, 0), args_str);
                }
            }
            // .set(index, value) → [index] = value
            if name == "set" && args.len() == 2 {
                if let Some(obj) = object {
                    return format!("{}[{}] = {}",
                        kt_render_expr_prec(obj, 0),
                        kt_render_expr(&args[0]),
                        kt_render_expr(&args[1]));
                }
            }
            match (kind, object) {
                (InvokeKind::Static, _) =>
                    format!("{}.{}({})", simple_name(owner), name, args_str),
                (InvokeKind::Special, Some(obj)) if name == "<init>" =>
                    format!("{}.{}({})", kt_render_expr_prec(obj, 0), name, args_str),
                (_, Some(obj)) =>
                    format!("{}.{}({})", kt_render_expr_prec(obj, 0), name, args_str),
                (_, None) =>
                    format!("{}.{}({})", simple_name(owner), name, args_str),
            }
        }
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
            Expr::Const(c) => {
                match &c.value {
                    ConstValue::StringRef(s) => template.push_str(s),
                    _ => {
                        template.push_str("${");
                        template.push_str(&kt_render_expr(arg));
                        template.push('}');
                    }
                }
            }
            Expr::Invoke { kind: InvokeKind::Static, owner, name, args: vargs, .. }
                if name == "valueOf" && owner == "java/lang/String" && vargs.len() == 1 => {
                template.push_str("${");
                template.push_str(&kt_render_expr(&vargs[0]));
                template.push('}');
            }
            Expr::LocalVar(lv) => {
                let fallback = format!("var{}", lv.slot);
                let var_name = lv.name.as_deref().unwrap_or(&fallback);
                template.push('$');
                // Simple variable names can use $name, complex need ${expr}
                if var_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    template.push_str(var_name);
                } else {
                    template.push('{');
                    template.push_str(var_name);
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
    let all_strings = parts.iter().all(|p| matches!(p, Expr::Const(c) if matches!(&c.value, ConstValue::StringRef(_))));
    if all_strings && parts.len() == 1 {
        if let Expr::Const(c) = parts[0] {
            if let ConstValue::StringRef(s) = &c.value {
                return Some(format!("\"{}\"", s.replace('"', "\\\"").replace('\n', "\\n")));
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
                let fallback = format!("var{}", lv.slot);
                let var_name = lv.name.as_deref().unwrap_or(&fallback);
                template.push('$');
                if var_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    template.push_str(var_name);
                } else {
                    template.push('{');
                    template.push_str(var_name);
                    template.push('}');
                }
            }
            Expr::Field { name, object: Some(obj), .. } => {
                // this.name → ${this.name} or $name
                let obj_str = kt_render_expr(obj);
                if obj_str == "this" {
                    template.push_str("${this.");
                    template.push_str(name);
                    template.push('}');
                } else {
                    template.push_str("${");
                    template.push_str(&obj_str);
                    template.push('.');
                    template.push_str(name);
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
        Expr::Invoke { name, object: Some(obj), args, owner, .. }
            if name == "append" && args.len() == 1
            && (owner == "java/lang/StringBuilder" || owner == "java/lang/StringBuffer") => {
            collect_append_args(obj, parts)?;
            parts.push(&args[0]);
            Some(())
        }
        // Base case: new StringBuilder() or new StringBuilder("")
        Expr::New { class_name, args, .. }
            if class_name == "java/lang/StringBuilder" || class_name == "java/lang/StringBuffer" => {
            if args.len() == 1 {
                // new StringBuilder("initial")
                parts.push(&args[0]);
            }
            Some(())
        }
        // Invoke on <init> pattern from the IR
        Expr::Invoke { name, owner, args, kind: InvokeKind::Special, .. }
            if name == "<init>" && (owner == "java/lang/StringBuilder" || owner == "java/lang/StringBuffer") => {
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

fn render_stmt(
    arena: &StmtArena,
    id: StmtId,
    code: &CodeAttribute,
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    names: &[(u16, String)],
    lvt: &[LvtEntry],
    cf: &ClassFile,
    w: &mut IndentWriter,
) {
    match arena.get(id) {
        Stmt::Exit => {}

        Stmt::Block(b) => {
            let result = simulate_block(&b.instructions, pool, vec![], is_static, this_class, names);
            kt_emit_stmts(&result.stmts, lvt, w);
        }

        Stmt::Seq(s) => {
            let children = s.children.clone();
            for child in children {
                render_stmt(arena, child, code, pool, is_static, this_class, names, lvt, cf, w);
            }
        }

        Stmt::If(s) => {
            render_if(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
        }

        Stmt::Loop(s) => {
            render_loop(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
        }

        Stmt::Switch(s) => {
            render_when(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
        }

        Stmt::TryCatch(s) => {
            render_try_catch(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
        }

        Stmt::Synchronized(s) => {
            w.line("synchronized(/* monitor */) {");
            w.indent();
            let body = s.body;
            render_stmt(arena, body, code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
        }
    }
}

/// Emit statements with Kotlin val/var declarations and destructuring.
fn kt_emit_stmts(stmts: &[Expr], lvt: &[LvtEntry], w: &mut IndentWriter) {
    let mut declared = std::collections::HashSet::<u16>::new();
    // Track which slots get reassigned (for val vs var decision)
    let mut assigned_slots = std::collections::HashSet::<u16>::new();
    for expr in stmts {
        if let Expr::Assign { lhs, .. } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                if assigned_slots.contains(&lv.slot) {
                    // Second assignment → must be var
                } else {
                    assigned_slots.insert(lv.slot);
                }
            }
        }
    }
    // Slots that appear more than once in assignments are var
    let mut assign_count = std::collections::HashMap::<u16, usize>::new();
    for expr in stmts {
        if let Expr::Assign { lhs, .. } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                *assign_count.entry(lv.slot).or_insert(0) += 1;
            }
        }
    }

    let mut i = 0;
    while i < stmts.len() {
        // Try to detect destructuring: consecutive componentN() calls on same source
        if let Some((destructuring_line, consumed)) = try_detect_destructuring(&stmts[i..], lvt, &mut declared) {
            w.line(&destructuring_line);
            i += consumed;
            continue;
        }

        let expr = &stmts[i];
        let line = if let Expr::Assign { lhs, rhs } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                let slot = lv.slot;
                if !declared.contains(&slot) {
                    if let Some(entry) = lvt.iter().find(|e| e.slot == slot) {
                        // Suppress compiler-internal variables
                        if entry.name.starts_with("$this$")
                            || entry.name.starts_with("$i$f$")
                            || entry.name.starts_with("$i$a$") {
                            declared.insert(slot);
                            i += 1;
                            continue;
                        }
                        declared.insert(slot);
                        let rhs_str = kt_render_expr_concat(rhs);
                        let keyword = if assign_count.get(&slot).copied().unwrap_or(0) > 1 {
                            "var"
                        } else {
                            "val"
                        };
                        format!("{} {} = {}", keyword, entry.name, rhs_str)
                    } else {
                        let rhs_str = kt_render_expr_concat(rhs);
                        format!("{} = {}", kt_render_expr(lhs), rhs_str)
                    }
                } else {
                    let rhs_str = kt_render_expr_concat(rhs);
                    format!("{} = {}", kt_render_expr(lhs), rhs_str)
                }
            } else {
                kt_render_expr_concat(expr)
            }
        } else {
            kt_render_expr_concat(expr)
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
    if stmts.len() < 2 { return None; }

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
                            entry.name.clone()
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
        Expr::Invoke { name, object: Some(obj), args, .. }
            if name.starts_with("component") && args.is_empty() => {
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
        Expr::Invoke { name: unbox_name, object: Some(inner), args, .. }
            if is_unbox_method(unbox_name) && args.is_empty() => {
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
    matches!(name, "intValue" | "longValue" | "floatValue" | "doubleValue"
        | "booleanValue" | "byteValue" | "shortValue" | "charValue")
}

/// Render expression, converting string concat to Kotlin string templates.
fn kt_render_expr_concat(expr: &Expr) -> String {
    if let Expr::InvokeDynamic { name, args, .. } = expr {
        if name == "makeConcatWithConstants" || name == "makeConcat" {
            if !args.is_empty() {
                return kt_render_string_template(args);
            }
        }
    }
    kt_render_expr(expr)
}

// ── Control flow rendering ────────────────────────────────────────────────

fn render_if(
    arena: &StmtArena, s: IfStmt,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    let cond_str = extract_branch_condition(s.cond_block, code, pool, is_static, this_class, s.negated, names);

    // Detect safe call / Elvis patterns:
    // if (x != null) { ... } else { null } → x?.method()
    // if (x == null) { default } else { x } → x ?: default
    if is_null_check(&cond_str) {
        if let Some(else_id) = s.else_branch {
            // Check if this is a simple null-check pattern
            let then_simple = is_simple_stmt(arena, s.then_branch);
            let else_simple = is_simple_stmt(arena, else_id);

            if then_simple || else_simple {
                // Render as compact null-check
                if cond_str.ends_with("!= null") {
                    // if (x != null) body else null → x?.body or x ?: body
                    w.line(&format!("if ({}) {{", cond_str));
                    w.indent();
                    render_stmt(arena, s.then_branch, code, pool, is_static, this_class, names, lvt, cf, w);
                    w.dedent();
                    w.line("} else {");
                    w.indent();
                    render_stmt(arena, else_id, code, pool, is_static, this_class, names, lvt, cf, w);
                    w.dedent();
                    w.line("}");
                    return;
                } else if cond_str.ends_with("== null") {
                    // if (x == null) default else x → x ?: default
                    w.line(&format!("if ({}) {{", cond_str));
                    w.indent();
                    render_stmt(arena, s.then_branch, code, pool, is_static, this_class, names, lvt, cf, w);
                    w.dedent();
                    w.line("} else {");
                    w.indent();
                    render_stmt(arena, else_id, code, pool, is_static, this_class, names, lvt, cf, w);
                    w.dedent();
                    w.line("}");
                    return;
                }
            }
        }
    }

    if let Some(else_id) = s.else_branch {
        w.line(&format!("if ({}) {{", cond_str));
        w.indent();
        render_stmt(arena, s.then_branch, code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
        w.line("} else {");
        w.indent();
        render_stmt(arena, else_id, code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
        w.line("}");
    } else {
        w.line(&format!("if ({}) {{", cond_str));
        w.indent();
        render_stmt(arena, s.then_branch, code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
        w.line("}");
    }
}

fn is_null_check(cond: &str) -> bool {
    cond.ends_with("== null") || cond.ends_with("!= null")
}

fn is_simple_stmt(arena: &StmtArena, id: StmtId) -> bool {
    match arena.get(id) {
        Stmt::Block(_) => true,
        Stmt::Exit => true,
        Stmt::Seq(s) => s.children.len() <= 1,
        _ => false,
    }
}

fn extract_branch_condition(
    _block_id: crate::cfg::BlockId,
    code: &CodeAttribute,
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    negated: bool,
    names: &[(u16, String)],
) -> String {
    use crate::classfile::opcodes::opc;

    let all_insns = &code.instructions;

    let branch_idx = all_insns.iter().rposition(|i| {
        matches!(i.opcode,
            opc::ifeq | opc::ifne | opc::iflt | opc::ifge | opc::ifgt | opc::ifle |
            opc::if_icmpeq | opc::if_icmpne | opc::if_icmplt |
            opc::if_icmpge | opc::if_icmpgt | opc::if_icmple |
            opc::if_acmpeq | opc::if_acmpne | opc::ifnull | opc::ifnonnull
        )
    }).unwrap_or(0);

    let branch_op = all_insns.get(branch_idx).map(|i| i.opcode).unwrap_or(opc::ifeq);
    let sim_insns = &all_insns[..branch_idx];
    let result = simulate_block(sim_insns, pool, vec![], is_static, this_class, names);

    build_kotlin_condition(branch_op, &result.stack_out, negated)
}

fn build_kotlin_condition(
    branch_op: u8,
    stack: &[crate::ir::stack_sim::SlotInfo],
    negated: bool,
) -> String {
    use crate::classfile::opcodes::opc;

    let top = stack.last().map(|s| kt_render_expr(&s.expr)).unwrap_or_else(|| "/* ? */".into());
    let sec = if stack.len() >= 2 {
        kt_render_expr(&stack[stack.len() - 2].expr)
    } else {
        "0".into()
    };

    // Kotlin-specific: null checks use == null / != null
    let (lhs, rhs, op_str) = match branch_op {
        opc::ifeq => (top, "0".into(), if negated { "!=" } else { "==" }),
        opc::ifne => (top, "0".into(), if negated { "==" } else { "!=" }),
        opc::iflt => (top, "0".into(), if negated { ">=" } else { "<" }),
        opc::ifge => (top, "0".into(), if negated { "<" } else { ">=" }),
        opc::ifgt => (top, "0".into(), if negated { "<=" } else { ">" }),
        opc::ifle => (top, "0".into(), if negated { ">" } else { "<=" }),
        opc::if_icmpeq => (sec, top, if negated { "!=" } else { "==" }),
        opc::if_icmpne => (sec, top, if negated { "==" } else { "!=" }),
        opc::if_icmplt => (sec, top, if negated { ">=" } else { "<" }),
        opc::if_icmpge => (sec, top, if negated { "<" } else { ">=" }),
        opc::if_icmpgt => (sec, top, if negated { "<=" } else { ">" }),
        opc::if_icmple => (sec, top, if negated { ">" } else { "<=" }),
        opc::if_acmpeq => (sec, top, if negated { "!=" } else { "==" }),
        opc::if_acmpne => (sec, top, if negated { "==" } else { "!=" }),
        opc::ifnull => (top, "null".into(), if negated { "!=" } else { "==" }),
        opc::ifnonnull => (top, "null".into(), if negated { "==" } else { "!=" }),
        _ => (top, "0".into(), "=="),
    };
    format!("{} {} {}", lhs, op_str, rhs)
}

fn render_loop(
    arena: &StmtArena, s: LoopStmt,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    match s.kind {
        LoopKind::While => {
            let cond = extract_branch_condition(
                s.header_block, code, pool, is_static, this_class, s.cond_negated, names);

            // Detect for-in pattern: while (iter.hasNext() ...)
            if cond.contains(".hasNext()") {
                // Render loop body to a buffer to extract iterator.next() assignment
                let mut body_buf = IndentWriter::new(4);
                render_stmt(arena, s.body, code, pool, is_static, this_class, names, lvt, cf, &mut body_buf);
                let body_text = body_buf.finish();

                // Parse the body: first line should be `val item = (iter.next()...)`
                let body_lines: Vec<&str> = body_text.lines().collect();
                let (element_var, remaining_lines) = if !body_lines.is_empty() {
                    let first = body_lines[0].trim();
                    if first.contains(".next()") {
                        let var_name = first.strip_prefix("val ")
                            .or_else(|| first.strip_prefix("var "))
                            .and_then(|s| s.split('=').next())
                            .map(|s| s.trim().to_string())
                            .unwrap_or_else(|| "item".into());
                        (var_name, &body_lines[1..])
                    } else {
                        ("item".into(), &body_lines[..])
                    }
                } else {
                    ("item".into(), &body_lines[..])
                };

                // Extract collection from pre-loop: look for "varX = expr.iterator()" pattern
                let iter_var = cond.split('.').next().unwrap_or("").trim();
                let collection_name = find_iterator_source(arena, s.body, iter_var, code, pool, is_static, this_class, names);

                w.line(&format!("for ({} in {}) {{", element_var,
                    collection_name.unwrap_or_else(|| "/* collection */".into())));
                w.indent();
                for line in remaining_lines {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        w.line(trimmed);
                    }
                }
                w.dedent();
                w.line("}");
                return;
            }

            // Detect range for loop: while (i <= N) or while (i < N)
            if let Some(range_for) = try_detect_range_for(&cond, lvt) {
                w.line(&format!("for ({} in {}) {{", range_for.var_name, range_for.range_expr));
                w.indent();
                render_stmt(arena, s.body, code, pool, is_static, this_class, names, lvt, cf, w);
                w.dedent();
                w.line("}");
                return;
            }

            w.line(&format!("while ({}) {{", cond));
            w.indent();
            render_stmt(arena, s.body, code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
        }
        LoopKind::DoWhile => {
            w.line("do {");
            w.indent();
            render_stmt(arena, s.body, code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            let cond = extract_branch_condition(
                s.tail_block, code, pool, is_static, this_class, s.cond_negated, names);
            w.line(&format!("}} while ({})", cond));
        }
        LoopKind::Infinite | LoopKind::For => {
            w.line("while (true) {");
            w.indent();
            render_stmt(arena, s.body, code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
        }
    }
}

struct ForInInfo {
    var_name: String,
    collection: String,
}

struct RangeForInfo {
    var_name: String,
    range_expr: String,
}

/// Detect for-in pattern: iterator/hasNext/next pattern
fn try_detect_for_in(cond: &str, _lvt: &[LvtEntry], _names: &[(u16, String)]) -> Option<ForInInfo> {
    if cond.contains(".hasNext()") {
        Some(ForInInfo {
            var_name: "item".into(),
            collection: "/* collection */".into(),
        })
    } else {
        None
    }
}

/// Try to find the collection expression for an iterator variable.
/// Looks at the pre-loop block for "iter_var = collection.iterator()" pattern.
fn find_iterator_source(
    _arena: &StmtArena, _body: StmtId, iter_var: &str,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str, names: &[(u16, String)],
) -> Option<String> {
    // Find the "iterator()" call in the pre-loop instructions
    // by simulating the entire pre-loop area
    let insns = &code.instructions;

    // Find the loop header (hasNext call position)
    let has_next_pos = insns.iter().position(|i| {
        if let crate::classfile::instruction::InsnKind::Invoke { index, .. } = &i.kind {
            if let Ok(entry) = pool.get(*index) {
                if let crate::classfile::constant_pool::CpEntry::Methodref(mr)
                    | crate::classfile::constant_pool::CpEntry::InterfaceMethodref(mr) = entry {
                    return mr.name == "hasNext";
                }
            }
        }
        false
    })?;

    // Find the "iterator()" call before hasNext
    let iter_pos = insns[..has_next_pos].iter().rposition(|i| {
        if let crate::classfile::instruction::InsnKind::Invoke { index, .. } = &i.kind {
            if let Ok(entry) = pool.get(*index) {
                if let crate::classfile::constant_pool::CpEntry::Methodref(mr)
                    | crate::classfile::constant_pool::CpEntry::InterfaceMethodref(mr) = entry {
                    return mr.name == "iterator";
                }
            }
        }
        false
    })?;

    // Simulate the instructions up to (but not including) the iterator() call
    // to get the collection expression on the stack
    let pre_iter = &insns[..iter_pos];
    let result = simulate_block(pre_iter, pool, vec![], is_static, this_class, names);
    if let Some(top) = result.stack_out.last() {
        let expr_str = kt_render_expr(&top.expr);
        // Clean up: remove "as Iterable" casts that are noise
        let cleaned = expr_str.replace(" as Iterable", "");
        if cleaned != iter_var && !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

/// Detect range for loop: while (i < N) or while (i <= N) patterns
/// Generated by: for (i in start..end) or for (i in start until end)
fn try_detect_range_for(cond: &str, _lvt: &[LvtEntry]) -> Option<RangeForInfo> {
    // Pattern: "varName <= N" (for `in start..end`)
    // or "varName < N" (for `in start until end`)
    // The variable must have been initialized just before the loop

    // Try to match "var >= N" which is the negated form of "var < N"
    // (branch taken when condition is FALSE, i.e., loop continues while var < N)
    if let Some(captures) = parse_range_condition(cond) {
        return Some(captures);
    }
    None
}

/// Parse range-style condition for Kotlin for-in detection.
/// Handles both the correct polarity (from the fixed negated flag) and the
/// legacy inverted forms as a fallback.
///
/// "i < 11"  → for (i in 1..10)
/// "i <= 10" → for (i in 1..10)
/// "i < N"   → for (i in 1 until N) [open upper end]
fn parse_range_condition(cond: &str) -> Option<RangeForInfo> {
    let parts: Vec<&str> = cond.split_whitespace().collect();
    if parts.len() != 3 { return None; }
    let var_name  = parts[0];
    let op        = parts[1];
    let limit_str = parts[2];

    if var_name.contains('.') || var_name.contains('[') {
        return None;
    }

    if let Ok(limit) = limit_str.parse::<i64>() {
        match op {
            // Correct-polarity forms (after the negated-flag fix)
            "<" => {
                // while (i < limit): body runs for i ∈ [start, limit-1] → `1..limit-1`
                return Some(RangeForInfo {
                    var_name:   var_name.to_string(),
                    range_expr: format!("1..{}", limit - 1),
                });
            }
            "<=" => {
                // while (i <= limit): body runs for i ∈ [start, limit] → `1..limit`
                return Some(RangeForInfo {
                    var_name:   var_name.to_string(),
                    range_expr: format!("1..{}", limit),
                });
            }
            // Legacy inverted forms (kept as fallback)
            ">=" => {
                return Some(RangeForInfo {
                    var_name:   var_name.to_string(),
                    range_expr: format!("1..{}", limit - 1),
                });
            }
            ">" => {
                return Some(RangeForInfo {
                    var_name:   var_name.to_string(),
                    range_expr: format!("1..{}", limit),
                });
            }
            _ => {}
        }
    }
    None
}

/// Render switch as Kotlin `when` expression.
fn render_when(
    arena: &StmtArena, s: SwitchStmt,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    let switch_pos = code.instructions.iter()
        .position(|i| matches!(i.opcode, 0xaa | 0xab))
        .unwrap_or(0);
    let sim_insns = &code.instructions[..switch_pos];
    let result = simulate_block(sim_insns, pool, vec![], is_static, this_class, names);
    let raw_expr = result.stack_out.last().map(|s| &s.expr);

    // ── Detect Kotlin enum when: WhenMappings.$EnumSwitchMapping$N[expr.ordinal()]
    if let Some(expr) = raw_expr {
        if let Some((subject, enum_map)) = detect_kotlin_enum_when(expr, cf) {
            w.line(&format!("when ({}) {{", subject));
            w.indent();
            for arm in &s.arms {
                match arm.value {
                    Some(v) => {
                        if let Some(name) = enum_map.get(&v) {
                            w.line(&format!("{} -> {{", name));
                        } else {
                            w.line(&format!("{} -> {{", v));
                        }
                    }
                    None => w.line("else -> {"),
                }
                w.indent();
                render_stmt(arena, arm.body, code, pool, is_static, this_class, names, lvt, cf, w);
                w.dedent();
                w.line("}");
            }
            w.dedent();
            w.line("}");
            return;
        }
    }

    // ── Detect string when: expr.hashCode() switch
    if let Some(expr) = raw_expr {
        if let Some(subject) = detect_string_when(expr) {
            let hash_map = build_string_hash_map(pool);
            w.line(&format!("when ({}) {{", subject));
            w.indent();
            for arm in &s.arms {
                match arm.value {
                    Some(v) => {
                        if let Some(literals) = hash_map.get(&v) {
                            for lit in literals {
                                w.line(&format!("\"{}\" -> {{", lit.replace('"', "\\\"")));
                            }
                        } else {
                            w.line(&format!("{} -> {{", v));
                        }
                    }
                    None => w.line("else -> {"),
                }
                w.indent();
                render_stmt(arena, arm.body, code, pool, is_static, this_class, names, lvt, cf, w);
                w.dedent();
                w.line("}");
            }
            w.dedent();
            w.line("}");
            return;
        }
    }

    // ── Plain when
    let expr_str = raw_expr
        .map(|e| kt_render_expr(e))
        .unwrap_or_else(|| "/* expr */".into());

    w.line(&format!("when ({}) {{", expr_str));
    w.indent();
    for arm in &s.arms {
        match arm.value {
            Some(v) => w.line(&format!("{} -> {{", v)),
            None => w.line("else -> {"),
        }
        w.indent();
        render_stmt(arena, arm.body, code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
        w.line("}");
    }
    w.dedent();
    w.line("}");
}

/// Detect Kotlin enum when pattern:
/// `WhenMappings.$EnumSwitchMapping$N[subject.ordinal()]`
/// Returns (subject_expression_str, case_value → enum_name map)
fn detect_kotlin_enum_when(expr: &Expr, cf: &ClassFile) -> Option<(String, std::collections::HashMap<i32, String>)> {
    if let Expr::ArrayLoad { array, index, .. } = expr {
        // Check array is a $EnumSwitchMapping field
        let is_when_mapping = match array.as_ref() {
            Expr::Field { name, .. } => name.contains("$EnumSwitchMapping"),
            _ => false,
        };
        if !is_when_mapping { return None; }

        // Check index is subject.ordinal()
        if let Expr::Invoke { name, object: Some(obj), .. } = index.as_ref() {
            if name == "ordinal" {
                let subject_str = kt_render_expr(obj);
                // Build enum constant name mapping:
                // case 1 = first enum field, case 2 = second, etc.
                let mut map = std::collections::HashMap::new();
                let mut case_idx = 1i32;
                for f in &cf.fields {
                    if f.is_enum() {
                        map.insert(case_idx, f.name.clone());
                        case_idx += 1;
                    }
                }
                return Some((subject_str, map));
            }
        }
    }
    None
}

/// Detect string when pattern: subject.hashCode()
/// Returns the subject expression string.
fn detect_string_when(expr: &Expr) -> Option<String> {
    if let Expr::Invoke { name, object: Some(obj), kind: InvokeKind::Virtual, .. } = expr {
        if name == "hashCode" {
            return Some(kt_render_expr(obj));
        }
    }
    None
}

/// Java String.hashCode() — for restoring string switch case labels.
fn java_string_hashcode(s: &str) -> i32 {
    let mut h: i32 = 0;
    for c in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as i32);
    }
    h
}

/// Build a hash→literals mapping from all String constants in the pool.
fn build_string_hash_map(pool: &ConstantPool) -> std::collections::HashMap<i32, Vec<String>> {
    use crate::classfile::constant_pool::CpEntry;
    let mut map: std::collections::HashMap<i32, Vec<String>> = std::collections::HashMap::new();
    for entry in pool.entries() {
        if let CpEntry::String(s) = entry {
            let h = java_string_hashcode(s);
            map.entry(h).or_default().push(s.clone());
        }
    }
    map
}

fn render_try_catch(
    arena: &StmtArena, s: TryCatchStmt,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    w.line("try {");
    w.indent();
    render_stmt(arena, s.try_body, code, pool, is_static, this_class, names, lvt, cf, w);
    w.dedent();
    for clause in &s.catches {
        let type_str = clause.catch_type.as_deref()
            .map(|t| format!("e: {}", simple_name(t)))
            .unwrap_or_else(|| "e: Throwable".into());
        w.line(&format!("}} catch ({}) {{", type_str));
        w.indent();
        render_stmt(arena, clause.body, code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
    }
    if let Some(finally) = s.finally_body {
        w.line("} finally {");
        w.indent();
        render_stmt(arena, finally, code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
    }
    w.line("}");
}
