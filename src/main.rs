use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use abyssflower_lib::{
    DecompileLanguage, DecompileOptions, DecompileOutput, Decompiler, Result,
    DEFAULT_MAX_ARCHIVE_ENTRY_SIZE, DEFAULT_MAX_ARCHIVE_SIZE, DEFAULT_MAX_CLASS_SIZE,
};
use clap::{ArgGroup, Parser};

#[derive(Debug, Parser)]
#[command(version, about, arg_required_else_help = true)]
#[command(group(ArgGroup::new("language").args(["java", "kotlin", "auto"]).multiple(false)))]
struct Cli {
    /// Class files or directories to decompile.
    #[arg(value_name = "INPUT", num_args = 0.., conflicts_with = "jar")]
    files: Vec<PathBuf>,

    /// Write sources below this directory instead of stdout.
    #[arg(short = 'o', long, value_name = "DIR")]
    output: Option<PathBuf>,

    /// Force Java output.
    #[arg(long, conflicts_with = "mcp")]
    java: bool,

    /// Force Kotlin output; invalid or absent metadata is an error.
    #[arg(long, conflicts_with = "mcp")]
    kotlin: bool,

    /// Auto-detect Kotlin metadata and otherwise emit Java.
    #[arg(long, conflicts_with = "mcp")]
    auto: bool,

    /// Decompile a JAR/ZIP archive, or one entry when --entry is provided.
    #[arg(long, value_name = "JAR")]
    jar: Option<PathBuf>,

    /// Optional internal .class path used with --jar.
    #[arg(long, value_name = "ENTRY", requires = "jar")]
    entry: Option<String>,

    /// Maximum uncompressed class size in bytes.
    #[arg(long, default_value_t = DEFAULT_MAX_CLASS_SIZE, conflicts_with = "mcp")]
    max_class_size: u64,

    /// Maximum JAR/ZIP file size in bytes.
    #[arg(long, default_value_t = DEFAULT_MAX_ARCHIVE_SIZE, conflicts_with = "mcp")]
    max_archive_size: u64,

    /// Maximum uncompressed JAR entry size in bytes.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_ARCHIVE_ENTRY_SIZE,
        conflicts_with = "mcp"
    )]
    max_archive_entry_size: u64,

    /// Start the MCP server over stdio.
    #[arg(long, conflicts_with_all = ["files", "jar", "entry", "output"])]
    mcp: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.mcp {
        #[cfg(feature = "mcp")]
        {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("Error: failed to create async runtime: {error}");
                    return ExitCode::FAILURE;
                }
            };
            runtime.block_on(abyssflower_lib::mcp::run_mcp_server());
            return ExitCode::SUCCESS;
        }
        #[cfg(not(feature = "mcp"))]
        {
            eprintln!("Error: this binary was built without MCP support");
            return ExitCode::FAILURE;
        }
    }

    let language = if cli.java {
        DecompileLanguage::Java
    } else if cli.kotlin {
        DecompileLanguage::Kotlin
    } else {
        DecompileLanguage::Auto
    };
    let decompiler = Decompiler::new(DecompileOptions {
        language,
        max_class_size: cli.max_class_size,
        max_archive_size: cli.max_archive_size,
        max_archive_entry_size: cli.max_archive_entry_size,
    });

    let mut failed = false;
    if let Some(jar) = &cli.jar {
        if let Some(entry) = &cli.entry {
            match decompiler.decompile_jar_entry(jar, entry) {
                Ok(output) => {
                    if let Err(error) = emit(&output, cli.output.as_deref(), Some(entry)) {
                        eprintln!("Error: could not write output: {error}");
                        failed = true;
                    }
                }
                Err(error) => {
                    eprintln!("Error: {}!{}: {error}", jar.display(), entry);
                    failed = true;
                }
            }
        } else {
            let Some(output_dir) = cli.output.as_deref() else {
                eprintln!("Error: --output is required when decompiling a complete archive");
                return ExitCode::from(2);
            };
            match decompiler.decompile_jar(jar) {
                Ok(outputs) => {
                    failed |= emit_all(&outputs, Some(output_dir));
                }
                Err(error) => {
                    eprintln!("Error: {}: {error}", jar.display());
                    failed = true;
                }
            }
        }
    } else {
        if cli.files.is_empty() {
            eprintln!("Error: provide at least one INPUT or use --jar");
            return ExitCode::from(2);
        }
        let contains_directory = cli.files.iter().any(|path| path.is_dir());
        if contains_directory && cli.output.is_none() {
            eprintln!("Error: --output is required when decompiling a directory");
            return ExitCode::from(2);
        }
        if cli.files.len() > 1 || contains_directory {
            match decompiler.decompile_paths(&cli.files) {
                Ok(outputs) => {
                    failed |= emit_all(&outputs, cli.output.as_deref());
                }
                Err(error) => {
                    eprintln!("Error: could not decompile inputs: {error}");
                    failed = true;
                }
            }
        } else {
            for path in &cli.files {
                match decompiler.decompile_file(path) {
                    Ok(output) => {
                        if let Err(error) = emit(&output, cli.output.as_deref(), path.to_str()) {
                            eprintln!(
                                "Error: could not write output for {}: {error}",
                                path.display()
                            );
                            failed = true;
                        }
                    }
                    Err(error) => {
                        eprintln!("Error: {}: {error}", path.display());
                        failed = true;
                    }
                }
            }
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn emit_all(outputs: &[DecompileOutput], output_dir: Option<&Path>) -> bool {
    let mut failed = false;
    for output in outputs {
        if let Err(error) = emit(output, output_dir, None) {
            eprintln!(
                "Error: could not write output for {}: {error}",
                output.class_name
            );
            failed = true;
        }
    }
    failed
}

fn emit(output: &DecompileOutput, output_dir: Option<&Path>, origin: Option<&str>) -> Result<()> {
    for diagnostic in &output.diagnostics {
        eprintln!("Warning: {}", diagnostic.message);
    }
    if let Some(dir) = output_dir {
        let path = write_output(dir, output)?;
        eprintln!("Wrote {}", path.display());
    } else {
        if let Some(origin) = origin {
            println!("// Decompiled from: {origin}");
        }
        print!("{}", output.source);
    }
    Ok(())
}

fn write_output(dir: &Path, output: &DecompileOutput) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let root = std::fs::canonicalize(dir)?;
    let relative = output_path(Path::new(""), output)?;
    let directory = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
    if let Some(parent) = relative.parent() {
        directory.create_dir_all(parent)?;
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file = directory.open_with(&relative, &options)?;
    file.write_all(output.source.as_bytes())?;
    Ok(root.join(relative))
}

fn output_path(dir: &Path, output: &DecompileOutput) -> Result<PathBuf> {
    abyssflower_lib::decompiler::validate_class_name(&output.class_name)?;
    let mut path = dir.to_path_buf();
    let mut parts = output.class_name.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            path.push(part);
        } else {
            let extension = match output.language {
                DecompileLanguage::Kotlin => "kt",
                _ => "java",
            };
            path.push(format!("{part}.{extension}"));
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(class_name: &str) -> DecompileOutput {
        DecompileOutput {
            source: String::new(),
            language: DecompileLanguage::Java,
            class_name: class_name.into(),
            diagnostics: vec![],
        }
    }

    #[test]
    fn output_path_stays_below_root() {
        let output = output("pkg/sub/Foo");
        assert_eq!(
            output_path(Path::new("out"), &output).unwrap(),
            Path::new("out/pkg/sub/Foo.java")
        );
    }

    #[test]
    fn writes_nested_output_below_capability_root() {
        let root = std::env::temp_dir().join(format!(
            "abyssflower-output-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = write_output(&root, &output("pkg/sub/Foo")).unwrap();
        assert!(target.starts_with(std::fs::canonicalize(&root).unwrap()));
        assert!(target.parent().unwrap().is_dir());
        assert_eq!(target.file_name().unwrap(), "Foo.java");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_output_through_link_outside_root() {
        let unique = format!(
            "abyssflower-output-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(format!("{unique}-root"));
        let outside = std::env::temp_dir().join(format!("{unique}-outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = root.join("pkg");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_dir(&outside, &link) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                std::fs::remove_dir_all(root).unwrap();
                std::fs::remove_dir_all(outside).unwrap();
                return;
            }
            panic!("could not create test directory link: {error}");
        }

        assert!(write_output(&root, &output("pkg/Escape")).is_err());
        assert!(!outside.join("Escape.java").exists());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
