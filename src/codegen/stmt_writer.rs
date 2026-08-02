/// Statement (Stmt) tree pretty-printer → Java source.
use crate::classfile::attribute::{Attribute, CodeAttribute};
use crate::classfile::constant_pool::{ConstantPool, CpEntry};
use crate::classfile::ClassFile;
use crate::codegen::expr_writer::{render_expr, simple_name, IndentWriter};
use crate::codegen::render_context::RenderContext;
use crate::ir::expr::{Expr, InvokeKind};
use crate::ir::stack_sim::SlotInfo;
use crate::ir::stmt::*;
use crate::ir::StmtArena;

/// One entry from the LocalVariableTable.
#[derive(Clone)]
pub struct LvtEntry {
    pub slot: u16,
    pub name: String,
    pub descriptor: String, // field descriptor, e.g. "Lpkg/res/Loader;"
    pub start_pc: u16,
    pub length: u16,
}

/// Extract full LVT entries (slot + name + descriptor) from the CodeAttribute.
pub fn lvt_entries(code: &CodeAttribute) -> Vec<LvtEntry> {
    for attr in &code.attributes {
        if let Attribute::LocalVariableTable(entries) = attr {
            return entries
                .iter()
                .map(|e| LvtEntry {
                    slot: e.index,
                    name: e.name.clone(),
                    descriptor: e.descriptor.clone(),
                    start_pc: e.start_pc,
                    length: e.length,
                })
                .collect();
        }
    }
    vec![]
}

// ── public entry point ─────────────────────────────────────────────────────

/// Render one method body to Java source text.
///
/// `arena`  — the stmt arena produced by Phase 4 recovery.
/// `root`   — the root StmtId.
/// `context` — explicit method, class, local-variable and bootstrap state.
pub fn render_method_body(
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

    render_stmt(arena, root, context, &mut w);
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
                let line_end = out[last_pos..]
                    .find('\n')
                    .map(|p| last_pos + p + 1)
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
    arena: &StmtArena,
    id: StmtId,
    initial_stack: Vec<SlotInfo>,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) -> Vec<SlotInfo> {
    match arena.get(id) {
        Stmt::Exit => vec![],

        Stmt::Block(b) => {
            let result = context.simulate(&b.instructions, initial_stack);
            emit_stmts(&result.stmts, context, w);
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
                        if let Some(else_val) = extract_return_expr(arena, next_id, vec![], context)
                        {
                            if let Some((conds, then_val)) =
                                try_extract_guard_chain(arena, child, context)
                            {
                                let cond = conds.join(" && ");
                                let ternary = Expr::Ternary {
                                    cond: cond.into(),
                                    then_expr: Box::new(then_val),
                                    else_expr: Box::new(else_val),
                                };
                                // Emit any side-effecting stmts from the outermost If's
                                // cond block (e.g. `module = Instance;`) before the ternary.
                                if let Stmt::If(s) = arena.get(child) {
                                    let pre = cond_pre_stmts(&s.cond_insns, context);
                                    if !pre.is_empty() {
                                        emit_stmts(&pre, context, w);
                                    }
                                }
                                emit_stmts(&[Expr::Return(Some(Box::new(ternary)))], context, w);
                                i += 2;
                                stack = vec![];
                                continue;
                            }
                        }

                        // ── No-else outer + full inner-if fold ────────────────
                        // `if (A) { if (B) { return X; } else { return Y; } }` + `return Y`
                        // The inner then-branch returns Y (same as the continuation);
                        // the inner else-branch returns X (the interesting value).
                        // → `return A && !B_cond ? X : Y`  (negate inner to get B's positive)
                        // Handles javac's `A && B ? X : Y` compiled as two nested branches.
                        if let Stmt::If(outer_s) = arena.get(child) {
                            if outer_s.else_branch.is_none() {
                                // Unwrap a single-child Seq if needed.
                                let inner_id = {
                                    let t = outer_s.then_branch;
                                    match arena.get(t) {
                                        Stmt::Seq(sq) => {
                                            let ne: Vec<_> = sq
                                                .children
                                                .iter()
                                                .copied()
                                                .filter(|&c| !is_stmt_empty(arena, c, context))
                                                .collect();
                                            if ne.len() == 1 {
                                                ne[0]
                                            } else {
                                                t
                                            }
                                        }
                                        _ => t,
                                    }
                                };
                                if let Stmt::If(inner_s) = arena.get(inner_id) {
                                    if let Some(inner_else_id) = inner_s.else_branch {
                                        if let Some(cont_val) =
                                            extract_return_expr(arena, next_id, vec![], context)
                                        {
                                            // The inner then-branch either explicitly returns the
                                            // same value as the continuation, or is Stmt::Exit
                                            // (falls through to the continuation implicitly).
                                            let inner_then_matches = match extract_return_expr(
                                                arena,
                                                inner_s.then_branch,
                                                vec![],
                                                context,
                                            ) {
                                                Some(v) => {
                                                    render_expr(&v) == render_expr(&cont_val)
                                                }
                                                None => is_stmt_empty(
                                                    arena,
                                                    inner_s.then_branch,
                                                    context,
                                                ),
                                            };
                                            if inner_then_matches {
                                                if let Some(inner_else_val) = extract_return_expr(
                                                    arena,
                                                    inner_else_id,
                                                    vec![],
                                                    context,
                                                ) {
                                                    if render_expr(&inner_else_val)
                                                        != render_expr(&cont_val)
                                                    {
                                                        // Emit cond pre-stmts for both outer and inner.
                                                        let pre_o = cond_pre_stmts(
                                                            &outer_s.cond_insns,
                                                            context,
                                                        );
                                                        if !pre_o.is_empty() {
                                                            emit_stmts(&pre_o, context, w);
                                                        }
                                                        let pre_i = cond_pre_stmts(
                                                            &inner_s.cond_insns,
                                                            context,
                                                        );
                                                        if !pre_i.is_empty() {
                                                            emit_stmts(&pre_i, context, w);
                                                        }
                                                        // Build compound `A && B` condition.
                                                        let outer_cond = condition_from_block_insns(
                                                            &outer_s.cond_insns,
                                                            context,
                                                            outer_s.negated,
                                                        );
                                                        // Negate inner to get the "interesting" (else) path.
                                                        let inner_cond = condition_from_block_insns(
                                                            &inner_s.cond_insns,
                                                            context,
                                                            !inner_s.negated,
                                                        );
                                                        let compound = format!(
                                                            "{} && {}",
                                                            outer_cond, inner_cond
                                                        );
                                                        let ternary = Expr::Ternary {
                                                            cond: compound.into(),
                                                            then_expr: Box::new(inner_else_val),
                                                            else_expr: Box::new(cont_val),
                                                        };
                                                        emit_stmts(
                                                            &[Expr::Return(Some(Box::new(
                                                                ternary,
                                                            )))],
                                                            context,
                                                            w,
                                                        );
                                                        i += 2;
                                                        stack = vec![];
                                                        continue;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // `if (A) { [if (B) { ] push_X [}] }` + push_Y
                        // → stack value `A [&& B] ? X : Y` (consumed by next stmt)
                        let else_residual = silent_eval(arena, next_id, vec![], context);
                        if else_residual.len() == 1 {
                            if let Some((conds, then_slot)) =
                                try_extract_guard_chain_value(arena, child, context)
                            {
                                let cond = conds.join(" && ");
                                let else_slot = else_residual.into_iter().next().unwrap();
                                let merged = merge_slots(cond, then_slot, else_slot);
                                // Prepend any pre-condition values pushed by the outer If.
                                let mut new_stack = if let Stmt::If(s) = arena.get(child) {
                                    // Emit side-effecting stmts from the cond block first.
                                    let pre = cond_pre_stmts(&s.cond_insns, context);
                                    if !pre.is_empty() {
                                        emit_stmts(&pre, context, w);
                                    }
                                    cond_base_stack(&s.cond_insns, stack.clone(), context)
                                } else {
                                    stack.clone()
                                };
                                new_stack.push(merged);
                                i += 2;
                                stack = new_stack;
                                continue;
                            }
                        }
                    }
                }

                // Default: render the child normally
                stack = render_stmt_stacked(arena, child, stack, context, w);
                i += 1;
            }
            stack
        }

        Stmt::If(s) => {
            // Compute the stack state *after* the condition instructions and branch
            // have consumed their operands.  Both arms inherit this base stack, so
            // blocks that start with `xreturn` (consuming a value pushed by their
            // predecessor) get the right initial_stack instead of an empty one.
            let cbs = cond_base_stack(&s.cond_insns, initial_stack.clone(), context);

            // ── Compound-condition return-ternary fold ─────────────────────
            if let Some((conds, then_val, else_val)) =
                try_extract_compound_return(arena, s, context)
            {
                if conds.len() >= 2 {
                    let cond = conds.join(" && ");
                    let ternary = Expr::Ternary {
                        cond: cond.into(),
                        then_expr: Box::new(then_val),
                        else_expr: Box::new(else_val),
                    };
                    emit_stmts(&[Expr::Return(Some(Box::new(ternary)))], context, w);
                    return vec![];
                }
            }

            // ── Return-ternary fold ────────────────────────────────────────
            // `if (c) { return A; } else { return B; }` → `return c ? A : B;`
            // Pass cbs so arms that pop a value off the predecessor stack don't
            // produce Opaque{} placeholders.
            if let Some(else_id) = s.else_branch {
                if let (Some(then_ret), Some(else_ret)) = (
                    extract_return_expr(arena, s.then_branch, cbs.clone(), context),
                    extract_return_expr(arena, else_id, cbs.clone(), context),
                ) {
                    let cond = condition_from_block_insns(&s.cond_insns, context, s.negated);
                    let ternary = Expr::Ternary {
                        cond: cond.into(),
                        then_expr: Box::new(then_ret),
                        else_expr: Box::new(else_ret),
                    };
                    emit_stmts(&[Expr::Return(Some(Box::new(ternary)))], context, w);
                    return vec![];
                }
            }

            // ── Value-ternary / stack-phi fold ──────────────────────────────
            // Evaluate both arms with the complete stack left below the branch
            // operands. This preserves receivers and earlier arguments while
            // merging only slots whose values differ between the two paths.
            let then_residual = silent_eval(arena, s.then_branch, cbs.clone(), context);
            let else_residual = s
                .else_branch
                .map(|e| silent_eval(arena, e, cbs.clone(), context))
                .unwrap_or_else(|| cbs.clone());

            if then_residual.len() == else_residual.len() && !then_residual.is_empty() {
                let cond = condition_from_block_insns(&s.cond_insns, context, s.negated);
                return then_residual
                    .into_iter()
                    .zip(else_residual)
                    .map(|(then_slot, else_slot)| merge_slots(cond.clone(), then_slot, else_slot))
                    .collect();
            }

            // ── Compound guard-chain + outer-else value fold ──────────────────
            // Pattern: `if (A) { if (B) { push X } } else { push Y }`
            // where the inner If has no else (recovery assigned the shared else-block
            // only to the outer If).  then_residual is [] but the then_branch itself
            // is a no-else guard chain whose leaf pushes a single value.
            // → fold to `A && B ? X : Y`, push result onto cbs.
            if then_residual.is_empty() && else_residual.len() == 1 && s.else_branch.is_some() {
                if let Some((inner_conds, then_slot)) =
                    try_extract_guard_chain_value(arena, s.then_branch, context)
                {
                    let outer_cond = condition_from_block_insns(&s.cond_insns, context, s.negated);
                    let mut all_conds = vec![outer_cond];
                    all_conds.extend(inner_conds);
                    let compound_cond = all_conds.join(" && ");
                    let else_slot = else_residual.into_iter().next().unwrap();
                    let merged = merge_slots(compound_cond, then_slot, else_slot);
                    let mut base = cbs;
                    base.push(merged);
                    return base;
                }
            }

            render_if(arena, s.clone(), context, w);

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
            render_loop(arena, s.clone(), context, w);
            vec![]
        }

        Stmt::BreakIf(s) => {
            let branch_idx = s
                .cond_insns
                .iter()
                .rposition(|instruction| {
                    use crate::classfile::opcodes::opc;
                    matches!(
                        instruction.opcode,
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
                            | opc::if_acmpeq
                            | opc::if_acmpne
                            | opc::ifnull
                            | opc::ifnonnull
                    )
                })
                .unwrap_or(s.cond_insns.len());
            let entry = context.block_entry(s.cond_block);
            let result = context.simulate_state(&s.cond_insns[..branch_idx], &entry);
            emit_stmts(&result.stmts, context, w);
            let branch_opcode = s
                .cond_insns
                .get(branch_idx)
                .map(|instruction| instruction.opcode)
                .unwrap_or(crate::classfile::opcodes::opc::ifeq);
            let condition = build_condition(branch_opcode, &result.stack_out, s.negated);
            w.line(&format!("if ({condition}) {{"));
            w.indent();
            w.line("break;");
            w.dedent();
            w.line("}");
            vec![]
        }

        Stmt::Switch(s) => {
            if let Some(stack) = eval_switch_value(arena, s, initial_stack.clone(), context) {
                return stack;
            }
            render_switch(arena, s.clone(), initial_stack, context, w);
            vec![]
        }

        Stmt::TryCatch(s) => render_try_catch(arena, s.clone(), context, w),

        Stmt::Synchronized(s) => {
            w.line("synchronized (/* monitor */) {");
            w.indent();
            let body = s.body;
            render_stmt_stacked(arena, body, vec![], context, w);
            w.dedent();
            w.line("}");
            vec![]
        }
    }
}

/// Convenience wrapper that discards the residual stack.
fn render_stmt(arena: &StmtArena, id: StmtId, context: &RenderContext<'_>, w: &mut IndentWriter) {
    render_stmt_stacked(arena, id, vec![], context, w);
}

/// Silently simulate a statement's effect on the stack WITHOUT emitting any
/// source text.  Used for ternary/phi detection: if both branches of an `if`
/// produce a non-empty residual stack (and write nothing visible), that value
/// is threaded to the next sibling in the `Seq`.
fn silent_eval(
    arena: &StmtArena,
    id: StmtId,
    initial_stack: Vec<SlotInfo>,
    context: &RenderContext<'_>,
) -> Vec<SlotInfo> {
    match arena.get(id) {
        Stmt::Exit => vec![],
        Stmt::Block(b) => {
            let result = context.simulate(&b.instructions, initial_stack);
            // Only propagate if the block produced no visible statements
            // (i.e. it only pushed a value — no assignments, calls, returns).
            if result.stmts.is_empty() {
                result.stack_out
            } else {
                vec![]
            }
        }
        Stmt::Seq(s) => {
            let mut stack = initial_stack;
            for &child in &s.children {
                stack = silent_eval(arena, child, stack, context);
            }
            stack
        }
        Stmt::If(s) => {
            let base = cond_base_stack(&s.cond_insns, initial_stack, context);
            let then_r = silent_eval(arena, s.then_branch, base.clone(), context);
            let else_r = s
                .else_branch
                .map(|e| silent_eval(arena, e, base.clone(), context))
                .unwrap_or_else(|| base.clone());
            if then_r.len() == else_r.len() && !then_r.is_empty() {
                let cond = condition_from_block_insns(&s.cond_insns, context, s.negated);
                then_r
                    .into_iter()
                    .zip(else_r)
                    .map(|(then_slot, else_slot)| merge_slots(cond.clone(), then_slot, else_slot))
                    .collect()
            } else if then_r.is_empty() && else_r.len() == 1 && s.else_branch.is_some() {
                // Compound guard-chain + outer-else:
                // `if (A) { if (B) { push X } } else { push Y }` → A && B ? X : Y
                if let Some((inner_conds, then_slot)) =
                    try_extract_guard_chain_value(arena, s.then_branch, context)
                {
                    let outer_cond = condition_from_block_insns(&s.cond_insns, context, s.negated);
                    let mut all_conds = vec![outer_cond];
                    all_conds.extend(inner_conds);
                    let compound_cond = all_conds.join(" && ");
                    let else_slot = else_r.into_iter().next().unwrap();
                    let ty = then_slot.ty.clone();
                    let ternary = crate::ir::expr::Expr::Ternary {
                        cond: compound_cond.into(),
                        then_expr: Box::new(then_slot.expr),
                        else_expr: Box::new(else_slot.expr),
                    };
                    vec![SlotInfo { expr: ternary, ty }]
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        }
        Stmt::Switch(s) => eval_switch_value(arena, s, initial_stack, context).unwrap_or_default(),
        Stmt::BreakIf(_) => vec![],
        _ => vec![],
    }
}

fn switch_stack_state(
    switch: &SwitchStmt,
    initial_stack: Vec<SlotInfo>,
    context: &RenderContext<'_>,
) -> Option<(SlotInfo, Vec<SlotInfo>)> {
    let switch_pos = switch
        .switch_insns
        .iter()
        .rposition(|instruction| matches!(instruction.opcode, 0xaa | 0xab))?;
    let mut stack = context
        .simulate(&switch.switch_insns[..switch_pos], initial_stack)
        .stack_out;
    let selector = stack.pop()?;
    Some((selector, stack))
}

fn eval_switch_value(
    arena: &StmtArena,
    switch: &SwitchStmt,
    initial_stack: Vec<SlotInfo>,
    context: &RenderContext<'_>,
) -> Option<Vec<SlotInfo>> {
    let (selector, base) = switch_stack_state(switch, initial_stack, context)?;
    if !switch.arms.iter().any(|arm| arm.value.is_none()) {
        return None;
    }

    let mut values = Vec::with_capacity(switch.arms.len());
    for arm in &switch.arms {
        let mut result = silent_eval(arena, arm.body, base.clone(), context);
        if result.len() != base.len() + 1 {
            return None;
        }
        for (actual, expected) in result.iter().zip(&base) {
            if render_expr(&actual.expr) != render_expr(&expected.expr) {
                return None;
            }
        }
        values.push((arm.value, result.pop().unwrap()));
    }

    let all_boolean = values.iter_mut().all(|(_, slot)| is_boolean_value(slot));
    let ty = if all_boolean {
        crate::types::java_type::JavaType::BOOLEAN
    } else {
        values.first()?.1.ty.clone()
    };
    let arms = values
        .into_iter()
        .map(|(value, slot)| (value, slot.expr))
        .collect();
    let mut result = base;
    result.push(SlotInfo {
        expr: Expr::SwitchExpression {
            selector: Box::new(selector.expr),
            arms,
        },
        ty,
    });
    Some(result)
}

fn is_boolean_value(slot: &mut SlotInfo) -> bool {
    if slot.ty == crate::types::java_type::JavaType::BOOLEAN {
        return true;
    }
    if let Expr::Const(constant) = &mut slot.expr {
        if matches!(constant.value, crate::ir::expr::ConstValue::Int(0 | 1)) {
            constant.ty = crate::types::java_type::JavaType::BOOLEAN;
            slot.ty = crate::types::java_type::JavaType::BOOLEAN;
            return true;
        }
    }
    false
}

fn merge_slots(cond: String, mut then_slot: SlotInfo, mut else_slot: SlotInfo) -> SlotInfo {
    if render_expr(&then_slot.expr) == render_expr(&else_slot.expr) {
        return then_slot;
    }
    let boolean_phi = is_boolean_value(&mut then_slot) && is_boolean_value(&mut else_slot);
    let ty = if boolean_phi {
        crate::types::java_type::JavaType::BOOLEAN
    } else if then_slot.ty == crate::types::java_type::JavaType::VOID {
        else_slot.ty.clone()
    } else {
        then_slot.ty.clone()
    };
    SlotInfo {
        expr: Expr::Ternary {
            cond: cond.into(),
            then_expr: Box::new(then_slot.expr),
            else_expr: Box::new(else_slot.expr),
        },
        ty,
    }
}

// ── statement emission with type-declaration upgrade ──────────────────────

/// Track which slots have already had their type declared.
/// Emit statements, promoting the first assignment of each LVT local from
/// `var = rhs` to `Type var = rhs`.
fn emit_stmts(stmts: &[Expr], context: &RenderContext<'_>, w: &mut IndentWriter) {
    let lvt = context.lvt.as_slice();
    let pool = context.pool;
    let cf = context.class;

    for expr in stmts {
        // Bare `return;` is emitted normally — callers that want to suppress it
        // (e.g. the last statement of a void method) can do so above.
        let line = if let Expr::Assign { lhs, rhs } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                let slot = lv.slot;
                // A boolean local assigned from iconst_0/1 reads false/true.
                let is_bool_slot = lvt
                    .iter()
                    .find(|e| e.slot == slot)
                    .map(|e| e.descriptor == "Z")
                    .unwrap_or(false);
                let render_rhs = |r: &Expr| -> String {
                    if is_bool_slot {
                        if let Expr::Const(c) = r {
                            if let crate::ir::expr::ConstValue::Int(i) = c.value {
                                return if i != 0 {
                                    "true".into()
                                } else {
                                    "false".into()
                                };
                            }
                        }
                    }
                    render_expr_concat(r, pool, cf)
                };
                if !context.declared_slots.borrow().contains(&slot) {
                    // Look up the LVT entry for this slot.
                    if let Some(entry) = lvt.iter().find(|e| e.slot == slot) {
                        // Derive the type string from the descriptor.
                        let ty_str = type_str_from_descriptor(&entry.descriptor);
                        context.declared_slots.borrow_mut().insert(slot);
                        // Desugar invokedynamic concat in rhs.
                        let rhs_str = render_rhs(rhs);
                        format!("{} {} = {};", ty_str, entry.name, rhs_str)
                    } else {
                        let rhs_str = render_rhs(rhs);
                        context.declared_slots.borrow_mut().insert(slot);
                        format!("{} {} = {};", lv.ty, render_expr(lhs), rhs_str)
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

    if let Expr::InvokeDynamic {
        name,
        args,
        bootstrap_index,
        ..
    } = expr
    {
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
                let mut result = String::new();
                let mut literal = String::new();
                let mut arg_it = args.iter();

                for ch in recipe.chars() {
                    match ch {
                        '\u{0001}' => {
                            // Flush accumulated literal, then the next dynamic arg.
                            if !literal.is_empty() {
                                if !result.is_empty() {
                                    result.push_str(" + ");
                                }
                                result.push('"');
                                result.push_str(
                                    &literal
                                        .replace('\\', "\\\\")
                                        .replace('"', "\\\"")
                                        .replace('\n', "\\n")
                                        .replace('\r', "\\r")
                                        .replace('\t', "\\t"),
                                );
                                result.push('"');
                                literal.clear();
                            }
                            if let Some(arg) = arg_it.next() {
                                if !result.is_empty() {
                                    result.push_str(" + ");
                                }
                                result.push_str(&strip_string_valueof(arg));
                            }
                        }
                        '\u{0002}' => {
                            // Static BSM constant — uncommon; fall back to plain join.
                            return args
                                .iter()
                                .map(strip_string_valueof)
                                .collect::<Vec<_>>()
                                .join(" + ");
                        }
                        c => literal.push(c),
                    }
                }
                // Flush any trailing literal.
                if !literal.is_empty() {
                    if !result.is_empty() {
                        result.push_str(" + ");
                    }
                    result.push('"');
                    result.push_str(
                        &literal
                            .replace('\\', "\\\\")
                            .replace('"', "\\\"")
                            .replace('\n', "\\n")
                            .replace('\r', "\\r")
                            .replace('\t', "\\t"),
                    );
                    result.push('"');
                }
                if !result.is_empty() {
                    return result;
                }
            }

            // Fallback: no recipe / empty — chain args with +.
            if !args.is_empty() {
                return args
                    .iter()
                    .map(strip_string_valueof)
                    .collect::<Vec<_>>()
                    .join(" + ");
            }
        }
    }
    render_expr(expr)
}

/// Strip `String.valueOf(x)` → render `x`, for concat clarity.
fn strip_string_valueof(expr: &Expr) -> String {
    if let Expr::Invoke {
        kind: crate::ir::expr::InvokeKind::Static,
        owner,
        name,
        args,
        ..
    } = expr
    {
        if name == "valueOf" && owner == "java/lang/String" && args.len() == 1 {
            return render_expr(&args[0]);
        }
    }
    render_expr(expr)
}

mod control_flow;
use control_flow::*;
