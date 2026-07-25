/// Statement (Stmt) tree pretty-printer → Java source.
use crate::classfile::attribute::{Attribute, CodeAttribute};
use crate::classfile::constant_pool::{CpEntry, ConstantPool};
use crate::classfile::ClassFile;
use crate::codegen::expr_writer::{render_expr, simple_name, IndentWriter};
use crate::ir::expr::{Expr, InvokeKind};
use crate::ir::stack_sim::{simulate_block, SlotInfo};
use crate::ir::stmt::*;
use crate::ir::StmtArena;

/// One entry from the LocalVariableTable.
#[derive(Clone)]
pub struct LvtEntry {
    pub slot:       u16,
    pub name:       String,
    pub descriptor: String,   // field descriptor, e.g. "Lpkg/res/Loader;"
}

/// Extract full LVT entries (slot + name + descriptor) from the CodeAttribute.
pub fn lvt_entries(code: &CodeAttribute) -> Vec<LvtEntry> {
    for attr in &code.attributes {
        if let Attribute::LocalVariableTable(entries) = attr {
            return entries.iter()
                .map(|e| LvtEntry { slot: e.index, name: e.name.clone(), descriptor: e.descriptor.clone() })
                .collect();
        }
    }
    vec![]
}

/// `(slot, name)` pairs — used by the stack simulator.
fn lvt_names(code: &CodeAttribute) -> Vec<(u16, String)> {
    lvt_entries(code).into_iter().map(|e| (e.slot, e.name)).collect()
}

// ── public entry point ─────────────────────────────────────────────────────

/// Render one method body to Java source text.
///
/// `arena`  — the stmt arena produced by Phase 4 recovery.
/// `root`   — the root StmtId.
/// `code`   — the original CodeAttribute (for the instruction list).
/// `pool`   — the class constant pool.
/// `is_static` — whether this is a static method (affects slot-0 handling).
/// `this_class` — binary name of the enclosing class.
pub fn render_method_body(
    arena:      &StmtArena,
    root:       StmtId,
    code:       &CodeAttribute,
    pool:       &ConstantPool,
    is_static:  bool,
    this_class: &str,
    indent:     usize,
    suppress_trailing_return: bool,
    cf:         &ClassFile,
) -> String {
    let mut w = IndentWriter::new(4);
    for _ in 0..indent { w.indent(); }

    let entries = lvt_entries(code);
    let names: Vec<(u16, String)> = entries.iter().map(|e| (e.slot, e.name.clone())).collect();
    render_stmt(arena, root, code, pool, is_static, this_class, &names, &entries, cf, &mut w);
    let mut out = w.finish();

    // Mirror Vineflower's removeRedundantReturns(): strip the trailing "return;"
    // from void/constructor methods — it's always implied by end-of-block.
    if suppress_trailing_return {
        // Find the last "    return;" line and remove it.
        let marker = "return;";
        if let Some(last_pos) = out.rfind(marker) {
            // Only strip if the line contains nothing but whitespace + "return;"
            let line_start = out[..last_pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let prefix = &out[line_start..last_pos];
            if prefix.chars().all(|c| c == ' ') {
                // Remove from line_start to end of that line (including the \n)
                let line_end = out[last_pos..].find('\n').map(|p| last_pos + p + 1)
                    .unwrap_or(out.len());
                out.replace_range(line_start..line_end, "");
            }
        }
    }
    out
}

// ── stmt dispatch ─────────────────────────────────────────────────────────

/// Render a statement, accepting an incoming stack (from a predecessor block)
/// and returning any residual stack at the statement's exit.
///
/// Most statements consume the incoming stack and return an empty one.
/// The exception is `Stmt::Block`: it runs the simulator with `initial_stack`
/// and returns whatever is left on the stack after the block's instructions.
/// This lets `Stmt::Seq` thread stack values between adjacent blocks — which
/// is required for ternary/phi patterns like:
///
/// ```text
/// if (cond) { push A } else { push B }
/// return top_of_stack;
/// ```
fn render_stmt_stacked(
    arena:         &StmtArena,
    id:            StmtId,
    initial_stack: Vec<SlotInfo>,
    code:          &CodeAttribute,
    pool:          &ConstantPool,
    is_static:     bool,
    this_class:    &str,
    names:         &[(u16, String)],
    lvt:           &[LvtEntry],
    cf:            &ClassFile,
    w:             &mut IndentWriter,
) -> Vec<SlotInfo> {
    match arena.get(id) {
        Stmt::Exit => vec![],

        Stmt::Block(b) => {
            let result = simulate_block(&b.instructions, pool, initial_stack, is_static, this_class, names);
            emit_stmts(&result.stmts, lvt, pool, cf, w);
            result.stack_out
        }

        Stmt::Seq(s) => {
            let children = s.children.clone();
            let mut stack = initial_stack;
            let mut i = 0;
            while i < children.len() {
                let child = children[i];

                if let Stmt::If(_) = arena.get(child) {
                    if let Some(next_id) = children.get(i + 1).copied() {
                        // ── Return guard-chain fold ────────────────────────────
                        // `if (A) { if (B) { return X; } }` + `return Y`
                        // → `return A && B ? X : Y`
                        if let Some(else_val) = extract_return_expr(
                            arena, next_id, vec![], pool, is_static, this_class, names)
                        {
                            if let Some((conds, then_val)) =
                                try_extract_guard_chain(arena, child, pool, is_static, this_class, names)
                            {
                                let cond = conds.join(" && ");
                                let ternary = Expr::Ternary {
                                    cond,
                                    then_expr: Box::new(then_val),
                                    else_expr: Box::new(else_val),
                                };
                                emit_stmts(&[Expr::Return(Some(Box::new(ternary)))], lvt, pool, cf, w);
                                i += 2;
                                stack = vec![];
                                continue;
                            }
                        }

                        // ── Value guard-chain fold ─────────────────────────────
                        // `if (A) { [if (B) { ] push_X [}] }` + push_Y
                        // → stack value `A [&& B] ? X : Y` (consumed by next stmt)
                        let else_residual = silent_eval(
                            arena, next_id, vec![], pool, is_static, this_class, names);
                        if else_residual.len() == 1 {
                            if let Some((conds, then_slot)) =
                                try_extract_guard_chain_value(arena, child, pool, is_static, this_class, names)
                            {
                                let cond = conds.join(" && ");
                                let else_slot = else_residual.into_iter().next().unwrap();
                                let ty = then_slot.ty.clone();
                                let ternary = Expr::Ternary {
                                    cond,
                                    then_expr: Box::new(then_slot.expr),
                                    else_expr: Box::new(else_slot.expr),
                                };
                                i += 2;
                                stack = vec![SlotInfo { expr: ternary, ty }];
                                continue;
                            }
                        }
                    }
                }

                // Default: render the child normally
                stack = render_stmt_stacked(
                    arena, child, stack,
                    code, pool, is_static, this_class, names, lvt, cf, w);
                i += 1;
            }
            stack
        }

        Stmt::If(s) => {
            // Compute the stack state *after* the condition instructions and branch
            // have consumed their operands.  Both arms inherit this base stack, so
            // blocks that start with `xreturn` (consuming a value pushed by their
            // predecessor) get the right initial_stack instead of an empty one.
            let cbs = cond_base_stack(
                &s.cond_insns, pool, initial_stack.clone(), is_static, this_class, names);

            // ── Compound-condition return-ternary fold ─────────────────────
            if let Some((conds, then_val, else_val)) =
                try_extract_compound_return(arena, &s, pool, is_static, this_class, names)
            {
                if conds.len() >= 2 {
                    let cond = conds.join(" && ");
                    let ternary = Expr::Ternary {
                        cond,
                        then_expr: Box::new(then_val),
                        else_expr: Box::new(else_val),
                    };
                    emit_stmts(&[Expr::Return(Some(Box::new(ternary)))], lvt, pool, cf, w);
                    return vec![];
                }
            }

            // ── Return-ternary fold ────────────────────────────────────────
            // `if (c) { return A; } else { return B; }` → `return c ? A : B;`
            // Pass cbs so arms that pop a value off the predecessor stack don't
            // produce Opaque{} placeholders.
            if let Some(else_id) = s.else_branch {
                if let (Some(then_ret), Some(else_ret)) = (
                    extract_return_expr(arena, s.then_branch, cbs.clone(), pool, is_static, this_class, names),
                    extract_return_expr(arena, else_id,        cbs.clone(), pool, is_static, this_class, names),
                ) {
                    let cond = condition_from_block_insns(
                        &s.cond_insns, pool, is_static, this_class, s.negated, names);
                    let ternary = Expr::Ternary {
                        cond,
                        then_expr: Box::new(then_ret),
                        else_expr: Box::new(else_ret),
                    };
                    emit_stmts(&[Expr::Return(Some(Box::new(ternary)))], lvt, pool, cf, w);
                    return vec![];
                }
            }

            // ── Value-ternary fold ─────────────────────────────────────────
            // Use vec![] here, NOT cbs: we only care what each arm itself produces.
            // Passing cbs would include pre-condition values (e.g. a `this` pushed
            // before the branch for a putfield) and inflate the residual to len>1,
            // breaking the len==1 guard.  The cbs is applied AFTER the fold fires.
            let then_residual = silent_eval(arena, s.then_branch, vec![], pool, is_static, this_class, names);
            let else_residual = s.else_branch
                .map(|e| silent_eval(arena, e, vec![], pool, is_static, this_class, names))
                .unwrap_or_default();

            // Both arms are pure single-value producers → fold into `c ? a : b`
            // and emit nothing at all.  Rendering the if/else shell here as well
            // would leave an empty `if {} else {}` in front of the consumer.
            if then_residual.len() == 1 && else_residual.len() == 1 {
                let cond = condition_from_block_insns(
                    &s.cond_insns, pool, is_static, this_class, s.negated, names);
                let then_slot = then_residual.into_iter().next().unwrap();
                let else_slot = else_residual.into_iter().next().unwrap();
                // `then_branch` is the fall-through path, which is where control
                // lands when the branch is NOT taken — and `cond` is rendered as
                // the not-taken predicate, so the arms line up directly.
                let ty = if then_slot.ty == crate::types::java_type::JavaType::VOID {
                             else_slot.ty.clone() }
                         else { then_slot.ty.clone() };
                let ternary = Expr::Ternary {
                    cond,
                    then_expr: Box::new(then_slot.expr),
                    else_expr: Box::new(else_slot.expr),
                };
                // cbs was computed above from initial_stack; reuse it.
                let mut base = cbs;
                base.push(SlotInfo { expr: ternary, ty });
                return base;
            }

            render_if(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);

            // Return the then-branch residual if both branches agree they produce a value.
            if !then_residual.is_empty() && !else_residual.is_empty() {
                let mut base = cbs;
                base.extend(then_residual);
                base
            } else {
                vec![]
            }
        }

        Stmt::Loop(s) => {
            render_loop(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
            vec![]
        }

        Stmt::Switch(s) => {
            render_switch(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
            vec![]
        }

        Stmt::TryCatch(s) => {
            render_try_catch(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w)
        }

        Stmt::Synchronized(s) => {
            w.line("synchronized (/* monitor */) {");
            w.indent();
            let body = s.body;
            render_stmt_stacked(arena, body, vec![], code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
            vec![]
        }
    }
}

/// Convenience wrapper that discards the residual stack.
fn render_stmt(
    arena:      &StmtArena,
    id:         StmtId,
    code:       &CodeAttribute,
    pool:       &ConstantPool,
    is_static:  bool,
    this_class: &str,
    names:      &[(u16, String)],
    lvt:        &[LvtEntry],
    cf:         &ClassFile,
    w:          &mut IndentWriter,
) {
    render_stmt_stacked(arena, id, vec![], code, pool, is_static, this_class, names, lvt, cf, w);
}

/// Silently simulate a statement's effect on the stack WITHOUT emitting any
/// source text.  Used for ternary/phi detection: if both branches of an `if`
/// produce a non-empty residual stack (and write nothing visible), that value
/// is threaded to the next sibling in the `Seq`.
fn silent_eval(
    arena:         &StmtArena,
    id:            StmtId,
    initial_stack: Vec<SlotInfo>,
    pool:          &ConstantPool,
    is_static:     bool,
    this_class:    &str,
    names:         &[(u16, String)],
) -> Vec<SlotInfo> {
    match arena.get(id) {
        Stmt::Exit => vec![],
        Stmt::Block(b) => {
            let result = simulate_block(&b.instructions, pool, initial_stack, is_static, this_class, names);
            // Only propagate if the block produced no visible statements
            // (i.e. it only pushed a value — no assignments, calls, returns).
            if result.stmts.is_empty() {
                result.stack_out
            } else {
                vec![]
            }
        }
        Stmt::Seq(s) => {
            let mut stack = vec![];
            for &child in &s.children {
                let child_res = silent_eval(arena, child, vec![], pool, is_static, this_class, names);
                if !child_res.is_empty() { stack = child_res; } else { stack = vec![]; }
            }
            stack
        }
        Stmt::If(s) => {
            // Recurse so nested ternaries like `a > b ? a : (b > 0 ? b : 0)`
            // propagate a value through the outer else-arm's If as well.
            let then_r = silent_eval(arena, s.then_branch, vec![], pool, is_static, this_class, names);
            let else_r = s.else_branch
                .map(|e| silent_eval(arena, e, vec![], pool, is_static, this_class, names))
                .unwrap_or_default();
            if then_r.len() == 1 && else_r.len() == 1 {
                let cond = condition_from_block_insns(
                    &s.cond_insns, pool, is_static, this_class, s.negated, names);
                let ty = then_r[0].ty.clone();
                let ternary = crate::ir::expr::Expr::Ternary {
                    cond,
                    then_expr: Box::new(then_r.into_iter().next().unwrap().expr),
                    else_expr: Box::new(else_r.into_iter().next().unwrap().expr),
                };
                vec![SlotInfo { expr: ternary, ty }]
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

// ── statement emission with type-declaration upgrade ──────────────────────

/// Track which slots have already had their type declared.
/// Emit statements, promoting the first assignment of each LVT local from
/// `var = rhs` to `Type var = rhs`.
fn emit_stmts(
    stmts: &[Expr],
    lvt:   &[LvtEntry],
    pool:  &ConstantPool,
    cf:    &ClassFile,
    w:     &mut IndentWriter,
) {
    // Build a set of slots that still need a declaration.
    // We use a simple Vec<bool> keyed by position in lvt.
    let mut declared = std::collections::HashSet::<u16>::new();

    for expr in stmts {
        // Bare `return;` is emitted normally — callers that want to suppress it
        // (e.g. the last statement of a void method) can do so above.
        let line = if let Expr::Assign { lhs, rhs } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                let slot = lv.slot;
                // A boolean local assigned from iconst_0/1 reads false/true.
                let is_bool_slot = lvt.iter()
                    .find(|e| e.slot == slot)
                    .map(|e| e.descriptor == "Z")
                    .unwrap_or(false);
                let render_rhs = |r: &Expr| -> String {
                    if is_bool_slot {
                        if let Expr::Const(c) = r {
                            if let crate::ir::expr::ConstValue::Int(i) = c.value {
                                return if i != 0 { "true".into() } else { "false".into() };
                            }
                        }
                    }
                    render_expr_concat(r, pool, cf)
                };
                if !declared.contains(&slot) {
                    // Look up the LVT entry for this slot.
                    if let Some(entry) = lvt.iter().find(|e| e.slot == slot) {
                        // Derive the type string from the descriptor.
                        let ty_str = type_str_from_descriptor(&entry.descriptor);
                        declared.insert(slot);
                        // Desugar invokedynamic concat in rhs.
                        let rhs_str = render_rhs(rhs);
                        format!("{} {} = {};", ty_str, entry.name, rhs_str)
                    } else {
                        let rhs_str = render_rhs(rhs);
                        format!("{} = {};", render_expr(lhs), rhs_str)
                    }
                } else {
                    let rhs_str = render_rhs(rhs);
                    format!("{} = {};", render_expr(lhs), rhs_str)
                }
            } else {
                format!("{};", render_expr_concat(expr, pool, cf))
            }
        } else {
            format!("{};", render_expr_concat(expr, pool, cf))
        };
        w.line(&line);
    }
}

/// Convert a field descriptor to a short Java type name (uses simple class name).
fn type_str_from_descriptor(desc: &str) -> String {
    use crate::types::descriptor::parse_field_descriptor;
    parse_field_descriptor(desc)
        .map(|(t, _)| {
            // Use simple name for object types to avoid full-qualified noise.
            // Arrays keep their dimension brackets.
            let full = t.to_string();
            // Convert java/lang/String → String, pkg/res/Loader → Loader
            // The JavaType Display already uses dots; strip down to last component.
            if full.contains('.') && !full.starts_with('[') {
                // Extract the last dot-separated segment (but preserve generics).
                if let Some(bracket) = full.find('<') {
                    let base = &full[..bracket];
                    let generics = &full[bracket..];
                    let simple = base.rsplit('.').next().unwrap_or(base);
                    format!("{}{}", simple, generics)
                } else {
                    full.rsplit('.').next().unwrap_or(&full).to_string()
                }
            } else {
                full
            }
        })
        .unwrap_or_else(|_| "var".into())
}

/// Render an expression, desugaring invokedynamic `makeConcatWithConstants`
/// into a Java `+` string concatenation chain using the BSM recipe string.
fn render_expr_concat(expr: &Expr, pool: &ConstantPool, cf: &ClassFile) -> String {
    use crate::classfile::attribute::Attribute;
    use crate::classfile::constant_pool::CpEntry;

    if let Expr::InvokeDynamic { name, args, bootstrap_index, .. } = expr {
        if name == "makeConcatWithConstants" || name == "makeConcat" {
            // Look up the recipe string from BootstrapMethods.
            // Recipe chars: '\u{0001}' = next dynamic arg slot, '\u{0002}' = BSM static constant.
            let recipe: Option<String> = cf.attributes.iter().find_map(|a| {
                if let Attribute::BootstrapMethods(bsm_list) = a {
                    if let Some(bsm) = bsm_list.get(*bootstrap_index as usize) {
                        if let Some(&str_idx) = bsm.arguments.first() {
                            if let Ok(CpEntry::String(s)) = pool.get(str_idx) {
                                return Some(s.clone());
                            }
                        }
                    }
                }
                None
            });

            if let Some(recipe) = recipe {
                let mut result  = String::new();
                let mut literal = String::new();
                let mut arg_it  = args.iter();

                for ch in recipe.chars() {
                    match ch {
                        '\u{0001}' => {
                            // Flush accumulated literal, then the next dynamic arg.
                            if !literal.is_empty() {
                                if !result.is_empty() { result.push_str(" + "); }
                                result.push('"');
                                result.push_str(&literal
                                    .replace('\\', "\\\\")
                                    .replace('"',  "\\\"")
                                    .replace('\n', "\\n")
                                    .replace('\r', "\\r")
                                    .replace('\t', "\\t"));
                                result.push('"');
                                literal.clear();
                            }
                            if let Some(arg) = arg_it.next() {
                                if !result.is_empty() { result.push_str(" + "); }
                                result.push_str(&strip_string_valueof(arg));
                            }
                        }
                        '\u{0002}' => {
                            // Static BSM constant — uncommon; fall back to plain join.
                            return args.iter().map(strip_string_valueof)
                                       .collect::<Vec<_>>().join(" + ");
                        }
                        c => literal.push(c),
                    }
                }
                // Flush any trailing literal.
                if !literal.is_empty() {
                    if !result.is_empty() { result.push_str(" + "); }
                    result.push('"');
                    result.push_str(&literal
                        .replace('\\', "\\\\")
                        .replace('"',  "\\\"")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t"));
                    result.push('"');
                }
                if !result.is_empty() { return result; }
            }

            // Fallback: no recipe / empty — chain args with +.
            if !args.is_empty() {
                return args.iter().map(strip_string_valueof)
                           .collect::<Vec<_>>().join(" + ");
            }
        }
    }
    render_expr(expr)
}

/// Strip `String.valueOf(x)` → render `x`, for concat clarity.
fn strip_string_valueof(expr: &Expr) -> String {
    if let Expr::Invoke { kind: crate::ir::expr::InvokeKind::Static, owner, name, args, .. } = expr {
        if name == "valueOf" && owner == "java/lang/String" && args.len() == 1 {
            return render_expr(&args[0]);
        }
    }
    render_expr(expr)
}

// ── if/else ────────────────────────────────────────────────────────────────

/// Returns true when rendering `id` would produce no visible output lines.
/// Handles the common cases: Exit, an empty Block (zero instructions that
/// produce no statements), and a Seq/If whose every child is empty.
/// Walk a no-else if-chain and extract the compound condition + innermost return value.
///
/// Handles: `if (A) { if (B) { if (C) { return X; } } }` → `(["A","B","C"], X)`.
/// Only follows then-branches with no else clause.  Stops when the then-branch
/// is a plain return block.
fn try_extract_guard_chain(
    arena:      &StmtArena,
    id:         StmtId,
    pool:       &ConstantPool,
    is_static:  bool,
    this_class: &str,
    names:      &[(u16, String)],
) -> Option<(Vec<String>, Expr)> {
    let mut conds: Vec<String> = Vec::new();
    let mut cur = id;

    loop {
        // Unwrap a single-element Seq to its If child.
        let effective = match arena.get(cur) {
            Stmt::Seq(sq) => {
                let non_empty: Vec<_> = sq.children.iter().copied()
                    .filter(|&c| !is_stmt_empty(arena, c, pool, is_static, this_class, names))
                    .collect();
                if non_empty.len() == 1 { non_empty[0] } else { cur }
            }
            _ => cur,
        };

        match arena.get(effective) {
            Stmt::If(s) => {
                // Only follow no-else chains.
                if s.else_branch.is_some() { return None; }
                let cond = condition_from_block_insns(
                    &s.cond_insns, pool, is_static, this_class, s.negated, names);
                conds.push(cond);
                cur = s.then_branch;
            }
            _ => {
                // Reached the leaf: must be a plain return.
                let val = extract_return_expr(arena, effective, vec![], pool, is_static, this_class, names)?;
                if conds.is_empty() { return None; }
                return Some((conds, val));
            }
        }
    }
}

/// Like `try_extract_guard_chain` but the leaf is a *value-producing* block
/// (one that pushes a single value onto the stack) instead of a return block.
///
/// Used for patterns like:
/// ```text
/// if (A) { [if (B) { ] push_X [}] }
/// push_Y               // else path
/// istore / method call // consumer
/// ```
/// → stack value `A [&& B] ? X : Y`
fn try_extract_guard_chain_value(
    arena:      &StmtArena,
    id:         StmtId,
    pool:       &ConstantPool,
    is_static:  bool,
    this_class: &str,
    names:      &[(u16, String)],
) -> Option<(Vec<String>, SlotInfo)> {
    let mut conds: Vec<String> = Vec::new();
    let mut cur = id;

    loop {
        let effective = match arena.get(cur) {
            Stmt::Seq(sq) => {
                let non_empty: Vec<_> = sq.children.iter().copied()
                    .filter(|&c| !is_stmt_empty(arena, c, pool, is_static, this_class, names))
                    .collect();
                if non_empty.len() == 1 { non_empty[0] } else { cur }
            }
            _ => cur,
        };

        match arena.get(effective) {
            Stmt::If(s) if s.else_branch.is_none() => {
                let cond = condition_from_block_insns(
                    &s.cond_insns, pool, is_static, this_class, s.negated, names);
                conds.push(cond);
                cur = s.then_branch;
            }
            _ => {
                // Leaf: must be a pure value-producing block (no stmts, 1 stack value).
                let vals = silent_eval(arena, effective, vec![], pool, is_static, this_class, names);
                if vals.len() != 1 { return None; }
                if conds.is_empty() { return None; }
                return Some((conds, vals.into_iter().next().unwrap()));
            }
        }
    }
}

/// Detect a short-circuit `&&` chain compiled as nested if-statements:
///
/// ```text
/// if (A) {
///     if (B) {
///         if (C) { return X; } else { return Y; }
///     } else { return Y; }
/// } else { return Y; }
/// ```
/// → `return A && B && C ? X : Y;`
///
/// Returns `(condition_parts, then_val, else_val)` if the pattern matches,
/// where each `condition_parts[i]` is a single rendered condition clause.
fn try_extract_compound_return(
    arena:      &StmtArena,
    s:          &IfStmt,
    pool:       &ConstantPool,
    is_static:  bool,
    this_class: &str,
    names:      &[(u16, String)],
) -> Option<(Vec<String>, Expr, Expr)> {
    // Else branch must return a value.
    let else_id = s.else_branch?;
    let else_val = extract_return_expr(arena, else_id, vec![], pool, is_static, this_class, names)?;

    // Collect the chain: then-branch is either a direct return (innermost) or
    // another Stmt::If with the same else value.
    let cond0 = condition_from_block_insns(
        &s.cond_insns, pool, is_static, this_class, s.negated, names);
    let mut conds = vec![cond0];

    let mut then_id = s.then_branch;
    loop {
        // Unwrap a single-child Seq to get the nested If, if present.
        let effective = match arena.get(then_id) {
            Stmt::Seq(sq) => {
                let non_empty: Vec<_> = sq.children.iter().copied()
                    .filter(|&c| !is_stmt_empty(arena, c, pool, is_static, this_class, names))
                    .collect();
                if non_empty.len() == 1 { non_empty[0] } else { then_id }
            }
            _ => then_id,
        };

        match arena.get(effective) {
            Stmt::If(inner) => {
                // The inner else must return the SAME value as outer else.
                let inner_else_id = inner.else_branch?;
                let inner_else_val = extract_return_expr(
                    arena, inner_else_id, vec![], pool, is_static, this_class, names)?;
                // Compare by rendered string — simple but sufficient for identical
                // `return original;` patterns.
                if render_expr(&inner_else_val) != render_expr(&else_val) {
                    return None;
                }
                let cond_i = condition_from_block_insns(
                    &inner.cond_insns, pool, is_static, this_class, inner.negated, names);
                conds.push(cond_i);
                then_id = inner.then_branch;
            }
            _ => {
                // Innermost: must be a direct return of the then-value.
                let then_val = extract_return_expr(
                    arena, effective, vec![], pool, is_static, this_class, names)?;
                // Must be different from else_val (otherwise it's a degenerate tautology).
                if render_expr(&then_val) == render_expr(&else_val) {
                    return None;
                }
                return Some((conds, then_val, else_val));
            }
        }
    }
}

/// If `id` is a block whose only effect is `return <expr>`, return that expr.
/// Used to fold `if (c) { return A; } else { return B; }` → `return c ? A : B`.
/// Replace `Const(Int(0))` / `Const(Int(1))` with proper boolean literals when
/// the enclosing method is boolean-returning.
fn coerce_bool_const(expr: Expr) -> Expr {
    use crate::ir::expr::{ConstExpr, ConstValue};
    use crate::types::java_type::JavaType;
    if let Expr::Const(ref c) = expr {
        if let ConstValue::Int(i) = c.value {
            return Expr::Const(ConstExpr {
                value: ConstValue::Int(i),
                ty:    JavaType::BOOLEAN,
            });
        }
    }
    expr
}

fn extract_return_expr(
    arena:         &StmtArena,
    id:            StmtId,
    initial_stack: Vec<SlotInfo>,
    pool:          &ConstantPool,
    is_static:     bool,
    this_class:    &str,
    names:         &[(u16, String)],
) -> Option<Expr> {
    match arena.get(id) {
        Stmt::Block(b) => {
            let result = simulate_block(&b.instructions, pool, initial_stack, is_static, this_class, names);
            if result.stmts.len() == 1 {
                if let Expr::Return(Some(val)) = &result.stmts[0] {
                    // Reject placeholder opaques: they mean the block relied on a
                    // value from its predecessor's stack (pop_expr on empty → Opaque).
                    if matches!(val.as_ref(), Expr::Opaque { .. }) {
                        return None;
                    }
                    let mut expr = *val.clone();
                    // In a boolean-returning method, iconst_0/1 means false/true.
                    if crate::ir::stack_sim::current_return_is_boolean() {
                        expr = coerce_bool_const(expr);
                    }
                    return Some(expr);
                }
            }
            None
        }
        Stmt::Seq(s) => {
            // A Seq is eligible if it contains exactly one child that is
            // a return-only Block (the rest are empty).
            let mut ret_expr = None;
            for &child in &s.children {
                match extract_return_expr(arena, child, vec![], pool, is_static, this_class, names) {
                    Some(e) if ret_expr.is_none() => ret_expr = Some(e),
                    None if is_stmt_empty(arena, child, pool, is_static, this_class, names) => {}
                    _ => return None, // multiple returns or non-empty non-return content
                }
            }
            ret_expr
        }
        _ => None,
    }
}

fn is_stmt_empty(
    arena: &StmtArena,
    id:    StmtId,
    pool:  &ConstantPool,
    is_static: bool,
    this_class: &str,
    names: &[(u16, String)],
) -> bool {
    match arena.get(id) {
        Stmt::Exit => true,
        Stmt::Block(b) => {
            // A block with no instructions produces no visible output.
            if b.instructions.is_empty() { return true; }
            // A block consisting solely of unconditional jumps (goto/goto_w)
            // produces no source output — the branch is structural, not textual.
            use crate::classfile::opcodes::opc;
            if b.instructions.iter().all(|i| matches!(i.opcode, opc::goto | opc::goto_w)) {
                return true;
            }
            // A block that only pushes values and has no side-effecting
            // statements also produces no visible output.
            let result = simulate_block(&b.instructions, pool, vec![], is_static, this_class, names);
            result.stmts.is_empty()
        }
        Stmt::Seq(s) => s.children.iter().all(|&c| is_stmt_empty(arena, c, pool, is_static, this_class, names)),
        Stmt::If(s) => {
            let then_empty = is_stmt_empty(arena, s.then_branch, pool, is_static, this_class, names);
            let else_empty = s.else_branch
                .map(|e| is_stmt_empty(arena, e, pool, is_static, this_class, names))
                .unwrap_or(true);
            then_empty && else_empty
        }
        _ => false,
    }
}

fn render_if(
    arena: &StmtArena, s: IfStmt,
    _code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    let cond_str = condition_from_block_insns(&s.cond_insns, pool, is_static, this_class, s.negated, names);

    // Normalise: `if (c) { } else { body }` → `if (!c) { body }`
    // Uses is_stmt_empty so structurally-non-Exit but semantically-empty blocks
    // (e.g. a Block with zero instructions, or a nested If whose every arm is empty)
    // are also treated as empty.
    let then_is_empty = is_stmt_empty(arena, s.then_branch, pool, is_static, this_class, names);
    if then_is_empty {
        if let Some(else_id) = s.else_branch {
            let else_also_empty = is_stmt_empty(arena, else_id, pool, is_static, this_class, names);
            if else_also_empty { return; }  // both empty → emit nothing
            let neg_cond = condition_from_block_insns(
                &s.cond_insns, pool, is_static, this_class, !s.negated, names);
            w.line(&format!("if ({}) {{", neg_cond));
            w.indent();
            render_stmt(arena, else_id, _code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
            return;
        }
        // then empty, no else — nothing to emit at all.
        return;
    }

    if let Some(else_id) = s.else_branch {
        // Suppress a trailing empty else clause too.
        let else_is_empty = is_stmt_empty(arena, else_id, pool, is_static, this_class, names);
        if else_is_empty {
            w.line(&format!("if ({}) {{", cond_str));
            w.indent();
            render_stmt(arena, s.then_branch, _code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
        } else {
            w.line(&format!("if ({}) {{", cond_str));
            w.indent();
            render_stmt(arena, s.then_branch, _code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("} else {");
            w.indent();
            render_stmt(arena, else_id, _code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
        }
    } else {
        w.line(&format!("if ({}) {{", cond_str));
        w.indent();
        render_stmt(arena, s.then_branch, _code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
        w.line("}");
    }
}

/// Simulate the condition block instructions up to (but not including) the
/// branch, return the stack as it would be *after* the branch operands are
/// consumed.  Used to recover the phi base when a ternary is embedded inside
/// a larger expression (e.g. `arr[i] = (cond ? a : b)`).
fn cond_base_stack(
    block_insns: &[crate::classfile::instruction::Instruction],
    pool: &ConstantPool,
    initial_stack: Vec<crate::ir::stack_sim::SlotInfo>,
    is_static: bool,
    this_class: &str,
    names: &[(u16, String)],
) -> Vec<crate::ir::stack_sim::SlotInfo> {
    use crate::classfile::opcodes::opc;
    let branch_idx = block_insns.iter().rposition(|i| {
        matches!(i.opcode,
            opc::ifeq | opc::ifne | opc::iflt | opc::ifge | opc::ifgt | opc::ifle |
            opc::if_icmpeq | opc::if_icmpne | opc::if_icmplt |
            opc::if_icmpge | opc::if_icmpgt | opc::if_icmple |
            opc::if_acmpeq | opc::if_acmpne | opc::ifnull | opc::ifnonnull
        )
    });
    let Some(branch_idx) = branch_idx else { return initial_stack; };
    let branch_op = block_insns[branch_idx].opcode;
    let sim_insns = &block_insns[..branch_idx];
    let mut out = simulate_block(sim_insns, pool, initial_stack, is_static, this_class, names).stack_out;
    // The branch instruction pops 1 operand (ifeq/ifne/… and ifnull/ifnonnull)
    // or 2 operands (if_icmp* and if_acmp*).
    let pops: usize = match branch_op {
        opc::if_icmpeq | opc::if_icmpne | opc::if_icmplt
        | opc::if_icmpge | opc::if_icmpgt | opc::if_icmple
        | opc::if_acmpeq | opc::if_acmpne => 2,
        _ => 1,
    };
    for _ in 0..pops { out.pop(); }
    out
}

/// Build a condition string from the instructions of a single basic block.
///
/// Finds the last conditional branch in `block_insns`, simulates everything
/// before it, and formats the comparison expression.  Because we only look at
/// the instructions belonging to the condition block (stored in `IfStmt /
/// LoopStmt`), this is always accurate regardless of method size.
fn condition_from_block_insns(
    block_insns: &[crate::classfile::instruction::Instruction],
    pool: &ConstantPool,
    is_static: bool,
    this_class: &str,
    negated: bool,
    names: &[(u16, String)],
) -> String {
    use crate::classfile::opcodes::opc;

    // Find the branch instruction inside this block.
    let branch_idx = block_insns.iter().rposition(|i| {
        matches!(i.opcode,
            opc::ifeq | opc::ifne | opc::iflt | opc::ifge | opc::ifgt | opc::ifle |
            opc::if_icmpeq | opc::if_icmpne | opc::if_icmplt |
            opc::if_icmpge | opc::if_icmpgt | opc::if_icmple |
            opc::if_acmpeq | opc::if_acmpne | opc::ifnull | opc::ifnonnull
        )
    });

    let Some(branch_idx) = branch_idx else {
        return "/* no branch */".into();
    };

    let branch_op = block_insns[branch_idx].opcode;
    // Simulate only the instructions before the branch inside this block.
    let sim_insns = &block_insns[..branch_idx];
    let result = simulate_block(sim_insns, pool, vec![], is_static, this_class, names);

    build_condition(branch_op, &result.stack_out, negated)
}

fn build_condition(
    branch_op: u8,
    stack: &[crate::ir::stack_sim::SlotInfo],
    negated: bool,
) -> String {
    use crate::classfile::opcodes::opc;
    use crate::types::java_type::JavaType;
    use crate::ir::expr::{BinOp, Expr};

    let top_slot = stack.last();
    let top_is_bool = top_slot.map(|s| s.ty == JavaType::BOOLEAN).unwrap_or(false);

    // ── lcmp / fcmpl / fcmpg / dcmpl / dcmpg ────────────────────────────────
    // These push a 3-way int (-1/0/1) that is immediately consumed by ifeq/ifne/iflt/…
    // Instead of rendering "a /*lcmp*/ b == 0", synthesize the correct operator.
    //
    // lcmp:  cmp(a,b) → then the branch opcode decides the relation
    // fcmpg/dcmpg: NaN → +1 (so iflt/ifle on the result treats NaN as "not less")
    // fcmpl/dcmpl: NaN → -1 (so ifgt/ifge on the result treats NaN as "not greater")
    // For readability we ignore NaN semantics and emit the obvious operator.
    if let Some(top_slot) = top_slot {
        if let Expr::BinOp(cmp_op, lhs, rhs) = &top_slot.expr {
            let is_cmp = matches!(cmp_op,
                BinOp::LCmp | BinOp::FCmpL | BinOp::FCmpG | BinOp::DCmpL | BinOp::DCmpG
            );
            if is_cmp {
                let lhs_s = render_expr(lhs);
                let rhs_s = render_expr(rhs);
                // The branch opcode is relative to the 3-way result:
                //   ifeq  → == 0 → lhs == rhs
                //   ifne  → != 0 → lhs != rhs
                //   iflt  → < 0  → lhs <  rhs
                //   ifle  → <= 0 → lhs <= rhs
                //   ifgt  → > 0  → lhs >  rhs
                //   ifge  → >= 0 → lhs >= rhs
                let op_str = match branch_op {
                    opc::ifeq => if negated { "!=" } else { "==" },
                    opc::ifne => if negated { "==" } else { "!=" },
                    opc::iflt => if negated { ">=" } else { "<"  },
                    opc::ifle => if negated { ">"  } else { "<=" },
                    opc::ifgt => if negated { "<=" } else { ">"  },
                    opc::ifge => if negated { "<"  } else { ">=" },
                    _         => if negated { "!=" } else { "==" },
                };
                return format!("{} {} {}", lhs_s, op_str, rhs_s);
            }
        }
    }

    let top  = top_slot.map(|s| render_expr(&s.expr)).unwrap_or_else(|| "/*?*/".into());
    let sec  = if stack.len() >= 2 {
        render_expr(&stack[stack.len()-2].expr)
    } else { "0".into() };

    // For boolean-typed values, ifeq/ifne can collapse to direct/negated form.
    if top_is_bool {
        match branch_op {
            opc::ifeq => return if negated { top } else { format!("!{}", top) },
            opc::ifne => return if negated { format!("!{}", top) } else { top },
            _ => {}
        }
    }

    let (lhs, rhs, op_str) = match branch_op {
        opc::ifeq     => (top, "0".into(),   if negated { "!=" } else { "==" }),
        opc::ifne     => (top, "0".into(),   if negated { "==" } else { "!=" }),
        opc::iflt     => (top, "0".into(),   if negated { ">=" } else { "<"  }),
        opc::ifge     => (top, "0".into(),   if negated { "<"  } else { ">=" }),
        opc::ifgt     => (top, "0".into(),   if negated { "<=" } else { ">"  }),
        opc::ifle     => (top, "0".into(),   if negated { ">"  } else { "<=" }),
        opc::if_icmpeq => (sec, top,         if negated { "!=" } else { "==" }),
        opc::if_icmpne => (sec, top,         if negated { "==" } else { "!=" }),
        opc::if_icmplt => (sec, top,         if negated { ">=" } else { "<"  }),
        opc::if_icmpge => (sec, top,         if negated { "<"  } else { ">=" }),
        opc::if_icmpgt => (sec, top,         if negated { "<=" } else { ">"  }),
        opc::if_icmple => (sec, top,         if negated { ">"  } else { "<=" }),
        opc::if_acmpeq => (sec, top,         if negated { "!=" } else { "==" }),
        opc::if_acmpne => (sec, top,         if negated { "==" } else { "!=" }),
        opc::ifnull    => (top, "null".into(),if negated { "!=" } else { "==" }),
        opc::ifnonnull => (top, "null".into(),if negated { "==" } else { "!=" }),
        _              => (top, "0".into(),  "=="),
    };
    format!("{} {} {}", lhs, op_str, rhs)
}

// ── loops ──────────────────────────────────────────────────────────────────

fn render_loop(
    arena: &StmtArena, s: LoopStmt,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    match s.kind {
        LoopKind::While => {
            let cond = condition_from_block_insns(
                &s.cond_insns, pool, is_static, this_class, s.cond_negated, names);
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
            let cond = condition_from_block_insns(
                &s.cond_insns, pool, is_static, this_class, s.cond_negated, names);
            w.line(&format!("}} while ({});", cond));
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

// ── String switch literal restoration ─────────────────────────────────────

/// Java String.hashCode() — identical algorithm used by javac for switch cases.
fn java_string_hashcode(s: &str) -> i32 {
    let mut h: i32 = 0;
    for c in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as i32);
    }
    h
}

/// Build a hash→literals mapping from all String constants in the pool.
fn build_string_hash_map(pool: &ConstantPool) -> std::collections::HashMap<i32, Vec<String>> {
    let mut map: std::collections::HashMap<i32, Vec<String>> = std::collections::HashMap::new();
    for entry in pool.entries() {
        if let CpEntry::String(s) = entry {
            let h = java_string_hashcode(s);
            map.entry(h).or_default().push(s.clone());
        }
    }
    map
}

// ── Enum switch case-name restoration ─────────────────────────────────────

/// Build a 1-based ordinal→enum constant name mapping from the class's enum fields.
/// In javac, enum fields are declared in source order; ordinal 0 = first field.
/// The $SWITCH_TABLE$ uses 1-based indices (ordinal+1).
fn build_enum_ordinal_map(cf: &ClassFile) -> std::collections::HashMap<i32, String> {
    let mut map = std::collections::HashMap::new();
    let mut ordinal = 1i32; // switch table uses 1-based
    for f in &cf.fields {
        if f.is_enum() {
            map.insert(ordinal, f.name.clone());
            ordinal += 1;
        }
    }
    map
}

/// Detect the enum switch pattern and return (enum_var_expr, enum_ordinal_map).
fn detect_enum_switch(expr: &Expr, cf: &ClassFile) -> Option<(String, std::collections::HashMap<i32, String>)> {
    if let Expr::ArrayLoad { array, index, .. } = expr {
        let is_table = match array.as_ref() {
            Expr::Invoke { name, .. } => name.starts_with("$SWITCH_TABLE$"),
            _ => false,
        };
        if !is_table { return None; }
        match index.as_ref() {
            Expr::Invoke { name, object: Some(obj), .. } if name == "ordinal" => {
                let enum_var = render_expr(obj);
                let ordinal_map = build_enum_ordinal_map(cf);
                Some((enum_var, ordinal_map))
            }
            _ => None,
        }
    } else {
        None
    }
}

fn render_switch(
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

    // ── Enum switch: replace case N → case CONSTANT_NAME ─────────────
    if let Some(raw) = raw_expr {
        if let Some((enum_var, ordinal_map)) = detect_enum_switch(raw, cf) {
            w.line(&format!("switch ({}) {{", enum_var));
            for arm in &s.arms {
                match arm.value {
                    Some(v) => {
                        if let Some(name) = ordinal_map.get(&v) {
                            w.line(&format!("case {}:", name));
                        } else {
                            w.line(&format!("case {}:", v));
                        }
                    }
                    None => w.line("default:"),
                }
                w.indent();
                render_stmt(arena, arm.body, code, pool, is_static, this_class, names, lvt, cf, w);
                if arm.breaks { w.line("break;"); }
                w.dedent();
            }
            w.line("}");
            return;
        }

        // ── String switch: replace case HASH → case "literal" ─────────
        if let Expr::Invoke { name, object: Some(obj), kind: InvokeKind::Virtual, .. } = raw {
            if name == "hashCode" {
                let str_var = render_expr(obj);
                let hash_map = build_string_hash_map(pool);
                w.line(&format!("switch ({}) {{", str_var));
                for arm in &s.arms {
                    match arm.value {
                        Some(v) => {
                            if let Some(literals) = hash_map.get(&v) {
                                for lit in literals {
                                    w.line(&format!("case \"{}\":", lit.replace('"', "\\\"")));
                                }
                            } else {
                                w.line(&format!("case {}:", v));
                            }
                        }
                        None => w.line("default:"),
                    }
                    w.indent();
                    render_stmt(arena, arm.body, code, pool, is_static, this_class, names, lvt, cf, w);
                    if arm.breaks { w.line("break;"); }
                    w.dedent();
                }
                w.line("}");
                return;
            }
        }
    }

    // ── Plain switch ──────────────────────────────────────────────────
    let expr_str = raw_expr
        .map(|e| render_expr(e))
        .unwrap_or_else(|| "/*switch_expr*/".into());

    w.line(&format!("switch ({}) {{", expr_str));
    for arm in &s.arms {
        match arm.value {
            Some(v) => w.line(&format!("case {}:", v)),
            None    => w.line("default:"),
        }
        w.indent();
        render_stmt(arena, arm.body, code, pool, is_static, this_class, names, lvt, cf, w);
        if arm.breaks { w.line("break;"); }
        w.dedent();
    }
    w.line("}");
}

// ── try/catch ─────────────────────────────────────────────────────────────

fn render_try_catch(
    arena: &StmtArena, s: TryCatchStmt,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) -> Vec<SlotInfo> {
    w.line("try {");
    w.indent();
    // Thread the try body's residual stack outward so that a block immediately
    // after the try/catch that only does `xreturn` (consuming a value pushed
    // inside the protected region) gets the correct initial_stack instead of
    // producing Opaque{}.
    let try_residual = render_stmt_stacked(
        arena, s.try_body, vec![], code, pool, is_static, this_class, names, lvt, cf, w);
    w.dedent();
    for clause in &s.catches {
        let ty  = clause.catch_type.as_deref().unwrap_or("java/lang/Throwable");
        let var = catch_var_name(arena, clause.body, lvt).unwrap_or_else(|| "e".to_string());
        w.line(&format!("}} catch ({} {}) {{", simple_name(ty), var));
        w.indent();
        // On entry to a handler the JVM clears the operand stack and pushes the
        // thrown exception.  Seed that so the leading `astore` renders as the
        // catch parameter binding rather than reading from an empty stack.
        render_stmt_with_exception(arena, clause.body, ty, &var,
                                   code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
    }
    if let Some(finally) = s.finally_body {
        w.line("} finally {");
        w.indent();
        render_stmt_with_exception(arena, finally, "java/lang/Throwable", "e",
                                   code, pool, is_static, this_class, names, lvt, cf, w);
        w.dedent();
    }
    w.line("}");
    // Return the try body's residual stack so the Seq can thread it to any
    // sibling block that consumes a value pushed inside the protected region.
    try_residual
}

/// Find the name the handler stores the exception into, from the LVT entry that
/// matches the handler's leading `astore` slot.  Falls back to `None`.
fn catch_var_name(arena: &StmtArena, body: StmtId, lvt: &[LvtEntry]) -> Option<String> {
    use crate::classfile::opcodes::opc;
    // Walk to the first Block in the handler body.
    let mut id = body;
    loop {
        match arena.get(id) {
            Stmt::Block(b) => {
                let first = b.instructions.first()?;
                if !matches!(first.opcode,
                    opc::astore | opc::astore_0 | opc::astore_1 | opc::astore_2 | opc::astore_3)
                { return None; }
                let slot = match first.kind {
                    crate::classfile::instruction::InsnKind::LocalVar { index } => index,
                    _ => match first.opcode {
                        opc::astore_0 => 0, opc::astore_1 => 1,
                        opc::astore_2 => 2, opc::astore_3 => 3,
                        _ => return None,
                    },
                };
                return lvt.iter().find(|e| e.slot == slot).map(|e| e.name.clone());
            }
            Stmt::Seq(s) => { id = *s.children.first()?; }
            _ => return None,
        }
    }
}

/// Render a handler body with the thrown exception pre-seeded on the operand
/// stack, and suppress the redundant `<var> = <exception>` assignment that the
/// leading `astore` would otherwise produce (the binding is already expressed
/// by the `catch (T var)` clause).
#[allow(clippy::too_many_arguments)]
fn render_stmt_with_exception(
    arena: &StmtArena, body: StmtId, exc_type: &str, var: &str,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    use crate::ir::stack_sim::SlotInfo;
    use crate::ir::expr::{Expr, LocalVarExpr};
    use crate::types::java_type::JavaType;

    // Seed the stack with a LocalVar naming the catch parameter, so the
    // handler's `astore <slot>` produces `var = var`, which we then drop.
    let seed = vec![SlotInfo {
        expr: Expr::LocalVar(LocalVarExpr {
            slot: u16::MAX, ty: JavaType::object(exc_type), name: Some(var.to_string()),
        }),
        ty: JavaType::object(exc_type),
    }];
    let before = w.len();
    render_stmt_stacked(arena, body, seed, code, pool, is_static, this_class, names, lvt, cf, w);
    // Drop a leading self-assignment line like `IOException e = e;` / `e = e;`.
    w.drop_line_if(before, |line| {
        let t = line.trim();
        t == format!("{} = {};", var, var)
            || (t.ends_with(&format!("{} = {};", var, var)) && t.contains(' '))
    });
}
