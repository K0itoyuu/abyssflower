//! MCP (Model Context Protocol) server mode.
//!
//! Run with `abyssflower --mcp` to start as an MCP server over stdio.
//! Exposes decompilation tools that AI assistants can call.

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerInfo, ServerCapabilities, Implementation},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::classfile::ClassFile;
use crate::codegen::class_writer::render_class;
use crate::kotlin::writer::{is_kotlin_class, render_kotlin_class};

// ── Tool parameter types ─────────────────────────────────────────────────

/// Parameters for decompiling a .class file from disk.
#[derive(Deserialize, JsonSchema)]
pub struct DecompileFileParams {
    /// Absolute or relative path to a .class file
    pub path: String,
}

/// Parameters for decompiling a class entry from a JAR file.
#[derive(Deserialize, JsonSchema)]
pub struct DecompileJarEntryParams {
    /// Path to the JAR file
    pub jar_path: String,
    /// Internal path of the .class entry (e.g. "com/example/Main.class")
    pub class_path: String,
}

/// Parameters for decompiling from base64-encoded bytes.
#[derive(Deserialize, JsonSchema)]
pub struct DecompileBytesParams {
    /// Base64-encoded .class file content
    pub bytes_base64: String,
}

// ── Server implementation ────────────────────────────────────────────────

/// The MCP server handler.
#[derive(Debug, Clone)]
pub struct AbyssflowerMcp {
    tool_router: ToolRouter<Self>,
}

impl AbyssflowerMcp {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AbyssflowerMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("abyssflower", env!("CARGO_PKG_VERSION")))
            .with_instructions("High-performance JVM bytecode decompiler with Kotlin support. Provide .class file paths or base64-encoded bytes to decompile.")
    }
}

#[tool_router(router = tool_router)]
impl AbyssflowerMcp {
    /// Decompile a .class file from disk. Returns idiomatic Kotlin or Java source.
    #[tool(name = "decompile_file", description = "Decompile a .class file at the given path. Returns Kotlin source if Kotlin metadata is present, otherwise Java.")]
    pub async fn decompile_file(&self, params: Parameters<DecompileFileParams>) -> String {
        let params = params.0;
        match std::fs::read(&params.path) {
            Ok(bytes) => decompile_bytes_impl(&bytes),
            Err(e) => format!("Error reading file '{}': {}", params.path, e),
        }
    }

    /// Decompile a class entry from inside a JAR/ZIP archive.
    #[tool(name = "decompile_jar_entry", description = "Decompile a .class entry from inside a JAR file. Provide the JAR path and the internal class path (e.g. 'com/example/Main.class').")]
    pub async fn decompile_jar_entry(&self, params: Parameters<DecompileJarEntryParams>) -> String {
        let params = params.0;
        match read_jar_entry(&params.jar_path, &params.class_path) {
            Some(bytes) => decompile_bytes_impl(&bytes),
            None => format!(
                "Error: could not read '{}' from '{}'",
                params.class_path, params.jar_path
            ),
        }
    }

    /// Decompile from base64-encoded .class file bytes.
    #[tool(name = "decompile_bytes", description = "Decompile a .class file from base64-encoded bytes. Useful when the class data is already in memory.")]
    pub async fn decompile_bytes(&self, params: Parameters<DecompileBytesParams>) -> String {
        let params = params.0;
        use base64::Engine;
        match base64::engine::general_purpose::STANDARD.decode(&params.bytes_base64) {
            Ok(bytes) => decompile_bytes_impl(&bytes),
            Err(e) => format!("Error decoding base64: {}", e),
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn decompile_bytes_impl(bytes: &[u8]) -> String {
    match ClassFile::parse(bytes) {
        Ok(cf) => {
            if is_kotlin_class(&cf) {
                render_kotlin_class(&cf)
            } else {
                render_class(&cf)
            }
        }
        Err(e) => format!("Error parsing class file: {}", e),
    }
}

fn read_jar_entry(jar_path: &str, entry_path: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(jar_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name(entry_path).ok()?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

// ── Entry point ──────────────────────────────────────────────────────────

/// Run the MCP server over stdio. This blocks until the client disconnects.
pub async fn run_mcp_server() {
    let server = AbyssflowerMcp::new();
    let service = server
        .serve(rmcp::transport::io::stdio())
        .await
        .expect("failed to start MCP server");
    service.waiting().await.expect("MCP server error");
}
