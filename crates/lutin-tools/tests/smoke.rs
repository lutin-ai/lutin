//! Smoke tests for every portable tool.
//!
//! Each test builds a `ToolContext` rooted at a `tempfile::TempDir`, wires
//! a single tool, and invokes `Tool::call` with minimal arguments.
//! Network-dependent tools (`http_request`, `web_search`) are marked
//! `#[ignore]` so CI stays hermetic.

use std::sync::Arc;

use lutin_llm::{CallId, ToolCall, ToolName};
use lutin_tools::{
    file_edit::FileEdit, file_glob::FileGlob, file_grep::FileGrep, file_list::FileList,
    file_read::FileRead, file_tree::FileTree, file_write::FileWrite, http_request::HttpRequest,
    image_view::ImageView, multi::Toolbox, read_state::ReadState, shell::Shell, wait::Wait,
    web_search::WebSearch, Tool, ToolCallContext, ToolContext, ToolError, ToolResult,
};
use serde_json::json;

fn make_ctx(root: &std::path::Path) -> Arc<ToolContext> {
    Arc::new(ToolContext {
        root: root.to_path_buf(),
        env: Arc::from([]),
        http: reqwest::Client::new(),
        read_state: Arc::new(ReadState::new(root.to_path_buf())),
    })
}

fn ctx_call() -> ToolCallContext {
    ToolCallContext::default()
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: CallId::new("c1"),
        name: ToolName::new(name),
        arguments: args,
    }
}

fn ok_content(outcome: ToolResult) -> (String, bool) {
    match outcome {
        ToolResult::Ok(r) => (r.content, r.is_error),
        ToolResult::Err(e) => panic!("unexpected dispatch error: {e}"),
        _ => panic!("unexpected ToolResult variant"),
    }
}

#[tokio::test]
async fn shell_runs_echo() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path());
    let host = Shell::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("shell", json!({"command": "echo hello"})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("hello"));
}

#[tokio::test]
async fn shell_respects_env_injection() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = Arc::new(ToolContext {
        root: tmp.path().to_path_buf(),
        env: Arc::from([("LUTIN_TEST_VAR".to_string(), "xyzzy".to_string())]),
        http: reqwest::Client::new(),
        read_state: Arc::new(ReadState::new(tmp.path().to_path_buf())),
    });
    let host = Shell::new(Arc::clone(&ctx));
    let out = host
        .call(
            &ctx_call(),
            call("shell", json!({"command": "echo $LUTIN_TEST_VAR"})),
        )
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("xyzzy"));
}

#[tokio::test]
async fn wait_completes() {
    let host = Wait::new();
    let out = host
        .call(&ctx_call(), call("wait", json!({"seconds": 1})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("waited 1"));
}

#[tokio::test]
async fn file_read_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "one\ntwo\n").unwrap();
    let ctx = make_ctx(tmp.path());
    let host = FileRead::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("read", json!({"path": "a.txt"})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("one"));
    assert!(content.contains("two"));
}

#[tokio::test]
async fn file_write_requires_prior_read() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "old").unwrap();
    let ctx = make_ctx(tmp.path());
    let host = FileWrite::new(Arc::clone(&ctx));
    let out = host
        .call(
            &ctx_call(),
            call("file_write", json!({"path": "a.txt", "content": "new"})),
        )
        .await;
    let (_, is_err) = ok_content(out);
    assert!(is_err);
}

#[tokio::test]
async fn file_write_creates_new() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path());
    let host = FileWrite::new(Arc::clone(&ctx));
    let out = host
        .call(
            &ctx_call(),
            call("file_write", json!({"path": "new.txt", "content": "hi"})),
        )
        .await;
    let (_, is_err) = ok_content(out);
    assert!(!is_err);
    assert_eq!(std::fs::read_to_string(tmp.path().join("new.txt")).unwrap(), "hi");
}

#[tokio::test]
async fn file_edit_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "alpha beta").unwrap();
    let ctx = make_ctx(tmp.path());
    let reader = FileRead::new(Arc::clone(&ctx));
    let _ = reader
        .call(&ctx_call(), call("read", json!({"path": "a.txt"})))
        .await;

    let editor = FileEdit::new(Arc::clone(&ctx));
    let out = editor
        .call(
            &ctx_call(),
            call(
                "file_edit",
                json!({"path": "a.txt", "old_string": "alpha", "new_string": "gamma"}),
            ),
        )
        .await;
    let (_, is_err) = ok_content(out);
    assert!(!is_err);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "gamma beta"
    );
}

#[tokio::test]
async fn file_list_shows_entries() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    let ctx = make_ctx(tmp.path());
    let host = FileList::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("list", json!({})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("sub"));
    assert!(content.contains("a.txt"));
}

#[tokio::test]
async fn file_glob_matches_pattern() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "").unwrap();
    let ctx = make_ctx(tmp.path());
    let host = FileGlob::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("glob", json!({"pattern": "*.rs"})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("a.rs"));
    assert!(!content.contains("b.txt"));
}

#[tokio::test]
async fn file_grep_finds_match() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hello world\ngoodbye").unwrap();
    let ctx = make_ctx(tmp.path());
    let host = FileGrep::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("grep", json!({"pattern": "hello"})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("hello world"));
}

#[tokio::test]
async fn file_tree_lists_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub").join("x.txt"), "").unwrap();
    let ctx = make_ctx(tmp.path());
    let host = FileTree::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("tree", json!({})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("sub/"));
    assert!(content.contains("x.txt"));
}

#[tokio::test]
async fn image_view_rejects_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path());
    let host = ImageView::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("image_view", json!({"path": "nope.png"})))
        .await;
    let (_, is_err) = ok_content(out);
    assert!(is_err);
}

#[tokio::test]
async fn image_view_accepts_png() {
    let tmp = tempfile::tempdir().unwrap();
    let png_bytes: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    std::fs::write(tmp.path().join("a.png"), png_bytes).unwrap();
    let ctx = make_ctx(tmp.path());
    let host = ImageView::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("image_view", json!({"path": "a.png"})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err, "{content}");
    assert!(content.contains("a.png"));
}

#[tokio::test]
async fn multi_rejects_duplicate_names() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(Wait::new()), Box::new(Wait::new())];
    let msg = match Toolbox::new(tools) {
        Ok(_) => panic!("duplicate should be rejected"),
        Err(e) => e.to_string(),
    };
    assert!(msg.contains("wait"));
}

#[tokio::test]
async fn multi_routes_to_correct_host() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path());
    std::fs::write(tmp.path().join("a.txt"), "hi\n").unwrap();
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(Wait::new()),
        Box::new(FileRead::new(Arc::clone(&ctx))),
    ];
    let multi = Toolbox::new(tools).unwrap();

    let names: Vec<String> = multi
        .definitions()
        .into_iter()
        .map(|d| d.name.into_inner())
        .collect();
    assert!(names.contains(&"wait".to_string()));
    assert!(names.contains(&"read".to_string()));

    let out = multi
        .call(&ctx_call(), call("read", json!({"path": "a.txt"})))
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err);
    assert!(content.contains("hi"));

    let missing = multi
        .call(&ctx_call(), call("nope", json!({})))
        .await;
    match missing {
        ToolResult::Err(ToolError::NotFound(n)) => assert_eq!(n, "nope"),
        other => panic!("expected NotFound, got {other:?}"),
    };
}

#[tokio::test]
#[ignore]
async fn http_request_networked() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path());
    let host = HttpRequest::new(Arc::clone(&ctx));
    let out = host
        .call(
            &ctx_call(),
            call("http_request", json!({"url": "https://example.com"})),
        )
        .await;
    let (content, is_err) = ok_content(out);
    assert!(!is_err, "{content}");
}

#[tokio::test]
#[ignore]
async fn web_search_networked() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path());
    let host = WebSearch::new(Arc::clone(&ctx));
    let out = host
        .call(&ctx_call(), call("web_search", json!({"query": "rust"})))
        .await;
    let _ = ok_content(out);
}
