//! Download-only probe for the MCP Files draft. The selected bytes are a synthetic fixture.
use axum::http::{HeaderName, header};
use axum::response::IntoResponse;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rmcp::model::{CallToolResult, ContentBlock, MetaObject, Tool, ToolAnnotations};
use serde_json::json;
use sha2::{Digest, Sha256};

pub(crate) const EXTENSION: &str = "io.modelcontextprotocol/files";
pub(crate) const PATH: &str = "/mcp-fixtures/loop-health.json";
const BYTES: &[u8] = include_bytes!("../../../fixtures/health/loop-health.json");

pub(crate) fn tool() -> Tool {
    Tool::new(
        "maestro_download_health_fixture",
        "Download a synthetic loop health report from the opt-in loopback Files pilot.",
        std::sync::Arc::new(json!({"type": "object", "additionalProperties": false}).as_object().unwrap().clone()),
    )
    .with_annotations(
        ToolAnnotations::with_title("Download health fixture")
            .read_only(true)
            .idempotent(true)
            .destructive(false)
            .open_world(false),
    )
}

pub(crate) fn result() -> CallToolResult {
    let mut meta = MetaObject::new();
    meta.insert(
        EXTENSION.into(),
        json!({"downloads": [{
            "name": "loop-health.json",
            "mimeType": "application/json",
            "size": BYTES.len(),
            "digest": {"algorithm": "sha-256", "value": URL_SAFE_NO_PAD.encode(Sha256::digest(BYTES))},
            "path": PATH,
            "credentials": "omit"
        }]}),
    );
    CallToolResult::success(vec![ContentBlock::text("Synthetic loop health report available for download.")])
        .with_meta(Some(meta))
}

pub(crate) async fn download() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CONTENT_DISPOSITION, "attachment; filename=loop-health.json"),
            (header::CACHE_CONTROL, "no-store"),
            (HeaderName::from_static("x-content-type-options"), "nosniff"),
        ],
        BYTES,
    )
}
