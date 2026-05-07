//! Chrome-hosted plugin shim served via the `lutin-shim` URI scheme.
//!
//! Plugins load `lutin-shim://localhost/shim.js` from their own
//! cross-origin document. The handler returns the embedded JS with
//! permissive CORS so the script loads cleanly across origins. The
//! shim sets `window.__lutinReady` (Promise<Lutin>) and `window.lutin`
//! after chrome's first `lutin-init` postMessage. Plugin code awaits
//! the promise instead of bundling its own copy of the shim.

use std::borrow::Cow;

use tauri::http::{Request, Response, StatusCode, header};
use tauri::{UriSchemeContext, Wry};

pub const SCHEME: &str = "lutin-shim";

const SHIM_JS: &str = include_str!("../shim/lutin.js");

pub fn handle(
    _ctx: UriSchemeContext<'_, Wry>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path().trim_start_matches('/');
    // Single asset for now; keep the match explicit so we 404 on
    // anything else rather than serving the same file under any path.
    if path != "shim.js" {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(b"unknown shim asset" as &[u8]))
            .expect("static 404");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/javascript; charset=utf-8")
        // Each plugin iframe lives on its own `lutin-plugin://<id>/`
        // origin, so the shim is always cross-origin to its caller.
        // `*` is fine — the shim is the same bytes for every plugin
        // and contains no secrets.
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Cow::Borrowed(SHIM_JS.as_bytes()))
        .expect("static shim response")
}
