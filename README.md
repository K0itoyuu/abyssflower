# Abyssflower

A high-performance JVM bytecode decompiler written in Rust, with first-class Kotlin support.

Abyssflower reads `.class` files and produces idiomatic Kotlin (or Java) source code, leveraging Kotlin metadata (protobuf) to restore language-specific constructs that are erased at the bytecode level.

## Features

**Kotlin-aware decompilation**
- `data class` with `val`/`var` constructor parameters — synthetic methods (`copy`, `componentN`, `hashCode`, `equals`, `toString`) are filtered
- `object` / `companion object` / `sealed class` / `enum class`
- Extension functions with `this` receiver restoration
- `operator` / `inline` / `infix` / `suspend` / `tailrec` / `reified` modifiers
- Nullable types (`T?`) and function types (`(T) -> R`)
- Generic type constraints (`T : Comparable<T>`) with `in`/`out` variance

**Expression-level recovery**
- `?.` safe calls and `?:` Elvis operator (stack-based pattern detection)
- String templates: `"Hello, ${name}!"` from `StringBuilder` chains
- `when` expressions with enum constant name resolution
- `if`/`else` expression rendering (ternary patterns)
- Destructuring declarations: `val (a, b) = pair`
- `for (item in collection)` loop restoration from `iterator`/`hasNext`/`next`
- Range expressions: `for (i in 1..10)`
- Default parameter values extracted from `$default` synthetic methods
- Property initializers from `<init>`/`<clinit>` bytecode

**Performance**
- ~23ms per class file (9x faster than Vineflower on equivalent workload)
- Zero JVM startup cost — native Rust binary
- Single-file processing — no classpath required

## Usage

```bash
abyssflower <class-file>
```

```bash
$ abyssflower Person.class
// Decompiled from: Person.class
package pkg

data class Person(val name: String, val age: Int) {
    fun greet(): String {
        return "Hello, ${this.name}!"
    }
}
```

## Building

```bash
cargo build --release
```

The binary is at `target/release/abyssflower`.

### Running tests

```bash
cargo test
```

## How It Works

1. **Class file parsing** — reads the constant pool, fields, methods, attributes, and Kotlin metadata annotation
2. **Kotlin metadata decoding** — deserializes the protobuf-encoded `@Metadata` annotation to recover type information, generics, modifiers, and class structure
3. **CFG construction** — builds a control flow graph from bytecode instructions
4. **Structural recovery** — converts the CFG into a structured statement tree (if/else, loops, switch/when, try/catch) using dominator-based analysis
5. **Stack simulation** — simulates the JVM operand stack to recover expressions from bytecode
6. **Kotlin rendering** — renders the structured IR as idiomatic Kotlin, applying pattern detection for `?.`, `?:`, string templates, destructuring, ranges, and other Kotlin idioms

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

## Acknowledgments

- [Vineflower](https://github.com/Vineflower/vineflower) — the state-of-the-art Java/Kotlin decompiler whose output quality served as the benchmark target for this project. Vineflower's Kotlin plugin demonstrated that metadata-driven decompilation is both feasible and valuable.
- [Fernflower](https://github.com/fesh0r/fernflower) — the foundational analytical decompiler that Vineflower builds upon, originally created by Stiver.
- [JetBrains](https://www.jetbrains.com/) — for the Kotlin language, its well-designed metadata format, and the protobuf schema that makes metadata-driven decompilation possible.
