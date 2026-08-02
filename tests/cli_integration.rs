use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};

const FIXTURE: &str = "tests/java_classes/fixture/ControlFlowFixture.class";
const ENTRY: &str = "fixture/ControlFlowFixture.class";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abyssflower"))
        .args(args)
        .output()
        .unwrap()
}

fn temp_jar() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "abyssflower-cli-{}-{}.jar",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let file = std::fs::File::create(&path).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    archive
        .start_file(ENTRY, zip::write::SimpleFileOptions::default())
        .unwrap();
    archive
        .write_all(include_bytes!(
            "java_classes/fixture/ControlFlowFixture.class"
        ))
        .unwrap();
    archive.finish().unwrap();
    path
}

#[test]
fn usage_and_conflicts_exit_with_code_2() {
    let missing = run(&[]);
    assert_eq!(missing.status.code(), Some(2));

    let conflict = run(&["--java", "--kotlin", FIXTURE]);
    assert_eq!(conflict.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("cannot be used with"));
}

#[test]
fn runtime_failures_exit_with_code_1() {
    let missing_file = run(&["does-not-exist.class"]);
    assert_eq!(missing_file.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing_file.stderr).contains("Error:"));

    let limited = run(&["--max-class-size", "1", FIXTURE]);
    assert_eq!(limited.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&limited.stderr).contains("too large"));
}

#[test]
fn class_and_jar_entry_success_exit_with_code_0() {
    let class = run(&["--java", FIXTURE]);
    assert_eq!(class.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&class.stdout).contains("class ControlFlowFixture"));

    let jar = temp_jar();
    let jar_str = jar.to_str().unwrap();
    let entry = run(&["--java", "--jar", jar_str, "--entry", ENTRY]);
    assert_eq!(entry.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&entry.stdout).contains("class ControlFlowFixture"));

    let limited = run(&[
        "--jar",
        jar_str,
        "--entry",
        ENTRY,
        "--max-archive-entry-size",
        "1",
    ]);
    assert_eq!(limited.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&limited.stderr).contains("too large"));
    std::fs::remove_file(jar).unwrap();
}

#[test]
fn complete_jar_requires_output_and_writes_sources() {
    let jar = temp_jar();
    let jar_str = jar.to_str().unwrap();
    let missing_output = run(&["--java", "--jar", jar_str]);
    assert_eq!(missing_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing_output.stderr).contains("--output is required"));

    let output = std::env::temp_dir().join(format!(
        "abyssflower-complete-jar-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = run(&[
        "--java",
        "--jar",
        jar_str,
        "--output",
        output.to_str().unwrap(),
    ]);
    assert_eq!(result.status.code(), Some(0), "{:?}", result.stderr);
    assert!(output.join("fixture/ControlFlowFixture.java").is_file());

    std::fs::remove_dir_all(output).unwrap();
    std::fs::remove_file(jar).unwrap();
}

#[test]
fn directory_input_requires_output_and_is_recursive() {
    let missing_output = run(&["--java", "tests/java_classes"]);
    assert_eq!(missing_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing_output.stderr).contains("--output is required"));

    let output = std::env::temp_dir().join(format!(
        "abyssflower-directory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = run(&[
        "--java",
        "--output",
        output.to_str().unwrap(),
        "tests/java_classes",
    ]);
    assert_eq!(result.status.code(), Some(0), "{:?}", result.stderr);
    assert!(output.join("fixture/ControlFlowFixture.java").is_file());
    assert!(output.join("fixture/WriterEdgeFixture.java").is_file());
    std::fs::remove_dir_all(output).unwrap();
}

#[test]
fn grouped_kotlin_input_merges_companion_output() {
    let output = std::env::temp_dir().join(format!(
        "abyssflower-kotlin-group-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = Command::new(env!("CARGO_BIN_EXE_abyssflower"))
        .args([
            "--kotlin",
            "--output",
            output.to_str().unwrap(),
            "tests/kotlin_classes/pkg/Counter.class",
            "tests/kotlin_classes/pkg/Counter$Companion.class",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(0), "{:?}", result.stderr);
    let source = std::fs::read_to_string(output.join("pkg/Counter.kt")).unwrap();
    assert_eq!(source.matches("companion object Companion").count(), 1);
    assert!(!output.join("pkg/Counter$Companion.kt").exists());
    std::fs::remove_dir_all(output).unwrap();
}
