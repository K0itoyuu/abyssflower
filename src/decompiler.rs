//! High-level, resource-bounded decompilation API shared by every frontend.

use std::fs::File;
use std::io::{Cursor, Read, Take};
use std::path::{Path, PathBuf};

use crate::classfile::ClassFile;
use crate::codegen::render_class;
use crate::error::{DecompileError, Result};
use crate::kotlin::writer::{is_kotlin_class, render_kotlin_group, try_render_kotlin_class};

pub const DEFAULT_MAX_CLASS_SIZE: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_ARCHIVE_SIZE: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_ARCHIVE_ENTRY_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecompileLanguage {
    Auto,
    Java,
    Kotlin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompileDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct DecompileOptions {
    pub language: DecompileLanguage,
    pub max_class_size: u64,
    pub max_archive_size: u64,
    pub max_archive_entry_size: u64,
}

impl Default for DecompileOptions {
    fn default() -> Self {
        Self {
            language: DecompileLanguage::Auto,
            max_class_size: DEFAULT_MAX_CLASS_SIZE,
            max_archive_size: DEFAULT_MAX_ARCHIVE_SIZE,
            max_archive_entry_size: DEFAULT_MAX_ARCHIVE_ENTRY_SIZE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DecompileOutput {
    pub source: String,
    pub language: DecompileLanguage,
    pub class_name: String,
    pub diagnostics: Vec<DecompileDiagnostic>,
}

#[derive(Debug, Clone, Default)]
pub struct Decompiler {
    options: DecompileOptions,
}

impl Decompiler {
    pub fn new(options: DecompileOptions) -> Self {
        Self { options }
    }

    pub fn options(&self) -> &DecompileOptions {
        &self.options
    }

    pub fn decompile_bytes(&self, bytes: &[u8]) -> Result<DecompileOutput> {
        enforce_size(bytes.len() as u64, self.options.max_class_size)?;
        let class = ClassFile::parse(bytes)?;
        validate_class_name(&class.this_class)?;
        self.decompile_class(&class)
    }

    pub fn decompile_file(&self, path: impl AsRef<Path>) -> Result<DecompileOutput> {
        let file = File::open(path)?;
        enforce_size(file.metadata()?.len(), self.options.max_class_size)?;
        let bytes = read_limited(file, self.options.max_class_size)?;
        self.decompile_bytes(&bytes)
    }

    pub fn decompile_kotlin_files(&self, paths: &[PathBuf]) -> Result<Vec<DecompileOutput>> {
        let mut classes = Vec::with_capacity(paths.len());
        for path in paths {
            let file = File::open(path)?;
            enforce_size(file.metadata()?.len(), self.options.max_class_size)?;
            let bytes = read_limited(file, self.options.max_class_size)?;
            let class = ClassFile::parse(&bytes)?;
            validate_class_name(&class.this_class)?;
            if !is_kotlin_class(&class) {
                return Err(DecompileError::InvalidKotlinMetadata);
            }
            classes.push(class);
        }
        render_kotlin_group(&classes)
            .into_iter()
            .map(|unit| {
                validate_class_name(&unit.class_name)?;
                let diagnostics = source_diagnostics(&unit.source);
                Ok(DecompileOutput {
                    source: unit.source,
                    language: DecompileLanguage::Kotlin,
                    class_name: unit.class_name,
                    diagnostics,
                })
            })
            .collect()
    }

    pub fn decompile_jar_entry(
        &self,
        jar_path: impl AsRef<Path>,
        entry_path: &str,
    ) -> Result<DecompileOutput> {
        let file = File::open(jar_path)?;
        enforce_size(file.metadata()?.len(), self.options.max_archive_size)?;
        let archive_bytes = read_limited(file, self.options.max_archive_size)?;
        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let entry = archive
            .by_name(entry_path)
            .map_err(|_| DecompileError::JarEntryNotFound(entry_path.to_owned()))?;
        enforce_size(entry.size(), self.options.max_archive_entry_size)?;
        let bytes = read_limited(entry, self.options.max_archive_entry_size)?;
        self.decompile_bytes(&bytes)
    }

    pub fn decompile_class(&self, class: &ClassFile) -> Result<DecompileOutput> {
        let mut diagnostics = Vec::new();
        let kotlin = is_kotlin_class(class);
        let (source, language) = match self.options.language {
            DecompileLanguage::Java => (render_class(class), DecompileLanguage::Java),
            DecompileLanguage::Kotlin => (
                try_render_kotlin_class(class).ok_or(DecompileError::InvalidKotlinMetadata)?,
                DecompileLanguage::Kotlin,
            ),
            DecompileLanguage::Auto if kotlin => match try_render_kotlin_class(class) {
                Some(source) => (source, DecompileLanguage::Kotlin),
                None => {
                    diagnostics.push(DecompileDiagnostic {
                        level: DiagnosticLevel::Warning,
                        message: "Kotlin metadata was invalid; emitted Java instead".into(),
                    });
                    (render_class(class), DecompileLanguage::Java)
                }
            },
            DecompileLanguage::Auto => (render_class(class), DecompileLanguage::Java),
        };
        diagnostics.extend(source_diagnostics(&source));
        Ok(DecompileOutput {
            source,
            language,
            class_name: class.this_class.clone(),
            diagnostics,
        })
    }
}

fn source_diagnostics(source: &str) -> Vec<DecompileDiagnostic> {
    let has_opaque = source.contains("/*opaque") || source.contains("/* opaque");
    let has_unresolved = [
        "/*?*/",
        "/* ? */",
        "/*switch_expr*/",
        "/* no branch */",
        "/* expr */",
        "/* collection */",
    ]
    .iter()
    .any(|placeholder| source.contains(placeholder));

    if has_opaque || has_unresolved {
        let message = if has_opaque {
            "Stack simulation produced opaque expressions; output is partial"
        } else {
            "Stack or control-flow simulation left unresolved expressions; output is partial"
        };
        vec![DecompileDiagnostic {
            level: DiagnosticLevel::Warning,
            message: message.into(),
        }]
    } else {
        Vec::new()
    }
}

pub fn validate_class_name(name: &str) -> Result<()> {
    let invalid_component = |part: &str| {
        let device_stem = part
            .split_once('.')
            .map_or(part, |(stem, _)| stem)
            .to_ascii_uppercase();
        let reserved_device = matches!(
            device_stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        );
        part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with([' ', '.'])
            || part
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
            || reserved_device
    };
    let invalid = name.is_empty()
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains('\\')
        || name.contains(':')
        || name.split('/').any(invalid_component);
    if invalid {
        Err(DecompileError::InvalidClassName(name.to_owned()))
    } else {
        Ok(())
    }
}

fn enforce_size(actual: u64, limit: u64) -> Result<()> {
    if actual > limit {
        Err(DecompileError::InputTooLarge { actual, limit })
    } else {
        Ok(())
    }
}

fn read_limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut reader: Take<_> = reader.take(limit.saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    enforce_size(bytes.len() as u64, limit)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "abyssflower-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn rejects_unsafe_class_names() {
        for name in [
            "",
            "/Foo",
            "Foo/",
            "../Foo",
            "pkg/../Foo",
            "pkg/.. /Foo",
            "pkg/Foo./Bar",
            "pkg/NUL",
            "pkg/con.txt",
            "pkg/Foo?Bar",
            "pkg/Foo\0Bar",
            "C:\\Foo",
        ] {
            assert!(validate_class_name(name).is_err(), "accepted {name:?}");
        }
        assert!(validate_class_name("pkg/Foo$Inner").is_ok());
    }

    #[test]
    fn enforces_archive_and_uncompressed_entry_limits() {
        let path = temp_path("limits.jar");
        let file = File::create(&path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "fixture/ControlFlowFixture.class",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive
            .write_all(include_bytes!(
                "../tests/java_classes/fixture/ControlFlowFixture.class"
            ))
            .unwrap();
        archive.finish().unwrap();

        let archive_limited = Decompiler::new(DecompileOptions {
            max_archive_size: 1,
            ..DecompileOptions::default()
        });
        assert!(matches!(
            archive_limited.decompile_jar_entry(&path, "fixture/ControlFlowFixture.class"),
            Err(DecompileError::InputTooLarge { limit: 1, .. })
        ));

        let entry_limited = Decompiler::new(DecompileOptions {
            max_archive_entry_size: 1,
            ..DecompileOptions::default()
        });
        assert!(matches!(
            entry_limited.decompile_jar_entry(&path, "fixture/ControlFlowFixture.class"),
            Err(DecompileError::InputTooLarge { limit: 1, .. })
        ));

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reports_partial_output_placeholders() {
        let opaque = source_diagnostics("return /*opaque opcode=0xff @1*/;");
        assert_eq!(opaque.len(), 1);
        assert!(opaque[0].message.contains("opaque"));
        assert!(opaque[0].message.contains("partial"));

        let unresolved = source_diagnostics("when (/* expr */) {}");
        assert_eq!(unresolved.len(), 1);
        assert!(unresolved[0].message.contains("unresolved"));

        assert!(source_diagnostics("return value;").is_empty());
    }
}
