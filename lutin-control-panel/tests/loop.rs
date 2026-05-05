use futures_util::{SinkExt, StreamExt};
use lutin_auth::{Scope, Subject, Ttl, generate_keypair, mint_with_ttl};
use lutin_control_panel::{SpawnBackend, SpawnConfig, Supervisor, run};
use lutin_control_protocol::{
    self as cp, ApiError, DisplayName, Event, ProjectInfo, ProjectStatus, Request, Response,
    ResponseOk, Slug, SpawnFailureKind,
};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

async fn spawn_server() -> (String, String) {
    let key = generate_keypair().unwrap();
    let token = mint_with_ttl(
        &key,
        Subject::parse("tester").unwrap(),
        Scope::ControlPanel,
        Ttl::from_secs(60),
    )
    .unwrap();
    // Tests that don't drive OpenProject pass an unreachable binary
    // path — the spawn config is held but never consulted.
    let config = SpawnConfig {
        backend: SpawnBackend::Subprocess {
            binary: "/nonexistent/lutin-project".into(),
        },
        projects_root: tempfile::tempdir().unwrap().keep(),
        global_config_dir: tempfile::tempdir().unwrap().keep(),
    };
    let sup = Supervisor::spawn(key, config);
    let state = sup.state.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _sup = sup;
        let _ = run(listener, state).await;
    });
    (addr, token)
}

async fn connect(
    addr: &str,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let hello = encode(&Frame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: token.into(),
    })
    .unwrap();
    ws.send(Message::Binary(hello.into())).await.unwrap();
    let ack_msg = ws.next().await.unwrap().unwrap();
    let ack_bytes = match ack_msg {
        Message::Binary(b) => b,
        other => panic!("expected binary ack, got {other:?}"),
    };
    match decode(&ack_bytes).unwrap() {
        Frame::HelloAck(HandshakeResult::Accepted) => {}
        other => panic!("bad ack: {other:?}"),
    }
    ws
}

async fn request(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request_id: u64,
    req: Request,
) -> Response {
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

async fn next_event(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Event {
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        let bytes = match msg {
            Message::Binary(b) => b,
            _ => continue,
        };
        if let Frame::Broadcast { body } = decode(&bytes).unwrap() {
            return cp::decode::<Event>(&body).unwrap();
        }
    }
}

#[tokio::test]
async fn rejects_bad_token() {
    let (addr, _good) = spawn_server().await;
    let url = format!("ws://{addr}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let hello = encode(&Frame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: "garbage".into(),
    })
    .unwrap();
    ws.send(Message::Binary(hello.into())).await.unwrap();
    let ack = ws.next().await.unwrap().unwrap();
    let bytes = match ack {
        Message::Binary(b) => b,
        other => panic!("{other:?}"),
    };
    match decode(&bytes).unwrap() {
        Frame::HelloAck(HandshakeResult::Rejected { .. }) => {}
        other => panic!("expected nack, got {other:?}"),
    }
}

#[tokio::test]
async fn create_list_delete_with_broadcast() {
    let (addr, token) = spawn_server().await;
    let mut a = connect(&addr, &token).await;
    let mut b = connect(&addr, &token).await;

    // a creates project; both should see broadcast.
    let resp = request(
        &mut a,
        1,
        Request::CreateProject {
            slug: Slug::parse("demo").unwrap(),
            display_name: DisplayName::parse("Demo").unwrap(),
        },
    )
    .await;
    assert!(matches!(resp, Response::Ok(ResponseOk::Created(_))));
    let ev_a = next_event(&mut a).await;
    let ev_b = next_event(&mut b).await;
    assert_eq!(
        ev_a,
        Event::ProjectCreated(ProjectInfo {
            slug: Slug::parse("demo").unwrap(),
            display_name: DisplayName::parse("Demo").unwrap(),
            status: ProjectStatus::Stopped,
        })
    );
    assert_eq!(ev_a, ev_b);

    // List from b.
    let resp = request(&mut b, 2, Request::ListProjects).await;
    match resp {
        Response::Ok(ResponseOk::Projects(v)) => {
            assert_eq!(v.len(), 1);
            assert_eq!(v[0].slug, Slug::parse("demo").unwrap());
        }
        other => panic!("{other:?}"),
    }

    // Delete from a, b sees ProjectDeleted.
    let resp = request(
        &mut a,
        3,
        Request::DeleteProject {
            slug: Slug::parse("demo").unwrap(),
        },
    )
    .await;
    assert!(matches!(resp, Response::Ok(ResponseOk::Deleted)));
    assert_eq!(
        next_event(&mut b).await,
        Event::ProjectDeleted {
            slug: Slug::parse("demo").unwrap()
        }
    );
}

#[tokio::test]
async fn open_project_with_bad_binary_reports_spawn_failure() {
    let (addr, token) = spawn_server().await;
    let mut a = connect(&addr, &token).await;
    let _ = request(
        &mut a,
        1,
        Request::CreateProject {
            slug: Slug::parse("demo").unwrap(),
            display_name: DisplayName::parse("Demo").unwrap(),
        },
    )
    .await;
    let _ = next_event(&mut a).await; // drain ProjectCreated

    let resp = request(
        &mut a,
        2,
        Request::OpenProject {
            slug: Slug::parse("demo").unwrap(),
        },
    )
    .await;
    assert!(matches!(
        resp,
        Response::Err(ApiError::SpawnFailed {
            kind: SpawnFailureKind::BinaryMissing,
            ..
        })
    ));

    // Stop on a never-running project is idempotent.
    let resp = request(
        &mut a,
        3,
        Request::StopProject {
            slug: Slug::parse("demo").unwrap(),
        },
    )
    .await;
    assert_eq!(resp, Response::Ok(ResponseOk::Stopped));
}

#[tokio::test]
async fn rejects_duplicate_and_invalid() {
    let (addr, token) = spawn_server().await;
    let mut a = connect(&addr, &token).await;
    let _ = request(
        &mut a,
        1,
        Request::CreateProject {
            slug: Slug::parse("x").unwrap(),
            display_name: DisplayName::parse("X").unwrap(),
        },
    )
    .await;
    let dup = request(
        &mut a,
        2,
        Request::CreateProject {
            slug: Slug::parse("x").unwrap(),
            display_name: DisplayName::parse("X2").unwrap(),
        },
    )
    .await;
    assert!(matches!(dup, Response::Err(_)));
    // Invalid slugs now fail at the parse boundary; no network round trip.
    assert!(Slug::parse("no spaces!").is_err());
}
