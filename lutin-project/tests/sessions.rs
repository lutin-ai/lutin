//! End-to-end: project supervisor spawns the stub workflow binary,
//! returns a working endpoint, and shuts it down on `StopSession`.
//!
//! The stub binary is built on demand via `escargot`. First run pays
//! a compile cost; subsequent runs reuse the cached artifact. The
//! fixture lives at `tests/fixtures/stub-workflow/` as a standalone
//! Cargo crate (NOT a workspace member) so its `target/` is local to
//! the fixture dir, which is what `WorkflowDef::binary_path` expects.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use futures_util::{SinkExt, StreamExt};
use lutin_auth::{Scope, Subject, Ttl, generate_keypair, mint_with_ttl, verify};
use lutin_project::workflows::{Profile, WorkflowDef};
use lutin_project::{SpawnConfig, Supervisor, run};
use lutin_project_protocol::{
    self as pp, ApiError, Event, Request, Response, ResponseOk, SessionEndpoint, SessionInfo,
    Slug, WorkflowId, WorkflowInfo,
};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Build the stub fixture once per test process and return the path
/// to its crate dir (the parent of `Cargo.toml` and `target/`). The
/// `WorkflowDef { crate_dir, profile: "debug" }` then resolves the
/// binary at `<crate_dir>/target/debug/stub`.
fn stub_binary_dir() -> PathBuf {
    static CRATE_DIR: OnceLock<PathBuf> = OnceLock::new();
    CRATE_DIR
        .get_or_init(|| {
            let manifest = "tests/fixtures/stub-workflow/Cargo.toml";
            // Run escargot purely for the build side-effect; we
            // intentionally discard its returned binary path because
            // `WorkflowDef::binary_path` recomputes it from the crate
            // dir + profile.
            let _ = escargot::CargoBuild::new()
                .package("stub")
                .bin("stub")
                .manifest_path(manifest)
                .run()
                .expect("build stub workflow");
            Path::new(manifest)
                .parent()
                .expect("manifest has parent")
                .canonicalize()
                .expect("canonicalize stub crate dir")
        })
        .clone()
}

async fn pp_request(ws: &mut Ws, request_id: u64, req: Request) -> Response {
    let body = pp::encode(&req).unwrap();
    let frame = encode(&Frame::Payload { request_id, body }).unwrap();
    ws.send(Message::Binary(frame.into())).await.unwrap();
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        let bytes = match msg {
            Message::Binary(b) => b,
            _ => continue,
        };
        match decode(&bytes).unwrap() {
            Frame::Payload {
                request_id: rid,
                body,
            } if rid == request_id => return pp::decode::<Response>(&body).unwrap(),
            _ => continue,
        }
    }
}

async fn drain_one_broadcast(ws: &mut Ws) -> Event {
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        if let Message::Binary(b) = msg
            && let Frame::Broadcast { body } = decode(&b).unwrap()
        {
            return pp::decode::<Event>(&body).unwrap();
        }
    }
}

async fn handshake(ws: &mut Ws, token: String) -> HandshakeResult {
    let hello = encode(&Frame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token,
    })
    .unwrap();
    ws.send(Message::Binary(hello.into())).await.unwrap();
    let ack = ws.next().await.unwrap().unwrap();
    let bytes = match ack {
        Message::Binary(b) => b,
        other => panic!("{other:?}"),
    };
    match decode(&bytes).unwrap() {
        Frame::HelloAck(r) => r,
        other => panic!("expected ack, got {other:?}"),
    }
}

#[tokio::test]
async fn start_open_stop_session_drives_real_subprocess() {
    let crate_dir = stub_binary_dir();

    let issuer = generate_keypair().unwrap();
    let project_signing = generate_keypair().unwrap();

    let slug = Slug::parse("demo").unwrap();
    let workflow_id = WorkflowId::parse("stub").unwrap();

    let project_token = mint_with_ttl(
        &issuer,
        Subject::parse("test").unwrap(),
        Scope::Project(slug.clone()),
        Ttl::from_secs(60),
    )
    .unwrap();

    let def = WorkflowDef {
        info: WorkflowInfo {
            id: workflow_id.clone(),
            name: "Stub".into(),
            description: None,
        },
        crate_dir,
        profile: Profile::Debug,
    };

    let global_dir = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let config = SpawnConfig {
        workflows: vec![def],
        global_config_dir: global_dir.path().to_path_buf(),
        project_config_dir: project_dir.path().to_path_buf(),
    };
    let sup = Supervisor::spawn(
        slug.clone(),
        issuer.verifying_key(),
        project_signing.clone(),
        config,
    );
    let state = sup.state.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proj_addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _sup = sup;
        let _ = run(listener, state).await;
    });

    let url = format!("ws://{proj_addr}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    match handshake(&mut ws, project_token).await {
        HandshakeResult::Accepted => {}
        other => panic!("project handshake: {other:?}"),
    }

    // 1. StartSession.
    let resp = pp_request(
        &mut ws,
        1,
        Request::StartSession {
            workflow: workflow_id.clone(),
        },
    )
    .await;
    let info: SessionInfo = match resp {
        Response::Ok(ResponseOk::Started(i)) => i,
        other => panic!("StartSession: {other:?}"),
    };
    assert_eq!(info.workflow, workflow_id);

    // 2. Drain SessionStarted broadcast.
    match drain_one_broadcast(&mut ws).await {
        Event::SessionStarted(s) => assert_eq!(s.id, info.id),
        other => panic!("expected SessionStarted, got {other:?}"),
    }

    // 3. OpenSession — returns addr + WorkflowSession token.
    let resp = pp_request(
        &mut ws,
        2,
        Request::OpenSession {
            session: info.id.clone(),
        },
    )
    .await;
    let endpoint: SessionEndpoint = match resp {
        Response::Ok(ResponseOk::Opened(e)) => e,
        other => panic!("OpenSession: {other:?}"),
    };
    let claims = verify(&endpoint.token, &project_signing.verifying_key())
        .expect("session token verifies against project pubkey");
    match claims.scope {
        Scope::WorkflowSession {
            project,
            workflow,
            session,
        } => {
            assert_eq!(project, slug);
            assert_eq!(workflow, workflow_id);
            assert_eq!(session, info.id);
        }
        other => panic!("expected WorkflowSession scope, got {other:?}"),
    }
    // Addr is reachable (stub bound a real listener).
    let stream = TcpStream::connect(endpoint.addr)
        .await
        .expect("stub session addr is reachable");
    drop(stream);

    // 4. StopSession.
    let resp = pp_request(
        &mut ws,
        3,
        Request::StopSession {
            session: info.id.clone(),
        },
    )
    .await;
    assert!(matches!(resp, Response::Ok(ResponseOk::Stopped)));
    match drain_one_broadcast(&mut ws).await {
        Event::SessionEnded { id } => assert_eq!(id, info.id),
        other => panic!("expected SessionEnded, got {other:?}"),
    }

    // After stop, the session is gone.
    let resp = pp_request(
        &mut ws,
        4,
        Request::OpenSession {
            session: info.id.clone(),
        },
    )
    .await;
    match resp {
        Response::Err(ApiError::SessionNotFound(id)) => assert_eq!(id, info.id),
        other => panic!("expected SessionNotFound, got {other:?}"),
    }
}
