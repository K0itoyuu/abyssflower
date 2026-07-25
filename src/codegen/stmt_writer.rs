/// Statement (Stmt) tree pretty-printer → Java source.
use crate::classfile::attribute::{Attribute, CodeAttribute};
use crate::classfile::constant_pool::{CpEntry, ConstantPool};
use crate::classfile::ClassFile;
use crate::codegen::expr_writer::{render_expr, simple_name, IndentWriter};
use crate::ir::expr::{Expr, InvokeKind};
use crate::ir::stack_sim::simulate_block;
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
    match arena.get(id) {
        Stmt::Exit => {}

        Stmt::Block(b) => {
            let result = simulate_block(&b.instructions, pool, vec![], is_static, this_class, names);
            emit_stmts(&result.stmts, lvt, w);
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
            render_switch(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
        }

        Stmt::TryCatch(s) => {
            render_try_catch(arena, s.clone(), code, pool, is_static, this_class, names, lvt, cf, w);
        }

        Stmt::Synchronized(s) => {
            w.line("synchronized (/* monitor */) {");
            w.indent();
            let body = s.body;
            render_stmt(arena, body, code, pool, is_static, this_class, names, lvt, cf, w);
            w.dedent();
            w.line("}");
        }
    }
}

// ── statement emission with type-declaration upgrade ──────────────────────

/// Track which slots have already had their type declared.
/// Emit statements, promoting the first assignment of each LVT local from
/// `var = rhs` to `Type var = rhs`.
fn emit_stmts(stmts: &[Expr], lvt: &[LvtEntry], w: &mut IndentWriter) {
    // Build a set of slots that still need a declaration.
    // We use a simple Vec<bool> keyed by position in lvt.
    let mut declared = std::collections::HashSet::<u16>::new();

    for expr in stmts {
        // Bare `return;` is emitted normally — callers that want to suppress it
        // (e.g. the last statement of a void method) can do so above.
        let line = if let Expr::Assign { lhs, rhs } = expr {
            if let Expr::LocalVar(lv) = lhs.as_ref() {
                let slot = lv.slot;
                if !declared.contains(&slot) {
                    // Look up the LVT entry for this slot.
                    if let Some(entry) = lvt.iter().find(|e| e.slot == slot) {
                        // Derive the type string from the descriptor.
                        let ty_str = type_str_from_descriptor(&entry.descriptor);
                        declared.insert(slot);
                        // Desugar invokedynamic concat in rhs.
                        let rhs_str = render_expr_concat(rhs);
                        format!("{} {} = {};", ty_str, entry.name, rhs_str)
                    } else {
                        let rhs_str = render_expr_concat(rhs);
                        format!("{} = {};", render_expr(lhs), rhs_str)
                    }
                } else {
                    let rhs_str = render_expr_concat(rhs);
                    format!("{} = {};", render_expr(lhs), rhs_str)
                }
            } else {
                format!("{};", render_expr_concat(expr))
            }
        } else {
            format!("{};", render_expr_concat(expr))
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
/// into a Java `+` string concatenation chain.
fn render_expr_concat(expr: &Expr) -> String {
    if let Expr::InvokeDynamic { name, args, .. } = expr {
        if name == "makeConcatWithConstants" || name == "makeConcat" {
            // args: the dynamic arguments (not the recipe).
            // Simple case: just chain them with +, removing String.valueOf() wrappers.
            if !args.is_empty() {
                let parts: Vec<String> = args.iter().map(|a| {
                    // Remove String.valueOf(x) → x
                    strip_string_valueof(a)
                }).collect();
                return parts.join(" + ");
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

fn render_if(
    arena: &StmtArena, s: IfStmt,
    code: &CodeAttribute, pool: &ConstantPool,
    is_static: bool, this_class: &str,
    names: &[(u16, String)], lvt: &[LvtEntry], cf: &ClassFile, w: &mut IndentWriter,
) {
    let cond_str = extract_branch_condition(s.cond_block, code, pool, is_static, this_class, s.negated, names);

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

/// Extract the condition expression string from the last conditional branch instruction.
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

    // Find last conditional branch and simulate up to it.
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

    build_condition(branch_op, &result.stack_out, negated)
}

fn build_condition(
    branch_op: u8,
    stack: &[crate::ir::stack_sim::SlotInfo],
    negated: bool,
) -> String {
    use crate::classfile::opcodes::opc;

    // Get top 1 or 2 stack values
    let top  = stack.last().map(|s| render_expr(&s.expr)).unwrap_or_else(|| "/*?*/".into());
    let sec  = if stack.len() >= 2 {
        render_expr(&stack[stack.len()-2].expr)
    } else { "0".into() };

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
            let cond = extract_branch_condition(
                s.header_block, code, pool, is_static, this_class, false, names);
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
                s.tail_block, code, pool, is_static, this_class, false, names);
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
) {
    w.line("try {");
    w.indent();
    render_stmt(arena, s.try_body, code, pool, is_static, this_class, names, lvt, cf, w);
    w.dedent();
    for clause in &s.catches {
        let type_str = clause.catch_type.as_deref()
            .map(|t| format!("{} e", simple_name(t)))
            .unwrap_or_else(|| "Throwable e".into());
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
