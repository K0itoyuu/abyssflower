#![allow(non_upper_case_globals)]
//! JVM bytecode instruction — decoded from a Code attribute's `code[]` byte array.
//!
//! Each instruction records its bytecode offset (for branch targets and
//! debug info) and its opcode-specific operands.
use crate::classfile::cursor::Cursor;
use crate::classfile::opcodes::opc;
use crate::error::{DecompileError, Result};

// ── Instruction ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Instruction {
    /// Byte offset within the `code[]` array where this instruction starts.
    pub offset: u32,
    /// The decoded opcode (after resolving `wide`).
    pub opcode: u8,
    /// True if the instruction was prefixed by `wide`.
    pub wide: bool,
    /// Opcode-specific operand data.
    pub kind: InsnKind,
}

impl Instruction {
    /// Whether execution can continue at the next sequential instruction.
    pub fn can_fall_through(&self) -> bool {
        opc::can_fall_through(self.opcode)
    }
}

// ── InsnKind ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum InsnKind {
    /// No operands (e.g. nop, pop, iadd, …)
    NoOperand,
    /// Local variable index (iload, istore, ret, …)
    LocalVar { index: u16 },
    /// Local variable index + constant increment (iinc)
    Iinc { index: u16, const_: i16 },
    /// Signed branch offset from the start of this instruction (goto, jsr, ifXX, …)
    Branch { offset: i32 },
    /// Constant pool index (ldc, new, checkcast, …)
    Cp { index: u16 },
    /// ldc / ldc_w / ldc2_w — index already resolved; kept as index
    Ldc { index: u16 },
    /// invokevirtual / invokespecial / invokestatic / invokeinterface
    Invoke { index: u16, count: u8 },        // count only for invokeinterface
    /// invokedynamic
    InvokeDynamic { index: u16 },
    /// bipush
    BytePush { value: i8 },
    /// sipush
    ShortPush { value: i16 },
    /// newarray
    NewArray { atype: u8 },
    /// multianewarray
    MultiNewArray { index: u16, dimensions: u8 },
    /// tableswitch
    TableSwitch {
        default_offset: i32,
        low: i32,
        high: i32,
        offsets: Vec<i32>,
    },
    /// lookupswitch
    LookupSwitch {
        default_offset: i32,
        pairs: Vec<(i32, i32)>,   // (match_value, offset)
    },
}

impl InsnKind {
    /// Number of bytes this instruction occupies in the bytecode stream,
    /// including the opcode byte (and `wide` prefix if `wide == true`).
    /// `insn_offset` is needed only for tableswitch/lookupswitch alignment.
    pub fn encoded_length(&self, opcode: u8, insn_offset: u32) -> usize {
        use opc::*;
        match self {
            InsnKind::NoOperand       => 1,
            InsnKind::LocalVar { .. } => 2,  // opcode + u8 (wide: +1 prefix +1 extra byte = 4)
            InsnKind::Iinc { .. }     => 3,  // opcode + u8 + i8 (wide: 6)
            InsnKind::Branch { .. }   => {
                if opcode == goto_w || opcode == jsr_w { 5 } else { 3 }
            }
            InsnKind::Cp { .. }       => 3,
            InsnKind::Ldc { index: _ }   => {
                if opcode == ldc { 2 } else { 3 }
            }
            InsnKind::BytePush { .. } => 2,
            InsnKind::ShortPush { .. }=> 3,
            InsnKind::NewArray { .. } => 2,
            InsnKind::MultiNewArray { .. } => 4,
            InsnKind::Invoke { .. }   => {
                if opcode == invokeinterface || opcode == invokedynamic { 5 } else { 3 }
            }
            InsnKind::InvokeDynamic { .. } => 5,
            InsnKind::TableSwitch { low, high, .. } => {
                // opcode (1) + pad + 4 (default) + 4 (low) + 4 (high) + 4*(high-low+1)
                let pad = ((4 - ((insn_offset + 1) % 4)) % 4) as usize;
                1 + pad + 12 + 4 * ((*high - *low + 1) as usize)
            }
            InsnKind::LookupSwitch { pairs, .. } => {
                let pad = ((4 - ((insn_offset + 1) % 4)) % 4) as usize;
                1 + pad + 8 + 8 * pairs.len()
            }
        }
    }

    /// Absolute target bytecode offsets for branch/switch instructions.
    /// Returns an empty `Vec` for non-branching instructions.
    pub fn branch_targets(&self, insn_offset: u32, _opcode: u8) -> Vec<u32> {
        let base = insn_offset as i64;
        match self {
            InsnKind::Branch { offset } => {
                vec![(base + *offset as i64) as u32]
            }
            InsnKind::TableSwitch { default_offset, offsets, .. } => {
                let mut targets: Vec<u32> = offsets.iter()
                    .map(|o| (base + *o as i64) as u32)
                    .collect();
                targets.push((base + *default_offset as i64) as u32);
                targets.dedup();
                targets
            }
            InsnKind::LookupSwitch { default_offset, pairs } => {
                let mut targets: Vec<u32> = pairs.iter()
                    .map(|(_, o)| (base + *o as i64) as u32)
                    .collect();
                targets.push((base + *default_offset as i64) as u32);
                targets.dedup();
                targets
            }
            _ => Vec::new(),
        }
    }
}

// ── parser ─────────────────────────────────────────────────────────────────

/// Decode all instructions in `code_bytes`.
///
/// `code_bytes` is the raw `code[]` array from a `Code` attribute —
/// *not* the whole attribute, just the bytecode itself.
pub fn decode(code_bytes: &[u8]) -> Result<Vec<Instruction>> {
    let mut cur = Cursor::new(code_bytes);
    let mut instructions = Vec::new();

    while cur.remaining() > 0 {
        let offset = cur.position() as u32;
        let byte   = cur.read_u8()?;

        let (opcode, wide) = if byte == opc::wide {
            // wide prefix: the actual opcode follows
            (cur.read_u8()?, true)
        } else {
            (byte, false)
        };

        let kind = decode_operands(&mut cur, opcode, wide, offset)?;
        instructions.push(Instruction { offset, opcode, wide, kind });
    }

    Ok(instructions)
}

fn decode_operands(cur: &mut Cursor, opcode: u8, is_wide: bool, insn_offset: u32) -> Result<InsnKind> {
    use opc::*;
    use InsnKind::*;

    // Helper: read local-variable index (u8 normal, u16 when prefixed by wide)
    let read_local = |cur: &mut Cursor| -> Result<u16> {
        if is_wide { cur.read_u16() } else { cur.read_u8().map(|v| v as u16) }
    };

    let kind = match opcode {
        // ── zero-operand instructions ──────────────────────────────────
        nop | aconst_null |
        iconst_m1 | iconst_0 | iconst_1 | iconst_2 | iconst_3 | iconst_4 | iconst_5 |
        lconst_0 | lconst_1 | fconst_0 | fconst_1 | fconst_2 | dconst_0 | dconst_1 |
        iload_0 | iload_1 | iload_2 | iload_3 |
        lload_0 | lload_1 | lload_2 | lload_3 |
        fload_0 | fload_1 | fload_2 | fload_3 |
        dload_0 | dload_1 | dload_2 | dload_3 |
        aload_0 | aload_1 | aload_2 | aload_3 |
        iaload | laload | faload | daload | aaload | baload | caload | saload |
        istore_0 | istore_1 | istore_2 | istore_3 |
        lstore_0 | lstore_1 | lstore_2 | lstore_3 |
        fstore_0 | fstore_1 | fstore_2 | fstore_3 |
        dstore_0 | dstore_1 | dstore_2 | dstore_3 |
        astore_0 | astore_1 | astore_2 | astore_3 |
        iastore | lastore | fastore | dastore | aastore | bastore | castore | sastore |
        pop | pop2 | dup | dup_x1 | dup_x2 | dup2 | dup2_x1 | dup2_x2 | swap |
        iadd | ladd | fadd | dadd | isub | lsub | fsub | dsub |
        imul | lmul | fmul | dmul | idiv | ldiv | fdiv | ddiv |
        irem | lrem | frem | drem | ineg | lneg | fneg | dneg |
        ishl | lshl | ishr | lshr | iushr | lushr |
        iand | land | ior | lor | ixor | lxor |
        i2l | i2f | i2d | l2i | l2f | l2d | f2i | f2l | f2d |
        d2i | d2l | d2f | i2b | i2c | i2s |
        lcmp | fcmpl | fcmpg | dcmpl | dcmpg |
        ireturn | lreturn | freturn | dreturn | areturn | r#return |
        arraylength | athrow | monitorenter | monitorexit => NoOperand,

        // ── local variable ────────────────────────────────────────────
        iload | lload | fload | dload | aload |
        istore | lstore | fstore | dstore | astore | ret => {
            LocalVar { index: read_local(cur)? }
        }

        // ── iinc ──────────────────────────────────────────────────────
        iinc => {
            let index = read_local(cur)?;
            let const_ = if is_wide { cur.read_i16()? } else { cur.read_u8()? as i8 as i16 };
            Iinc { index, const_ }
        }

        // ── bipush / sipush ───────────────────────────────────────────
        bipush => BytePush  { value: cur.read_u8()? as i8  },
        sipush => ShortPush { value: cur.read_i16()?        },

        // ── ldc variants ─────────────────────────────────────────────
        ldc    => Ldc { index: cur.read_u8()? as u16 },
        ldc_w | ldc2_w => Ldc { index: cur.read_u16()? },

        // ── branches (16-bit offset relative to instruction start) ────
        ifeq | ifne | iflt | ifge | ifgt | ifle |
        if_icmpeq | if_icmpne | if_icmplt | if_icmpge | if_icmpgt | if_icmple |
        if_acmpeq | if_acmpne | ifnull | ifnonnull | jsr => {
            Branch { offset: cur.read_i16()? as i32 }
        }
        goto => Branch { offset: cur.read_i16()? as i32 },

        // ── wide branches (32-bit) ────────────────────────────────────
        goto_w | jsr_w => Branch { offset: cur.read_i32()? },

        // ── CP-indexed instructions ───────────────────────────────────
        getstatic | putstatic | getfield | putfield |
        new | anewarray | checkcast | instanceof => {
            Cp { index: cur.read_u16()? }
        }

        // ── invocations ───────────────────────────────────────────────
        invokevirtual | invokespecial | invokestatic => {
            Invoke { index: cur.read_u16()?, count: 0 }
        }
        invokeinterface => {
            let index = cur.read_u16()?;
            let count = cur.read_u8()?;
            cur.skip(1)?; // always 0
            Invoke { index, count }
        }
        invokedynamic => {
            let index = cur.read_u16()?;
            cur.skip(2)?; // two reserved zero bytes
            InvokeDynamic { index }
        }

        // ── newarray ──────────────────────────────────────────────────
        newarray => NewArray { atype: cur.read_u8()? },

        // ── multianewarray ────────────────────────────────────────────
        multianewarray => {
            let index      = cur.read_u16()?;
            let dimensions = cur.read_u8()?;
            MultiNewArray { index, dimensions }
        }

        // ── tableswitch ───────────────────────────────────────────────
        tableswitch => {
            // Align to 4-byte boundary relative to the start of the method bytecode.
            // The offset of the opcode itself is `insn_offset`.
            let align = (4 - ((insn_offset + 1) % 4)) % 4;
            cur.skip(align as usize)?;
            let default_offset = cur.read_i32()?;
            let low            = cur.read_i32()?;
            let high           = cur.read_i32()?;
            let count          = (high - low + 1) as usize;
            let mut offsets    = Vec::with_capacity(count);
            for _ in 0..count {
                offsets.push(cur.read_i32()?);
            }
            TableSwitch { default_offset, low, high, offsets }
        }

        // ── lookupswitch ──────────────────────────────────────────────
        lookupswitch => {
            let align = (4 - ((insn_offset + 1) % 4)) % 4;
            cur.skip(align as usize)?;
            let default_offset = cur.read_i32()?;
            let npairs         = cur.read_i32()? as usize;
            let mut pairs      = Vec::with_capacity(npairs);
            for _ in 0..npairs {
                let match_val = cur.read_i32()?;
                let offset    = cur.read_i32()?;
                pairs.push((match_val, offset));
            }
            LookupSwitch { default_offset, pairs }
        }

        other => {
            return Err(DecompileError::InvalidOpcode(other, insn_offset));
        }
    };

    Ok(kind)
}
