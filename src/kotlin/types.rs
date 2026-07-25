/// Kotlin type rendering — converts KType to Kotlin syntax strings.

use super::metadata::*;

/// Render a KType to its Kotlin string representation.
pub fn render_kotlin_type(ty: &KType, type_params: &[KTypeParameter]) -> String {
    // Use abbreviated type if available
    if let Some(ref abbr) = ty.abbreviated_type {
        return render_kotlin_type(abbr, type_params);
    }

    let base = if let Some(ref class_name) = ty.class_name {
        kotlin_type_name(class_name)
    } else if let Some(tp_id) = ty.type_parameter_id {
        // Resolve type parameter name from the type_params list
        type_params.iter()
            .find(|p| p.id == tp_id)
            .map(|p| p.name.clone())
            .or_else(|| ty.type_parameter_name.clone())
            .unwrap_or_else(|| format!("T{}", tp_id))
    } else if let Some(ref tp_name) = ty.type_parameter_name {
        tp_name.clone()
    } else {
        "Any".to_string()
    };

    // Check if this is a function type
    if let Some(ref class_name) = ty.class_name {
        if is_function_type(class_name) {
            let rendered = render_function_type(ty, type_params);
            if ty.nullable {
                return format!("({})?", rendered);
            }
            return rendered;
        }
    }

    // Render type arguments
    let mut result = base;
    if !ty.arguments.is_empty() {
        let args: Vec<String> = ty.arguments.iter().map(|a| {
            render_type_argument(a, type_params)
        }).collect();
        result = format!("{}<{}>", result, args.join(", "));
    }

    if ty.nullable {
        result.push('?');
    }

    result
}

/// Render a type argument.
fn render_type_argument(arg: &KTypeArgument, type_params: &[KTypeParameter]) -> String {
    match arg.projection {
        Projection::Star => "*".to_string(),
        Projection::In => {
            if let Some(ref t) = arg.type_ {
                format!("in {}", render_kotlin_type(t, type_params))
            } else {
                "in Any?".to_string()
            }
        }
        Projection::Out => {
            if let Some(ref t) = arg.type_ {
                format!("out {}", render_kotlin_type(t, type_params))
            } else {
                "out Any?".to_string()
            }
        }
        Projection::Inv => {
            if let Some(ref t) = arg.type_ {
                render_kotlin_type(t, type_params)
            } else {
                "Any?".to_string()
            }
        }
    }
}

/// Check if a class name represents a Kotlin function type.
fn is_function_type(class_name: &str) -> bool {
    class_name.starts_with("kotlin/Function")
        || class_name.starts_with("kotlin/jvm/functions/Function")
        || class_name.starts_with("kotlin/reflect/KFunction")
}

/// Render a function type as `(P1, P2) -> R`.
fn render_function_type(ty: &KType, type_params: &[KTypeParameter]) -> String {
    if ty.arguments.is_empty() {
        return "() -> Unit".to_string();
    }

    // Last argument is return type, rest are parameter types
    let params = &ty.arguments[..ty.arguments.len() - 1];
    let return_arg = &ty.arguments[ty.arguments.len() - 1];

    let param_strs: Vec<String> = params.iter().map(|a| {
        if let Some(ref t) = a.type_ {
            render_kotlin_type(t, type_params)
        } else {
            "Any?".to_string()
        }
    }).collect();

    let return_str = if let Some(ref t) = return_arg.type_ {
        render_kotlin_type(t, type_params)
    } else {
        "Unit".to_string()
    };

    format!("({}) -> {}", param_strs.join(", "), return_str)
}

/// Convert a Kotlin internal class name to its Kotlin display name.
pub fn kotlin_type_name(internal: &str) -> String {
    // Map JVM primitives and standard library types
    match internal {
        "kotlin/Any" => "Any".to_string(),
        "kotlin/Nothing" => "Nothing".to_string(),
        "kotlin/Unit" => "Unit".to_string(),
        "kotlin/Boolean" => "Boolean".to_string(),
        "kotlin/Byte" => "Byte".to_string(),
        "kotlin/Short" => "Short".to_string(),
        "kotlin/Int" => "Int".to_string(),
        "kotlin/Long" => "Long".to_string(),
        "kotlin/Float" => "Float".to_string(),
        "kotlin/Double" => "Double".to_string(),
        "kotlin/Char" => "Char".to_string(),
        "kotlin/String" => "String".to_string(),
        "kotlin/Array" => "Array".to_string(),
        "kotlin/IntArray" => "IntArray".to_string(),
        "kotlin/LongArray" => "LongArray".to_string(),
        "kotlin/ByteArray" => "ByteArray".to_string(),
        "kotlin/ShortArray" => "ShortArray".to_string(),
        "kotlin/FloatArray" => "FloatArray".to_string(),
        "kotlin/DoubleArray" => "DoubleArray".to_string(),
        "kotlin/BooleanArray" => "BooleanArray".to_string(),
        "kotlin/CharArray" => "CharArray".to_string(),
        "kotlin/Number" => "Number".to_string(),
        "kotlin/Throwable" => "Throwable".to_string(),
        "kotlin/Comparable" => "Comparable".to_string(),
        "kotlin/Enum" => "Enum".to_string(),
        "kotlin/CharSequence" => "CharSequence".to_string(),
        "kotlin/Cloneable" => "Cloneable".to_string(),
        "kotlin/Annotation" => "Annotation".to_string(),
        "kotlin/collections/List" => "List".to_string(),
        "kotlin/collections/MutableList" => "MutableList".to_string(),
        "kotlin/collections/Set" => "Set".to_string(),
        "kotlin/collections/MutableSet" => "MutableSet".to_string(),
        "kotlin/collections/Map" => "Map".to_string(),
        "kotlin/collections/MutableMap" => "MutableMap".to_string(),
        "kotlin/collections/Iterable" => "Iterable".to_string(),
        "kotlin/collections/MutableIterable" => "MutableIterable".to_string(),
        "kotlin/collections/Collection" => "Collection".to_string(),
        "kotlin/collections/MutableCollection" => "MutableCollection".to_string(),
        "kotlin/collections/Iterator" => "Iterator".to_string(),
        "kotlin/collections/MutableIterator" => "MutableIterator".to_string(),
        "kotlin/collections/Map.Entry" => "Map.Entry".to_string(),
        "kotlin/collections/MutableMap.MutableEntry" => "MutableMap.MutableEntry".to_string(),
        _ => {
            // For other types, extract the simple name
            let name = internal.rsplit('/').next().unwrap_or(internal);
            name.replace('$', ".").to_string()
        }
    }
}

/// Render type parameters declaration: `<T : Comparable<T>, R>`.
pub fn render_type_params_decl(params: &[KTypeParameter]) -> String {
    if params.is_empty() {
        return String::new();
    }

    let strs: Vec<String> = params.iter().map(|tp| {
        let mut s = String::new();

        if tp.reified {
            s.push_str("reified ");
        }
        match tp.variance {
            Variance::In => s.push_str("in "),
            Variance::Out => s.push_str("out "),
            Variance::Inv => {}
        }
        s.push_str(&tp.name);

        // Upper bounds (only first one goes here, rest go in where clause)
        if let Some(first_bound) = tp.upper_bounds.first() {
            let bound_str = render_kotlin_type(first_bound, params);
            if bound_str != "Any?" {
                s.push_str(" : ");
                s.push_str(&bound_str);
            }
        }

        s
    }).collect();

    format!("<{}>", strs.join(", "))
}
