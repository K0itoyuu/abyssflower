use super::*;

// ── Control flow rendering ────────────────────────────────────────────────

pub(super) fn render_if(
    arena: &StmtArena,
    s: IfStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let cond_str = extract_branch_condition(s.cond_block, &s.cond_insns, context, s.negated);
    if let Some(value_loop) = context
        .value_producing_loops
        .iter()
        .find(|value_loop| value_loop.predicate_block == s.cond_block)
    {
        emit_branch_prefix(s.cond_block, &s.cond_insns, context, w);
        let success_condition = extract_branch_condition(
            s.cond_block,
            &s.cond_insns,
            context,
            value_loop.predicate_negated,
        );
        w.line(&format!("if ({success_condition}) {{"));
        w.indent();
        w.line(&format!(
            "{} = {}",
            value_loop.name,
            kt_render_expr(&value_loop.success_value)
        ));
        w.line("break");
        w.dedent();
        w.line("}");
        return;
    }

    if context.is_coroutine_state_machine {
        // Continuation reuse/construction is a compiler protocol prelude.
        if cond_str.contains("$completion")
            || cond_str.contains("$continuation`.label")
            || cond_str.contains("$continuation`.label")
        {
            return;
        }
        // A suspend call is compiled as `result == COROUTINE_SUSPENDED`
        // followed by an early return. Source-level control simply continues.
        if condition_contains_suspend_call(s.cond_block, &s.cond_insns, context) {
            if let Some(else_id) = s.else_branch {
                render_stmt_in_local_scope(arena, else_id, context, w);
            }
            return;
        }
    }

    emit_branch_prefix(s.cond_block, &s.cond_insns, context, w);

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
                    render_stmt_in_local_scope(arena, s.then_branch, context, w);
                    w.dedent();
                    w.line("} else {");
                    w.indent();
                    render_stmt_in_local_scope(arena, else_id, context, w);
                    w.dedent();
                    w.line("}");
                    return;
                } else if cond_str.ends_with("== null") {
                    // if (x == null) default else x → x ?: default
                    w.line(&format!("if ({}) {{", cond_str));
                    w.indent();
                    render_stmt_in_local_scope(arena, s.then_branch, context, w);
                    w.dedent();
                    w.line("} else {");
                    w.indent();
                    render_stmt_in_local_scope(arena, else_id, context, w);
                    w.dedent();
                    w.line("}");
                    return;
                }
            }
        }
    }

    if let Some(else_id) = s.else_branch {
        // Normalise: `if (c) { } else { body }` → `if (!c) { body }`
        let then_is_empty = matches!(arena.get(s.then_branch), Stmt::Exit);
        if then_is_empty {
            let neg_cond =
                extract_branch_condition(s.cond_block, &s.cond_insns, context, !s.negated);
            w.line(&format!("if ({}) {{", neg_cond));
            w.indent();
            render_stmt_in_local_scope(arena, else_id, context, w);
            w.dedent();
            w.line("}");
            return;
        }
        let else_is_empty = matches!(arena.get(else_id), Stmt::Exit);
        if else_is_empty {
            w.line(&format!("if ({}) {{", cond_str));
            w.indent();
            render_stmt_in_local_scope(arena, s.then_branch, context, w);
            w.dedent();
            w.line("}");
        } else {
            w.line(&format!("if ({}) {{", cond_str));
            w.indent();
            render_stmt_in_local_scope(arena, s.then_branch, context, w);
            w.dedent();
            w.line("} else {");
            w.indent();
            render_stmt_in_local_scope(arena, else_id, context, w);
            w.dedent();
            w.line("}");
        }
    } else {
        let then_is_empty = matches!(arena.get(s.then_branch), Stmt::Exit);
        if !then_is_empty {
            w.line(&format!("if ({}) {{", cond_str));
            w.indent();
            render_stmt_in_local_scope(arena, s.then_branch, context, w);
            w.dedent();
            w.line("}");
        }
    }
}

fn render_stmt_in_local_scope(
    arena: &StmtArena,
    id: StmtId,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let saved_declared = context.declared_slots.borrow().clone();
    let saved_names = context.declared_local_names.borrow().clone();
    render_stmt(arena, id, context, w);
    *context.declared_slots.borrow_mut() = saved_declared;
    *context.declared_local_names.borrow_mut() = saved_names;
}

pub(super) fn emit_branch_prefix(
    block_id: crate::cfg::BlockId,
    cond_insns: &[crate::classfile::instruction::Instruction],
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let Some(branch_idx) = cond_insns
        .iter()
        .rposition(|instruction| is_conditional_branch(instruction.opcode))
    else {
        return;
    };
    let entry = context.block_entry(block_id);
    let result = context.simulate_state(&cond_insns[..branch_idx], &entry);
    kt_emit_stmts(&result.stmts, context, w);
}

fn is_conditional_branch(opcode: u8) -> bool {
    use crate::classfile::opcodes::opc;

    matches!(
        opcode,
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
}

pub(super) fn is_null_check(cond: &str) -> bool {
    cond.ends_with("== null") || cond.ends_with("!= null")
}

pub(super) fn is_simple_stmt(arena: &StmtArena, id: StmtId) -> bool {
    match arena.get(id) {
        Stmt::Block(_) => true,
        Stmt::Exit => true,
        Stmt::Seq(s) => s.children.len() <= 1,
        _ => false,
    }
}

pub(super) fn extract_branch_condition(
    block_id: crate::cfg::BlockId,
    cond_insns: &[crate::classfile::instruction::Instruction],
    context: &RenderContext<'_>,
    negated: bool,
) -> String {
    use crate::classfile::opcodes::opc;

    let branch_idx = cond_insns
        .iter()
        .rposition(|i| {
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
        })
        .unwrap_or(0);

    let branch_op = cond_insns
        .get(branch_idx)
        .map(|i| i.opcode)
        .unwrap_or(opc::ifeq);
    let sim_insns = &cond_insns[..branch_idx];
    let entry = context.block_entry(block_id);
    let result = context.simulate_state(sim_insns, &entry);

    build_kotlin_condition(branch_op, &result.stack_out, negated)
}

pub(super) fn build_kotlin_condition(
    branch_op: u8,
    stack: &[crate::ir::stack_sim::SlotInfo],
    negated: bool,
) -> String {
    use crate::classfile::opcodes::opc;
    use crate::ir::expr::{BinOp, Expr};

    let top_slot = stack.last();

    // JVM three-way comparisons push -1/0/1 and are immediately consumed by
    // an integer branch. Collapse the pair back to the source comparison so
    // raw lcmp/fcmp/dcmp markers never leak into Kotlin output.
    if let Some(Expr::BinOp(
        BinOp::LCmp | BinOp::FCmpL | BinOp::FCmpG | BinOp::DCmpL | BinOp::DCmpG,
        lhs,
        rhs,
    )) = top_slot.map(|slot| &slot.expr)
    {
        let op = match (branch_op, negated) {
            (opc::ifeq, false) | (opc::ifne, true) => "==",
            (opc::ifeq, true) | (opc::ifne, false) => "!=",
            (opc::iflt, false) | (opc::ifge, true) => "<",
            (opc::iflt, true) | (opc::ifge, false) => ">=",
            (opc::ifle, false) | (opc::ifgt, true) => "<=",
            (opc::ifle, true) | (opc::ifgt, false) => ">",
            _ => "==",
        };
        return format!(
            "{} {} {}",
            kt_render_expr_prec(lhs, 6),
            op,
            kt_render_expr_prec(rhs, 6)
        );
    }

    let top = top_slot
        .map(|s| kt_render_expr_prec(&s.expr, 7))
        .unwrap_or_else(|| "/* ? */".into());
    let sec = if stack.len() >= 2 {
        kt_render_expr_prec(&stack[stack.len() - 2].expr, 7)
    } else {
        "0".into()
    };

    if top_slot.is_some_and(|slot| slot.ty == crate::types::java_type::JavaType::BOOLEAN) {
        return match (branch_op, negated) {
            (opc::ifeq, false) | (opc::ifne, true) => format!("!({})", top),
            (opc::ifeq, true) | (opc::ifne, false) => top,
            _ => top,
        };
    }

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

pub(super) fn render_loop(
    arena: &StmtArena,
    s: LoopStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let value_loop = context
        .value_producing_loops
        .iter()
        .find(|value_loop| value_loop.header_block == s.header_block)
        .cloned();
    emit_loop_entry_local_initializers(&s, context, w);
    if let Some(value_loop) = value_loop {
        if s.kind == LoopKind::While {
            emit_branch_prefix(s.header_block, &s.cond_insns, context, w);
            let condition =
                extract_branch_condition(s.header_block, &s.cond_insns, context, s.cond_negated);
            w.line(&format!("while ({condition}) {{"));
            w.indent();
            render_stmt_in_local_scope(arena, s.body, context, w);
            w.dedent();
            w.line("}");
            context
                .completed_value_producing_loops
                .borrow_mut()
                .push(value_loop);
            return;
        }
    }
    match s.kind {
        LoopKind::While => {
            emit_branch_prefix(s.header_block, &s.cond_insns, context, w);
            let cond =
                extract_branch_condition(s.header_block, &s.cond_insns, context, s.cond_negated);

            // Detect for-in pattern: while (iter.hasNext() ...)
            if cond.contains(".hasNext()") {
                // Render loop body to a buffer to extract iterator.next() assignment
                let saved_declared = context.declared_slots.borrow().clone();
                let saved_names = context.declared_local_names.borrow().clone();
                let mut body_buf = IndentWriter::new(4);
                render_stmt(arena, s.body, context, &mut body_buf);
                let body_text = body_buf.finish();
                *context.declared_slots.borrow_mut() = saved_declared;
                *context.declared_local_names.borrow_mut() = saved_names;

                // Parse the body: first line should be `val item = (iter.next()...)`
                let body_lines: Vec<&str> = body_text.lines().collect();
                let preferred_name = iterator_element_name(&s, context);
                let parsed_element = parse_iterator_element_lines(&body_lines, preferred_name);

                // If the iterator element was hoisted because it is referenced
                // after the loop, its first statement is an assignment rather
                // than a declaration.  Keep the while loop so that assignment is
                // not discarded by the source-level for-in rewrite.
                let Some((element_var, remaining_start)) = parsed_element else {
                    w.line(&format!("while ({}) {{", cond));
                    w.indent();
                    render_stmt_in_local_scope(arena, s.body, context, w);
                    w.dedent();
                    w.line("}");
                    return;
                };
                let remaining_lines = &body_lines[remaining_start..];
                if iterator_element_escapes_loop(&s, context) {
                    w.line(&format!("while ({}) {{", cond));
                    w.indent();
                    render_stmt_in_local_scope(arena, s.body, context, w);
                    w.dedent();
                    w.line("}");
                    return;
                }

                // Extract collection from pre-loop: look for "varX = expr.iterator()" pattern
                let iter_var = cond.split('.').next().unwrap_or("").trim();
                let collection_name = find_iterator_source(
                    arena,
                    s.body,
                    s.header_block,
                    &s.cond_insns,
                    iter_var,
                    context,
                );
                let Some(collection_name) = collection_name else {
                    w.line(&format!("while ({}) {{", cond));
                    w.indent();
                    render_stmt_in_local_scope(arena, s.body, context, w);
                    w.dedent();
                    w.line("}");
                    return;
                };

                w.line(&format!("for ({} in {}) {{", element_var, collection_name));
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
            if let Some(range_for) = try_detect_range_for(&cond, s.header_block, context) {
                w.line(&format!(
                    "for ({} in {}) {{",
                    range_for.var_name, range_for.range_expr
                ));
                w.indent();
                render_stmt_in_local_scope(arena, s.body, context, w);
                w.dedent();
                w.line("}");
                return;
            }

            w.line(&format!("while ({}) {{", cond));
            w.indent();
            render_stmt_in_local_scope(arena, s.body, context, w);
            w.dedent();
            w.line("}");
        }
        LoopKind::DoWhile => {
            w.line("do {");
            w.indent();
            render_stmt_in_local_scope(arena, s.body, context, w);
            emit_branch_prefix(s.tail_block, &s.cond_insns, context, w);
            w.dedent();
            let cond =
                extract_branch_condition(s.tail_block, &s.cond_insns, context, s.cond_negated);
            w.line(&format!("}} while ({})", cond));
        }
        LoopKind::Infinite | LoopKind::For => {
            w.line("while (true) {");
            w.indent();
            render_stmt_in_local_scope(arena, s.body, context, w);
            w.dedent();
            w.line("}");
        }
    }
}

fn parse_iterator_element_lines(
    body_lines: &[&str],
    preferred_name: Option<String>,
) -> Option<(String, usize)> {
    let first = body_lines.first()?.trim();
    if !first.contains(".next()") {
        return None;
    }
    let raw_name = declaration_name(first)?;
    let mut remaining_start = 1;
    let cast_name = body_lines.get(1).and_then(|line| {
        let line = line.trim();
        let (lhs, rhs) = line.split_once('=')?;
        let name = lhs
            .trim()
            .strip_prefix("val ")
            .or_else(|| lhs.trim().strip_prefix("var "))
            .unwrap_or_else(|| lhs.trim())
            .trim()
            .to_string();
        (!name.is_empty() && rhs.trim().starts_with(&format!("{raw_name} as "))).then_some(name)
    });
    if cast_name.is_some() {
        remaining_start = 2;
    }
    Some((
        preferred_name.or(cast_name).unwrap_or(raw_name),
        remaining_start,
    ))
}

fn declaration_name(line: &str) -> Option<String> {
    line.strip_prefix("val ")
        .or_else(|| line.strip_prefix("var "))
        .and_then(|declaration| declaration.split_once('='))
        .map(|(name, _)| name.trim().to_string())
}

fn iterator_element_name(loop_stmt: &LoopStmt, context: &RenderContext<'_>) -> Option<String> {
    let mut instructions = loop_stmt
        .body_blocks
        .iter()
        .filter_map(|block| context.block_instructions.get(block))
        .flat_map(|instructions| instructions.iter())
        .collect::<Vec<_>>();
    instructions.sort_by_key(|instruction| instruction.offset);

    let next_index = instructions.iter().position(|instruction| {
        let crate::classfile::instruction::InsnKind::Invoke {
            index: cp_index, ..
        } = instruction.kind
        else {
            return false;
        };
        matches!(
            context.pool.get(cp_index),
            Ok(crate::classfile::constant_pool::CpEntry::Methodref(member)
                | crate::classfile::constant_pool::CpEntry::InterfaceMethodref(member))
                if member.name == "next"
        )
    })?;
    let raw_store_index = instructions[next_index + 1..]
        .iter()
        .position(|instruction| local_store_slot(instruction).is_some())?
        + next_index
        + 1;
    let raw_slot = local_store_slot(instructions[raw_store_index])?;

    let typed_store = instructions[raw_store_index + 1..]
        .windows(3)
        .find_map(|window| {
            (local_load_slot(window[0]) == Some(raw_slot) && window[1].opcode == opc::checkcast)
                .then(|| local_store_slot(window[2]).map(|slot| (slot, window[2].offset)))?
        });
    let (element_slot, store_offset) =
        typed_store.unwrap_or((raw_slot, instructions[raw_store_index].offset));
    context
        .lvt
        .iter()
        .filter(|entry| {
            entry.slot == element_slot
                && u32::from(entry.start_pc) >= store_offset
                && loop_stmt.body_blocks.iter().any(|block| {
                    context
                        .block_instructions
                        .get(block)
                        .is_some_and(|instructions| {
                            instructions.iter().any(|instruction| {
                                instruction.offset >= u32::from(entry.start_pc)
                                    && instruction.offset
                                        < u32::from(entry.start_pc) + u32::from(entry.length)
                                    && local_load_slot(instruction) == Some(element_slot)
                            })
                        })
                })
        })
        .min_by_key(|entry| entry.start_pc)
        .map(|entry| kotlin_identifier(&entry.name))
}

fn iterator_element_escapes_loop(loop_stmt: &LoopStmt, context: &RenderContext<'_>) -> bool {
    let body = loop_stmt
        .body_blocks
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut ordered = loop_stmt
        .body_blocks
        .iter()
        .filter_map(|block| context.block_instructions.get(block))
        .flat_map(|instructions| instructions.iter())
        .collect::<Vec<_>>();
    ordered.sort_by_key(|instruction| instruction.offset);

    let element = ordered.iter().enumerate().find_map(|(index, instruction)| {
        let crate::classfile::instruction::InsnKind::Invoke {
            index: cp_index, ..
        } = instruction.kind
        else {
            return None;
        };
        let member = match context.pool.get(cp_index).ok()? {
            crate::classfile::constant_pool::CpEntry::Methodref(member)
            | crate::classfile::constant_pool::CpEntry::InterfaceMethodref(member) => member,
            _ => return None,
        };
        if member.name != "next" {
            return None;
        }
        ordered[index + 1..]
            .iter()
            .find_map(|candidate| local_store_slot(candidate).map(|slot| (slot, candidate.offset)))
    });
    let Some((element_slot, store_offset)) = element else {
        return false;
    };

    context
        .block_instructions
        .iter()
        .any(|(block, instructions)| {
            !body.contains(block)
                && instructions.iter().any(|instruction| {
                    instruction.offset > store_offset
                        && local_load_slot(instruction) == Some(element_slot)
                })
        })
}

fn emit_loop_entry_local_initializers(
    loop_stmt: &LoopStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let entry = context.block_entry(loop_stmt.header_block);
    let mut loaded_slots = std::collections::BTreeSet::new();
    for block in &loop_stmt.body_blocks {
        if let Some(instructions) = context.block_instructions.get(block) {
            for instruction in instructions {
                if let Some(slot) = local_load_slot(instruction) {
                    loaded_slots.insert(slot);
                }
            }
        }
    }
    for instruction in &loop_stmt.cond_insns {
        if let Some(slot) = local_load_slot(instruction) {
            loaded_slots.insert(slot);
        }
    }

    let mut assignments = Vec::new();
    for slot in loaded_slots {
        if context.declared_slots.borrow().contains(&slot) {
            continue;
        }
        let Some(value) = entry.local_values.get(&slot) else {
            continue;
        };
        let (ty, name) = entry
            .local_types
            .iter()
            .find(|(candidate, _, _)| *candidate == slot)
            .map(|(_, ty, name)| (ty.clone(), name.clone()))
            .unwrap_or((crate::types::java_type::JavaType::UNKNOWN, None));
        let local = Expr::LocalVar(crate::ir::LocalVarExpr { slot, ty, name });
        if format!("{:?}", local) == format!("{:?}", value) {
            continue;
        }
        assignments.push(Expr::Assign {
            lhs: Box::new(local),
            rhs: Box::new(value.clone()),
        });
    }
    kt_emit_stmts(&assignments, context, w);
}

pub(super) struct RangeForInfo {
    var_name: String,
    range_expr: String,
}

/// Try to find the collection expression for an iterator variable.
/// Looks at the pre-loop block for "iter_var = collection.iterator()" pattern.
pub(super) fn find_iterator_source(
    _arena: &StmtArena,
    _body: StmtId,
    header_block: crate::cfg::BlockId,
    cond_insns: &[crate::classfile::instruction::Instruction],
    iter_var: &str,
    context: &RenderContext<'_>,
) -> Option<String> {
    // Prefer the iterator value reaching this exact loop header. A method can
    // contain several lowered for-loops, so a method-wide search for the first
    // hasNext()/iterator() pair associates later loops with the wrong source.
    let iterator_slot = cond_insns
        .iter()
        .enumerate()
        .find_map(|(index, instruction)| {
            let crate::classfile::instruction::InsnKind::Invoke {
                index: cp_index, ..
            } = instruction.kind
            else {
                return None;
            };
            let member = match context.pool.get(cp_index).ok()? {
                crate::classfile::constant_pool::CpEntry::Methodref(member)
                | crate::classfile::constant_pool::CpEntry::InterfaceMethodref(member) => member,
                _ => return None,
            };
            (member.name == "hasNext")
                .then(|| cond_insns[..index].iter().rev().find_map(local_load_slot))?
        });
    if let Some(iterator_slot) = iterator_slot {
        if let Some(Expr::Invoke {
            name,
            object: Some(collection),
            ..
        }) = context
            .block_entry(header_block)
            .local_values
            .get(&iterator_slot)
        {
            if name == "iterator" {
                let cleaned = kt_render_expr(collection).replace(" as Iterable", "");
                if cleaned != iter_var && !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
    }

    // Find the "iterator()" call in the pre-loop instructions
    // by simulating the entire pre-loop area
    let insns = &context.code.instructions;
    let pool = context.pool;

    // Find the loop header (hasNext call position)
    let header_offset = cond_insns
        .first()
        .map(|instruction| instruction.offset)
        .or_else(|| {
            context
                .block_instructions
                .get(&header_block)?
                .first()
                .map(|i| i.offset)
        })?;
    let has_next_pos = insns.iter().position(|i| {
        if i.offset < header_offset {
            return false;
        }
        if let crate::classfile::instruction::InsnKind::Invoke { index, .. } = &i.kind {
            if let Ok(
                crate::classfile::constant_pool::CpEntry::Methodref(mr)
                | crate::classfile::constant_pool::CpEntry::InterfaceMethodref(mr),
            ) = pool.get(*index)
            {
                return mr.name == "hasNext";
            }
        }
        false
    })?;

    // Find the "iterator()" call before hasNext
    let iter_pos = insns[..has_next_pos].iter().rposition(|i| {
        if let crate::classfile::instruction::InsnKind::Invoke { index, .. } = &i.kind {
            if let Ok(
                crate::classfile::constant_pool::CpEntry::Methodref(mr)
                | crate::classfile::constant_pool::CpEntry::InterfaceMethodref(mr),
            ) = pool.get(*index)
            {
                return mr.name == "iterator";
            }
        }
        false
    })?;

    // Simulate the instructions up to (but not including) the iterator() call
    // to get the collection expression on the stack
    let pre_iter = &insns[..iter_pos];
    let result = context.simulate(pre_iter, vec![]);
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
pub(super) fn try_detect_range_for(
    cond: &str,
    header_block: crate::cfg::BlockId,
    context: &RenderContext<'_>,
) -> Option<RangeForInfo> {
    // Pattern: "varName <= N" (for `in start..end`)
    // or "varName < N" (for `in start until end`)
    // The variable must have been initialized just before the loop

    // Try to match "var >= N" which is the negated form of "var < N"
    // (branch taken when condition is FALSE, i.e., loop continues while var < N)
    let captures = parse_range_condition(cond)?;
    let header_offset = context
        .block_instructions
        .get(&header_block)?
        .first()?
        .offset;
    context
        .lvt
        .iter()
        .any(|entry| {
            entry.descriptor == "I"
                && kotlin_identifier(&entry.name) == captures.var_name
                && u32::from(entry.start_pc) <= header_offset
                && header_offset < u32::from(entry.start_pc) + u32::from(entry.length)
        })
        .then_some(captures)
}

/// Parse range-style condition for Kotlin for-in detection.
/// Handles both the correct polarity (from the fixed negated flag) and the
/// legacy inverted forms as a fallback.
///
/// "i < 11"  → for (i in 1..10)
/// "i <= 10" → for (i in 1..10)
/// "i < N"   → for (i in 1 until N) [open upper end]
pub(super) fn parse_range_condition(cond: &str) -> Option<RangeForInfo> {
    let parts: Vec<&str> = cond.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }
    let var_name = parts[0];
    let op = parts[1];
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
                    var_name: var_name.to_string(),
                    range_expr: format!("1..{}", limit - 1),
                });
            }
            "<=" => {
                // while (i <= limit): body runs for i ∈ [start, limit] → `1..limit`
                return Some(RangeForInfo {
                    var_name: var_name.to_string(),
                    range_expr: format!("1..{}", limit),
                });
            }
            // Legacy inverted forms (kept as fallback)
            ">=" => {
                return Some(RangeForInfo {
                    var_name: var_name.to_string(),
                    range_expr: format!("1..{}", limit - 1),
                });
            }
            ">" => {
                return Some(RangeForInfo {
                    var_name: var_name.to_string(),
                    range_expr: format!("1..{}", limit),
                });
            }
            _ => {}
        }
    }
    None
}

/// Render switch as Kotlin `when` expression.
pub(super) fn render_when(
    arena: &StmtArena,
    s: SwitchStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    let pool = context.pool;
    let switch_pos = s
        .switch_insns
        .iter()
        .position(|i| matches!(i.opcode, 0xaa | 0xab))
        .unwrap_or(0);
    let sim_insns = &s.switch_insns[..switch_pos];
    let entry = context.block_entry(s.switch_block);
    let result = context.simulate_state(sim_insns, &entry);
    let raw_expr = result.stack_out.last().map(|s| &s.expr);

    // Kotlin string/enum when lowering commonly stores the source selector in
    // a temporary before invoking hashCode()/ordinal(). The switch renderer
    // owns this condition block, so its prefix assignments must be emitted
    // here rather than discarded when only the residual stack is inspected.
    kt_emit_stmts(&result.stmts, context, w);

    if let Some(expression) = s
        .arms
        .iter()
        .find_map(|arm| find_switch_return_expr(arena, arm.body, context))
    {
        if matches!(&expression, Expr::SwitchExpression { selector, .. }
            if raw_expr.is_some_and(|raw| format!("{:?}", selector) == format!("{:?}", raw)))
        {
            w.line(&format!("return {}", kt_render_expr(&expression)));
            return;
        }
    }

    if context.is_coroutine_state_machine
        && raw_expr.is_some_and(|expr| matches!(expr, Expr::Field { name, .. } if name == "label"))
    {
        if let Some(initial) = s.arms.iter().find(|arm| arm.value == Some(0)) {
            render_stmt(arena, initial.body, context, w);
        }
        return;
    }

    // ── Detect Kotlin enum when: WhenMappings.$EnumSwitchMapping$N[expr.ordinal()]
    if let Some(expr) = raw_expr {
        if let Some((subject, enum_map)) = detect_kotlin_enum_when(expr, context.class) {
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
                render_stmt(arena, arm.body, context, w);
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
                render_stmt(arena, arm.body, context, w);
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
        .map(kt_render_expr)
        .unwrap_or_else(|| "/* expr */".into());

    w.line(&format!("when ({}) {{", expr_str));
    w.indent();
    for arm in &s.arms {
        match arm.value {
            Some(v) => w.line(&format!("{} -> {{", v)),
            None => w.line("else -> {"),
        }
        w.indent();
        render_stmt(arena, arm.body, context, w);
        w.dedent();
        w.line("}");
    }
    w.dedent();
    w.line("}");
}

fn find_switch_return_expr(
    arena: &StmtArena,
    id: StmtId,
    context: &RenderContext<'_>,
) -> Option<Expr> {
    match arena.get(id) {
        Stmt::Block(block) => {
            let entry = context.block_entry(block.block_id);
            context
                .simulate_state(&block.instructions, &entry)
                .stmts
                .into_iter()
                .find_map(|expression| match expression {
                    Expr::Return(Some(value))
                        if matches!(value.as_ref(), Expr::SwitchExpression { .. }) =>
                    {
                        Some(*value)
                    }
                    _ => None,
                })
        }
        Stmt::Seq(sequence) => sequence
            .children
            .iter()
            .find_map(|child| find_switch_return_expr(arena, *child, context)),
        _ => None,
    }
}

fn condition_contains_suspend_call(
    _block_id: crate::cfg::BlockId,
    instructions: &[crate::classfile::instruction::Instruction],
    context: &RenderContext<'_>,
) -> bool {
    instructions.iter().any(|instruction| {
        let crate::classfile::instruction::InsnKind::Invoke { index, .. } = instruction.kind else {
            return false;
        };
        let member = match context.pool.get(index) {
            Ok(crate::classfile::constant_pool::CpEntry::Methodref(member))
            | Ok(crate::classfile::constant_pool::CpEntry::InterfaceMethodref(member)) => member,
            _ => return false,
        };
        crate::types::descriptor::MethodDescriptor::parse(&member.descriptor)
            .ok()
            .is_some_and(|method| {
                method
                    .params
                    .iter()
                    .any(|ty| ty.class_name.as_deref() == Some("kotlin/coroutines/Continuation"))
            })
    })
}

/// Detect Kotlin enum when pattern:
/// `WhenMappings.$EnumSwitchMapping$N[subject.ordinal()]`
/// Returns (subject_expression_str, case_value → enum_name map)
pub(super) fn detect_kotlin_enum_when(
    expr: &Expr,
    cf: &ClassFile,
) -> Option<(String, std::collections::HashMap<i32, String>)> {
    if let Expr::ArrayLoad { array, index, .. } = expr {
        // Check array is a $EnumSwitchMapping field
        let is_when_mapping = match array.as_ref() {
            Expr::Field { name, .. } => name.contains("$EnumSwitchMapping"),
            _ => false,
        };
        if !is_when_mapping {
            return None;
        }

        // Check index is subject.ordinal()
        if let Expr::Invoke {
            name,
            object: Some(obj),
            ..
        } = index.as_ref()
        {
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
pub(super) fn detect_string_when(expr: &Expr) -> Option<String> {
    if let Expr::Invoke {
        name,
        object: Some(obj),
        kind: InvokeKind::Virtual,
        ..
    } = expr
    {
        if name == "hashCode" {
            return Some(kt_render_expr(obj));
        }
    }
    None
}

/// Java String.hashCode() — for restoring string switch case labels.
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

pub(super) fn render_try_catch(
    arena: &StmtArena,
    s: TryCatchStmt,
    context: &RenderContext<'_>,
    w: &mut IndentWriter,
) {
    w.line("try {");
    w.indent();
    render_stmt(arena, s.try_body, context, w);
    w.dedent();
    for clause in &s.catches {
        let var =
            kotlin_catch_var_name(arena, clause.body, &context.lvt).unwrap_or_else(|| "e".into());
        let type_str = clause
            .catch_type
            .as_deref()
            .map(|t| format!("{}: {}", var, kotlin_class_name(t)))
            .unwrap_or_else(|| format!("{}: Throwable", var));
        w.line(&format!("}} catch ({}) {{", type_str));
        w.indent();
        let before = w.len();
        render_stmt(arena, clause.body, context, w);
        w.drop_line_if(before, |line| {
            let line = line.trim();
            line == format!("val {} = exception", var)
                || line == format!("var {} = exception", var)
                || line == format!("{} = exception", var)
        });
        w.dedent();
    }
    if let Some(finally) = s.finally_body {
        w.line("} finally {");
        w.indent();
        render_stmt(arena, finally, context, w);
        w.dedent();
    }
    w.line("}");
}

fn kotlin_catch_var_name(arena: &StmtArena, body: StmtId, lvt: &[LvtEntry]) -> Option<String> {
    let mut id = body;
    loop {
        match arena.get(id) {
            Stmt::Block(block) => {
                let first = block.instructions.first()?;
                let slot = match first.kind {
                    InsnKind::LocalVar { index } if first.opcode == opc::astore => index,
                    _ => match first.opcode {
                        opc::astore_0 => 0,
                        opc::astore_1 => 1,
                        opc::astore_2 => 2,
                        opc::astore_3 => 3,
                        _ => return None,
                    },
                };
                return lvt
                    .iter()
                    .find(|entry| entry.slot == slot)
                    .map(|entry| kotlin_identifier(&entry.name));
            }
            Stmt::Seq(seq) => id = *seq.children.first()?,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_kotlin_condition, parse_iterator_element_lines};
    use crate::classfile::opcodes::opc;
    use crate::ir::expr::{BinOp, Expr, LocalVarExpr};
    use crate::ir::stack_sim::SlotInfo;
    use crate::types::java_type::JavaType;

    fn comparison_slot(op: BinOp) -> SlotInfo {
        let local = |slot: u16, name: &str, ty: JavaType| {
            Expr::LocalVar(LocalVarExpr {
                slot,
                ty,
                name: Some(name.into()),
            })
        };
        let ty = match op {
            BinOp::LCmp => JavaType::LONG,
            BinOp::FCmpL | BinOp::FCmpG => JavaType::FLOAT,
            BinOp::DCmpL | BinOp::DCmpG => JavaType::DOUBLE,
            _ => unreachable!(),
        };
        SlotInfo {
            expr: Expr::BinOp(
                op,
                Box::new(local(0, "left", ty.clone())),
                Box::new(local(1, "right", ty)),
            ),
            ty: JavaType::INT,
        }
    }

    #[test]
    fn three_way_comparisons_collapse_into_kotlin_relations() {
        for cmp in [
            BinOp::LCmp,
            BinOp::FCmpL,
            BinOp::FCmpG,
            BinOp::DCmpL,
            BinOp::DCmpG,
        ] {
            let stack = [comparison_slot(cmp)];
            assert_eq!(
                build_kotlin_condition(opc::ifgt, &stack, false),
                "left > right"
            );
            assert_eq!(
                build_kotlin_condition(opc::ifgt, &stack, true),
                "left <= right"
            );
            assert_eq!(
                build_kotlin_condition(opc::ifeq, &stack, false),
                "left == right"
            );
            assert!(!build_kotlin_condition(opc::ifge, &stack, false).contains("/*"));
        }
    }

    #[test]
    fn iterator_element_parser_discards_raw_and_cast_aliases() {
        let lines = [
            "val var5 = var4.next()",
            "val var6 = var5 as Task",
            "val progress = it.getProgress()",
        ];
        assert_eq!(
            parse_iterator_element_lines(&lines, Some("it".into())),
            Some(("it".into(), 2))
        );

        let hoisted = [
            "var var7 = var6.next()",
            "var8 = var7 as ItemStack",
            "val damage = it_8.getDamageValue()",
        ];
        assert_eq!(
            parse_iterator_element_lines(&hoisted, Some("it_8".into())),
            Some(("it_8".into(), 2))
        );
    }
}
