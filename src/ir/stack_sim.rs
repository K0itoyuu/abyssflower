#![allow(non_upper_case_globals)]
//! JVM operand stack simulator for Phase 5.
use crate::classfile::constant_pool::{CpEntry, ConstantPool};
use crate::classfile::instruction::{InsnKind, Instruction};
use crate::classfile::opcodes::opc;
use crate::ir::expr::{
    BinOp, CastKind, ConstExpr, ConstValue, Expr, FieldDir,
    InvokeKind, LocalVarExpr, NewKind, UnOp,
};
use crate::types::java_type::JavaType;

// ── SlotInfo ─────────────────────────────────────────────────────────────

/// A typed slot on the operand stack.
#[derive(Debug, Clone)]
pub struct SlotInfo {
    pub expr: Expr,
    pub ty:   JavaType,
}

impl SlotInfo {
    fn new(expr: Expr, ty: JavaType) -> Self { SlotInfo { expr, ty } }
}

// ── SimResult ────────────────────────────────────────────────────────────

/// Output of simulating one basic block.
#[derive(Debug)]
pub struct SimResult {
    /// Side-effecting statements emitted in order.
    pub stmts:    Vec<Expr>,
    /// Residual operand stack at block exit (for successor merge).
    pub stack_out: Vec<SlotInfo>,
    /// Local variable assignments produced (slot → Expr).
    pub locals:   Vec<(u16, Expr, JavaType)>,
}

// ── OperandStack ─────────────────────────────────────────────────────────

struct OperandStack {
    slots: Vec<SlotInfo>,
}

impl OperandStack {
    fn new() -> Self { OperandStack { slots: Vec::new() } }

    fn push(&mut self, expr: Expr, ty: JavaType) {
        self.slots.push(SlotInfo::new(expr, ty));
    }

    fn pop(&mut self) -> Option<SlotInfo> {
        self.slots.pop()
    }

    fn pop_expr(&mut self) -> Expr {
        self.pop().map(|s| s.expr).unwrap_or(Expr::Opaque { opcode: 0, offset: 0 })
    }

    fn pop2(&mut self) -> (Expr, Expr) {
        let b = self.pop_expr();
        let a = self.pop_expr();
        (a, b)
    }

    fn peek(&self) -> Option<&SlotInfo> { self.slots.last() }

    fn peek_mut(&mut self) -> Option<&mut SlotInfo> { self.slots.last_mut() }

    fn dup(&mut self) {
        if let Some(top) = self.slots.last().cloned() {
            self.slots.push(top);
        }
    }

    fn dup_x1(&mut self) {
        if self.slots.len() >= 2 {
            let top = self.slots.pop().unwrap();
            let second = self.slots.pop().unwrap();
            self.slots.push(top.clone());
            self.slots.push(second);
            self.slots.push(top);
        }
    }

    fn dup_x2(&mut self) {
        if self.slots.len() >= 3 {
            let top = self.slots.pop().unwrap();
            let second = self.slots.pop().unwrap();
            let third = self.slots.pop().unwrap();
            self.slots.push(top.clone());
            self.slots.push(third);
            self.slots.push(second);
            self.slots.push(top);
        }
    }

    fn dup2(&mut self) {
        if self.slots.len() >= 2 {
            let top    = self.slots[self.slots.len()-1].clone();
            let second = self.slots[self.slots.len()-2].clone();
            self.slots.push(second);
            self.slots.push(top);
        }
    }

    fn swap(&mut self) {
        let n = self.slots.len();
        if n >= 2 {
            self.slots.swap(n-1, n-2);
        }
    }

    fn drain(self) -> Vec<SlotInfo> { self.slots }
}

// ── LocalVars ────────────────────────────────────────────────────────────

/// Tracks the last-known type and debug name for each local slot.
struct LocalVars {
    slots: std::collections::HashMap<u16, (JavaType, Option<String>)>,
}

impl LocalVars {
    fn new() -> Self { LocalVars { slots: std::collections::HashMap::new() } }

    fn set(&mut self, slot: u16, ty: JavaType, name: Option<String>) {
        self.slots.insert(slot, (ty, name));
    }

    fn get_ty(&self, slot: u16) -> JavaType {
        self.slots.get(&slot)
            .map(|(t,_)| t.clone())
            .unwrap_or(JavaType::UNKNOWN)
    }

    fn get_name(&self, slot: u16) -> Option<String> {
        self.slots.get(&slot).and_then(|(_,n)| n.clone())
    }
}

// ── Parameter type seeding ────────────────────────────────────────────────

thread_local! {
    /// `(slot, JavaType, Option<name>)` for the current method's parameters,
    /// derived from its descriptor.  Seeded by `set_param_types` before a
    /// method body is rendered.  Needed so `iload` of a
    /// `boolean`/`byte`/`short`/`char` parameter recovers its true type
    /// instead of defaulting to `int` — which in turn lets conditions render
    /// as `!flag` rather than `flag == 0`.  The name keeps the body's
    /// identifiers in sync with the rendered method signature.
    static PARAM_TYPES: std::cell::RefCell<Vec<(u16, JavaType, Option<String>)>> =
        std::cell::RefCell::new(Vec::new());
}

/// Record the parameter slot→type mapping for the method about to be simulated.
/// Pass the method descriptor and whether the method is static.
pub fn set_param_types(descriptor: &str, is_static: bool) {
    set_param_types_named(descriptor, is_static, &[]);
}

/// Like `set_param_types`, but also records display names for each parameter
/// (indexed by parameter position, not slot).  Pass the same names used to
/// render the method signature so the body agrees with the declaration.
pub fn set_param_types_named(descriptor: &str, is_static: bool, param_names: &[String]) {
    let mut out: Vec<(u16, JavaType, Option<String>)> = Vec::new();
    let mut ret: Option<JavaType> = None;
    if let Ok(md) = crate::types::descriptor::MethodDescriptor::parse(descriptor) {
        let mut slot: u16 = if is_static { 0 } else { 1 };
        for (i, p) in md.params.iter().enumerate() {
            out.push((slot, p.clone(), param_names.get(i).cloned()));
            // long/double occupy two slots
            slot += if *p == JavaType::LONG || *p == JavaType::DOUBLE { 2 } else { 1 };
        }
        ret = Some(md.return_type.clone());
    }
    PARAM_TYPES.with(|c| *c.borrow_mut() = out);
    RETURN_TYPE.with(|c| *c.borrow_mut() = ret);
}

/// Add local-variable types from the LocalVariableTable, so declared locals
/// (not just parameters) recover their true primitive type.  Existing entries
/// for the same slot are replaced.  Call after `set_param_types_named`.
pub fn add_local_types(entries: &[(u16, String, String)]) {
    PARAM_TYPES.with(|c| {
        let mut v = c.borrow_mut();
        for (slot, name, desc) in entries {
            let ty = crate::types::descriptor::parse_field_descriptor(desc)
                .map(|(t, _)| t)
                .unwrap_or(JavaType::UNKNOWN);
            if ty == JavaType::UNKNOWN { continue; }
            if let Some(existing) = v.iter_mut().find(|(s, _, _)| s == slot) {
                existing.1 = ty;
                existing.2 = Some(name.clone());
            } else {
                v.push((*slot, ty, Some(name.clone())));
            }
        }
    });
}

/// Clear the recorded parameter types (call after a method body is done).
pub fn clear_param_types() {
    PARAM_TYPES.with(|c| c.borrow_mut().clear());
    RETURN_TYPE.with(|c| *c.borrow_mut() = None);
}

thread_local! {
    /// Return type of the method currently being rendered.  Lets `ireturn 0`
    /// render as `return false` in a `boolean` method instead of `return 0`.
    static RETURN_TYPE: std::cell::RefCell<Option<JavaType>> =
        std::cell::RefCell::new(None);
}

/// True when the method currently being rendered returns `boolean`.
pub fn current_return_is_boolean() -> bool {
    RETURN_TYPE.with(|c| c.borrow().as_ref() == Some(&JavaType::BOOLEAN))
}

thread_local! {
    /// Bootstrap-method recipes for `makeConcatWithConstants`.
    /// Key = bootstrap_attr_index, value = resolved recipe string.
    /// Set once per ClassFile (before any method bodies are rendered) by
    /// `set_concat_recipes`; cleared when the ClassFile finishes.
    static CONCAT_RECIPES: std::cell::RefCell<std::collections::HashMap<u16, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Populate the per-ClassFile recipe table from its BootstrapMethods attribute.
pub fn set_concat_recipes(
    cf:   &crate::classfile::classfile::ClassFile,
    pool: &crate::classfile::constant_pool::ConstantPool,
) {
    use crate::classfile::attribute::Attribute;
    use crate::classfile::constant_pool::CpEntry;

    let mut map = std::collections::HashMap::new();
    for attr in &cf.attributes {
        if let Attribute::BootstrapMethods(bsm_list) = attr {
            for (idx, bsm) in bsm_list.iter().enumerate() {
                if let Some(&str_idx) = bsm.arguments.first() {
                    if let Ok(CpEntry::String(s)) = pool.get(str_idx) {
                        map.insert(idx as u16, s.clone());
                    }
                }
            }
        }
    }
    CONCAT_RECIPES.with(|c| *c.borrow_mut() = map);
}

/// Look up the recipe string for the given bootstrap_attr_index.
pub fn get_concat_recipe(idx: u16) -> Option<String> {
    CONCAT_RECIPES.with(|c| c.borrow().get(&idx).cloned())
}

// ── Public entry point ────────────────────────────────────────────────────

/// Simulate a single basic block given an initial operand stack.
///
/// `pool` is used to resolve constant-pool references.
/// `initial_stack` carries values from predecessor blocks (may be empty).
/// `local_names` is a list of `(slot, name)` pairs from `LocalVariableTable`;
/// pass `&[]` when no debug info is available.
pub fn simulate_block(
    instructions: &[Instruction],
    pool: &ConstantPool,
    initial_stack: Vec<SlotInfo>,
    is_static: bool,
    this_class: &str,
    local_names: &[(u16, String)],
) -> SimResult {
    let mut stack  = OperandStack { slots: initial_stack };
    let mut locals = LocalVars::new();
    let mut stmts: Vec<Expr> = Vec::new();
    let mut local_assignments: Vec<(u16, Expr, JavaType)> = Vec::new();

    // Seed slot 0 for instance methods
    if !is_static {
        locals.set(0, JavaType::object(this_class), Some("this".into()));
    }
    // Seed parameter types from the method descriptor.  Do this BEFORE the
    // LVT name pass so names (which carry UNKNOWN types) don't clobber them.
    PARAM_TYPES.with(|c| {
        for (slot, ty, name) in c.borrow().iter() {
            locals.set(*slot, ty.clone(), name.clone());
        }
    });
    // Seed names from LocalVariableTable, preserving any type already known.
    for (slot, name) in local_names {
        let existing = locals.get_ty(*slot);
        locals.set(*slot, existing, Some(name.clone()));
    }

    for insn in instructions {
        step(insn, pool, &mut stack, &mut locals,
             &mut stmts, &mut local_assignments, this_class);
    }

    SimResult {
        stmts,
        stack_out: stack.drain(),
        locals: local_assignments,
    }
}

// ── Instruction dispatch ──────────────────────────────────────────────────

#[allow(non_upper_case_globals)]
fn step(
    insn:    &Instruction,
    pool:    &ConstantPool,
    stack:   &mut OperandStack,
    locals:  &mut LocalVars,
    stmts:   &mut Vec<Expr>,
    local_assignments: &mut Vec<(u16, Expr, JavaType)>,
    _this_class: &str,
) {
    use opc::*;
    let op = insn.opcode;

    match op {
        // ── constants ────────────────────────────────────────────────
        nop => {}
        aconst_null => stack.push(Expr::Null, JavaType::NULL),
        iconst_m1   => push_int(stack, -1),
        iconst_0    => push_int(stack, 0),
        iconst_1    => push_int(stack, 1),
        iconst_2    => push_int(stack, 2),
        iconst_3    => push_int(stack, 3),
        iconst_4    => push_int(stack, 4),
        iconst_5    => push_int(stack, 5),
        lconst_0    => push_long(stack, 0),
        lconst_1    => push_long(stack, 1),
        fconst_0    => push_float(stack, 0.0),
        fconst_1    => push_float(stack, 1.0),
        fconst_2    => push_float(stack, 2.0),
        dconst_0    => push_double(stack, 0.0),
        dconst_1    => push_double(stack, 1.0),
        bipush => if let InsnKind::BytePush { value } = insn.kind { push_int(stack, value as i32); },
        sipush => if let InsnKind::ShortPush { value } = insn.kind { push_int(stack, value as i32); },

        // ── ldc variants ────────────────────────────────────────────
        ldc | ldc_w | ldc2_w => lift_ldc(insn, pool, stack),

        // ── loads ────────────────────────────────────────────────────
        // `iload` covers boolean/byte/short/char/int at the bytecode level.
        // push_local prefers the slot's known type (seeded from the method
        // descriptor / LVT), falling back to int only when unknown.
        iload | iload_0 | iload_1 | iload_2 | iload_3 => {
            let slot = local_slot(op, insn, iload, iload_0);
            push_local(stack, locals, slot, JavaType::INT);
        }
        lload | lload_0 | lload_1 | lload_2 | lload_3 => {
            let slot = local_slot(op, insn, lload, lload_0);
            push_local(stack, locals, slot, JavaType::LONG);
        }
        fload | fload_0 | fload_1 | fload_2 | fload_3 => {
            let slot = local_slot(op, insn, fload, fload_0);
            push_local(stack, locals, slot, JavaType::FLOAT);
        }
        dload | dload_0 | dload_1 | dload_2 | dload_3 => {
            let slot = local_slot(op, insn, dload, dload_0);
            push_local(stack, locals, slot, JavaType::DOUBLE);
        }
        aload | aload_0 | aload_1 | aload_2 | aload_3 => {
            let slot = local_slot(op, insn, aload, aload_0);
            let ty = locals.get_ty(slot);
            let ty = if ty == JavaType::UNKNOWN { JavaType::object("java/lang/Object") } else { ty };
            push_local(stack, locals, slot, ty);
        }

        // ── array loads ──────────────────────────────────────────────
        iaload => array_load(stack, JavaType::INT),
        laload => array_load(stack, JavaType::LONG),
        faload => array_load(stack, JavaType::FLOAT),
        daload => array_load(stack, JavaType::DOUBLE),
        aaload => array_load(stack, JavaType::object("java/lang/Object")),
        baload => array_load(stack, JavaType::BYTE),
        caload => array_load(stack, JavaType::CHAR),
        saload => array_load(stack, JavaType::SHORT),

        // ── stores ───────────────────────────────────────────────────
        istore | istore_0 | istore_1 | istore_2 | istore_3 => {
            let slot = local_slot(op, insn, istore, istore_0);
            store_local(stack, locals, stmts, local_assignments, slot, JavaType::INT);
        }
        lstore | lstore_0 | lstore_1 | lstore_2 | lstore_3 => {
            let slot = local_slot(op, insn, lstore, lstore_0);
            store_local(stack, locals, stmts, local_assignments, slot, JavaType::LONG);
        }
        fstore | fstore_0 | fstore_1 | fstore_2 | fstore_3 => {
            let slot = local_slot(op, insn, fstore, fstore_0);
            store_local(stack, locals, stmts, local_assignments, slot, JavaType::FLOAT);
        }
        dstore | dstore_0 | dstore_1 | dstore_2 | dstore_3 => {
            let slot = local_slot(op, insn, dstore, dstore_0);
            store_local(stack, locals, stmts, local_assignments, slot, JavaType::DOUBLE);
        }
        astore | astore_0 | astore_1 | astore_2 | astore_3 => {
            let slot = local_slot(op, insn, astore, astore_0);
            let ty = stack.peek().map(|s| s.ty.clone())
                .unwrap_or_else(|| JavaType::object("java/lang/Object"));
            store_local(stack, locals, stmts, local_assignments, slot, ty);
        }

        // ── array stores ─────────────────────────────────────────────
        iastore | lastore | fastore | dastore |
        aastore | bastore | castore | sastore => {
            let value = stack.pop_expr();
            let index = stack.pop_expr();
            let array = stack.pop_expr();

            // Array-initializer / varargs idiom: javac emits
            //   anewarray; (dup; iconst_<i>; <value>; aastore)*
            // so at this point `array` is the duplicated NewArray and the
            // original is still beneath us on the stack.  Accumulate the
            // element into that allocation instead of emitting a standalone
            // `arr[i] = v;` statement, which would reference a temporary that
            // has no name in the source.
            if matches!(array, Expr::NewArray { .. }) {
                if let Some(top) = stack.peek_mut() {
                    if let Expr::NewArray { initializer, .. } = &mut top.expr {
                        let idx = const_int_value(&index);
                        let elems = initializer.get_or_insert_with(Vec::new);
                        match idx {
                            // Place at the literal index, padding if javac
                            // emitted them out of order.
                            Some(i) if i >= 0 => {
                                let i = i as usize;
                                while elems.len() <= i {
                                    elems.push(Expr::Const(ConstExpr {
                                        value: ConstValue::Null, ty: JavaType::UNKNOWN,
                                    }));
                                }
                                elems[i] = value;
                            }
                            _ => elems.push(value),
                        }
                        return;
                    }
                }
            }

            stmts.push(Expr::ArrayStore {
                array: Box::new(array),
                index: Box::new(index),
                value: Box::new(value),
            });
        }

        // ── stack manipulation ────────────────────────────────────────
        pop => {
            // If a value with side-effects is discarded, emit it as a stmt.
            if let Some(slot) = stack.pop() {
                if slot.expr.has_side_effects() {
                    stmts.push(slot.expr);
                }
            }
        }
        // pop2 removes one category-2 value (long/double) or two category-1 values.
        // In both cases the net effect on stack depth is -2 slots.
        pop2 => { stack.pop(); stack.pop(); }
        dup        => stack.dup(),
        dup_x1     => stack.dup_x1(),
        dup_x2     => stack.dup_x2(),
        dup2       => stack.dup2(),
        dup2_x1    => { stack.dup2(); }   // simplified
        dup2_x2    => { stack.dup2(); }   // simplified
        swap       => stack.swap(),

        // ── arithmetic ───────────────────────────────────────────────
        iadd | ladd | fadd | dadd => binop(stack, BinOp::Add, arith_type(op)),
        isub | lsub | fsub | dsub => binop(stack, BinOp::Sub, arith_type(op)),
        imul | lmul | fmul | dmul => binop(stack, BinOp::Mul, arith_type(op)),
        idiv | ldiv | fdiv | ddiv => binop(stack, BinOp::Div, arith_type(op)),
        irem | lrem | frem | drem => binop(stack, BinOp::Rem, arith_type(op)),
        ineg | lneg | fneg | dneg => unop(stack, UnOp::Neg,   arith_type(op)),

        // ── shifts ───────────────────────────────────────────────────
        ishl | lshl => binop(stack, BinOp::Shl,  shift_type(op)),
        ishr | lshr => binop(stack, BinOp::Shr,  shift_type(op)),
        iushr | lushr => binop(stack, BinOp::Ushr, shift_type(op)),

        // ── bitwise ──────────────────────────────────────────────────
        iand | land => binop(stack, BinOp::And, bit_type(op)),
        ior  | lor  => binop(stack, BinOp::Or,  bit_type(op)),
        ixor | lxor => binop(stack, BinOp::Xor, bit_type(op)),

        // ── iinc ─────────────────────────────────────────────────────
        iinc => {
            if let InsnKind::Iinc { index, const_ } = insn.kind {
                let name = locals.get_name(index);
                stmts.push(Expr::IInc { slot: index, delta: const_, name });
            }
        }

        // ── casts ────────────────────────────────────────────────────
        i2l => cast(stack, CastKind::I2L, JavaType::LONG),
        i2f => cast(stack, CastKind::I2F, JavaType::FLOAT),
        i2d => cast(stack, CastKind::I2D, JavaType::DOUBLE),
        l2i => cast(stack, CastKind::L2I, JavaType::INT),
        l2f => cast(stack, CastKind::L2F, JavaType::FLOAT),
        l2d => cast(stack, CastKind::L2D, JavaType::DOUBLE),
        f2i => cast(stack, CastKind::F2I, JavaType::INT),
        f2l => cast(stack, CastKind::F2L, JavaType::LONG),
        f2d => cast(stack, CastKind::F2D, JavaType::DOUBLE),
        d2i => cast(stack, CastKind::D2I, JavaType::INT),
        d2l => cast(stack, CastKind::D2L, JavaType::LONG),
        d2f => cast(stack, CastKind::D2F, JavaType::FLOAT),
        i2b => cast(stack, CastKind::I2B, JavaType::BYTE),
        i2c => cast(stack, CastKind::I2C, JavaType::CHAR),
        i2s => cast(stack, CastKind::I2S, JavaType::SHORT),

        // ── comparisons ──────────────────────────────────────────────
        lcmp  => binop(stack, BinOp::LCmp,  JavaType::INT),
        fcmpl => binop(stack, BinOp::FCmpL, JavaType::INT),
        fcmpg => binop(stack, BinOp::FCmpG, JavaType::INT),
        dcmpl => binop(stack, BinOp::DCmpL, JavaType::INT),
        dcmpg => binop(stack, BinOp::DCmpG, JavaType::INT),

        // ── returns ──────────────────────────────────────────────────
        ireturn | lreturn | freturn | dreturn | areturn => {
            let val = stack.pop_expr();
            stmts.push(Expr::Return(Some(Box::new(val))));
        }
        r#return => stmts.push(Expr::Return(None)),

        // ── field access ─────────────────────────────────────────────
        getstatic => lift_field(insn, pool, stack, FieldDir::Get, true),
        putstatic => {
            // Same as putfield: lift_field pushes a Field{Put} node, then we pop
            // and emit it as a side-effecting statement. Without the pop the node
            // sits on the stack and gets consumed as an operand of the next instruction.
            lift_field(insn, pool, stack, FieldDir::Put, true);
            if let Some(e) = stack.pop() { stmts.push(e.expr); }
        }
        getfield  => lift_field(insn, pool, stack, FieldDir::Get, false),
        putfield  => {
            lift_field(insn, pool, stack, FieldDir::Put, false);
            // putfield produces a side effect — emit it
            if let Some(e) = stack.pop() { stmts.push(e.expr); }
        }

        // ── method invocations ────────────────────────────────────────
        invokevirtual | invokespecial | invokestatic | invokeinterface => {
            lift_invoke(insn, pool, stack, stmts, op, _this_class);
        }
        invokedynamic => lift_invokedynamic(insn, pool, stack, stmts),

        // ── new / newarray ────────────────────────────────────────────
        new => lift_new(insn, pool, stack),
        newarray => {
            if let InsnKind::NewArray { atype } = insn.kind {
                let count = stack.pop_expr();
                let ty = primitive_array_type(atype);
                stack.push(Expr::NewArray {
                    kind: NewKind::PrimitiveArray { atype },
                    type_: ty,
                    dimensions: vec![count],
                    initializer: None,
                }, JavaType::object("[primitive"));
            }
        }
        anewarray => {
            if let InsnKind::Cp { index } = insn.kind {
                let count = stack.pop_expr();
                let elem_name = pool.class_name(index).unwrap_or("java/lang/Object").to_string();
                let ty = JavaType::object(&elem_name);
                stack.push(Expr::NewArray {
                    kind: NewKind::RefArray,
                    type_: ty,
                    dimensions: vec![count],
                    initializer: None,
                }, JavaType::object(&elem_name).array_of());
            }
        }
        multianewarray => {
            if let InsnKind::MultiNewArray { index, dimensions } = insn.kind {
                let mut dims = Vec::new();
                for _ in 0..dimensions { dims.push(stack.pop_expr()); }
                dims.reverse();
                let class_name = pool.class_name(index).unwrap_or("java/lang/Object").to_string();
                stack.push(Expr::NewArray {
                    kind: NewKind::MultiArray { dims: dimensions },
                    type_: JavaType::object(&class_name),
                    dimensions: dims,
                    initializer: None,
                }, JavaType::object(&class_name));
            }
        }

        // ── misc ──────────────────────────────────────────────────────
        arraylength => {
            let arr = stack.pop_expr();
            stack.push(Expr::ArrayLength(Box::new(arr)), JavaType::INT);
        }
        athrow => {
            let exc = stack.pop_expr();
            stmts.push(Expr::Throw(Box::new(exc)));
        }
        checkcast => {
            if let InsnKind::Cp { index } = insn.kind {
                let obj = stack.pop_expr();
                let name = pool.class_name(index).unwrap_or("java/lang/Object").to_string();
                let ty = JavaType::object(&name);
                stack.push(Expr::Cast(CastKind::CheckCast, ty.clone(), Box::new(obj)), ty);
            }
        }
        instanceof => {
            if let InsnKind::Cp { index } = insn.kind {
                let obj = stack.pop_expr();
                let name = pool.class_name(index).unwrap_or("java/lang/Object").to_string();
                stack.push(
                    Expr::InstanceOf(Box::new(obj), JavaType::object(&name)),
                    JavaType::BOOLEAN,
                );
            }
        }
        monitorenter => {
            let obj = stack.pop_expr();
            stmts.push(Expr::Monitor { enter: true,  object: Box::new(obj) });
        }
        monitorexit => {
            let obj = stack.pop_expr();
            stmts.push(Expr::Monitor { enter: false, object: Box::new(obj) });
        }

        // ── branches — we emit nothing; handled by cfg/recovery ───────
        ifeq | ifne | iflt | ifge | ifgt | ifle |
        if_icmpeq | if_icmpne | if_icmplt | if_icmpge | if_icmpgt | if_icmple |
        if_acmpeq | if_acmpne | ifnull | ifnonnull |
        goto | goto_w | jsr | jsr_w | ret |
        tableswitch | lookupswitch => {}

        _ => {}
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn push_int(stack: &mut OperandStack, v: i32) {
    stack.push(Expr::Const(ConstExpr { value: ConstValue::Int(v), ty: JavaType::INT }), JavaType::INT);
}
fn push_long(stack: &mut OperandStack, v: i64) {
    stack.push(Expr::Const(ConstExpr { value: ConstValue::Long(v), ty: JavaType::LONG }), JavaType::LONG);
}
fn push_float(stack: &mut OperandStack, v: f32) {
    stack.push(Expr::Const(ConstExpr { value: ConstValue::Float(v), ty: JavaType::FLOAT }), JavaType::FLOAT);
}
fn push_double(stack: &mut OperandStack, v: f64) {
    stack.push(Expr::Const(ConstExpr { value: ConstValue::Double(v), ty: JavaType::DOUBLE }), JavaType::DOUBLE);
}

fn push_local(stack: &mut OperandStack, locals: &LocalVars, slot: u16, default_ty: JavaType) {
    let ty   = locals.get_ty(slot);
    let ty   = if ty == JavaType::UNKNOWN { default_ty.clone() } else { ty };
    let name = locals.get_name(slot);
    stack.push(Expr::LocalVar(LocalVarExpr { slot, ty: ty.clone(), name }), ty);
}

fn store_local(
    stack:  &mut OperandStack,
    locals: &mut LocalVars,
    stmts:  &mut Vec<Expr>,
    local_assignments: &mut Vec<(u16, Expr, JavaType)>,
    slot:   u16,
    ty:     JavaType,
) {
    let val = stack.pop_expr();
    // Preserve the debug name from LVT if already set.
    let existing_name = locals.get_name(slot);
    // `istore` is used for boolean/byte/short/char/int alike.  If we already
    // know a narrower type for this slot (seeded from the LVT or the method
    // descriptor), keep it rather than widening everything back to int.
    let existing_ty = locals.get_ty(slot);
    let ty = if ty == JavaType::INT && is_int_like(&existing_ty) {
        existing_ty
    } else {
        ty
    };
    locals.set(slot, ty.clone(), existing_name);
    let lv  = Expr::LocalVar(LocalVarExpr { slot, ty: ty.clone(), name: locals.get_name(slot) });
    let assign = Expr::Assign { lhs: Box::new(lv), rhs: Box::new(val.clone()) };
    stmts.push(assign);
    local_assignments.push((slot, val, ty));
}

/// The integer value of a constant expression, if it is one.
fn const_int_value(expr: &Expr) -> Option<i32> {
    if let Expr::Const(c) = expr {
        if let ConstValue::Int(i) = c.value { return Some(i); }
    }
    None
}

/// True for the types that share the JVM's `int` computational type, so an
/// `istore`/`iload` against such a slot should not clobber the narrower type.
fn is_int_like(ty: &JavaType) -> bool {
    *ty == JavaType::BOOLEAN || *ty == JavaType::BYTE
        || *ty == JavaType::CHAR || *ty == JavaType::SHORT
}

fn binop(stack: &mut OperandStack, op: BinOp, ty: JavaType) {
    let (a, b) = stack.pop2();
    stack.push(Expr::BinOp(op, Box::new(a), Box::new(b)), ty);
}

fn unop(stack: &mut OperandStack, op: UnOp, ty: JavaType) {
    let a = stack.pop_expr();
    stack.push(Expr::UnOp(op, Box::new(a)), ty);
}

fn cast(stack: &mut OperandStack, kind: CastKind, ty: JavaType) {
    let a = stack.pop_expr();
    stack.push(Expr::Cast(kind, ty.clone(), Box::new(a)), ty);
}

fn array_load(stack: &mut OperandStack, elem_ty: JavaType) {
    let index = stack.pop_expr();
    let array = stack.pop_expr();
    stack.push(Expr::ArrayLoad {
        array:     Box::new(array),
        index:     Box::new(index),
        elem_type: elem_ty.clone(),
    }, elem_ty);
}

/// Resolve the local-variable slot for a normal or short-form load/store opcode.
fn local_slot(op: u8, insn: &Instruction, base_op: u8, base_short: u8) -> u16 {
    if op == base_op {
        if let InsnKind::LocalVar { index } = insn.kind { return index; }
    }
    // short forms: base_short+0 .. base_short+3
    (op - base_short) as u16
}

/// Arithmetic result type from an opcode.
fn arith_type(op: u8) -> JavaType {
    use opc::*;
    match op {
        ladd | lsub | lmul | ldiv | lrem | lneg => JavaType::LONG,
        fadd | fsub | fmul | fdiv | frem | fneg => JavaType::FLOAT,
        dadd | dsub | dmul | ddiv | drem | dneg => JavaType::DOUBLE,
        _                                        => JavaType::INT,
    }
}

fn shift_type(op: u8) -> JavaType {
    use opc::*;
    if op == lshl || op == lshr || op == lushr { JavaType::LONG } else { JavaType::INT }
}

fn bit_type(op: u8) -> JavaType {
    use opc::*;
    if op == land || op == lor || op == lxor { JavaType::LONG } else { JavaType::INT }
}

fn primitive_array_type(atype: u8) -> JavaType {
    match atype {
        4 => JavaType::BOOLEAN, 5 => JavaType::CHAR,  6 => JavaType::FLOAT,
        7 => JavaType::DOUBLE,  8 => JavaType::BYTE,  9 => JavaType::SHORT,
        10=> JavaType::INT,    11 => JavaType::LONG,
        _  => JavaType::INT,
    }
}

// ── ldc lifting ───────────────────────────────────────────────────────────

fn lift_ldc(insn: &Instruction, pool: &ConstantPool, stack: &mut OperandStack) {
    let index = match insn.kind {
        InsnKind::Ldc { index } => index,
        _ => return,
    };
    match pool.get(index) {
        Ok(CpEntry::Integer(v)) => push_int(stack, *v),
        Ok(CpEntry::Float(v))   => push_float(stack, *v),
        Ok(CpEntry::Long(v))    => push_long(stack, *v),
        Ok(CpEntry::Double(v))  => push_double(stack, *v),
        Ok(CpEntry::String(s))  => {
            stack.push(Expr::Const(ConstExpr {
                value: ConstValue::StringRef(s.clone()),
                ty:    JavaType::object("java/lang/String"),
            }), JavaType::object("java/lang/String"));
        }
        Ok(CpEntry::Class(n)) => {
            stack.push(Expr::Const(ConstExpr {
                value: ConstValue::ClassRef(n.clone()),
                ty:    JavaType::object("java/lang/Class"),
            }), JavaType::object("java/lang/Class"));
        }
        _ => stack.push(Expr::Opaque { opcode: insn.opcode, offset: insn.offset }, JavaType::UNKNOWN),
    }
}

// ── field access ──────────────────────────────────────────────────────────

fn lift_field(
    insn: &Instruction, pool: &ConstantPool,
    stack: &mut OperandStack, dir: FieldDir, is_static: bool,
) {
    let index = match insn.kind { InsnKind::Cp { index } => index, _ => return };
    let (owner, name, descriptor) = match pool.get(index) {
        Ok(CpEntry::Fieldref(mr)) =>
            (mr.class_name.clone(), mr.name.clone(), mr.descriptor.clone()),
        _ => return,
    };
    let field_ty = crate::types::descriptor::parse_field_descriptor(&descriptor)
        .map(|(t, _)| t).unwrap_or(JavaType::UNKNOWN);
    match dir {
        FieldDir::Get => {
            let object = if is_static { None } else { Some(Box::new(stack.pop_expr())) };
            stack.push(Expr::Field { dir: FieldDir::Get, owner, name, descriptor,
                object, value: None }, field_ty);
        }
        FieldDir::Put => {
            let value  = stack.pop_expr();
            let object = if is_static { None } else { Some(Box::new(stack.pop_expr())) };
            // putstatic emits directly as statement; caller handles pop
            stack.push(Expr::Field { dir: FieldDir::Put, owner, name, descriptor,
                object, value: Some(Box::new(value)) }, JavaType::VOID);
        }
    }
}

// ── method invocation ─────────────────────────────────────────────────────

fn lift_invoke(
    insn: &Instruction, pool: &ConstantPool,
    stack: &mut OperandStack, stmts: &mut Vec<Expr>, op: u8,
    this_class: &str,
) {
    use opc::*;
    let index = match insn.kind {
        InsnKind::Invoke { index, .. } => index,
        _ => return,
    };
    let mr = match pool.get(index) {
        Ok(CpEntry::Methodref(mr)) | Ok(CpEntry::InterfaceMethodref(mr)) => mr.clone(),
        _ => return,
    };

    let kind = match op {
        invokestatic    => InvokeKind::Static,
        invokespecial   => InvokeKind::Special,
        invokeinterface => InvokeKind::Interface,
        _               => InvokeKind::Virtual,
    };

    let md = crate::types::descriptor::MethodDescriptor::parse(&mr.descriptor)
        .unwrap_or_else(|_| crate::types::descriptor::MethodDescriptor {
            params: vec![], return_type: JavaType::VOID,
        });

    // Pop args in reverse
    let mut args: Vec<Expr> = (0..md.params.len()).map(|_| stack.pop_expr()).collect();
    args.reverse();

    // For <init> calls, pop the receiver and try to merge with a pending `new`.
    if mr.name == "<init>" {
        let receiver = stack.pop_expr();

        match receiver {
            // ── new Foo() pattern ─────────────────────────────────────
            // The bytecode is: new Foo / dup / invokespecial <init>
            // After `dup` there are two copies of the New{} placeholder.
            // We consumed one as the receiver; pop the other (dup'd copy)
            // and push back a fully-completed New node.
            Expr::New { class_name, .. } => {
                // Discard the dup'd bottom copy (same placeholder).
                stack.pop();
                let completed = Expr::New {
                    class_name,
                    args,
                    descriptor: mr.descriptor,
                };
                // Push the initialised object reference back; whoever stores
                // it (astore / putfield / etc.) will consume it from the stack.
                let obj_ty = JavaType::object(&mr.class_name);
                stack.push(completed, obj_ty);
            }

            // ── super() / this() constructor chain call ───────────────
            Expr::LocalVar(ref lv) if lv.slot == 0 => {
                let call_name = if mr.class_name == this_class { "this" } else { "super" };
                // Only emit if this isn't a plain Object.<init>() with no args
                // (the implicit default-constructor super call that adds no info).
                let is_trivial_object_init =
                    mr.class_name == "java/lang/Object" && args.is_empty();
                if !is_trivial_object_init {
                    stmts.push(Expr::Invoke {
                        kind:       InvokeKind::Special,
                        owner:      mr.class_name,
                        name:       call_name.into(),
                        descriptor: mr.descriptor,
                        object:     None,
                        args,
                    });
                }
            }

            // ── other (inline <init> on non-new receiver) ─────────────
            other => {
                stmts.push(Expr::Invoke {
                    kind:       InvokeKind::Special,
                    owner:      mr.class_name,
                    name:       mr.name,
                    descriptor: mr.descriptor,
                    object:     Some(Box::new(other)),
                    args,
                });
            }
        }
        return;
    }

    let object = if kind == InvokeKind::Static { None }
                 else { Some(Box::new(stack.pop_expr())) };

    let ret_ty = md.return_type.clone();
    let expr = Expr::Invoke {
        kind, owner: mr.class_name, name: mr.name, descriptor: mr.descriptor,
        object, args,
    };

    if ret_ty.is_void() {
        stmts.push(expr);
    } else {
        stack.push(expr, ret_ty);
    }
}

fn lift_invokedynamic(
    insn: &Instruction, pool: &ConstantPool,
    stack: &mut OperandStack, stmts: &mut Vec<Expr>,
) {
    let (bootstrap_index, name, descriptor) = match insn.kind {
        InsnKind::InvokeDynamic { index } => {
            match pool.get(index) {
                Ok(CpEntry::InvokeDynamic { bootstrap_attr_index, name, descriptor }) =>
                    (*bootstrap_attr_index, name.clone(), descriptor.clone()),
                _ => return,
            }
        }
        _ => return,
    };

    let md = crate::types::descriptor::MethodDescriptor::parse(&descriptor)
        .unwrap_or_else(|_| crate::types::descriptor::MethodDescriptor {
            params: vec![], return_type: JavaType::VOID,
        });

    let mut args: Vec<Expr> = (0..md.params.len()).map(|_| stack.pop_expr()).collect();
    args.reverse();

    let ret_ty = md.return_type.clone();
    let expr = Expr::InvokeDynamic { name, descriptor, bootstrap_index, args };

    if ret_ty.is_void() { stmts.push(expr); } else { stack.push(expr, ret_ty); }
}

// ── new object ────────────────────────────────────────────────────────────

fn lift_new(insn: &Instruction, pool: &ConstantPool, stack: &mut OperandStack) {
    let index = match insn.kind { InsnKind::Cp { index } => index, _ => return };
    let class_name = pool.class_name(index).unwrap_or("java/lang/Object").to_string();
    // We push a placeholder; the <init> call will complete the `new` node.
    stack.push(Expr::New {
        class_name: class_name.clone(),
        args: vec![],
        descriptor: String::new(),
    }, JavaType::object(&class_name));
}
