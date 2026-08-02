/// JVM opcode constants — §6 of the JVM spec.
/// Named to match the JVM specification mnemonics.
#[allow(non_upper_case_globals, dead_code, non_snake_case)]
pub mod opc {
    pub const nop: u8 = 0;
    pub const aconst_null: u8 = 1;
    pub const iconst_m1: u8 = 2;
    pub const iconst_0: u8 = 3;
    pub const iconst_1: u8 = 4;
    pub const iconst_2: u8 = 5;
    pub const iconst_3: u8 = 6;
    pub const iconst_4: u8 = 7;
    pub const iconst_5: u8 = 8;
    pub const lconst_0: u8 = 9;
    pub const lconst_1: u8 = 10;
    pub const fconst_0: u8 = 11;
    pub const fconst_1: u8 = 12;
    pub const fconst_2: u8 = 13;
    pub const dconst_0: u8 = 14;
    pub const dconst_1: u8 = 15;
    pub const bipush: u8 = 16;
    pub const sipush: u8 = 17;
    pub const ldc: u8 = 18;
    pub const ldc_w: u8 = 19;
    pub const ldc2_w: u8 = 20;
    pub const iload: u8 = 21;
    pub const lload: u8 = 22;
    pub const fload: u8 = 23;
    pub const dload: u8 = 24;
    pub const aload: u8 = 25;
    pub const iload_0: u8 = 26;
    pub const iload_1: u8 = 27;
    pub const iload_2: u8 = 28;
    pub const iload_3: u8 = 29;
    pub const lload_0: u8 = 30;
    pub const lload_1: u8 = 31;
    pub const lload_2: u8 = 32;
    pub const lload_3: u8 = 33;
    pub const fload_0: u8 = 34;
    pub const fload_1: u8 = 35;
    pub const fload_2: u8 = 36;
    pub const fload_3: u8 = 37;
    pub const dload_0: u8 = 38;
    pub const dload_1: u8 = 39;
    pub const dload_2: u8 = 40;
    pub const dload_3: u8 = 41;
    pub const aload_0: u8 = 42;
    pub const aload_1: u8 = 43;
    pub const aload_2: u8 = 44;
    pub const aload_3: u8 = 45;
    pub const iaload: u8 = 46;
    pub const laload: u8 = 47;
    pub const faload: u8 = 48;
    pub const daload: u8 = 49;
    pub const aaload: u8 = 50;
    pub const baload: u8 = 51;
    pub const caload: u8 = 52;
    pub const saload: u8 = 53;
    pub const istore: u8 = 54;
    pub const lstore: u8 = 55;
    pub const fstore: u8 = 56;
    pub const dstore: u8 = 57;
    pub const astore: u8 = 58;
    pub const istore_0: u8 = 59;
    pub const istore_1: u8 = 60;
    pub const istore_2: u8 = 61;
    pub const istore_3: u8 = 62;
    pub const lstore_0: u8 = 63;
    pub const lstore_1: u8 = 64;
    pub const lstore_2: u8 = 65;
    pub const lstore_3: u8 = 66;
    pub const fstore_0: u8 = 67;
    pub const fstore_1: u8 = 68;
    pub const fstore_2: u8 = 69;
    pub const fstore_3: u8 = 70;
    pub const dstore_0: u8 = 71;
    pub const dstore_1: u8 = 72;
    pub const dstore_2: u8 = 73;
    pub const dstore_3: u8 = 74;
    pub const astore_0: u8 = 75;
    pub const astore_1: u8 = 76;
    pub const astore_2: u8 = 77;
    pub const astore_3: u8 = 78;
    pub const iastore: u8 = 79;
    pub const lastore: u8 = 80;
    pub const fastore: u8 = 81;
    pub const dastore: u8 = 82;
    pub const aastore: u8 = 83;
    pub const bastore: u8 = 84;
    pub const castore: u8 = 85;
    pub const sastore: u8 = 86;
    pub const pop: u8 = 87;
    pub const pop2: u8 = 88;
    pub const dup: u8 = 89;
    pub const dup_x1: u8 = 90;
    pub const dup_x2: u8 = 91;
    pub const dup2: u8 = 92;
    pub const dup2_x1: u8 = 93;
    pub const dup2_x2: u8 = 94;
    pub const swap: u8 = 95;
    pub const iadd: u8 = 96;
    pub const ladd: u8 = 97;
    pub const fadd: u8 = 98;
    pub const dadd: u8 = 99;
    pub const isub: u8 = 100;
    pub const lsub: u8 = 101;
    pub const fsub: u8 = 102;
    pub const dsub: u8 = 103;
    pub const imul: u8 = 104;
    pub const lmul: u8 = 105;
    pub const fmul: u8 = 106;
    pub const dmul: u8 = 107;
    pub const idiv: u8 = 108;
    pub const ldiv: u8 = 109;
    pub const fdiv: u8 = 110;
    pub const ddiv: u8 = 111;
    pub const irem: u8 = 112;
    pub const lrem: u8 = 113;
    pub const frem: u8 = 114;
    pub const drem: u8 = 115;
    pub const ineg: u8 = 116;
    pub const lneg: u8 = 117;
    pub const fneg: u8 = 118;
    pub const dneg: u8 = 119;
    pub const ishl: u8 = 120;
    pub const lshl: u8 = 121;
    pub const ishr: u8 = 122;
    pub const lshr: u8 = 123;
    pub const iushr: u8 = 124;
    pub const lushr: u8 = 125;
    pub const iand: u8 = 126;
    pub const land: u8 = 127;
    pub const ior: u8 = 128;
    pub const lor: u8 = 129;
    pub const ixor: u8 = 130;
    pub const lxor: u8 = 131;
    pub const iinc: u8 = 132;
    pub const i2l: u8 = 133;
    pub const i2f: u8 = 134;
    pub const i2d: u8 = 135;
    pub const l2i: u8 = 136;
    pub const l2f: u8 = 137;
    pub const l2d: u8 = 138;
    pub const f2i: u8 = 139;
    pub const f2l: u8 = 140;
    pub const f2d: u8 = 141;
    pub const d2i: u8 = 142;
    pub const d2l: u8 = 143;
    pub const d2f: u8 = 144;
    pub const i2b: u8 = 145;
    pub const i2c: u8 = 146;
    pub const i2s: u8 = 147;
    pub const lcmp: u8 = 148;
    pub const fcmpl: u8 = 149;
    pub const fcmpg: u8 = 150;
    pub const dcmpl: u8 = 151;
    pub const dcmpg: u8 = 152;
    pub const ifeq: u8 = 153;
    pub const ifne: u8 = 154;
    pub const iflt: u8 = 155;
    pub const ifge: u8 = 156;
    pub const ifgt: u8 = 157;
    pub const ifle: u8 = 158;
    pub const if_icmpeq: u8 = 159;
    pub const if_icmpne: u8 = 160;
    pub const if_icmplt: u8 = 161;
    pub const if_icmpge: u8 = 162;
    pub const if_icmpgt: u8 = 163;
    pub const if_icmple: u8 = 164;
    pub const if_acmpeq: u8 = 165;
    pub const if_acmpne: u8 = 166;
    pub const goto: u8 = 167;
    pub const jsr: u8 = 168;
    pub const ret: u8 = 169;
    pub const tableswitch: u8 = 170;
    pub const lookupswitch: u8 = 171;
    pub const ireturn: u8 = 172;
    pub const lreturn: u8 = 173;
    pub const freturn: u8 = 174;
    pub const dreturn: u8 = 175;
    pub const areturn: u8 = 176;
    pub const r#return: u8 = 177;
    pub const getstatic: u8 = 178;
    pub const putstatic: u8 = 179;
    pub const getfield: u8 = 180;
    pub const putfield: u8 = 181;
    pub const invokevirtual: u8 = 182;
    pub const invokespecial: u8 = 183;
    pub const invokestatic: u8 = 184;
    pub const invokeinterface: u8 = 185;
    pub const invokedynamic: u8 = 186;
    pub const new: u8 = 187;
    pub const newarray: u8 = 188;
    pub const anewarray: u8 = 189;
    pub const arraylength: u8 = 190;
    pub const athrow: u8 = 191;
    pub const checkcast: u8 = 192;
    pub const instanceof: u8 = 193;
    pub const monitorenter: u8 = 194;
    pub const monitorexit: u8 = 195;
    pub const wide: u8 = 196;
    pub const multianewarray: u8 = 197;
    pub const ifnull: u8 = 198;
    pub const ifnonnull: u8 = 199;
    pub const goto_w: u8 = 200;
    pub const jsr_w: u8 = 201;

    /// Human-readable mnemonic for an opcode.
    pub fn name(op: u8) -> &'static str {
        NAMES.get(op as usize).copied().unwrap_or("<unknown>")
    }

    /// Whether this opcode can fall through to the next instruction.
    pub fn can_fall_through(op: u8) -> bool {
        !matches!(
            op,
            goto | goto_w
                | ret
                | ireturn
                | lreturn
                | freturn
                | dreturn
                | areturn
                | r#return
                | athrow
                | tableswitch
                | lookupswitch
                | jsr
                | jsr_w
        )
    }

    /// Whether this is a conditional or unconditional branch.
    pub fn is_branch(op: u8) -> bool {
        matches!(
            op,
            ifeq | ifne
                | iflt
                | ifge
                | ifgt
                | ifle
                | if_icmpeq
                | if_icmpne
                | if_icmplt
                | if_icmpge
                | if_icmpgt
                | if_icmple
                | if_acmpeq
                | if_acmpne
                | ifnull
                | ifnonnull
                | goto
                | goto_w
                | jsr
                | jsr_w
        )
    }

    static NAMES: &[&str] = &[
        "nop",
        "aconst_null",
        "iconst_m1",
        "iconst_0",
        "iconst_1",
        "iconst_2",
        "iconst_3",
        "iconst_4",
        "iconst_5",
        "lconst_0",
        "lconst_1",
        "fconst_0",
        "fconst_1",
        "fconst_2",
        "dconst_0",
        "dconst_1",
        "bipush",
        "sipush",
        "ldc",
        "ldc_w",
        "ldc2_w",
        "iload",
        "lload",
        "fload",
        "dload",
        "aload",
        "iload_0",
        "iload_1",
        "iload_2",
        "iload_3",
        "lload_0",
        "lload_1",
        "lload_2",
        "lload_3",
        "fload_0",
        "fload_1",
        "fload_2",
        "fload_3",
        "dload_0",
        "dload_1",
        "dload_2",
        "dload_3",
        "aload_0",
        "aload_1",
        "aload_2",
        "aload_3",
        "iaload",
        "laload",
        "faload",
        "daload",
        "aaload",
        "baload",
        "caload",
        "saload",
        "istore",
        "lstore",
        "fstore",
        "dstore",
        "astore",
        "istore_0",
        "istore_1",
        "istore_2",
        "istore_3",
        "lstore_0",
        "lstore_1",
        "lstore_2",
        "lstore_3",
        "fstore_0",
        "fstore_1",
        "fstore_2",
        "fstore_3",
        "dstore_0",
        "dstore_1",
        "dstore_2",
        "dstore_3",
        "astore_0",
        "astore_1",
        "astore_2",
        "astore_3",
        "iastore",
        "lastore",
        "fastore",
        "dastore",
        "aastore",
        "bastore",
        "castore",
        "sastore",
        "pop",
        "pop2",
        "dup",
        "dup_x1",
        "dup_x2",
        "dup2",
        "dup2_x1",
        "dup2_x2",
        "swap",
        "iadd",
        "ladd",
        "fadd",
        "dadd",
        "isub",
        "lsub",
        "fsub",
        "dsub",
        "imul",
        "lmul",
        "fmul",
        "dmul",
        "idiv",
        "ldiv",
        "fdiv",
        "ddiv",
        "irem",
        "lrem",
        "frem",
        "drem",
        "ineg",
        "lneg",
        "fneg",
        "dneg",
        "ishl",
        "lshl",
        "ishr",
        "lshr",
        "iushr",
        "lushr",
        "iand",
        "land",
        "ior",
        "lor",
        "ixor",
        "lxor",
        "iinc",
        "i2l",
        "i2f",
        "i2d",
        "l2i",
        "l2f",
        "l2d",
        "f2i",
        "f2l",
        "f2d",
        "d2i",
        "d2l",
        "d2f",
        "i2b",
        "i2c",
        "i2s",
        "lcmp",
        "fcmpl",
        "fcmpg",
        "dcmpl",
        "dcmpg",
        "ifeq",
        "ifne",
        "iflt",
        "ifge",
        "ifgt",
        "ifle",
        "if_icmpeq",
        "if_icmpne",
        "if_icmplt",
        "if_icmpge",
        "if_icmpgt",
        "if_icmple",
        "if_acmpeq",
        "if_acmpne",
        "goto",
        "jsr",
        "ret",
        "tableswitch",
        "lookupswitch",
        "ireturn",
        "lreturn",
        "freturn",
        "dreturn",
        "areturn",
        "return",
        "getstatic",
        "putstatic",
        "getfield",
        "putfield",
        "invokevirtual",
        "invokespecial",
        "invokestatic",
        "invokeinterface",
        "invokedynamic",
        "new",
        "newarray",
        "anewarray",
        "arraylength",
        "athrow",
        "checkcast",
        "instanceof",
        "monitorenter",
        "monitorexit",
        "wide",
        "multianewarray",
        "ifnull",
        "ifnonnull",
        "goto_w",
        "jsr_w",
    ];
}

/// `newarray` type codes.
#[allow(dead_code)]
pub mod array_type {
    pub const T_BOOLEAN: u8 = 4;
    pub const T_CHAR: u8 = 5;
    pub const T_FLOAT: u8 = 6;
    pub const T_DOUBLE: u8 = 7;
    pub const T_BYTE: u8 = 8;
    pub const T_SHORT: u8 = 9;
    pub const T_INT: u8 = 10;
    pub const T_LONG: u8 = 11;
}
