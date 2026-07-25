/// Expression tree nodes (Exprents) — the output of stack simulation.
///
/// Each node represents a Java expression at the source level.
/// Nodes are heap-allocated via `Box` since expressions form trees,
/// not DAGs, at this stage.
use crate::types::java_type::JavaType;

// ── ExprId ────────────────────────────────────────────────────────────────
pub type ExprId = u32;

// ── BinOp ─────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // arithmetic
    Add, Sub, Mul, Div, Rem,
    // bitwise
    And, Or, Xor,
    // shifts
    Shl, Shr, Ushr,
    // comparisons (produce int/boolean)
    Eq, Ne, Lt, Le, Gt, Ge,
    // long/float comparisons (raw, need context to negate)
    LCmp, FCmpL, FCmpG, DCmpL, DCmpG,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add  => "+",  BinOp::Sub  => "-",
            BinOp::Mul  => "*",  BinOp::Div  => "/",  BinOp::Rem  => "%",
            BinOp::And  => "&",  BinOp::Or   => "|",  BinOp::Xor  => "^",
            BinOp::Shl  => "<<", BinOp::Shr  => ">>", BinOp::Ushr => ">>>",
            BinOp::Eq   => "==", BinOp::Ne   => "!=",
            BinOp::Lt   => "<",  BinOp::Le   => "<=",
            BinOp::Gt   => ">",  BinOp::Ge   => ">=",
            BinOp::LCmp  => "/*lcmp*/",
            BinOp::FCmpL => "/*fcmpl*/", BinOp::FCmpG => "/*fcmpg*/",
            BinOp::DCmpL => "/*dcmpl*/", BinOp::DCmpG => "/*dcmpg*/",
        }
    }

    /// Java operator precedence (lower = tighter).
    pub fn precedence(self) -> u8 {
        match self {
            BinOp::Mul | BinOp::Div | BinOp::Rem => 2,
            BinOp::Add | BinOp::Sub => 3,
            BinOp::Shl | BinOp::Shr | BinOp::Ushr => 4,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 6,
            BinOp::Eq | BinOp::Ne => 7,
            BinOp::And => 8,
            BinOp::Xor => 9,
            BinOp::Or  => 10,
            _ => 0,
        }
    }
}

// ── UnOp ──────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,     // -x
    BitNot,  // ~x
    BoolNot, // !x
}

impl UnOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnOp::Neg    => "-",
            UnOp::BitNot => "~",
            UnOp::BoolNot=> "!",
        }
    }
}

// ── CastKind ──────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    I2L, I2F, I2D,
    L2I, L2F, L2D,
    F2I, F2L, F2D,
    D2I, D2L, D2F,
    I2B, I2C, I2S,
    CheckCast,   // (SomeClass) expr
}

// ── InvokeKind ────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvokeKind {
    Virtual,
    Special,
    Static,
    Interface,
    Dynamic,
}

// ── FieldAccess ───────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldDir { Get, Put }

// ── NewKind ───────────────────────────────────────────────────────────────
#[derive(Debug, Clone)]
pub enum NewKind {
    Object,                     // new Foo(...)
    PrimitiveArray { atype: u8 },  // new int[n]
    RefArray,                   // new Foo[n]
    MultiArray { dims: u8 },    // new Foo[a][b][c]
}

// ── Expr ──────────────────────────────────────────────────────────────────

/// A single Java expression node.
///
/// Sub-expressions are heap-allocated (`Box<Expr>`).  This avoids the need
/// for an arena while keeping the tree owned and self-contained.
#[derive(Debug, Clone)]
pub enum Expr {
    // ── leaves ────────────────────────────────────────────────────────
    /// Integer / long / float / double / string / class literal constant.
    Const(ConstExpr),
    /// Local variable or method parameter reference.
    LocalVar(LocalVarExpr),
    /// `null` literal.
    Null,
    /// `this` reference (slot 0 in instance methods).
    This(String),   // binary class name

    // ── arithmetic / logical ──────────────────────────────────────────
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    UnOp(UnOp, Box<Expr>),

    // ── type coercion / checks ─────────────────────────────────────────
    Cast(CastKind, JavaType, Box<Expr>),
    InstanceOf(Box<Expr>, JavaType),

    // ── field access ──────────────────────────────────────────────────
    Field {
        dir:        FieldDir,
        owner:      String,       // binary class name
        name:       String,
        descriptor: String,
        /// `None` for static fields.
        object:     Option<Box<Expr>>,
        value:      Option<Box<Expr>>,  // only for Put
    },

    // ── method invocation ─────────────────────────────────────────────
    Invoke {
        kind:       InvokeKind,
        owner:      String,
        name:       String,
        descriptor: String,
        /// `None` for static calls.
        object:     Option<Box<Expr>>,
        args:       Vec<Expr>,
    },

    // ── invokedynamic ─────────────────────────────────────────────────
    InvokeDynamic {
        name:       String,
        descriptor: String,
        bootstrap_index: u16,
        args:       Vec<Expr>,
    },

    // ── array operations ──────────────────────────────────────────────
    ArrayLoad {
        array:      Box<Expr>,
        index:      Box<Expr>,
        elem_type:  JavaType,
    },
    ArrayStore {
        array:      Box<Expr>,
        index:      Box<Expr>,
        value:      Box<Expr>,
    },
    ArrayLength(Box<Expr>),
    NewArray {
        kind:       NewKind,
        type_:      JavaType,
        dimensions: Vec<Expr>,
    },

    // ── object creation ───────────────────────────────────────────────
    New {
        class_name: String,
        args:       Vec<Expr>,
        descriptor: String,
    },

    // ── assignment ────────────────────────────────────────────────────
    Assign {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },

    // ── stack / control helpers ───────────────────────────────────────
    /// iinc implemented as compound assignment: local += const
    IInc { slot: u16, delta: i16, name: Option<String> },
    /// monitorenter / monitorexit
    Monitor { enter: bool, object: Box<Expr> },
    /// athrow
    Throw(Box<Expr>),
    /// ireturn / lreturn / freturn / dreturn / areturn / return
    Return(Option<Box<Expr>>),

    /// Placeholder for an expression that couldn't be lifted.
    Opaque { opcode: u8, offset: u32 },
}

impl Expr {
    /// Java precedence for parenthesisation decisions.
    pub fn precedence(&self) -> u8 {
        match self {
            Expr::Const(_) | Expr::LocalVar(_) | Expr::Null | Expr::This(_) => 0,
            Expr::UnOp(_, _) => 1,
            Expr::BinOp(op, _, _) => op.precedence(),
            Expr::Cast(_, _, _) => 1,
            Expr::InstanceOf(_, _) => 6,
            Expr::Invoke { .. } | Expr::Field { .. } => 0,
            Expr::New { .. } | Expr::NewArray { .. } => 0,
            Expr::Assign { .. } => 14,
            _ => 0,
        }
    }

    /// True if this expression has side effects (relevant for emission order).
    pub fn has_side_effects(&self) -> bool {
        matches!(self,
            Expr::Invoke { .. } | Expr::InvokeDynamic { .. } |
            Expr::New { .. }    | Expr::NewArray { .. } |
            Expr::Field { dir: FieldDir::Put, .. } |
            Expr::ArrayStore { .. } | Expr::Assign { .. } |
            Expr::IInc { .. }  | Expr::Monitor { .. } |
            Expr::Throw(_)     | Expr::Return(_)
        )
    }
}

// ── ConstExpr ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ConstExpr {
    pub value: ConstValue,
    pub ty:    JavaType,
}

#[derive(Debug, Clone)]
pub enum ConstValue {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    StringRef(String),
    ClassRef(String),    // "Foo.class"
    Null,
}

// ── LocalVarExpr ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LocalVarExpr {
    /// JVM local-variable slot index.
    pub slot:    u16,
    /// Inferred or declared type.
    pub ty:      JavaType,
    /// Debug name from LocalVariableTable, if available.
    pub name:    Option<String>,
}
