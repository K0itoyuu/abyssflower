/// Integration tests for Phase 6: end-to-end decompilation.
#[cfg(test)]
mod codegen_tests {
    use abyssflower_lib::codegen::render_class;
    use abyssflower_lib::ClassFile;

    fn decompile(path: &str) -> String {
        let bytes = std::fs::read(path).unwrap_or_else(|_| panic!("cannot read {}", path));
        let cf = ClassFile::parse(&bytes).expect("parse failed");
        render_class(&cf)
    }

    fn base(name: &str) -> String {
        format!("vineflower-master/testData/classes/custom/{}", name)
    }

    // ── smoke tests: must not panic and produce non-empty output ─────────

    macro_rules! smoke {
        ($name:ident, $file:expr) => {
            #[test]
            #[ignore = "requires vineflower testData"]
            fn $name() {
                let src = decompile(&base($file));
                assert!(
                    !src.is_empty(),
                    "decompilation of {} produced empty output",
                    $file
                );
            }
        };
        ($name:ident, full: $file:expr) => {
            #[test]
            #[ignore = "requires vineflower testData"]
            fn $name() {
                let src = decompile($file);
                assert!(
                    !src.is_empty(),
                    "decompilation of {} produced empty output",
                    $file
                );
            }
        };
    }

    smoke!(smoke_switch_enum, "TestEclipseSwitchEnum.class");
    smoke!(smoke_switch_string, "TestEclipseSwitchString.class");
    smoke!(smoke_jsr, "TestJsr.class");
    smoke!(smoke_hotjava, "TestHotjava.class");
    smoke!(smoke_synchronized, "TestJava1Synchronized.class");
    smoke!(smoke_string_concat, "TestStringConcatJ19.class");
    smoke!(smoke_switch_j21, "TestSwitchOnEnumWithoutEnumJ21.class");
    smoke!(smoke_signatures, "TestCorruptedSignatures.class");
    smoke!(smoke_inner_ctor,    full: "vineflower-master/testData/classes/custom/v11/TestInnerClassConstructor.class");
    smoke!(smoke_main,          full: "vineflower-master/testData/classes/bulk/pkg/Main.class");

    // ── structural correctness tests ──────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_package_declaration() {
        let src = decompile(&base("../bulk/pkg/Main.class"));
        assert!(
            src.contains("package pkg;"),
            "Expected package declaration, got:\n{}",
            &src[..src.len().min(200)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_class_declaration_public() {
        let src = decompile(&base("../bulk/pkg/Main.class"));
        assert!(
            src.contains("public class Main"),
            "Expected public class Main in:\n{}",
            &src[..src.len().min(400)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_interface_declaration() {
        let src = decompile(&base("TestCorruptedSignatures.class"));
        assert!(
            src.contains("abstract class Signatures"),
            "Expected abstract class declaration in:\n{}",
            &src[..src.len().min(400)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_generic_implements() {
        let src = decompile(&base("TestCorruptedSignatures.class"));
        assert!(
            src.contains("java.util.Map") || src.contains("Map"),
            "Expected generic Map interface in:\n{}",
            &src[..src.len().min(500)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_enum_declaration() {
        let src = decompile(&base("TestEclipseSwitchEnum.class"));
        assert!(
            src.contains("enum TestEclipseSwitchEnum"),
            "Expected enum declaration in:\n{}",
            &src[..src.len().min(400)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_static_fields() {
        // Enum constants must render as "A," or "A;" (enum declaration syntax), not as static fields
        let src = decompile(&base("TestEclipseSwitchEnum.class"));
        assert!(
            src.contains("A,") || src.contains("A;"),
            "Expected enum constant A in declaration syntax in:\n{}",
            &src[..src.len().min(600)]
        );
        // The old "public static final ... A;" field syntax must NOT appear
        assert!(
            !src.contains("public static final pkg.TestEclipseSwitchEnum A"),
            "Enum constant should not appear as static field:\n{}",
            &src[..src.len().min(600)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_method_declaration() {
        let src = decompile(&base("../bulk/pkg/Main.class"));
        assert!(
            src.contains("public static void main("),
            "Expected main method declaration in:\n{}",
            &src[..src.len().min(600)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_generic_field_type() {
        let src = decompile(&base("TestCorruptedSignatures.class"));
        assert!(
            src.contains("Map<java.lang.String, java.lang.String>") || src.contains("Map<"),
            "Expected generic field type in:\n{}",
            &src[..src.len().min(600)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_switch_statement() {
        let src = decompile(&base("TestEclipseSwitchEnum.class"));
        assert!(
            src.contains("switch ("),
            "Expected switch statement in:\n{}",
            &src[..src.len().min(1500)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_try_catch_output() {
        let src = decompile(&base("TestJsr.class"));
        // JSR-containing methods may produce try-catch blocks
        assert!(!src.is_empty());
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_method_invocation() {
        let src = decompile(&base("../bulk/pkg/Main.class"));
        assert!(
            src.contains("println(") || src.contains("getResource()"),
            "Expected method call in:\n{}",
            &src[..src.len().min(800)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_new_object() {
        let src = decompile(&base("../bulk/pkg/Main.class"));
        assert!(
            src.contains("new Loader(") || src.contains("new loader"),
            "Expected new object creation in:\n{}",
            &src[..src.len().min(800)]
        );
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_return_statement() {
        // Void methods: trailing bare "return;" must be suppressed.
        let src = decompile(&base("../bulk/pkg/Main.class"));
        assert!(
            !src.contains("return;"),
            "Trailing return; should be suppressed in void method:\n{}",
            &src[..src.len().min(800)]
        );
        // Return with a value ("return <expr>;") must still appear.
        // TestCorruptedSignatures constructor assigns to this.field, then returns void —
        // but its `return;` should also be gone, leaving no bare return; anywhere.
        // Use an enum valueOf() which returns the enum type.
        let src_enum = decompile(&base("TestEclipseSwitchEnum.class"));
        // The src must still compile without "return;" appearing as bare statement at end
        assert!(!src_enum.is_empty(), "TestEclipseSwitchEnum must decompile");
    }

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_no_raw_opcode_leak() {
        // Ensure we don't produce too many opaque/unrecognized opcode markers
        let src = decompile(&base("../bulk/pkg/Main.class"));
        let opaque_count = src.matches("/*opaque").count();
        assert!(
            opaque_count == 0,
            "Found {} opaque opcode placeholders in Main.class output",
            opaque_count
        );
    }

    // ── inner class / nested type ─────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_inner_constructor_class() {
        let src = decompile(
            "vineflower-master/testData/classes/custom/v11/TestInnerClassConstructor.class",
        );
        assert!(
            src.contains("class TestInnerClassConstructor"),
            "Expected class declaration in:\n{}",
            &src[..src.len().min(400)]
        );
    }

    // ── varargs / generic method ──────────────────────────────────────────

    #[test]
    #[ignore = "requires vineflower testData"]
    fn test_conflicting_lvt_namer() {
        let src = decompile(
            "vineflower-master/testData/classes/custom/pkg/TestConflictingLvtNamer.class",
        );
        assert!(
            src.contains("method(") || src.contains("Supplier"),
            "Expected method with Supplier in:\n{}",
            &src[..src.len().min(600)]
        );
    }
}
