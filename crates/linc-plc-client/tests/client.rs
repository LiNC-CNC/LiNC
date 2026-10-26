use std::os::unix::net::UnixListener;
use std::thread;

use linc_plc_client::{default_socket_path, Client};
use linc_plc_core::protocol::{read_message, write_message};
use linc_plc_core::{Request, Response, PROTOCOL_VERSION, SCHEMA_VERSION};

#[test]
fn default_socket_path_honors_explicit_socket_env() {
    let dir = tempfile::tempdir().expect("temp dir");
    let explicit = dir.path().join("plc.sock");
    std::env::set_var("LINC_PLC_SOCKET", &explicit);
    let resolved = default_socket_path();
    std::env::remove_var("LINC_PLC_SOCKET");

    assert_eq!(resolved, explicit);
}

#[test]
fn default_socket_path_falls_back_under_runtime_dir() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::env::remove_var("LINC_PLC_SOCKET");
    std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    let resolved = default_socket_path();
    std::env::remove_var("XDG_RUNTIME_DIR");

    assert_eq!(resolved.parent(), Some(dir.path().join("linc").as_path()));
    assert!(resolved
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("plc-") && name.ends_with(".sock")));
}

#[test]
fn hello_round_trip_over_unix_socket() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("plc.sock");
    let listener = UnixListener::bind(&path).expect("bind");

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let request: Request = read_message(&mut stream).expect("read request");
        assert_eq!(
            request,
            Request::Hello {
                protocol: PROTOCOL_VERSION
            }
        );
        write_message(
            &mut stream,
            &Response::Hello {
                protocol: PROTOCOL_VERSION,
                schema: SCHEMA_VERSION,
                daemon: "test-daemon".to_string(),
                hal: false,
            },
        )
        .expect("write response");
    });

    let mut client = Client::connect(&path).expect("connect");
    let response = client.hello().expect("hello");
    server.join().expect("server thread");

    assert_eq!(
        response,
        Response::Hello {
            protocol: PROTOCOL_VERSION,
            schema: SCHEMA_VERSION,
            daemon: "test-daemon".to_string(),
            hal: false,
        }
    );
}
