use abyssflower_lib::{DecompileLanguage, DecompileOptions, Decompiler};

const FIXTURE: &[u8] = include_bytes!("java_classes/fixture/ControlFlowFixture.class");
const WRITER_EDGE_FIXTURE: &[u8] = include_bytes!("java_classes/fixture/WriterEdgeFixture.class");
const ANONYMOUS_CLASS_FIXTURE: &[u8] =
    include_bytes!("java_classes/fixture/WriterEdgeFixture$1.class");

#[test]
fn decompiles_repository_java_fixture() {
    let output = Decompiler::default().decompile_bytes(FIXTURE).unwrap();
    assert_eq!(output.language, DecompileLanguage::Java);
    assert_eq!(output.class_name, "fixture/ControlFlowFixture");
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(output.source.contains("class ControlFlowFixture"));
    assert!(output.source.contains("switch ("));
    assert!(output.source.contains("return this.name;"));
    assert!(output.source.contains("return \"small\";"));
    assert!(output.source.contains("return value * 2L;"));
    assert!(output.source.contains("long doubleValue("));
    assert!(!output.source.contains("/*opaque"));
}

#[test]
fn renders_generic_class_and_instance_lambda_as_valid_java_syntax() {
    let output = Decompiler::default()
        .decompile_bytes(WRITER_EDGE_FIXTURE)
        .unwrap();

    assert!(
        output.source.contains(
            "class WriterEdgeFixture<T extends java.lang.Number & java.lang.Comparable<T>>"
        ),
        "generic parameters must follow the class name:\n{}",
        output.source
    );
    assert!(
        output.source.contains("(java.lang.String text) ->"),
        "instance lambda parameter should come from LVT slot 1:\n{}",
        output.source
    );
    assert!(!output.source.contains("WriterEdgeFixture.1"));
    assert!(!output.source.contains("String this)"));
    assert!(
        !output.source.contains("/*opaque"),
        "stack merges must not underflow:\n{}",
        output.source
    );
    assert!(
        output.source.contains("int latchGuardLoop(")
            && output.source.contains("if (i == limit)")
            && output.source.contains("break;"),
        "a conditional latch exit must remain an explicit break:\n{}",
        output.source
    );
    assert!(
        !output.source.contains("/* no branch */"),
        "loop recovery must not synthesize a condition from an unconditional tail:\n{}",
        output.source
    );
    assert!(
        output.source.contains("int realDoWhile(")
            && output.source.contains("do {")
            && output.source.contains("} while ("),
        "a conditional tail back-edge must remain a do-while loop:\n{}",
        output.source
    );
    assert!(
        output
            .source
            .contains("setRotation(180f, enabled ? left : right)"),
        "receiver and first argument must survive the conditional merge:\n{}",
        output.source
    );
    assert!(
        output.source.contains("return switch ("),
        "value-producing switch must reach its return consumer:\n{}",
        output.source
    );
    assert!(
        output
            .source
            .contains("boolean switchValueWithShortCircuit("),
        "switch arms with multiple incoming values must remain decompilable:\n{}",
        output.source
    );
    assert!(
        output
            .source
            .contains("java.lang.String nestedSwitchValue("),
        "nested value-producing switches must remain decompilable:\n{}",
        output.source
    );
    assert!(
        output.source.contains("long switchBehindConditional("),
        "an outer conditional and inner value switch must share a return value:\n{}",
        output.source
    );
    assert!(
        !output.source.contains("/*opaque"),
        "nested value merges must not degrade to opaque expressions:\n{}",
        output.source
    );
    assert!(output.source.contains("java.lang.String var2 = text;"));
    assert!(output.source.contains("int var3 = -1;"));
    assert!(output.source.contains("case \"\\n\":"));
    assert!(
        output.source.contains("this::consume"),
        "bound instance method references must use the captured receiver:\n{}",
        output.source
    );
    assert!(
        !output.source.contains("/*invokedynamic*/"),
        "nested lambdas must resolve after their inner bootstrap target:\n{}",
        output.source
    );
}

#[test]
fn preserves_numeric_inner_class_suffix_in_declaration() {
    let output = Decompiler::default()
        .decompile_bytes(ANONYMOUS_CLASS_FIXTURE)
        .unwrap();

    assert!(
        output.source.contains("class WriterEdgeFixture$1"),
        "numeric inner class suffix must remain a legal identifier:\n{}",
        output.source
    );
    assert!(!output.source.contains("WriterEdgeFixture.1"));
}

#[test]
fn enforces_class_size_before_parsing() {
    let decompiler = Decompiler::new(DecompileOptions {
        max_class_size: (FIXTURE.len() - 1) as u64,
        ..DecompileOptions::default()
    });
    assert!(decompiler.decompile_bytes(FIXTURE).is_err());
}

#[test]
fn forced_kotlin_rejects_java_fixture() {
    let decompiler = Decompiler::new(DecompileOptions {
        language: DecompileLanguage::Kotlin,
        ..DecompileOptions::default()
    });
    assert!(decompiler.decompile_bytes(FIXTURE).is_err());
}
