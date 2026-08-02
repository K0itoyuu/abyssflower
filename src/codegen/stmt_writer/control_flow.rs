use super::*;

// ── if/else ────────────────────────────────────────────────────────────────

/// Returns true when rendering `id` would produce no visible output lines.
/// Handles the common cases: Exit, an empty Block (zero instructions that
/// produce no statements), and a Seq/If whose every child is empty.
/// Walk a no-else if-chain and extract the compound condition + innermost return value.
///
/// Handles: `if (A) { if (B) { if (C) { return X; } } }` → `(["A","B","C"], X)`.
/// Only follows then-branches with no else clause.  Stops when the then-branch
/// is a plain return block.
pub(super) fn try_extract_guard_chain(
    arena: &StmtArena,
    id: StmtId,
    context: &RenderContext<'_>,
) -> Option<(Vec<String>, Expr)> {
    let mut conds: Vec<String> = Vec::new();
    let mut cur = id;

    loop {
        // Unwrap a single-element Seq to its If child.
        let effective = match arena.get(cur) {
            Stmt::Seq(sq) => {
                let non_empty: Vec<_> = sq
                    .children
                    .iter()
                    .copied()
                    .filter(|&c| !is_stmt_empty(arena, c, context))
                    .collect();
                if non_empty.len() == 1 {
                    non_empty[0]
                } else {
                    cur
                }
            }
            _ => cur,
        };

        match arena.get(effective) {
            Stmt::If(s) => {
                // Special leaf case: If(cond, then: Exit, else: return X)
                // Recovery places the "interesting" value in the else branch when the
                // then-branch falls through to the continuation.  Negate the condition
                // to get the positive predicate and extract the else value.
                if let Some(else_id) = s.else_branch {
                    if is_stmt_empty(arena, s.then_branch, context) {
                        let val = extract_return_expr(arena, else_id, vec![], context)?;
                        // Negate: condition_from_block_insns with !negated gives the TAKEN path.
                        let cond = condition_from_block_insns(&s.cond_insns, context, !s.negated);
                        conds.push(cond);
                        if conds.is_empty() {
                            return None;
                        }
                        return Some((conds, val));
                    }
                    return None; // else with non-empty then → can't fold
                }
                let cond = condition_from_block_insns(&s.cond_insns, context, s.negated);
                conds.push(cond);
                cur = s.then_branch;
            }
            _ => {
                // Reached the leaf: must be a plain return.
                let val = extract_return_expr(arena, effective, vec![], context)?;
                if conds.is_empty() {
                    return None;
                }
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
pub(super) fn try_extract_guard_chain_value(
    arena: &StmtArena,
    id: StmtId,
    context: &RenderContext<'_>,
) -> Option<(Vec<String>, SlotInfo)> {
    let mut conds: Vec<String> = Vec::new();
    let mut cur = id;

    loop {
        let effective = match arena.get(cur) {
            Stmt::Seq(sq) => {
                let non_empty: Vec<_> = sq
                    .children
                    .iter()
                    .copied()
                    .filter(|&c| !is_stmt_empty(arena, c, context))
                    .collect();
                if non_empty.len() == 1 {
                    non_empty[0]
                } else {
                    cur
                }
            }
            _ => cur,
        };

        match arena.get(effective) {
            Stmt::If(s) if s.else_branch.is_none() => {
                let cond = condition_from_block_insns(&s.cond_insns, context, s.negated);
                conds.push(cond);
                cur = s.then_branch;
            }
            _ => {
                // Leaf: must be a pure value-producing block (no stmts, 1 stack value).
                let vals = silent_eval(arena, effective, vec![], context);
                if vals.len() != 1 {
                    return None;
                }
                if conds.is_empty() {
                    return None;
                }
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
pub(super) fn try_extract_compound_return(
    arena: &StmtArena,
    s: &IfStmt,
    context: &RenderContext<'_>,
) -> Option<(Vec<String>, Expr, Expr)> {
    // Else branch must return a value.
    let else_id = s.else_branch?;
    let else_val = extract_return_expr(arena, else_id, vec![], context)?;

    // Collect the chain: then-branch is either a direct return (innermost) or
    // another Stmt::If with the same else value.
    let cond0 = condition_from_block_insns(&s.cond_insns, context, s.negated);
    let mut conds = vec![cond0];

    let mut then_id = s.then_branch;
    loop {
        // Unwrap a single-child Seq to get the nested If, if present.
        let effective = match arena.get(then_id) {
            Stmt::Seq(sq) => {
                let non_empty: Vec<_> = sq
                    .children
                    .iter()
                    .copied()
                    .filter(|&c| !is_stmt_empty(arena, c, context))
                    .collect();
                if non_empty.len() == 1 {
                    non_empty[0]
                } else {
                    then_id
                }
            }
            _ => then_id,
        };

        match arena.get(effective) {
            Stmt::If(inner) => {
                // The inner else must return the SAME value as outer else.
                let inner_else_id = inner.else_branch?;
                let inner_else_val = extract_return_expr(arena, inner_else_id, vec![], context)?;
                // Compare by rendered string — simple but sufficient for identical
                // `return original;` patterns.
                if render_expr(&inner_else_val) != render_expr(&else_val) {
                    return None;
                }
                let cond_i = condition_from_block_insns(&inner.cond_insns, context, inner.negated);
                conds.push(cond_i);
                then_id = inner.then_branch;
            }
            _ => {
                // Innermost: must be a direct return of the then-value.
                let then_val = extract_return_expr(arena, effective, vec![], context)?;
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
pub(super) fn extract_return_expr(
    arena: &StmtArena,
    id: StmtId,
    initial_stack: Vec<SlotInfo>,
    context: &RenderContext<'_>,
) -> Option<Expr> {
    match arena.get(id) {
        Stmt::Block(b) => {
            let result = context.simulate(&b.instructions, initial_stack);
            if result.stmts.len() == 1 {
                if let Expr::Return(Some(val)) = &result.stmts[0] {
                    // Reject placeholder opaques: they mean the block relied on a
                    // value from its predecessor's stack (pop_expr on empty → Opaque).
                    if matches!(val.as_ref(), Expr::Opaque { .. }) {
                        return None;
                    }
                    return Some(*val.clone());
                }
            }
            None
        }
        Stmt::Seq(s) => {
            // A Seq is eligible if it contains exactly one child that is
            // a return-only Block (the rest are empty).
            let mut ret_expr = None;
            for &child in &s.children {
                match extract_return_expr(arena, child, vec![], context) {
                    Some(e) if ret_expr.is_none() => ret_expr = Some(e),
                    None if is_stmt_empty(arena, child, context) => {}
                    _ => return None, // multiple returns or non-empty non-return content
                }
            }
            ret_expr
        }
        _ => None,
    }
}

pub(super) fn is_stmt_empty(arena: &StmtArena, id: StmtId, context: &RenderContext<'_>) -> bool {
    match arena.get(id) {
        Stmt::Exit => true,
        Stmt::Block(b) => {
            // A block with no instructions produces no visible output.
            if b.instructions.is_empty() {
                return true;
            }
            // A block consisting solely of unconditional jumps (goto/goto_w)
            // produces no source output — the branch is structural, not textual.
            use crate::classfile::opcodes::opc;
            if b.instructions
                .iter()
                .all(|i| matches!(i.opcode, opc::goto | opc::goto_w))
            {
                return true;
            }
            // A block that only pushes values and has no side-effecting
            // statements also produces no visible output.
            let result = context.simulate(&b.instructions, vec![]);
            result.stmts.is_empty()
        }
        Stmt::Seq(s) => s.children.iter().all(|&c| is_stmt_empty(arena, c, context)),
        Stmt::If(s) => {
            let then_empty = is_stmt_empty(arena, s.then_branch, context);
            let else_empty = s
                .else_branch
                .map(|e| is_stmt_empty(arena, e, context))
                .unwrap_or(true);
            then_empty && else_empty
        }
        _ => false,
    }
}

/// Simulate the instructions in a condition block BEFORE the branch and return
/// any side-effecting statements (e.g. `module = ModuleXRay.INSTANCE;`).
///
/// These must be emitted as regular Java statements before the `if (cond) {`
/// line; they are normally invisible because `condition_from_block_insns` only
/// uses the operand stack and throws the emitted stmts away.
pub(super) fn cond_pre_stmts(
    cond_insns: &[crate::classfile::instruction::Instruction],
    context: &RenderContext<'_>,
) -> Vec<Expr> {
    use crate::classfile::opcodes::opc;
    let branch_idx = cond_insns.iter().rposition(|i| {
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
                | opc::if_acmpeq
                | opc::if_acmpne
                | opc::ifnull
                | opc::ifnonnull
        )
    });
    let Some(branch_idx) = branch_idx else {
        return vec![];
    };
    let result = context.simulate(&cond_insns[..branch_idx], vec![]);
    result.stmts
}

pub(super) fn render_if(
    arena: &StmtArena,
    s: IfStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let cond_str = condition_from_block_insns(&s.cond_insns, context, s.negated);

    // Normalise: `if (c) { } else { body }` → `if (!c) { body }`
    // Uses is_stmt_empty so structurally-non-Exit but semantically-empty blocks
    // (e.g. a Block with zero instructions, or a nested If whose every arm is empty)
    // are also treated as empty.
    let then_is_empty = is_stmt_empty(arena, s.then_branch, context);
    if then_is_empty {
        if let Some(else_id) = s.else_branch {
            let else_also_empty = is_stmt_empty(arena, else_id, context);
            if else_also_empty {
                return;
            } // both empty → emit nothing
            let neg_cond = condition_from_block_insns(&s.cond_insns, context, !s.negated);
            w.line(&format!("if ({}) {{", neg_cond));
            w.indent();
            render_stmt(arena, else_id, context, w);
            w.dedent();
            w.line("}");
            return;
        }
        // then empty, no else — nothing to emit at all.
        return;
    }

    if let Some(else_id) = s.else_branch {
        // Suppress a trailing empty else clause too.
        let else_is_empty = is_stmt_empty(arena, else_id, context);
        if else_is_empty {
            w.line(&format!("if ({}) {{", cond_str));
            w.indent();
            render_stmt(arena, s.then_branch, context, w);
            w.dedent();
            w.line("}");
        } else {
            w.line(&format!("if ({}) {{", cond_str));
            w.indent();
            render_stmt(arena, s.then_branch, context, w);
            w.dedent();
            w.line("} else {");
            w.indent();
            render_stmt(arena, else_id, context, w);
            w.dedent();
            w.line("}");
        }
    } else {
        w.line(&format!("if ({}) {{", cond_str));
        w.indent();
        render_stmt(arena, s.then_branch, context, w);
        w.dedent();
        w.line("}");
    }
}

/// Simulate the condition block instructions up to (but not including) the
/// branch, return the stack as it would be *after* the branch operands are
/// consumed.  Used to recover the phi base when a ternary is embedded inside
/// a larger expression (e.g. `arr[i] = (cond ? a : b)`).
pub(super) fn cond_base_stack(
    block_insns: &[crate::classfile::instruction::Instruction],
    initial_stack: Vec<crate::ir::stack_sim::SlotInfo>,
    context: &RenderContext<'_>,
) -> Vec<crate::ir::stack_sim::SlotInfo> {
    use crate::classfile::opcodes::opc;
    let branch_idx = block_insns.iter().rposition(|i| {
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
                | opc::if_acmpeq
                | opc::if_acmpne
                | opc::ifnull
                | opc::ifnonnull
        )
    });
    let Some(branch_idx) = branch_idx else {
        return initial_stack;
    };
    let branch_op = block_insns[branch_idx].opcode;
    let sim_insns = &block_insns[..branch_idx];
    let mut out = context.simulate(sim_insns, initial_stack).stack_out;
    // The branch instruction pops 1 operand (ifeq/ifne/… and ifnull/ifnonnull)
    // or 2 operands (if_icmp* and if_acmp*).
    let pops: usize = match branch_op {
        opc::if_icmpeq
        | opc::if_icmpne
        | opc::if_icmplt
        | opc::if_icmpge
        | opc::if_icmpgt
        | opc::if_icmple
        | opc::if_acmpeq
        | opc::if_acmpne => 2,
        _ => 1,
    };
    for _ in 0..pops {
        out.pop();
    }
    out
}

/// Build a condition string from the instructions of a single basic block.
///
/// Finds the last conditional branch in `block_insns`, simulates everything
/// before it, and formats the comparison expression.  Because we only look at
/// the instructions belonging to the condition block (stored in `IfStmt /
/// LoopStmt`), this is always accurate regardless of method size.
pub(super) fn condition_from_block_insns(
    block_insns: &[crate::classfile::instruction::Instruction],
    context: &RenderContext<'_>,
    negated: bool,
) -> String {
    use crate::classfile::opcodes::opc;

    // Find the branch instruction inside this block.
    let branch_idx = block_insns.iter().rposition(|i| {
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
                | opc::if_acmpeq
                | opc::if_acmpne
                | opc::ifnull
                | opc::ifnonnull
        )
    });

    let Some(branch_idx) = branch_idx else {
        return "/* no branch */".into();
    };

    let branch_op = block_insns[branch_idx].opcode;
    // Simulate only the instructions before the branch inside this block.
    let sim_insns = &block_insns[..branch_idx];
    let result = context.simulate(sim_insns, vec![]);

    build_condition(branch_op, &result.stack_out, negated)
}

pub(super) fn build_condition(
    branch_op: u8,
    stack: &[crate::ir::stack_sim::SlotInfo],
    negated: bool,
) -> String {
    use crate::classfile::opcodes::opc;
    use crate::ir::expr::{BinOp, Expr};
    use crate::types::java_type::JavaType;

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
            let is_cmp = matches!(
                cmp_op,
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
                    opc::ifeq => {
                        if negated {
                            "!="
                        } else {
                            "=="
                        }
                    }
                    opc::ifne => {
                        if negated {
                            "=="
                        } else {
                            "!="
                        }
                    }
                    opc::iflt => {
                        if negated {
                            ">="
                        } else {
                            "<"
                        }
                    }
                    opc::ifle => {
                        if negated {
                            ">"
                        } else {
                            "<="
                        }
                    }
                    opc::ifgt => {
                        if negated {
                            "<="
                        } else {
                            ">"
                        }
                    }
                    opc::ifge => {
                        if negated {
                            "<"
                        } else {
                            ">="
                        }
                    }
                    _ => {
                        if negated {
                            "!="
                        } else {
                            "=="
                        }
                    }
                };
                return format!("{} {} {}", lhs_s, op_str, rhs_s);
            }
        }
    }

    let top = top_slot
        .map(|s| render_expr(&s.expr))
        .unwrap_or_else(|| "/*?*/".into());
    let sec = if stack.len() >= 2 {
        render_expr(&stack[stack.len() - 2].expr)
    } else {
        "0".into()
    };

    // For boolean-typed values, ifeq/ifne can collapse to direct/negated form.
    if top_is_bool {
        match branch_op {
            opc::ifeq => return if negated { top } else { format!("!{}", top) },
            opc::ifne => return if negated { format!("!{}", top) } else { top },
            _ => {}
        }
    }

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

// ── loops ──────────────────────────────────────────────────────────────────

pub(super) fn render_loop(
    arena: &StmtArena,
    s: LoopStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    match s.kind {
        LoopKind::While => {
            let cond = condition_from_block_insns(&s.cond_insns, context, s.cond_negated);
            w.line(&format!("while ({}) {{", cond));
            w.indent();
            render_stmt(arena, s.body, context, w);
            w.dedent();
            w.line("}");
        }
        LoopKind::DoWhile => {
            w.line("do {");
            w.indent();
            render_stmt(arena, s.body, context, w);
            w.dedent();
            let cond = condition_from_block_insns(&s.cond_insns, context, s.cond_negated);
            w.line(&format!("}} while ({});", cond));
        }
        LoopKind::Infinite | LoopKind::For => {
            w.line("while (true) {");
            w.indent();
            render_stmt(arena, s.body, context, w);
            w.dedent();
            w.line("}");
        }
    }
}

// ── String switch literal restoration ─────────────────────────────────────

/// Java String.hashCode() — identical algorithm used by javac for switch cases.
pub(super) fn java_string_hashcode(s: &str) -> i32 {
    let mut h: i32 = 0;
    for c in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as i32);
    }
    h
}

/// Build a hash→literals mapping from all String constants in the pool.
pub(super) fn build_string_hash_map(
    pool: &ConstantPool,
) -> std::collections::HashMap<i32, Vec<String>> {
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
pub(super) fn build_enum_ordinal_map(cf: &ClassFile) -> std::collections::HashMap<i32, String> {
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
pub(super) fn detect_enum_switch(
    expr: &Expr,
    cf: &ClassFile,
) -> Option<(String, std::collections::HashMap<i32, String>)> {
    if let Expr::ArrayLoad { array, index, .. } = expr {
        let is_table = match array.as_ref() {
            Expr::Invoke { name, .. } => name.starts_with("$SWITCH_TABLE$"),
            _ => false,
        };
        if !is_table {
            return None;
        }
        match index.as_ref() {
            Expr::Invoke {
                name,
                object: Some(obj),
                ..
            } if name == "ordinal" => {
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

pub(super) fn render_switch(
    arena: &StmtArena,
    s: SwitchStmt,
    initial_stack: Vec<SlotInfo>,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let pool = context.pool;
    let cf = context.class;
    let switch_pos = s
        .switch_insns
        .iter()
        .rposition(|i| matches!(i.opcode, 0xaa | 0xab))
        .unwrap_or(0);
    let result = context.simulate(&s.switch_insns[..switch_pos], initial_stack);
    if !result.stmts.is_empty() {
        emit_stmts(&result.stmts, context, w);
    }
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
                render_stmt(arena, arm.body, context, w);
                if arm.breaks && !stmt_terminates(arena, arm.body) {
                    w.line("break;");
                }
                w.dedent();
            }
            w.line("}");
            return;
        }

        // ── String switch: replace case HASH → case "literal" ─────────
        if let Expr::Invoke {
            name,
            object: Some(obj),
            kind: InvokeKind::Virtual,
            ..
        } = raw
        {
            if name == "hashCode" {
                let str_var = render_expr(obj);
                let hash_map = build_string_hash_map(pool);
                w.line(&format!("switch ({}) {{", str_var));
                for arm in &s.arms {
                    match arm.value {
                        Some(v) => {
                            if let Some(literals) = hash_map.get(&v) {
                                for lit in literals {
                                    let escaped = lit
                                        .replace('\\', "\\\\")
                                        .replace('"', "\\\"")
                                        .replace('\n', "\\n")
                                        .replace('\r', "\\r")
                                        .replace('\t', "\\t");
                                    w.line(&format!("case \"{}\":", escaped));
                                }
                            } else {
                                w.line(&format!("case {}:", v));
                            }
                        }
                        None => w.line("default:"),
                    }
                    w.indent();
                    render_stmt(arena, arm.body, context, w);
                    if arm.breaks && !stmt_terminates(arena, arm.body) {
                        w.line("break;");
                    }
                    w.dedent();
                }
                w.line("}");
                return;
            }
        }
    }

    // ── Plain switch ──────────────────────────────────────────────────
    let expr_str = raw_expr
        .map(render_expr)
        .unwrap_or_else(|| "/*switch_expr*/".into());

    w.line(&format!("switch ({}) {{", expr_str));
    for arm in &s.arms {
        match arm.value {
            Some(v) => w.line(&format!("case {}:", v)),
            None => w.line("default:"),
        }
        w.indent();
        render_stmt(arena, arm.body, context, w);
        if arm.breaks && !stmt_terminates(arena, arm.body) {
            w.line("break;");
        }
        w.dedent();
    }
    w.line("}");
}

pub(super) fn stmt_terminates(arena: &StmtArena, id: StmtId) -> bool {
    use crate::classfile::opcodes::opc;

    match arena.get(id) {
        Stmt::Block(block) => block.instructions.last().is_some_and(|instruction| {
            matches!(
                instruction.opcode,
                opc::ireturn
                    | opc::lreturn
                    | opc::freturn
                    | opc::dreturn
                    | opc::areturn
                    | opc::r#return
                    | opc::athrow
            )
        }),
        Stmt::Seq(sequence) => sequence
            .children
            .last()
            .is_some_and(|&child| stmt_terminates(arena, child)),
        Stmt::If(branch) => {
            stmt_terminates(arena, branch.then_branch)
                && branch
                    .else_branch
                    .is_some_and(|else_branch| stmt_terminates(arena, else_branch))
        }
        Stmt::Switch(switch) => {
            switch.arms.iter().any(|arm| arm.value.is_none())
                && switch
                    .arms
                    .iter()
                    .all(|arm| stmt_terminates(arena, arm.body))
        }
        Stmt::TryCatch(try_catch) => {
            stmt_terminates(arena, try_catch.try_body)
                && try_catch
                    .catches
                    .iter()
                    .all(|catch| stmt_terminates(arena, catch.body))
                && try_catch
                    .finally_body
                    .is_none_or(|finally| stmt_terminates(arena, finally))
        }
        Stmt::Exit => false,
        Stmt::Loop(_) | Stmt::BreakIf(_) | Stmt::Synchronized(_) => false,
    }
}

// ── try/catch ─────────────────────────────────────────────────────────────

pub(super) fn render_try_catch(
    arena: &StmtArena,
    s: TryCatchStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) -> Vec<SlotInfo> {
    w.line("try {");
    w.indent();
    // Thread the try body's residual stack outward so that a block immediately
    // after the try/catch that only does `xreturn` (consuming a value pushed
    // inside the protected region) gets the correct initial_stack instead of
    // producing Opaque{}.
    let try_residual = render_stmt_stacked(arena, s.try_body, vec![], context, w);
    w.dedent();
    for clause in &s.catches {
        let ty = clause
            .catch_type
            .as_deref()
            .unwrap_or("java/lang/Throwable");
        let var =
            catch_var_name(arena, clause.body, &context.lvt).unwrap_or_else(|| "e".to_string());
        w.line(&format!("}} catch ({} {}) {{", simple_name(ty), var));
        w.indent();
        // On entry to a handler the JVM clears the operand stack and pushes the
        // thrown exception.  Seed that so the leading `astore` renders as the
        // catch parameter binding rather than reading from an empty stack.
        render_stmt_with_exception(arena, clause.body, ty, &var, context, w);
        w.dedent();
    }
    if let Some(finally) = s.finally_body {
        w.line("} finally {");
        w.indent();
        render_stmt_with_exception(arena, finally, "java/lang/Throwable", "e", context, w);
        w.dedent();
    }
    w.line("}");
    // Return the try body's residual stack so the Seq can thread it to any
    // sibling block that consumes a value pushed inside the protected region.
    try_residual
}

/// Find the name the handler stores the exception into, from the LVT entry that
/// matches the handler's leading `astore` slot.  Falls back to `None`.
pub(super) fn catch_var_name(arena: &StmtArena, body: StmtId, lvt: &[LvtEntry]) -> Option<String> {
    use crate::classfile::opcodes::opc;
    // Walk to the first Block in the handler body.
    let mut id = body;
    loop {
        match arena.get(id) {
            Stmt::Block(b) => {
                let first = b.instructions.first()?;
                if !matches!(
                    first.opcode,
                    opc::astore | opc::astore_0 | opc::astore_1 | opc::astore_2 | opc::astore_3
                ) {
                    return None;
                }
                let slot = match first.kind {
                    crate::classfile::instruction::InsnKind::LocalVar { index } => index,
                    _ => match first.opcode {
                        opc::astore_0 => 0,
                        opc::astore_1 => 1,
                        opc::astore_2 => 2,
                        opc::astore_3 => 3,
                        _ => return None,
                    },
                };
                return lvt.iter().find(|e| e.slot == slot).map(|e| e.name.clone());
            }
            Stmt::Seq(s) => {
                id = *s.children.first()?;
            }
            _ => return None,
        }
    }
}

/// Render a handler body with the thrown exception pre-seeded on the operand
/// stack, and suppress the redundant `<var> = <exception>` assignment that the
/// leading `astore` would otherwise produce (the binding is already expressed
/// by the `catch (T var)` clause).
pub(super) fn render_stmt_with_exception(
    arena: &StmtArena,
    body: StmtId,
    exc_type: &str,
    var: &str,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    use crate::ir::expr::{Expr, LocalVarExpr};
    use crate::ir::stack_sim::SlotInfo;
    use crate::types::java_type::JavaType;

    // Seed the stack with a LocalVar naming the catch parameter, so the
    // handler's `astore <slot>` produces `var = var`, which we then drop.
    let seed = vec![SlotInfo {
        expr: Expr::LocalVar(LocalVarExpr {
            slot: u16::MAX,
            ty: JavaType::object(exc_type),
            name: Some(var.to_string()),
        }),
        ty: JavaType::object(exc_type),
    }];
    let before = w.len();
    render_stmt_stacked(arena, body, seed, context, w);
    // Drop a leading self-assignment line like `IOException e = e;` / `e = e;`.
    w.drop_line_if(before, |line| {
        let t = line.trim();
        t == format!("{} = {};", var, var)
            || (t.ends_with(&format!("{} = {};", var, var)) && t.contains(' '))
    });
}
