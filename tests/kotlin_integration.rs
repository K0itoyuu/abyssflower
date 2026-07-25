/// Integration tests for Kotlin decompilation.
/// Tests that compiled Kotlin .class files produce correct Kotlin source output.

use abyssflower_lib::ClassFile;
use abyssflower_lib::kotlin::writer::{is_kotlin_class, render_kotlin_class};

fn decompile_kotlin(class_bytes: &[u8]) -> String {
    let cf = ClassFile::parse(class_bytes).expect("Failed to parse class file");
    assert!(is_kotlin_class(&cf), "Not detected as Kotlin class");
    render_kotlin_class(&cf)
}

// ── Data class ────────────────────────────────────────────────────────────

#[test]
fn test_data_class() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Person.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("data class Person"), "should have 'data class Person'");
    assert!(src.contains("val name: String"), "should have val name param");
    assert!(src.contains("val age: Int"), "should have val age param");
    assert!(src.contains("fun greet(): String"), "should have greet()");
    // Should NOT contain generated methods
    assert!(!src.contains("fun copy("), "should not show copy()");
    assert!(!src.contains("fun component1("), "should not show component1()");
    assert!(!src.contains("fun hashCode("), "should not show hashCode()");
}

// ── Object / Singleton ────────────────────────────────────────────────────

#[test]
fn test_object_singleton() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Singleton.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("object Singleton"), "should have 'object Singleton'");
    assert!(src.contains("val version: String"), "should have val version");
    assert!(src.contains("fun doSomething(): Int"), "should have doSomething()");
}

// ── Sealed class ──────────────────────────────────────────────────────────

#[test]
fn test_sealed_class() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Result.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("sealed class Result"), "should have 'sealed class Result'");
}

#[test]
fn test_sealed_subclass_data() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Result$Success.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("data class Success"), "should have 'data class Success'");
    assert!(src.contains("val value: String"), "should have val value");
    assert!(src.contains(": Result"), "should extend Result");
}

#[test]
fn test_sealed_subclass_object() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Result$Loading.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("object Loading"), "should have 'object Loading'");
    assert!(src.contains(": Result"), "should extend Result");
}

// ── Enum class ────────────────────────────────────────────────────────────

#[test]
fn test_enum_class() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Direction.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("enum class Direction"), "should have 'enum class Direction'");
    assert!(src.contains("NORTH"), "should have NORTH");
    assert!(src.contains("SOUTH"), "should have SOUTH");
    assert!(src.contains("EAST"), "should have EAST");
    assert!(src.contains("WEST"), "should have WEST");
    assert!(src.contains("fun opposite(): Direction"), "should have opposite()");
    // Should NOT show Enum<Direction> supertype
    assert!(!src.contains("Enum<"), "should not show Enum<Direction> supertype");
}

// ── Interface with generics ───────────────────────────────────────────────

#[test]
fn test_interface_generics() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Repository.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("interface Repository<out T, in K>"), "should have variance annotations");
    assert!(src.contains("fun findById(id: K): T?"), "should have nullable return");
    assert!(src.contains("fun findAll(): List<T>"), "should have List<T> return");
    // Should NOT have abstract keyword (implicit in interfaces)
    assert!(!src.contains("abstract fun"), "should not show abstract");
}

// ── Companion object ──────────────────────────────────────────────────────

#[test]
fn test_companion_object() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Counter.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("class Counter"), "should have class Counter");
    assert!(src.contains("private constructor"), "should have private constructor");
    assert!(src.contains("companion object"), "should have companion object");
    assert!(src.contains("val count: Int"), "should have val count");
    assert!(src.contains("fun increment(): Counter"), "should have increment()");
}

// ── Extension functions and suspend ───────────────────────────────────────

#[test]
fn test_extension_functions() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/TestExtensionKt.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("fun String.wordCount(): Int"), "should have extension function");
    assert!(src.contains("suspend fun fetchData(url: String): String"), "should have suspend");
    assert!(src.contains("inline fun <reified T>"), "should have inline reified");
}

// ── Generics with bounds ──────────────────────────────────────────────────

#[test]
fn test_class_generics() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Container.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("class Container<T : Comparable<T>>"), "should have type bound");
    assert!(src.contains("fun <R> map("), "should have function type param");
    assert!(src.contains("(T) -> R"), "should have function type syntax");
}

// ── Method body decompilation ─────────────────────────────────────────────

#[test]
fn test_method_body_decompiled() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Person.class").unwrap();
    let src = decompile_kotlin(&bytes);
    // Method bodies should NOT be placeholders
    assert!(!src.contains("{ ... }"), "should not have placeholder bodies");
    // greet() should have actual body with string template
    assert!(src.contains("return \"Hello, ${this.name}!\""), "should have string template in greet()");
}

#[test]
fn test_method_body_simple_return() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Singleton.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(!src.contains("{ ... }"), "should not have placeholder bodies");
    assert!(src.contains("return 42"), "doSomething should return 42");
}

#[test]
fn test_method_body_constructor_call() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Counter.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(!src.contains("{ ... }"), "should not have placeholder bodies");
    assert!(src.contains("Counter(this.count + 1)"), "increment() should create new Counter");
}

#[test]
fn test_method_body_intrinsics_suppressed() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/TestExtensionKt.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(!src.contains("{ ... }"), "should not have placeholder bodies");
    // Intrinsics.checkNotNullParameter should be suppressed
    assert!(!src.contains("checkNotNullParameter"), "should suppress intrinsics null checks");
}

// ── Pattern decompilation ─────────────────────────────────────────────────

#[test]
fn test_destructuring_declaration() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Patterns.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("val (a, b) = pair"), "should detect destructuring");
}

#[test]
fn test_range_for_loop() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Patterns.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("for (i in 1..10)"), "should detect range for loop");
}

#[test]
fn test_property_custom_getter() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Patterns.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("get()"), "should have custom getter");
    assert!(src.contains("this.items.isEmpty()"), "getter should access items");
}

#[test]
fn test_for_in_loop_detection() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Patterns.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("for (item in"), "should detect for-in loop");
    assert!(!src.contains("while (var3.hasNext()"), "should not show raw iterator loop");
}

#[test]
fn test_val_var_detection() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Patterns.class").unwrap();
    let src = decompile_kotlin(&bytes);
    // Declared variables should use val/var, not Java type declarations like "int x ="
    assert!(!src.contains("int sum ="), "should not have Java-style 'int' declarations");
    assert!(src.contains("val sum = 0") || src.contains("var sum = 0"),
        "should use val/var for declarations");
}

#[test]
fn test_when_enum_names() {
    let bytes = std::fs::read("tests/kotlin_classes/pkg/Direction.class").unwrap();
    let src = decompile_kotlin(&bytes);
    assert!(src.contains("when (this)"), "should use 'when' with subject");
    assert!(src.contains("NORTH ->"), "should map enum case to NORTH");
    assert!(src.contains("SOUTH ->"), "should map enum case to SOUTH");
}
