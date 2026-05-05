//! End-to-end: control-panel spawns the real `lutin-project` binary,
//! returns a working endpoint, and shuts it down on `StopProject`.
//!
//! The binary is built on demand via `escargot`. First run pays a
//! compile cost; subsequent runs reuse the cached artifact.

use futures_util::{SinkExt, StreamExt};
use lutin_auth::{Scope, Subject, Ttl, VerifyingKey, generate_keypair, mint_with_ttl};
use lutin_control_panel::{SpawnBackend, SpawnConfig, Supervisor, run};
use lutin_control_protocol::{
    self as cp, DisplayName, ProjectEndpoint, Request, Response, ResponseOk, Slug,
};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

fn project_binary() -> PathBuf {
    escargot::CargoBuild::new()
        .package("lutin-project")
        .bin("lutin-project")
        .manifest_path("../lutin-project/Cargo.toml")
        .run()
        .expect("build lutin-project")
        .path()
        .to_path_buf()
}

async fn cp_request(ws: &mut Ws, request_id: u64, req: Request) -> Response {
    let body = cp::encode(&req).unwrap();
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
            } if rid == request_id => return cp::decode::<Response>(&body).unwrap(),
            _ => continue,
        }
    }
}

async fn drain_one_broadcast(ws: &mut Ws) {
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        if let Message::Binary(b) = msg
            && let Frame::Broadcast { .. } = decode(&b).unwrap()
        {
            return;
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
async fn open_then_stop_real_project() {
    let cp_key = generate_keypair().unwrap();
    let admin_token = mint_with_ttl(
        &cp_key,
        Subject::parse("admin").unwrap(),
        Scope::ControlPanel,
        Ttl::from_secs(60),
    )
    .unwrap();

    let projects_root = tempfile::tempdir().unwrap();
    let config = SpawnConfig {
        backend: SpawnBackend::Subprocess { binary: project_binary() },
        projects_root: projects_root.path().to_path_buf(),
        global_config_dir: tempfile::tempdir().unwrap().keep(),
    };
    let sup = Supervisor::spawn(cp_key, config);
    let state = sup.state.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cp_addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _sup = sup;
        let _ = run(listener, state).await;
    });

    let url = format!("ws://{cp_addr}");
    let (mut cp_ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    match handshake(&mut cp_ws, admin_token).await {
        HandshakeResult::Accepted => {}
        other => panic!("cp handshake: {other:?}"),
    }

    let slug = Slug::parse("demo").unwrap();
    cp_request(
        &mut cp_ws,
        1,
        Request::CreateProject {
            slug: slug.clone(),
            display_name: DisplayName::parse("Demo").unwrap(),
        },
    )
    .await;
    drain_one_broadcast(&mut cp_ws).await; // ProjectCreated

    let resp = cp_request(
        &mut cp_ws,
        2,
        Request::OpenProject { slug: slug.clone() },
    )
    .await;
    let endpoint: ProjectEndpoint = match resp {
        Response::Ok(ResponseOk::Opened(e)) => e,
        other => panic!("OpenProject: {other:?}"),
    };
    // Status broadcasts: Starting then Running.
    drain_one_broadcast(&mut cp_ws).await;
    drain_one_broadcast(&mut cp_ws).await;

    // The advertised project pubkey must be a valid ed25519 key.
    let _project_pubkey = VerifyingKey::from_bytes(endpoint.project_pubkey.as_bytes())
        .expect("project pubkey is valid ed25519");

    // Connect to the spawned project supervisor with the returned token.
    let proj_url = format!("ws://{}", endpoint.addr);
    let (mut proj_ws, _) = tokio_tungstenite::connect_async(&proj_url).await.unwrap();
    match handshake(&mut proj_ws, endpoint.token.clone()).await {
        HandshakeResult::Accepted => {}
        other => panic!("project handshake: {other:?}"),
    }
    drop(proj_ws);

    let resp = cp_request(
        &mut cp_ws,
        3,
        Request::StopProject { slug: slug.clone() },
    )
    .await;
    assert!(matches!(resp, Response::Ok(ResponseOk::Stopped)));
}

#[tokio::test]
async fn slug_reuse_after_delete_starts_fresh_identity() {
    let cp_key = generate_keypair().unwrap();
    let admin_token = mint_with_ttl(
        &cp_key,
        Subject::parse("admin").unwrap(),
        Scope::ControlPanel,
        Ttl::from_secs(60),
    )
    .unwrap();

    let projects_root = tempfile::tempdir().unwrap();
    let config = SpawnConfig {
        backend: SpawnBackend::Subprocess { binary: project_binary() },
        projects_root: projects_root.path().to_path_buf(),
        global_config_dir: tempfile::tempdir().unwrap().keep(),
    };
    let sup = Supervisor::spawn(cp_key, config);
    let state = sup.state.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cp_addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _sup = sup;
        let _ = run(listener, state).await;
    });

    let url = format!("ws://{cp_addr}");
    let (mut cp_ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    match handshake(&mut cp_ws, admin_token).await {
        HandshakeResult::Accepted => {}
        other => panic!("cp handshake: {other:?}"),
    }

    let slug = Slug::parse("revival").unwrap();
    let display = || DisplayName::parse("Revival").unwrap();

    // First incarnation.
    cp_request(
        &mut cp_ws,
        1,
        Request::CreateProject {
            slug: slug.clone(),
            display_name: display(),
        },
    )
    .await;
    drain_one_broadcast(&mut cp_ws).await; // ProjectCreated

    let resp = cp_request(
        &mut cp_ws,
        2,
        Request::OpenProject { slug: slug.clone() },
    )
    .await;
    let first_endpoint: ProjectEndpoint = match resp {
        Response::Ok(ResponseOk::Opened(e)) => e,
        other => panic!("OpenProject #1: {other:?}"),
    };
    drain_one_broadcast(&mut cp_ws).await; // Starting
    drain_one_broadcast(&mut cp_ws).await; // Running
    let first_pubkey = first_endpoint.project_pubkey;

    // Stop, then delete. Both must succeed.
    let resp = cp_request(
        &mut cp_ws,
        3,
        Request::StopProject { slug: slug.clone() },
    )
    .await;
    assert!(matches!(resp, Response::Ok(ResponseOk::Stopped)));
    drain_one_broadcast(&mut cp_ws).await; // Stopped status

    let resp = cp_request(
        &mut cp_ws,
        4,
        Request::DeleteProject { slug: slug.clone() },
    )
    .await;
    assert!(matches!(resp, Response::Ok(ResponseOk::Deleted)));
    drain_one_broadcast(&mut cp_ws).await; // ProjectDeleted

    // Second incarnation with same slug — must get a brand new identity.
    cp_request(
        &mut cp_ws,
        5,
        Request::CreateProject {
            slug: slug.clone(),
            display_name: display(),
        },
    )
    .await;
    drain_one_broadcast(&mut cp_ws).await; // ProjectCreated

    let resp = cp_request(
        &mut cp_ws,
        6,
        Request::OpenProject { slug: slug.clone() },
    )
    .await;
    let second_endpoint: ProjectEndpoint = match resp {
        Response::Ok(ResponseOk::Opened(e)) => e,
        other => panic!("OpenProject #2: {other:?}"),
    };
    drain_one_broadcast(&mut cp_ws).await; // Starting
    drain_one_broadcast(&mut cp_ws).await; // Running

    assert_ne!(
        first_pubkey.as_bytes(),
        second_endpoint.project_pubkey.as_bytes(),
        "slug reuse after delete must mint a fresh project identity, not revive the old keypair"
    );

    let _ = cp_request(
        &mut cp_ws,
        7,
        Request::StopProject { slug: slug.clone() },
    )
    .await;
}

#[tokio::test]
async fn projects_persist_and_auto_launch_on_boot() {
    let cp_key = generate_keypair().unwrap();
    let admin_token = mint_with_ttl(
        &cp_key,
        Subject::parse("admin").unwrap(),
        Scope::ControlPanel,
        Ttl::from_secs(60),
    )
    .unwrap();

    let projects_root = tempfile::tempdir().unwrap();
    let global_dir = tempfile::tempdir().unwrap().keep();
    let mk_config = || SpawnConfig {
        backend: SpawnBackend::Subprocess { binary: project_binary() },
        projects_root: projects_root.path().to_path_buf(),
        global_config_dir: global_dir.clone(),
    };

    // Boot 1: create a project, then shut down cleanly.
    {
        let sup = Supervisor::spawn(cp_key.clone(), mk_config());
        let state = sup.state.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cp_addr = listener.local_addr().unwrap().to_string();
        let server = tokio::spawn(async move {
            let _ = run(listener, state).await;
        });
        let url = format!("ws://{cp_addr}");
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        match handshake(&mut ws, admin_token.clone()).await {
            HandshakeResult::Accepted => {}
            other => panic!("cp handshake: {other:?}"),
        }
        cp_request(
            &mut ws,
            1,
            Request::CreateProject {
                slug: Slug::parse("alpha").unwrap(),
                display_name: DisplayName::parse("Alpha").unwrap(),
            },
        )
        .await;
        drain_one_broadcast(&mut ws).await;
        drop(ws);
        sup.shutdown().await;
        server.abort();
    }

    // Boot 2: same projects_root. Registry must hydrate `alpha`, and the
    // boot-time auto-launch must drive it to Running before clients
    // connect — so a fresh ListProjects sees Status::Running without
    // anyone calling OpenProject.
    {
        let sup = Supervisor::spawn(cp_key, mk_config());
        let state = sup.state.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cp_addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _sup = sup;
            let _ = run(listener, state).await;
        });
        let url = format!("ws://{cp_addr}");
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        match handshake(&mut ws, admin_token).await {
            HandshakeResult::Accepted => {}
            other => panic!("cp handshake: {other:?}"),
        }
        let resp = cp_request(&mut ws, 1, Request::ListProjects).await;
        let projects = match resp {
            Response::Ok(ResponseOk::Projects(p)) => p,
            other => panic!("ListProjects: {other:?}"),
        };
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].slug.as_str(), "alpha");
        assert!(
            matches!(projects[0].status, lutin_control_protocol::ProjectStatus::Running),
            "expected auto-launched project to be Running, got {:?}",
            projects[0].status
        );
    }
}
