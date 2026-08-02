#![allow(non_upper_case_globals)]
//! JVM operand stack simulator for Phase 5.
use crate::classfile::constant_pool::{ConstantPool, CpEntry};
use crate::classfile::instruction::{InsnKind, Instruction};
use crate::classfile::opcodes::opc;
use crate::ir::expr::{
    BinOp, CastKind, ConstExpr, ConstValue, Expr, FieldDir, InvokeKind, LambdaBootstrap,
    LocalVarExpr, NewKind, UnOp,
};
use crate::types::java_type::JavaType;

// ── SlotInfo ─────────────────────────────────────────────────────────────

/// A typed slot on the operand stack.
#[derive(Debug, Clone)]
pub struct SlotInfo {
    pub expr: Expr,
    pub ty: JavaType,
}

impl SlotInfo {
    fn new(expr: Expr, ty: JavaType) -> Self {
        SlotInfo { expr, ty }
    }

    fn is_category1(&self) -> bool {
        !matches!(self.ty, JavaType::LONG | JavaType::DOUBLE)
    }

    fn is_category2(&self) -> bool {
        !self.is_category1()
    }
}

// ── SimResult ────────────────────────────────────────────────────────────

/// Output of simulating one basic block.
#[derive(Debug)]
pub struct SimResult {
    /// Side-effecting statements emitted in order.
    pub stmts: Vec<Expr>,
    /// Residual operand stack at block exit (for successor merge).
    pub stack_out: Vec<SlotInfo>,
    /// Local variable assignments produced (slot → Expr).
    pub locals: Vec<(u16, Expr, JavaType)>,
    /// Recoverable verifier-style failures encountered while simulating.
    pub errors: Vec<SimulationError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulationErrorKind {
    StackUnderflow,
    InvalidStackForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimulationError {
    pub offset: u32,
    pub opcode: u8,
    pub kind: SimulationErrorKind,
}

/// Explicit method-level inputs used while simulating a basic block.
///
/// Keeping these values in the call graph makes concurrent decompilation
/// deterministic and avoids coupling simulation to ambient thread state.
pub struct SimulationContext<'a> {
    pub is_static: bool,
    pub this_class: &'a str,
    pub local_names: &'a [(u16, String)],
    pub local_scopes: &'a [LocalScope],
    pub local_types: &'a [(u16, JavaType, Option<String>)],
    pub return_type: Option<&'a JavaType>,
    pub concat_recipes: &'a std::collections::HashMap<u16, String>,
    pub lambda_bootstrap: &'a std::collections::HashMap<u16, LambdaBootstrap>,
}

#[derive(Debug, Clone)]
pub struct LocalScope {
    pub slot: u16,
    pub name: String,
    pub start_pc: u32,
    pub end_pc: u32,
}

// ── OperandStack ─────────────────────────────────────────────────────────

struct OperandStack {
    slots: Vec<SlotInfo>,
    errors: Vec<SimulationError>,
    offset: u32,
    opcode: u8,
}

impl OperandStack {
    fn begin_instruction(&mut self, instruction: &Instruction) {
        self.offset = instruction.offset;
        self.opcode = instruction.opcode;
    }

    fn report(&mut self, kind: SimulationErrorKind) {
        self.errors.push(SimulationError {
            offset: self.offset,
            opcode: self.opcode,
            kind,
        });
    }

    fn push(&mut self, expr: Expr, ty: JavaType) {
        self.slots.push(SlotInfo::new(expr, ty));
    }

    fn pop(&mut self) -> Option<SlotInfo> {
        let value = self.slots.pop();
        if value.is_none() {
            self.report(SimulationErrorKind::StackUnderflow);
        }
        value
    }

    fn pop_expr(&mut self) -> Expr {
        self.pop().map(|s| s.expr).unwrap_or(Expr::Opaque {
            opcode: self.opcode,
            offset: self.offset,
        })
    }

    fn pop2(&mut self) -> (Expr, Expr) {
        let b = self.pop_expr();
        let a = self.pop_expr();
        (a, b)
    }

    fn peek(&self) -> Option<&SlotInfo> {
        self.slots.last()
    }

    fn peek_mut(&mut self) -> Option<&mut SlotInfo> {
        self.slots.last_mut()
    }

    fn dup(&mut self) {
        match self.slots.last() {
            Some(top) if top.is_category1() => self.slots.push(top.clone()),
            Some(_) => self.report(SimulationErrorKind::InvalidStackForm),
            None => self.report(SimulationErrorKind::StackUnderflow),
        }
    }

    fn dup_x1(&mut self) {
        let len = self.slots.len();
        if len < 2 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 1].is_category1() && self.slots[len - 2].is_category1() {
            let top = self.slots.pop().unwrap();
            let second = self.slots.pop().unwrap();
            self.slots.push(top.clone());
            self.slots.push(second);
            self.slots.push(top);
        } else {
            self.report(SimulationErrorKind::InvalidStackForm);
        }
    }

    fn dup_x2(&mut self) {
        let len = self.slots.len();
        if len == 0 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 1].is_category2() {
            self.report(SimulationErrorKind::InvalidStackForm);
        } else if len == 1 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 2].is_category2() {
            let top = self.slots.pop().unwrap();
            let second = self.slots.pop().unwrap();
            self.slots.push(top.clone());
            self.slots.push(second);
            self.slots.push(top);
        } else if len < 3 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 3].is_category1() {
            let top = self.slots.pop().unwrap();
            let second = self.slots.pop().unwrap();
            let third = self.slots.pop().unwrap();
            self.slots.push(top.clone());
            self.slots.push(third);
            self.slots.push(second);
            self.slots.push(top);
        } else {
            self.report(SimulationErrorKind::InvalidStackForm);
        }
    }

    fn dup2(&mut self) {
        let len = self.slots.len();
        if len == 0 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 1].is_category2() {
            self.slots.push(self.slots[len - 1].clone());
        } else if len < 2 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 2].is_category1() {
            let top = self.slots[len - 1].clone();
            let second = self.slots[len - 2].clone();
            self.slots.push(second);
            self.slots.push(top);
        } else {
            self.report(SimulationErrorKind::InvalidStackForm);
        }
    }

    fn dup2_x1(&mut self) {
        let len = self.slots.len();
        if len == 0 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 1].is_category2() {
            if len < 2 {
                self.report(SimulationErrorKind::StackUnderflow);
                return;
            }
            if self.slots[len - 2].is_category2() {
                self.report(SimulationErrorKind::InvalidStackForm);
                return;
            }
            let top = self.slots.pop().unwrap();
            let second = self.slots.pop().unwrap();
            self.slots.push(top.clone());
            self.slots.push(second);
            self.slots.push(top);
        } else if len < 3 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 2].is_category1() && self.slots[len - 3].is_category1() {
            let first = self.slots.pop().unwrap();
            let second = self.slots.pop().unwrap();
            let third = self.slots.pop().unwrap();
            self.slots.push(second.clone());
            self.slots.push(first.clone());
            self.slots.push(third);
            self.slots.push(second);
            self.slots.push(first);
        } else {
            self.report(SimulationErrorKind::InvalidStackForm);
        }
    }

    fn dup2_x2(&mut self) {
        let len = self.slots.len();
        let widths: Vec<bool> = self
            .slots
            .iter()
            .rev()
            .take(4)
            .map(SlotInfo::is_category2)
            .collect();
        match widths.as_slice() {
            // ..., value2(cat2), value1(cat2)
            [true, true, ..] if len >= 2 => {
                let first = self.slots.pop().unwrap();
                let second = self.slots.pop().unwrap();
                self.slots.push(first.clone());
                self.slots.push(second);
                self.slots.push(first);
            }
            // ..., value3(cat1), value2(cat1), value1(cat2)
            [true, false, false, ..] if len >= 3 => {
                let first = self.slots.pop().unwrap();
                let second = self.slots.pop().unwrap();
                let third = self.slots.pop().unwrap();
                self.slots.push(first.clone());
                self.slots.push(third);
                self.slots.push(second);
                self.slots.push(first);
            }
            // ..., value3(cat2), value2(cat1), value1(cat1)
            [false, false, true, ..] if len >= 3 => {
                let first = self.slots.pop().unwrap();
                let second = self.slots.pop().unwrap();
                let third = self.slots.pop().unwrap();
                self.slots.push(second.clone());
                self.slots.push(first.clone());
                self.slots.push(third);
                self.slots.push(second);
                self.slots.push(first);
            }
            // ..., value4(cat1), value3(cat1), value2(cat1), value1(cat1)
            [false, false, false, false] if len >= 4 => {
                let first = self.slots.pop().unwrap();
                let second = self.slots.pop().unwrap();
                let third = self.slots.pop().unwrap();
                let fourth = self.slots.pop().unwrap();
                self.slots.push(second.clone());
                self.slots.push(first.clone());
                self.slots.push(fourth);
                self.slots.push(third);
                self.slots.push(second);
                self.slots.push(first);
            }
            _ if matches!(
                widths.as_slice(),
                [] | [true] | [false] | [true, false] | [false, false] | [false, false, false]
            ) =>
            {
                self.report(SimulationErrorKind::StackUnderflow);
            }
            _ => self.report(SimulationErrorKind::InvalidStackForm),
        }
    }

    fn pop2_values(&mut self) {
        let len = self.slots.len();
        if len == 0 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 1].is_category2() {
            self.slots.pop();
        } else if len < 2 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[len - 2].is_category1() {
            self.slots.truncate(len - 2);
        } else {
            self.report(SimulationErrorKind::InvalidStackForm);
        }
    }

    fn pop_value(&mut self) -> Option<SlotInfo> {
        match self.slots.last() {
            Some(value) if value.is_category1() => self.slots.pop(),
            Some(_) => {
                self.report(SimulationErrorKind::InvalidStackForm);
                None
            }
            None => {
                self.report(SimulationErrorKind::StackUnderflow);
                None
            }
        }
    }

    fn swap(&mut self) {
        let n = self.slots.len();
        if n < 2 {
            self.report(SimulationErrorKind::StackUnderflow);
        } else if self.slots[n - 1].is_category1() && self.slots[n - 2].is_category1() {
            self.slots.swap(n - 1, n - 2);
        } else {
            self.report(SimulationErrorKind::InvalidStackForm);
        }
    }

    fn finish(self) -> (Vec<SlotInfo>, Vec<SimulationError>) {
        (self.slots, self.errors)
    }
}

// ── LocalVars ────────────────────────────────────────────────────────────

/// Tracks the last-known type and debug name for each local slot.
struct LocalVars {
    slots: std::collections::HashMap<u16, (JavaType, Option<String>)>,
}

impl LocalVars {
    fn new() -> Self {
        LocalVars {
            slots: std::collections::HashMap::new(),
        }
    }

    fn set(&mut self, slot: u16, ty: JavaType, name: Option<String>) {
        self.slots.insert(slot, (ty, name));
    }

    fn get_ty(&self, slot: u16) -> JavaType {
        self.slots
            .get(&slot)
            .map(|(t, _)| t.clone())
            .unwrap_or(JavaType::UNKNOWN)
    }

    fn get_name(&self, slot: u16) -> Option<String> {
        self.slots.get(&slot).and_then(|(_, n)| n.clone())
    }

    fn set_name(&mut self, slot: u16, name: Option<String>) {
        let ty = self.get_ty(slot);
        self.set(slot, ty, name);
    }

    fn refresh_scoped_names(&mut self, scopes: &[LocalScope], offset: u32) {
        let slots = scopes
            .iter()
            .map(|scope| scope.slot)
            .collect::<std::collections::HashSet<_>>();
        for slot in slots {
            let name = scopes
                .iter()
                .filter(|scope| {
                    scope.slot == slot && scope.start_pc <= offset && offset < scope.end_pc
                })
                .max_by_key(|scope| scope.start_pc)
                .map(|scope| scope.name.clone());
            self.set_name(slot, name);
        }
    }
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
    let empty_concat = std::collections::HashMap::new();
    let empty_lambda = std::collections::HashMap::new();
    let context = SimulationContext {
        is_static,
        this_class,
        local_names,
        local_scopes: &[],
        local_types: &[],
        return_type: None,
        concat_recipes: &empty_concat,
        lambda_bootstrap: &empty_lambda,
    };
    simulate_block_with_context(instructions, pool, initial_stack, &context)
}

pub fn simulate_block_with_context(
    instructions: &[Instruction],
    pool: &ConstantPool,
    initial_stack: Vec<SlotInfo>,
    context: &SimulationContext<'_>,
) -> SimResult {
    let mut stack = OperandStack {
        slots: initial_stack,
        errors: Vec::new(),
        offset: 0,
        opcode: 0,
    };
    let mut locals = LocalVars::new();
    let mut stmts: Vec<Expr> = Vec::new();
    let mut local_assignments: Vec<(u16, Expr, JavaType)> = Vec::new();

    // Seed slot 0 for instance methods
    if !context.is_static {
        locals.set(0, JavaType::object(context.this_class), Some("this".into()));
    }
    for (slot, ty, name) in context.local_types {
        locals.set(*slot, ty.clone(), name.clone());
    }
    // Seed names from LocalVariableTable, preserving any type already known.
    for (slot, name) in context.local_names {
        let existing = locals.get_ty(*slot);
        locals.set(*slot, existing, Some(name.clone()));
    }

    for (index, insn) in instructions.iter().enumerate() {
        let name_offset = if is_local_store(insn.opcode) {
            instructions
                .get(index + 1)
                .map_or_else(|| local_store_end_offset(insn), |next| next.offset)
        } else {
            insn.offset
        };
        locals.refresh_scoped_names(context.local_scopes, name_offset);
        stack.begin_instruction(insn);
        step(
            insn,
            pool,
            &mut stack,
            &mut locals,
            &mut stmts,
            &mut local_assignments,
            context,
        );
    }

    let (stack_out, errors) = stack.finish();
    SimResult {
        stmts,
        stack_out,
        locals: local_assignments,
        errors,
    }
}

fn local_store_end_offset(instruction: &Instruction) -> u32 {
    instruction.offset
        + instruction
            .kind
            .encoded_length(instruction.opcode, instruction.offset) as u32
}

fn is_local_store(opcode: u8) -> bool {
    matches!(
        opcode,
        opc::istore
            | opc::lstore
            | opc::fstore
            | opc::dstore
            | opc::astore
            | opc::istore_0
            | opc::istore_1
            | opc::istore_2
            | opc::istore_3
            | opc::lstore_0
            | opc::lstore_1
            | opc::lstore_2
            | opc::lstore_3
            | opc::fstore_0
            | opc::fstore_1
            | opc::fstore_2
            | opc::fstore_3
            | opc::dstore_0
            | opc::dstore_1
            | opc::dstore_2
            | opc::dstore_3
            | opc::astore_0
            | opc::astore_1
            | opc::astore_2
            | opc::astore_3
    )
}

// ── Instruction dispatch ──────────────────────────────────────────────────

#[allow(non_upper_case_globals)]
fn step(
    insn: &Instruction,
    pool: &ConstantPool,
    stack: &mut OperandStack,
    locals: &mut LocalVars,
    stmts: &mut Vec<Expr>,
    local_assignments: &mut Vec<(u16, Expr, JavaType)>,
    context: &SimulationContext<'_>,
) {
    use opc::*;
    let op = insn.opcode;

    match op {
        // ── constants ────────────────────────────────────────────────
        nop => {}
        aconst_null => stack.push(Expr::Null, JavaType::NULL),
        iconst_m1 => push_int(stack, -1),
        iconst_0 => push_int(stack, 0),
        iconst_1 => push_int(stack, 1),
        iconst_2 => push_int(stack, 2),
        iconst_3 => push_int(stack, 3),
        iconst_4 => push_int(stack, 4),
        iconst_5 => push_int(stack, 5),
        lconst_0 => push_long(stack, 0),
        lconst_1 => push_long(stack, 1),
        fconst_0 => push_float(stack, 0.0),
        fconst_1 => push_float(stack, 1.0),
        fconst_2 => push_float(stack, 2.0),
        dconst_0 => push_double(stack, 0.0),
        dconst_1 => push_double(stack, 1.0),
        bipush => {
            if let InsnKind::BytePush { value } = insn.kind {
                push_int(stack, value as i32);
            }
        }
        sipush => {
            if let InsnKind::ShortPush { value } = insn.kind {
                push_int(stack, value as i32);
            }
        }

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
            let ty = if ty == JavaType::UNKNOWN {
                JavaType::object("java/lang/Object")
            } else {
                ty
            };
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
            store_local(
                stack,
                locals,
                stmts,
                local_assignments,
                slot,
                JavaType::LONG,
            );
        }
        fstore | fstore_0 | fstore_1 | fstore_2 | fstore_3 => {
            let slot = local_slot(op, insn, fstore, fstore_0);
            store_local(
                stack,
                locals,
                stmts,
                local_assignments,
                slot,
                JavaType::FLOAT,
            );
        }
        dstore | dstore_0 | dstore_1 | dstore_2 | dstore_3 => {
            let slot = local_slot(op, insn, dstore, dstore_0);
            store_local(
                stack,
                locals,
                stmts,
                local_assignments,
                slot,
                JavaType::DOUBLE,
            );
        }
        astore | astore_0 | astore_1 | astore_2 | astore_3 => {
            let slot = local_slot(op, insn, astore, astore_0);
            let ty = stack
                .peek()
                .map(|s| s.ty.clone())
                .unwrap_or_else(|| JavaType::object("java/lang/Object"));
            store_local(stack, locals, stmts, local_assignments, slot, ty);
        }

        // ── array stores ─────────────────────────────────────────────
        iastore | lastore | fastore | dastore | aastore | bastore | castore | sastore => {
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
                                        value: ConstValue::Null,
                                        ty: JavaType::UNKNOWN,
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
            if let Some(slot) = stack.pop_value() {
                if slot.expr.has_side_effects() {
                    stmts.push(slot.expr);
                }
            }
        }
        pop2 => stack.pop2_values(),
        dup => stack.dup(),
        dup_x1 => stack.dup_x1(),
        dup_x2 => stack.dup_x2(),
        dup2 => stack.dup2(),
        dup2_x1 => stack.dup2_x1(),
        dup2_x2 => stack.dup2_x2(),
        swap => stack.swap(),

        // ── arithmetic ───────────────────────────────────────────────
        iadd | ladd | fadd | dadd => binop(stack, BinOp::Add, arith_type(op)),
        isub | lsub | fsub | dsub => binop(stack, BinOp::Sub, arith_type(op)),
        imul | lmul | fmul | dmul => binop(stack, BinOp::Mul, arith_type(op)),
        idiv | ldiv | fdiv | ddiv => binop(stack, BinOp::Div, arith_type(op)),
        irem | lrem | frem | drem => binop(stack, BinOp::Rem, arith_type(op)),
        ineg | lneg | fneg | dneg => unop(stack, UnOp::Neg, arith_type(op)),

        // ── shifts ───────────────────────────────────────────────────
        ishl | lshl => binop(stack, BinOp::Shl, shift_type(op)),
        ishr | lshr => binop(stack, BinOp::Shr, shift_type(op)),
        iushr | lushr => binop(stack, BinOp::Ushr, shift_type(op)),

        // ── bitwise ──────────────────────────────────────────────────
        iand | land => binop(stack, BinOp::And, bit_type(op)),
        ior | lor => binop(stack, BinOp::Or, bit_type(op)),
        ixor | lxor => binop(stack, BinOp::Xor, bit_type(op)),

        // ── iinc ─────────────────────────────────────────────────────
        iinc => {
            if let InsnKind::Iinc { index, const_ } = insn.kind {
                let name = locals.get_name(index);
                stmts.push(Expr::IInc {
                    slot: index,
                    delta: const_,
                    name,
                });
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
        lcmp => binop(stack, BinOp::LCmp, JavaType::INT),
        fcmpl => binop(stack, BinOp::FCmpL, JavaType::INT),
        fcmpg => binop(stack, BinOp::FCmpG, JavaType::INT),
        dcmpl => binop(stack, BinOp::DCmpL, JavaType::INT),
        dcmpg => binop(stack, BinOp::DCmpG, JavaType::INT),

        // ── returns ──────────────────────────────────────────────────
        ireturn | lreturn | freturn | dreturn | areturn => {
            let mut val = stack.pop_expr();
            if context.return_type == Some(&JavaType::BOOLEAN) {
                coerce_boolean_expression(&mut val);
            }
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
            if let Some(e) = stack.pop() {
                stmts.push(e.expr);
            }
        }
        getfield => lift_field(insn, pool, stack, FieldDir::Get, false),
        putfield => {
            lift_field(insn, pool, stack, FieldDir::Put, false);
            // putfield produces a side effect — emit it
            if let Some(e) = stack.pop() {
                stmts.push(e.expr);
            }
        }

        // ── method invocations ────────────────────────────────────────
        invokevirtual | invokespecial | invokestatic | invokeinterface => {
            lift_invoke(insn, pool, stack, stmts, op, context.this_class);
        }
        invokedynamic => lift_invokedynamic(insn, pool, stack, stmts, context),

        // ── new / newarray ────────────────────────────────────────────
        new => lift_new(insn, pool, stack),
        newarray => {
            if let InsnKind::NewArray { atype } = insn.kind {
                let count = stack.pop_expr();
                let ty = primitive_array_type(atype);
                stack.push(
                    Expr::NewArray {
                        kind: NewKind::PrimitiveArray { atype },
                        type_: ty,
                        dimensions: vec![count],
                        initializer: None,
                    },
                    JavaType::object("[primitive"),
                );
            }
        }
        anewarray => {
            if let InsnKind::Cp { index } = insn.kind {
                let count = stack.pop_expr();
                let elem_name = pool
                    .class_name(index)
                    .unwrap_or("java/lang/Object")
                    .to_string();
                let ty = class_constant_type(&elem_name);
                stack.push(
                    Expr::NewArray {
                        kind: NewKind::RefArray,
                        type_: ty,
                        dimensions: vec![count],
                        initializer: None,
                    },
                    JavaType::object(&elem_name).array_of(),
                );
            }
        }
        multianewarray => {
            if let InsnKind::MultiNewArray { index, dimensions } = insn.kind {
                let mut dims = Vec::new();
                for _ in 0..dimensions {
                    dims.push(stack.pop_expr());
                }
                dims.reverse();
                let class_name = pool
                    .class_name(index)
                    .unwrap_or("java/lang/Object")
                    .to_string();
                let ty = class_constant_type(&class_name);
                stack.push(
                    Expr::NewArray {
                        kind: NewKind::MultiArray { dims: dimensions },
                        type_: ty.clone(),
                        dimensions: dims,
                        initializer: None,
                    },
                    ty,
                );
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
                let name = pool
                    .class_name(index)
                    .unwrap_or("java/lang/Object")
                    .to_string();
                let ty = class_constant_type(&name);
                stack.push(
                    Expr::Cast(CastKind::CheckCast, ty.clone(), Box::new(obj)),
                    ty,
                );
            }
        }
        instanceof => {
            if let InsnKind::Cp { index } = insn.kind {
                let obj = stack.pop_expr();
                let name = pool
                    .class_name(index)
                    .unwrap_or("java/lang/Object")
                    .to_string();
                stack.push(
                    Expr::InstanceOf(Box::new(obj), class_constant_type(&name)),
                    JavaType::BOOLEAN,
                );
            }
        }
        monitorenter => {
            let obj = stack.pop_expr();
            stmts.push(Expr::Monitor {
                enter: true,
                object: Box::new(obj),
            });
        }
        monitorexit => {
            let obj = stack.pop_expr();
            stmts.push(Expr::Monitor {
                enter: false,
                object: Box::new(obj),
            });
        }

        // ── branches — we emit nothing; handled by cfg/recovery ───────
        ifeq | ifne | iflt | ifge | ifgt | ifle | if_icmpeq | if_icmpne | if_icmplt | if_icmpge
        | if_icmpgt | if_icmple | if_acmpeq | if_acmpne | ifnull | ifnonnull | goto | goto_w
        | jsr | jsr_w | ret | tableswitch | lookupswitch => {}

        _ => {}
    }
}

/// A CONSTANT_Class used by type instructions contains a binary name for an
/// ordinary class, but contains a field descriptor for an array class.
fn class_constant_type(name: &str) -> JavaType {
    if name.starts_with('[') {
        if let Ok((ty, consumed)) = crate::types::descriptor::parse_field_descriptor(name) {
            if consumed == name.len() {
                return ty;
            }
        }
    }
    JavaType::object(name)
}

fn coerce_boolean_expression(expr: &mut Expr) {
    match expr {
        Expr::Const(constant) if matches!(constant.value, ConstValue::Int(0 | 1)) => {
            constant.ty = JavaType::BOOLEAN;
        }
        Expr::Ternary {
            then_expr,
            else_expr,
            ..
        } => {
            coerce_boolean_expression(then_expr);
            coerce_boolean_expression(else_expr);
        }
        Expr::SwitchExpression { arms, .. } => {
            for (_, value) in arms {
                coerce_boolean_expression(value);
            }
        }
        _ => {}
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn push_int(stack: &mut OperandStack, v: i32) {
    stack.push(
        Expr::Const(ConstExpr {
            value: ConstValue::Int(v),
            ty: JavaType::INT,
        }),
        JavaType::INT,
    );
}
fn push_long(stack: &mut OperandStack, v: i64) {
    stack.push(
        Expr::Const(ConstExpr {
            value: ConstValue::Long(v),
            ty: JavaType::LONG,
        }),
        JavaType::LONG,
    );
}
fn push_float(stack: &mut OperandStack, v: f32) {
    stack.push(
        Expr::Const(ConstExpr {
            value: ConstValue::Float(v),
            ty: JavaType::FLOAT,
        }),
        JavaType::FLOAT,
    );
}
fn push_double(stack: &mut OperandStack, v: f64) {
    stack.push(
        Expr::Const(ConstExpr {
            value: ConstValue::Double(v),
            ty: JavaType::DOUBLE,
        }),
        JavaType::DOUBLE,
    );
}

fn push_local(stack: &mut OperandStack, locals: &LocalVars, slot: u16, default_ty: JavaType) {
    let ty = locals.get_ty(slot);
    let ty = if ty == JavaType::UNKNOWN {
        default_ty.clone()
    } else {
        ty
    };
    let name = locals.get_name(slot);
    stack.push(
        Expr::LocalVar(LocalVarExpr {
            slot,
            ty: ty.clone(),
            name,
        }),
        ty,
    );
}

fn store_local(
    stack: &mut OperandStack,
    locals: &mut LocalVars,
    stmts: &mut Vec<Expr>,
    local_assignments: &mut Vec<(u16, Expr, JavaType)>,
    slot: u16,
    ty: JavaType,
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
    let lv = Expr::LocalVar(LocalVarExpr {
        slot,
        ty: ty.clone(),
        name: locals.get_name(slot),
    });
    let assign = Expr::Assign {
        lhs: Box::new(lv),
        rhs: Box::new(val.clone()),
    };
    stmts.push(assign);
    local_assignments.push((slot, val, ty));
}

/// The integer value of a constant expression, if it is one.
fn const_int_value(expr: &Expr) -> Option<i32> {
    if let Expr::Const(c) = expr {
        if let ConstValue::Int(i) = c.value {
            return Some(i);
        }
    }
    None
}

/// True for the types that share the JVM's `int` computational type, so an
/// `istore`/`iload` against such a slot should not clobber the narrower type.
fn is_int_like(ty: &JavaType) -> bool {
    *ty == JavaType::BOOLEAN
        || *ty == JavaType::BYTE
        || *ty == JavaType::CHAR
        || *ty == JavaType::SHORT
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
    stack.push(
        Expr::ArrayLoad {
            array: Box::new(array),
            index: Box::new(index),
            elem_type: elem_ty.clone(),
        },
        elem_ty,
    );
}

/// Resolve the local-variable slot for a normal or short-form load/store opcode.
fn local_slot(op: u8, insn: &Instruction, base_op: u8, base_short: u8) -> u16 {
    if op == base_op {
        if let InsnKind::LocalVar { index } = insn.kind {
            return index;
        }
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
        _ => JavaType::INT,
    }
}

fn shift_type(op: u8) -> JavaType {
    use opc::*;
    if op == lshl || op == lshr || op == lushr {
        JavaType::LONG
    } else {
        JavaType::INT
    }
}

fn bit_type(op: u8) -> JavaType {
    use opc::*;
    if op == land || op == lor || op == lxor {
        JavaType::LONG
    } else {
        JavaType::INT
    }
}

fn primitive_array_type(atype: u8) -> JavaType {
    match atype {
        4 => JavaType::BOOLEAN,
        5 => JavaType::CHAR,
        6 => JavaType::FLOAT,
        7 => JavaType::DOUBLE,
        8 => JavaType::BYTE,
        9 => JavaType::SHORT,
        10 => JavaType::INT,
        11 => JavaType::LONG,
        _ => JavaType::INT,
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
        Ok(CpEntry::Float(v)) => push_float(stack, *v),
        Ok(CpEntry::Long(v)) => push_long(stack, *v),
        Ok(CpEntry::Double(v)) => push_double(stack, *v),
        Ok(CpEntry::String(s)) => {
            stack.push(
                Expr::Const(ConstExpr {
                    value: ConstValue::StringRef(s.clone()),
                    ty: JavaType::object("java/lang/String"),
                }),
                JavaType::object("java/lang/String"),
            );
        }
        Ok(CpEntry::Class(n)) => {
            stack.push(
                Expr::Const(ConstExpr {
                    value: ConstValue::ClassRef(n.clone()),
                    ty: JavaType::object("java/lang/Class"),
                }),
                JavaType::object("java/lang/Class"),
            );
        }
        _ => stack.push(
            Expr::Opaque {
                opcode: insn.opcode,
                offset: insn.offset,
            },
            JavaType::UNKNOWN,
        ),
    }
}

// ── field access ──────────────────────────────────────────────────────────

fn lift_field(
    insn: &Instruction,
    pool: &ConstantPool,
    stack: &mut OperandStack,
    dir: FieldDir,
    is_static: bool,
) {
    let index = match insn.kind {
        InsnKind::Cp { index } => index,
        _ => return,
    };
    let (owner, name, descriptor) = match pool.get(index) {
        Ok(CpEntry::Fieldref(mr)) => (
            mr.class_name.clone(),
            mr.name.clone(),
            mr.descriptor.clone(),
        ),
        _ => return,
    };
    let field_ty = crate::types::descriptor::parse_field_descriptor(&descriptor)
        .map(|(t, _)| t)
        .unwrap_or(JavaType::UNKNOWN);
    match dir {
        FieldDir::Get => {
            let object = if is_static {
                None
            } else {
                Some(Box::new(stack.pop_expr()))
            };
            stack.push(
                Expr::Field {
                    dir: FieldDir::Get,
                    owner,
                    name,
                    descriptor,
                    object,
                    value: None,
                },
                field_ty,
            );
        }
        FieldDir::Put => {
            let value = stack.pop_expr();
            let object = if is_static {
                None
            } else {
                Some(Box::new(stack.pop_expr()))
            };
            // putstatic emits directly as statement; caller handles pop
            stack.push(
                Expr::Field {
                    dir: FieldDir::Put,
                    owner,
                    name,
                    descriptor,
                    object,
                    value: Some(Box::new(value)),
                },
                JavaType::VOID,
            );
        }
    }
}

// ── method invocation ─────────────────────────────────────────────────────

fn lift_invoke(
    insn: &Instruction,
    pool: &ConstantPool,
    stack: &mut OperandStack,
    stmts: &mut Vec<Expr>,
    op: u8,
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
        invokestatic => InvokeKind::Static,
        invokespecial => InvokeKind::Special,
        invokeinterface => InvokeKind::Interface,
        _ => InvokeKind::Virtual,
    };

    let md =
        crate::types::descriptor::MethodDescriptor::parse(&mr.descriptor).unwrap_or_else(|_| {
            crate::types::descriptor::MethodDescriptor {
                params: vec![],
                return_type: JavaType::VOID,
            }
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
                let call_name = if mr.class_name == this_class {
                    "this"
                } else {
                    "super"
                };
                // Only emit if this isn't a plain Object.<init>() with no args
                // (the implicit default-constructor super call that adds no info).
                let is_trivial_object_init = mr.class_name == "java/lang/Object" && args.is_empty();
                if !is_trivial_object_init {
                    stmts.push(Expr::Invoke {
                        kind: InvokeKind::Special,
                        owner: mr.class_name,
                        name: call_name.into(),
                        descriptor: mr.descriptor,
                        object: None,
                        args,
                    });
                }
            }

            // ── other (inline <init> on non-new receiver) ─────────────
            other => {
                stmts.push(Expr::Invoke {
                    kind: InvokeKind::Special,
                    owner: mr.class_name,
                    name: mr.name,
                    descriptor: mr.descriptor,
                    object: Some(Box::new(other)),
                    args,
                });
            }
        }
        return;
    }

    let object = if kind == InvokeKind::Static {
        None
    } else {
        Some(Box::new(stack.pop_expr()))
    };

    let ret_ty = md.return_type.clone();
    let expr = Expr::Invoke {
        kind,
        owner: mr.class_name,
        name: mr.name,
        descriptor: mr.descriptor,
        object,
        args,
    };

    if ret_ty.is_void() {
        stmts.push(expr);
    } else {
        stack.push(expr, ret_ty);
    }
}

fn lift_invokedynamic(
    insn: &Instruction,
    pool: &ConstantPool,
    stack: &mut OperandStack,
    stmts: &mut Vec<Expr>,
    context: &SimulationContext<'_>,
) {
    let (bootstrap_index, name, descriptor) = match insn.kind {
        InsnKind::InvokeDynamic { index } => match pool.get(index) {
            Ok(CpEntry::InvokeDynamic {
                bootstrap_attr_index,
                name,
                descriptor,
            }) => (*bootstrap_attr_index, name.clone(), descriptor.clone()),
            _ => return,
        },
        _ => return,
    };

    let md = crate::types::descriptor::MethodDescriptor::parse(&descriptor).unwrap_or_else(|_| {
        crate::types::descriptor::MethodDescriptor {
            params: vec![],
            return_type: JavaType::VOID,
        }
    });

    let mut args: Vec<Expr> = (0..md.params.len()).map(|_| stack.pop_expr()).collect();
    args.reverse();

    let ret_ty = md.return_type.clone();
    let expr = Expr::InvokeDynamic {
        name,
        descriptor,
        bootstrap_index,
        args,
        concat_recipe: context.concat_recipes.get(&bootstrap_index).cloned(),
        lambda_body: context.lambda_bootstrap.get(&bootstrap_index).cloned(),
    };

    if ret_ty.is_void() {
        stmts.push(expr);
    } else {
        stack.push(expr, ret_ty);
    }
}

// ── new object ────────────────────────────────────────────────────────────

fn lift_new(insn: &Instruction, pool: &ConstantPool, stack: &mut OperandStack) {
    let index = match insn.kind {
        InsnKind::Cp { index } => index,
        _ => return,
    };
    let class_name = pool
        .class_name(index)
        .unwrap_or("java/lang/Object")
        .to_string();
    // We push a placeholder; the <init> call will complete the `new` node.
    stack.push(
        Expr::New {
            class_name: class_name.clone(),
            args: vec![],
            descriptor: String::new(),
        },
        JavaType::object(&class_name),
    );
}
