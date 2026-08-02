//! MCP (Model Context Protocol) server mode.
//!
//! Run with `abyssflower --mcp` to start as an MCP server over stdio.
//! Exposes decompilation tools that AI assistants can call.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{Decompiler, DEFAULT_MAX_CLASS_SIZE};

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

impl Default for AbyssflowerMcp {
    fn default() -> Self {
        Self::new()
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
    #[tool(
        name = "decompile_file",
        description = "Decompile a .class file at the given path. Returns Kotlin source if Kotlin metadata is present, otherwise Java."
    )]
    pub async fn decompile_file(&self, params: Parameters<DecompileFileParams>) -> String {
        let params = params.0;
        match Decompiler::default().decompile_file(&params.path) {
            Ok(output) => output.source,
            Err(e) => format!("Error decompiling file '{}': {}", params.path, e),
        }
    }

    /// Decompile a class entry from inside a JAR/ZIP archive.
    #[tool(
        name = "decompile_jar_entry",
        description = "Decompile a .class entry from inside a JAR file. Provide the JAR path and the internal class path (e.g. 'com/example/Main.class')."
    )]
    pub async fn decompile_jar_entry(&self, params: Parameters<DecompileJarEntryParams>) -> String {
        let params = params.0;
        match Decompiler::default().decompile_jar_entry(&params.jar_path, &params.class_path) {
            Ok(output) => output.source,
            Err(error) => format!(
                "Error decompiling '{}' from '{}': {}",
                params.class_path, params.jar_path, error
            ),
        }
    }

    /// Decompile from base64-encoded .class file bytes.
    #[tool(
        name = "decompile_bytes",
        description = "Decompile a .class file from base64-encoded bytes. Useful when the class data is already in memory."
    )]
    pub async fn decompile_bytes(&self, params: Parameters<DecompileBytesParams>) -> String {
        let params = params.0;
        use base64::Engine;
        if !base64_length_within_limit(params.bytes_base64.len() as u64, DEFAULT_MAX_CLASS_SIZE) {
            return format!(
                "Error: base64 input exceeds the encoded {}-byte class limit",
                DEFAULT_MAX_CLASS_SIZE
            );
        }
        match base64::engine::general_purpose::STANDARD.decode(&params.bytes_base64) {
            Ok(bytes) => decompile_bytes_impl(&bytes),
            Err(e) => format!("Error decoding base64: {}", e),
        }
    }
}

fn base64_length_within_limit(encoded_len: u64, decoded_limit: u64) -> bool {
    let max_encoded_len = decoded_limit
        .saturating_add(2)
        .saturating_div(3)
        .saturating_mul(4);
    encoded_len <= max_encoded_len
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn decompile_bytes_impl(bytes: &[u8]) -> String {
    match Decompiler::default().decompile_bytes(bytes) {
        Ok(output) => output.source,
        Err(e) => format!("Error parsing class file: {}", e),
    }
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

#[cfg(test)]
mod tests {
    use super::base64_length_within_limit;

    #[test]
    fn checks_encoded_length_before_base64_decode() {
        assert!(base64_length_within_limit(4, 1));
        assert!(!base64_length_within_limit(5, 1));
        assert!(base64_length_within_limit(8, 4));
        assert!(!base64_length_within_limit(9, 4));

        let default_encoded_limit = DEFAULT_MAX_CLASS_SIZE.div_ceil(3) * 4;
        assert!(base64_length_within_limit(
            default_encoded_limit,
            DEFAULT_MAX_CLASS_SIZE
        ));
        assert!(!base64_length_within_limit(
            default_encoded_limit + 1,
            DEFAULT_MAX_CLASS_SIZE
        ));
    }

    use crate::DEFAULT_MAX_CLASS_SIZE;
}
