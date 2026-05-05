//! Per-plugin custom URI scheme.
//!
//! All plugin iframes share a single `lutin-plugin` scheme; the
//! workflow id goes in the **host** position so each plugin gets its
//! own browser origin (`lutin-plugin://chat/index.html` is a different
//! origin from `lutin-plugin://stub/index.html`). Cross-origin
//! isolation falls out of that — browsers compare `(scheme, host,
//! port)` tuples, so postMessage between iframe and chrome is
//! cross-origin and origin checks become meaningful.
//!
//! On Windows, Tauri rewrites custom schemes to
//! `https://lutin-plugin.localhost/...` and routes the host to the
//! path; the React side reads the canonical URL out of the
//! `workflow_open_plugin` command instead of constructing it itself,
//! so platform differences stay confined to this file.

use std::borrow::Cow;
use std::path::Path;

use lutin_control_protocol::WorkflowId;
use tauri::{Manager, UriSchemeContext, Wry};
use tauri::http::{Request, Response, StatusCode, header};

use crate::AppState;

pub const SCHEME: &str = "lutin-plugin";

/// Build the iframe `src` URL for a plugin. Single source of truth so
/// the React side never has to know which scheme/host format the
/// current Tauri build expects.
pub fn url_for(workflow: &WorkflowId, path: &str) -> String {
    let path = path.trim_start_matches('/');
    if cfg!(windows) {
        // Tauri 2 on Windows: `https://<scheme>.localhost/<host>/<path>`.
        // Host segment becomes part of the path; the origin remains
        // distinct because the URL scheme differs from the chrome's.
        // We could route by the first path segment in the handler,
        // but Tauri's Windows mapping puts the host into a header
        // rather than the URL, so check the request's URI carefully.
        format!("https://{SCHEME}.localhost/{}/{path}", workflow.as_str())
    } else {
        format!("{SCHEME}://{}/{path}", workflow.as_str())
    }
}

/// URI scheme handler. Resolves to a file inside the workflow's
/// extracted bundle and serves its bytes with a guessed Content-Type.
pub fn handle(
    ctx: UriSchemeContext<'_, Wry>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let app = ctx.app_handle();
    let state = app.state::<AppState>();
    let cache = &state.bundles;

    let uri = request.uri().clone();
    let host = uri.host().unwrap_or("");
    let path = uri.path();

    // On Windows the host ends up encoded in the path; non-Windows
    // gives us the host directly. Either way we need a workflow id
    // and a relative file path.
    let (workflow_str, rel) = if host.is_empty() {
        // Windows shape: /<workflow>/<rest>
        let trimmed = path.trim_start_matches('/');
        match trimmed.split_once('/') {
            Some((w, r)) => (w.to_owned(), r.to_owned()),
            None => (trimmed.to_owned(), String::new()),
        }
    } else {
        (host.to_owned(), path.trim_start_matches('/').to_owned())
    };

    if workflow_str.is_empty() {
        return not_found("missing workflow id");
    }
    let Ok(workflow) = WorkflowId::parse(&workflow_str) else {
        return not_found("invalid workflow id");
    };

    let rel = if rel.is_empty() { "index.html" } else { &rel };
    let Some(file) = cache.resolve_asset(&workflow, rel) else {
        return not_found("asset not in bundle");
    };

    match std::fs::read(&file) {
        Ok(bytes) => {
            let mime = guess_mime(&file);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                // Iframe loads the document at the bundle origin; we
                // rely on browser same-origin defaults inside the
                // iframe and don't need permissive CORS here.
                .body(Cow::Owned(bytes))
                .unwrap_or_else(|_| not_found("response build failed"))
        }
        Err(_) => not_found("read failed"),
    }
}

fn not_found(reason: &'static str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Cow::Borrowed(reason.as_bytes()))
        .expect("static 404")
}

fn guess_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()) {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}
