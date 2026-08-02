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
- Zero JVM startup cost — native Rust binary
- Parallel class parsing and Kotlin source-unit rendering
- Indexed cross-class lookup for grouped Kotlin decompilation
- Single-file, directory, and whole-archive processing

## Usage

### CLI

```bash
abyssflower <class-file>
abyssflower file1.class file2.class ...
abyssflower <class-file> -o <output-dir>
abyssflower <classes-directory> -o <output-dir>
abyssflower --jar app.jar -o <output-dir>
abyssflower --jar app.jar --entry com/example/Main.class
```

Output language can be selected explicitly with `--java`, `--kotlin`, or
`--auto` (the default). `--kotlin` fails when valid Kotlin metadata is not
present; `--auto` falls back to Java and reports a diagnostic when malformed
Kotlin metadata is encountered. Invalid arguments exit with status 2, runtime
decompilation or I/O failures with status 1, and successful runs with status 0.
Complete archives and directory inputs require `--output`; archive entries and
individual class files may still be written to stdout. Batch inputs are parsed
in parallel, while related Kotlin classes share one indexed group context so
nested classes, companions, and multi-file facades can be merged consistently.

Inputs are resource-bounded by default: class files and uncompressed archive
entries are limited to 64 MiB, while the archive itself is limited to 512 MiB.
The limits can be reduced or raised with `--max-class-size`,
`--max-archive-size`, and `--max-archive-entry-size`. Source files written with
`-o` are derived from validated JVM class names and remain below that output
directory.

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

### MCP Server (AI Assistant Integration)

Run as an [MCP](https://modelcontextprotocol.io) server over stdio, exposing decompilation tools to AI assistants like Claude:

```bash
abyssflower --mcp
```

**Claude Desktop configuration** (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "abyssflower": {
      "command": "/path/to/abyssflower",
      "args": ["--mcp"]
    }
  }
}
```

**Exposed tools:**

| Tool | Parameters | Description |
|------|-----------|-------------|
| `decompile_file` | `path` | Decompile a .class file at the given path |
| `decompile_jar_entry` | `jar_path`, `class_path` | Decompile a class entry from inside a JAR |
| `decompile_bytes` | `bytes_base64` | Decompile from base64-encoded .class bytes |

### Shared Library (DLL / SO / dylib)

Build produces a shared library alongside the CLI binary:

| Platform | File |
|----------|------|
| Windows | `abyssflower_lib.dll` |
| Linux | `libabyssflower_lib.so` |
| macOS | `libabyssflower_lib.dylib` |

**C API:**

```c
// Decompile from raw bytes (auto-detects Kotlin/Java)
char* abyssflower_decompile(const uint8_t* data, size_t len);

// Decompile forcing Java output
char* abyssflower_decompile_java(const uint8_t* data, size_t len);

// Decompile a .class file by path
char* abyssflower_decompile_file(const char* path);

// Decompile a class entry from a JAR file
char* abyssflower_decompile_jar_entry(const char* jar_path, const char* class_path);

// Free a returned string (must call after using the result)
void abyssflower_free(char* ptr);

// Get version (static, do NOT free)
const char* abyssflower_version();
```

**Python example:**

```python
import ctypes

lib = ctypes.CDLL("abyssflower_lib.dll")  # or .so / .dylib
lib.abyssflower_decompile_file.restype = ctypes.c_void_p
lib.abyssflower_decompile_file.argtypes = [ctypes.c_char_p]
lib.abyssflower_decompile_jar_entry.restype = ctypes.c_void_p
lib.abyssflower_decompile_jar_entry.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
lib.abyssflower_free.argtypes = [ctypes.c_void_p]

# Decompile a file
ptr = lib.abyssflower_decompile_file(b"Person.class")
print(ctypes.string_at(ptr).decode())
lib.abyssflower_free(ptr)

# Decompile from a JAR
ptr = lib.abyssflower_decompile_jar_entry(b"app.jar", b"com/example/Main.class")
print(ctypes.string_at(ptr).decode())
lib.abyssflower_free(ptr)
```

**C/C++ example:**

```c
#include <stdio.h>
#include <stdint.h>

extern char* abyssflower_decompile_file(const char* path);
extern void abyssflower_free(char* ptr);

int main() {
    char* result = abyssflower_decompile_file("Person.class");
    if (result) {
        printf("%s\n", result);
        abyssflower_free(result);
    }
    return 0;
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
cargo test --no-default-features
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
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
