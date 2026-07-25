/// Integration tests for the types module.
#[cfg(test)]
mod descriptor_tests {
    use abyssflower_lib::types::descriptor::{parse_field_descriptor, MethodDescriptor};
    use abyssflower_lib::types::java_type::JavaType;

    #[test]
    fn test_multi_dim_array() {
        let (ty, _) = parse_field_descriptor("[[[I").unwrap();
        assert_eq!(ty.to_string(), "int[][][]");
        assert_eq!(ty.array_dim, 3);
    }

    #[test]
    fn test_inner_class_descriptor() {
        let (ty, _) = parse_field_descriptor("Ljava/util/Map$Entry;").unwrap();
        assert_eq!(ty.to_string(), "java.util.Map.Entry");
    }

    #[test]
    fn test_method_no_args() {
        let md = MethodDescriptor::parse("()Ljava/lang/String;").unwrap();
        assert_eq!(md.params.len(), 0);
        assert_eq!(md.return_type.to_string(), "java.lang.String");
    }

    #[test]
    fn test_method_multi_args() {
        let md = MethodDescriptor::parse("(IILjava/lang/String;Z)[Ljava/lang/Object;").unwrap();
        assert_eq!(md.params.len(), 4);
        assert_eq!(md.params[0], JavaType::INT);
        assert_eq!(md.params[1], JavaType::INT);
        assert_eq!(md.params[2].to_string(), "java.lang.String");
        assert_eq!(md.params[3], JavaType::BOOLEAN);
        assert_eq!(md.return_type.to_string(), "java.lang.Object[]");
    }

    #[test]
    fn test_method_param_slots() {
        // (J, I) -> void: J=2 slots + I=1 slot = 3
        let md = MethodDescriptor::parse("(JI)V").unwrap();
        assert_eq!(md.param_slots(), 3);
    }

    #[test]
    fn test_wide_types() {
        let (ty, _) = parse_field_descriptor("J").unwrap();
        assert_eq!(ty.stack_size(), 2);
        let (ty, _) = parse_field_descriptor("D").unwrap();
        assert_eq!(ty.stack_size(), 2);
    }

    #[test]
    fn test_descriptor_roundtrip() {
        let cases = ["I", "Ljava/lang/String;", "[[[B", "[[Ljava/util/List;"];
        for desc in cases {
            let (ty, n) = parse_field_descriptor(desc).unwrap();
            assert_eq!(n, desc.len(), "consumed bytes mismatch for {}", desc);
            assert_eq!(ty.to_descriptor(), desc, "roundtrip failed for {}", desc);
        }
    }
}

#[cfg(test)]
mod signature_tests {
    use abyssflower_lib::types::signature::*;

    #[test]
    fn test_simple_class_sig() {
        // ArrayList<E>: <E:Ljava/lang/Object;>Ljava/util/AbstractList<TE;>;Ljava/util/List<TE;>;...
        let sig = "<E:Ljava/lang/Object;>Ljava/util/AbstractList<TE;>;Ljava/util/List<TE;>;";
        let cs = parse_class_signature(sig).unwrap();
        assert_eq!(cs.type_params.len(), 1);
        assert_eq!(cs.type_params[0].name, "E");
        assert_eq!(cs.superinterfaces.len(), 1);
    }

    #[test]
    fn test_method_sig_generic() {
        // <T:Ljava/lang/Comparable<TT;>;>(TT;)TT;
        let sig = "<T:Ljava/lang/Comparable<TT;>;>(TT;)TT;";
        let ms = parse_method_signature(sig).unwrap();
        assert_eq!(ms.type_params.len(), 1);
        assert_eq!(ms.type_params[0].name, "T");
        assert_eq!(ms.params.len(), 1);
        // return type is a type var
        assert!(matches!(&ms.return_type, GenericType::TypeVar(n) if n == "T"));
    }

    #[test]
    fn test_method_sig_wildcard() {
        // (Ljava/util/List<+Ljava/lang/Number;>;)V
        let sig = "(Ljava/util/List<+Ljava/lang/Number;>;)V";
        let ms = parse_method_signature(sig).unwrap();
        assert_eq!(ms.params.len(), 1);
        if let GenericType::Class { args, .. } = &ms.params[0] {
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], TypeArg::Bounded { wildcard: Wildcard::Extends, .. }));
        } else {
            panic!("expected Class type");
        }
    }

    #[test]
    fn test_field_sig() {
        let sig = "Ljava/util/Map<Ljava/lang/String;Ljava/lang/Integer;>;";
        let fs = parse_field_signature(sig).unwrap();
        let rendered = fs.ty.to_string();
        assert!(rendered.contains("Map"), "expected Map in: {}", rendered);
        assert!(rendered.contains("String"), "expected String in: {}", rendered);
        assert!(rendered.contains("Integer"), "expected Integer in: {}", rendered);
    }

    #[test]
    fn test_type_var_sig() {
        let sig = "(TE;)V";
        let ms = parse_method_signature(sig).unwrap();
        assert!(matches!(&ms.params[0], GenericType::TypeVar(n) if n == "E"));
    }

    #[test]
    fn test_array_in_sig() {
        // ([Ljava/lang/String;)V
        let sig = "([Ljava/lang/String;)V";
        let ms = parse_method_signature(sig).unwrap();
        assert!(matches!(&ms.params[0], GenericType::Array { dims: 1, .. }));
    }

    #[test]
    fn test_throws_sig() {
        // ()V^Ljava/io/IOException;
        let sig = "()V^Ljava/io/IOException;";
        let ms = parse_method_signature(sig).unwrap();
        assert_eq!(ms.throws.len(), 1);
        assert!(ms.throws[0].to_string().contains("IOException"));
    }

    #[test]
    fn test_display_type_param() {
        let sig = "<T:Ljava/lang/Object;:Ljava/lang/Comparable<TT;>;>";
        // just check it parses without panic
        let ms = parse_method_signature(&format!("{}()V", sig)).unwrap();
        assert_eq!(ms.type_params.len(), 1);
    }
}
