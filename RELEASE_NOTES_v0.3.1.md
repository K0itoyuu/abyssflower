## Abyssflower v0.3.1

This release improves grouped Kotlin decompilation throughput and adds native
whole-directory and whole-archive workflows.

### Performance

- Add a shared indexed Kotlin group context for constant-time related-class
  lookup.
- Cache recovered synthetic function objects across the complete Kotlin group.
- Parse class files and render independent Kotlin source units in parallel with
  deterministic nested-class and multi-file-facade merging.
- Improve the 4,026-class LiquidBounce Kotlin corpus median from 124.82 seconds
  to 32.36 seconds while keeping peak memory near 300 MiB.
- Reach throughput comparable to Vineflower's single-thread configuration on
  the same corpus while using substantially less memory.

### Batch decompilation

- Add `Decompiler::decompile_paths`, `decompile_directory`, and
  `decompile_jar` library APIs.
- Accept directories as CLI inputs and recursively discover class files.
- Allow `--jar <archive> --output <directory>` to decompile a complete JAR;
  `--entry` remains available for one-entry operation.
- Open each archive once and read class entries directly through a buffered ZIP
  reader instead of reloading the complete archive for every entry.
- Process mixed archives in auto mode by grouping valid Kotlin metadata classes
  and emitting the remaining classes as Java.

### Validation

- The LiquidBounce directory corpus still produces the same 1,727-file set
  with no opaque expressions, TODOs, unresolved placeholders, raw JVM compare
  opcodes, or unrecovered invokedynamic markers in Kotlin output.
- Native whole-JAR auto mode processes all 4,250 LiquidBounce classes into
  1,727 Kotlin and 224 Java files in approximately 33 seconds on the reference
  machine.
- Formatting, Clippy, all-feature tests, no-default-feature tests, release
  builds, and package generation pass.
