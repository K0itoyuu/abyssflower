## Abyssflower v0.3.0

This release substantially expands Kotlin-aware decompilation and hardens the
CLI and library interfaces for production use.

### Kotlin recovery

- Recover Kotlin coroutine state machines and directly instantiated
  `SuspendLambda` implementations, including captured values and suspension
  points.
- Decode JVM metadata member signatures and associate functions, properties,
  accessors, constructors, and backing fields by name and descriptor.
- Recover property initializers, delegates, default parameters, anonymous
  objects, `runCatching` class probes, computed delegates, and function
  references.
- Decode LambdaMetafactory bootstrap methods and Kotlin synthetic function
  objects without leaving invokedynamic placeholders.
- Merge nested classes, companion objects, enum-entry bodies, synthetic
  implementation classes, and multi-file facades into source-level units.
- Apply consistent Kotlin identifier, class-name, and package-name escaping.

### Control flow and stack simulation

- Add block-entry operand-stack, local-type, and local-value dataflow across
  the CFG.
- Add post-dominator-aware branch recovery and Kotlin guard convergence for
  shared continuations with nested early returns.
- Improve null-check, safe-call, Elvis, comparison, loop, switch-expression,
  exception-range, and value-producing control-flow recovery.
- Validate stack manipulation categories and report malformed or underflowing
  bytecode without panicking.

### CLI and library

- Add `--java`, `--kotlin`, and `--auto` language selection.
- Add bounded JAR entry decompilation with `--jar` and `--entry`.
- Add grouped Kotlin input processing and capability-based output-path
  confinement.
- Share resource-bounded decompilation behavior across the CLI, C API, and MCP
  server.
- Distinguish usage errors from runtime failures with stable exit codes.

### Validation

- The LiquidBounce Kotlin corpus decompiles in 308/308 directory groups to
  1,727 source files with zero Kotlin PSI syntax errors.
- The corpus contains no unrecovered TODOs, invokedynamic placeholders, raw
  JVM compare opcodes, opaque comments, or unresolved-expression markers.
- The full Rust test suite passes; tests requiring an external Vineflower
  checkout remain explicitly ignored.
