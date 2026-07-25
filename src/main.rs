use std::path::PathBuf;
use std::process;

use abyssflower_lib::codegen::render_class;
use abyssflower_lib::kotlin::writer::{is_kotlin_class, render_kotlin_class};
use abyssflower_lib::ClassFile;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // MCP server mode
    #[cfg(feature = "mcp")]
    if args.iter().any(|a| a == "--mcp") {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(abyssflower_lib::mcp::run_mcp_server());
        return;
    }

    if args.len() < 2 {
        eprintln!("Usage: abyssflower <file.class> [-o <output_dir>]");
        eprintln!("       abyssflower file1.class file2.class ...");
        eprintln!("       abyssflower --mcp    (start MCP server over stdio)");
        process::exit(1);
    }

    let mut had_error = false;

    // Collect class files and optional output directory
    let mut files: Vec<PathBuf> = Vec::new();
    let mut out_dir: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "-o" && i + 1 < args.len() {
            out_dir = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else {
            files.push(PathBuf::from(&args[i]));
            i += 1;
        }
    }

    for path in &files {
        match decompile_file(path, out_dir.as_ref()) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error: {}: {}", path.display(), e);
                had_error = true;
            }
        }
    }

    if had_error { process::exit(1); }
}

fn decompile_file(path: &PathBuf, out_dir: Option<&PathBuf>) -> abyssflower_lib::Result<()> {
    let bytes = std::fs::read(path)?;
    let cf    = ClassFile::parse(&bytes)?;

    // Auto-detect Kotlin vs Java
    let (src, ext) = if is_kotlin_class(&cf) {
        (render_kotlin_class(&cf), ".kt")
    } else {
        (render_class(&cf), ".java")
    };

    match out_dir {
        Some(dir) => {
            let out_path = dir.join(cf.this_class.replace('/', "/").to_string() + ext);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out_path, &src)?;
            eprintln!("Wrote {}", out_path.display());
        }
        None => {
            println!("// Decompiled from: {}", path.display());
            print!("{}", src);
        }
    }

    Ok(())
}
