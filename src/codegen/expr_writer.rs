//! Pretty-printer for Java expressions with precedence and name formatting.

use crate::ir::expr::*;
use crate::types::java_type::{binary_name_to_source, JavaType};

// ── IndentWriter ──────────────────────────────────────────────────────────

pub struct IndentWriter {
    buf: String,
    indent: usize,
    step: usize,
}

impl IndentWriter {
    pub fn new(indent_size: usize) -> Self {
        IndentWriter {
            buf: String::new(),
            indent: 0,
            step: indent_size,
        }
    }

    pub fn indent(&mut self) {
        self.indent += self.step;
    }
    pub fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(self.step);
    }

    pub fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.buf.push(' ');
        }
        self.buf.push_str(s);
        self.buf.push('\n');
    }

    pub fn push_str(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    /// Current length of the emitted buffer — use with `drop_line_if` to
    /// retroactively remove a line emitted after this point.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// If the first line emitted after byte offset `from` satisfies `pred`,
    /// remove that line from the buffer.  Used to suppress redundant
    /// synthesised assignments (e.g. a catch parameter's self-assignment).
    pub fn drop_line_if<F: Fn(&str) -> bool>(&mut self, from: usize, pred: F) {
        if from >= self.buf.len() {
            return;
        }
        let tail = &self.buf[from..];
        let Some(nl) = tail.find('\n') else { return };
        let first = &tail[..nl];
        if pred(first) {
            self.buf.replace_range(from..from + nl + 1, "");
        }
    }

    pub fn finish(self) -> String {
        self.buf
    }
}

// ── Expression rendering ──────────────────────────────────────────────────

/// If `expr` is an integer constant, return it as a Java boolean literal.
/// `iconst_0` / `iconst_1` are how javac encodes `false` / `true`.
fn int_const_as_bool(expr: &Expr) -> Option<&'static str> {
    if let Expr::Const(c) = expr {
        if let crate::ir::expr::ConstValue::Int(i) = c.value {
            return Some(if i != 0 { "true" } else { "false" });
        }
    }
    None
}

/// Render a value being stored into a location with the given field
/// descriptor, mapping integer constants to `true`/`false` for `Z`.
fn render_value_for_descriptor(value: &Expr, descriptor: &str) -> String {
    if descriptor == "Z" {
        if let Some(b) = int_const_as_bool(value) {
            return b.to_string();
        }
    }
    render_expr(value)
}

/// Render an expression to a String.
pub fn render_expr(expr: &Expr) -> String {
    render_expr_prec(expr, 15) // 15 = lowest precedence
}

fn render_expr_prec(expr: &Expr, parent_prec: u8) -> String {
    let s = render_expr_inner(expr);
    let my_prec = expr.precedence();
    if my_prec > parent_prec {
        format!("({})", s)
    } else {
        s
    }
}

fn render_expr_inner(expr: &Expr) -> String {
    match expr {
        // ── leaves ────────────────────────────────────────────────────
        Expr::Null => "null".into(),
        Expr::This(_) => "this".into(),

        Expr::Const(c) => render_const(c),

        Expr::LocalVar(lv) => {
            if let Some(name) = &lv.name {
                name.clone()
            } else {
                format!("var{}", lv.slot)
            }
        }

        // ── arithmetic / logical ──────────────────────────────────────
        Expr::BinOp(op, lhs, rhs) => {
            let prec = op.precedence();
            format!(
                "{} {} {}",
                render_expr_prec(lhs, prec),
                op.symbol(),
                render_expr_prec(rhs, prec)
            )
        }

        Expr::UnOp(op, operand) => {
            format!("{}{}", op.symbol(), render_expr_prec(operand, 1))
        }

        // ── casts ─────────────────────────────────────────────────────
        Expr::Cast(kind, ty, inner) => {
            match kind {
                CastKind::CheckCast => {
                    format!("({}){}", render_type(ty), render_expr_prec(inner, 1))
                }
                _ => {
                    // Primitive narrowing/widening — emit explicit cast
                    format!(
                        "({}){}",
                        render_cast_type(*kind),
                        render_expr_prec(inner, 1)
                    )
                }
            }
        }

        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            let cond = match cond {
                TernaryCondition::Rendered(cond) => cond.clone(),
                TernaryCondition::Expression(cond) => render_expr(cond),
            };
            // `?:` is right-associative: the false arm may nest another
            // ternary without parentheses, the true arm may not.
            format!(
                "{} ? {} : {}",
                cond,
                render_expr_prec(then_expr, 12),
                render_expr_prec(else_expr, 13)
            )
        }

        Expr::SwitchExpression { selector, arms } => {
            let rendered_arms = arms
                .iter()
                .map(|(value, expr)| match value {
                    Some(value) => format!("case {value} -> {};", render_expr(expr)),
                    None => format!("default -> {};", render_expr(expr)),
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("switch ({}) {{ {} }}", render_expr(selector), rendered_arms)
        }

        Expr::InstanceOf(obj, ty) => {
            format!(
                "{} instanceof {}",
                render_expr_prec(obj, 6),
                render_type(ty)
            )
        }

        // ── field access ──────────────────────────────────────────────
        Expr::Field {
            dir: FieldDir::Get,
            owner,
            name,
            object,
            ..
        } => match object {
            Some(obj) => format!("{}.{}", render_expr_prec(obj, 0), name),
            None => format!("{}.{}", simple_name(owner), name),
        },

        Expr::Field {
            dir: FieldDir::Put,
            owner,
            name,
            object,
            value,
            descriptor,
        } => {
            let lhs = match object {
                Some(obj) => format!("{}.{}", render_expr_prec(obj, 0), name),
                None => format!("{}.{}", simple_name(owner), name),
            };
            // A boolean field assigned from iconst_0/1 should read false/true.
            let rhs = value
                .as_ref()
                .map(|v| render_value_for_descriptor(v, descriptor))
                .unwrap_or_default();
            format!("{} = {}", lhs, rhs)
        }

        // ── method invocations ────────────────────────────────────────
        Expr::Invoke {
            kind,
            owner,
            name,
            object,
            args,
            ..
        } => {
            let args_str = args.iter().map(render_expr).collect::<Vec<_>>().join(", ");
            match (kind, object) {
                (InvokeKind::Static, _) => format!("{}.{}({})", simple_name(owner), name, args_str),
                (InvokeKind::Special, Some(obj)) if name == "<init>" => {
                    // <init> calls are handled at the new-object level; fallback
                    format!("{}.{}({})", render_expr_prec(obj, 0), name, args_str)
                }
                (_, Some(obj)) => format!("{}.{}({})", render_expr_prec(obj, 0), name, args_str),
                (_, None) => format!("{}.{}({})", simple_name(owner), name, args_str),
            }
        }

        Expr::InvokeDynamic {
            name,
            args,
            concat_recipe,
            lambda_body,
            ..
        } => {
            // Desugar Java 9+ string concatenation factory into + chain.
            if name == "makeConcatWithConstants" || name == "makeConcat" {
                let strip = |a: &Expr| -> String {
                    if let Expr::Invoke {
                        kind: InvokeKind::Static,
                        owner,
                        name: vname,
                        args: vargs,
                        ..
                    } = a
                    {
                        if vname == "valueOf" && owner == "java/lang/String" && vargs.len() == 1 {
                            return render_expr(&vargs[0]);
                        }
                    }
                    render_expr(a)
                };

                if let Some(recipe) = concat_recipe {
                    let mut result = String::new();
                    let mut literal = String::new();
                    let mut arg_it = args.iter();

                    for ch in recipe.chars() {
                        match ch {
                            '\u{0001}' => {
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
                                    result.push_str(&strip(arg));
                                }
                            }
                            '\u{0002}' => {
                                // Static BSM constant — fall back to plain join
                                let parts: Vec<String> = args.iter().map(strip).collect();
                                return parts.join(" + ");
                            }
                            c => literal.push(c),
                        }
                    }
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

                // Fallback: no recipe — chain args with +.
                if !args.is_empty() {
                    let parts: Vec<String> = args.iter().map(strip).collect();
                    return parts.join(" + ");
                }
            }

            // Desugar LambdaMetafactory invokedynamic → () -> expr
            // Look up the pre-compiled lambda body by bootstrap_attr_index.
            if let Some(lambda) = lambda_body {
                return match lambda {
                    LambdaBootstrap::Lambda(body) => body.clone(),
                    LambdaBootstrap::KotlinLambda { body, .. } => body.clone(),
                    LambdaBootstrap::MethodReference {
                        reference_kind,
                        owner,
                        name,
                        ..
                    } => match *reference_kind {
                        // REF_newInvokeSpecial
                        8 => format!("{}::new", simple_name(owner)),
                        // REF_invokeStatic
                        6 => format!("{}::{}", simple_name(owner), name),
                        // A captured call-site argument is the receiver of a bound
                        // virtual/interface/special method reference.
                        5 | 7 | 9 if !args.is_empty() => {
                            format!("{}::{}", render_expr_prec(&args[0], 0), name)
                        }
                        5 | 7 | 9 => format!("{}::{}", simple_name(owner), name),
                        _ => format!(
                            "/*method-handle-{}*/ {}::{}",
                            reference_kind,
                            simple_name(owner),
                            name
                        ),
                    },
                };
            }

            let args_str = args.iter().map(render_expr).collect::<Vec<_>>().join(", ");
            format!("/*invokedynamic*/ {}({})", name, args_str)
        }

        // ── arrays ────────────────────────────────────────────────────
        Expr::ArrayLoad { array, index, .. } => {
            format!("{}[{}]", render_expr_prec(array, 0), render_expr(index))
        }

        Expr::ArrayStore {
            array,
            index,
            value,
        } => {
            format!(
                "{}[{}] = {}",
                render_expr_prec(array, 0),
                render_expr(index),
                render_expr(value)
            )
        }

        Expr::ArrayLength(arr) => {
            format!("{}.length", render_expr_prec(arr, 0))
        }

        Expr::NewArray {
            kind,
            type_,
            dimensions,
            initializer,
        } => {
            // `new Foo[]{a, b, c}` — the size is implied by the element list.
            if let Some(elems) = initializer {
                let inner: Vec<String> = elems.iter().map(render_expr).collect();
                return format!("new {}[]{{{}}}", render_type(type_), inner.join(", "));
            }
            let dims_str = dimensions.iter().map(render_expr).collect::<Vec<_>>();
            match kind {
                NewKind::PrimitiveArray { .. } | NewKind::RefArray => {
                    format!("new {}[{}]", render_type(type_), dims_str.join("]["))
                }
                NewKind::MultiArray { .. } => {
                    format!("new {}[{}]", render_type(type_), dims_str.join("]["))
                }
                NewKind::Object => format!("new {}()", render_type(type_)),
            }
        }

        // ── object creation ───────────────────────────────────────────
        Expr::New {
            class_name, args, ..
        } => {
            let args_str = args.iter().map(render_expr).collect::<Vec<_>>().join(", ");
            format!("new {}({})", simple_name(class_name), args_str)
        }

        // ── assignment ────────────────────────────────────────────────
        Expr::Assign { lhs, rhs } => {
            format!("{} = {}", render_expr_prec(lhs, 14), render_expr(rhs))
        }

        // ── misc ──────────────────────────────────────────────────────
        Expr::IInc { slot, delta, name } => {
            let fallback = format!("var{}", slot);
            let var = name.as_deref().unwrap_or(&fallback);
            if *delta == 1 {
                format!("{}++", var)
            } else if *delta == -1 {
                format!("{}--", var)
            } else if *delta > 0 {
                format!("{} += {}", var, delta)
            } else {
                format!("{} -= {}", var, -delta)
            }
        }

        Expr::Monitor { enter, object } => {
            if *enter {
                format!("/*monitorenter*/ {}", render_expr(object))
            } else {
                format!("/*monitorexit*/  {}", render_expr(object))
            }
        }

        Expr::Throw(exc) => format!("throw {}", render_expr(exc)),

        Expr::Return(Some(val)) => {
            format!("return {}", render_expr(val))
        }
        Expr::Return(None) => "return".into(),

        Expr::Opaque { opcode, offset } => {
            format!("/*opaque opcode=0x{:02x} @{}*/", opcode, offset)
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn render_const(c: &ConstExpr) -> String {
    match &c.value {
        ConstValue::Int(v) => {
            if c.ty == JavaType::BOOLEAN {
                if *v == 0 {
                    "false".into()
                } else {
                    "true".into()
                }
            } else if c.ty == JavaType::CHAR && *v >= 32 && *v < 127 {
                format!("'{}'", *v as u8 as char)
            } else {
                v.to_string()
            }
        }
        ConstValue::Long(v) => format!("{}L", v),
        ConstValue::Float(v) => format!("{}f", v),
        ConstValue::Double(v) => format!("{}d", v),
        ConstValue::StringRef(s) => format!("\"{}\"", s.replace('"', "\\\"").replace('\n', "\\n")),
        ConstValue::ClassRef(s) => format!("{}.class", simple_name(s)),
        ConstValue::Null => "null".into(),
    }
}

pub fn render_type(ty: &JavaType) -> String {
    ty.to_string()
}

fn render_cast_type(kind: CastKind) -> &'static str {
    match kind {
        CastKind::I2L => "long",
        CastKind::I2F => "float",
        CastKind::I2D => "double",
        CastKind::L2I => "int",
        CastKind::L2F => "float",
        CastKind::L2D => "double",
        CastKind::F2I => "int",
        CastKind::F2L => "long",
        CastKind::F2D => "double",
        CastKind::D2I => "int",
        CastKind::D2L => "long",
        CastKind::D2F => "float",
        CastKind::I2B => "byte",
        CastKind::I2C => "char",
        CastKind::I2S => "short",
        CastKind::CheckCast => "",
    }
}

/// `java/lang/String` → `String`
pub fn simple_name(binary: &str) -> String {
    binary_name_to_source(binary.rsplit('/').next().unwrap_or(binary))
}
