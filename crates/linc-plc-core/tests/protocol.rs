use std::io::Cursor;

use linc_plc_core::protocol::{
    read_frame, read_message, write_frame, write_message, ProtocolError, Request, Response,
    RunState, StateSnapshot,
};
use linc_plc_core::{Diagnostic, Severity, SymbolKind, VarValue, MAX_FRAME, PROTOCOL_VERSION};

fn snapshot() -> StateSnapshot {
    StateSnapshot {
        state: RunState::Running,
        project: "example".to_string(),
        hash: "0123456789abcdef".to_string(),
        scan_count: 42,
        scan_time_us: 17,
        overruns: 0,
        alive: true,
        fault: None,
        vars: vec![VarValue {
            name: "run".to_string(),
            value: true,
            kind: SymbolKind::Output,
            forced: false,
        }],
        forced: Vec::new(),
    }
}

#[test]
fn request_and_response_frames_round_trip() {
    let requests = vec![
        Request::Hello {
            protocol: PROTOCOL_VERSION,
        },
        Request::Start,
        Request::ClearForces,
        Request::Force {
            name: "run".to_string(),
            value: true,
            ttl_ms: 500,
        },
        Request::Validate {
            project: linc_plc_core::Project::example(),
        },
    ];
    for request in requests {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &request).expect("write request");
        let mut cursor = Cursor::new(buffer);
        let parsed: Request = read_message(&mut cursor).expect("read request");
        assert_eq!(parsed, request);
    }

    let responses = vec![
        Response::Hello {
            protocol: PROTOCOL_VERSION,
            schema: 1,
            daemon: "daemon".to_string(),
            hal: false,
        },
        Response::Validated {
            ok: false,
            diagnostics: vec![Diagnostic::error("schema", "bad", "schema")],
        },
        Response::State(snapshot()),
        Response::Error {
            code: "fault".to_string(),
            message: "boom".to_string(),
        },
        Response::Bye,
    ];
    for response in responses {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &response).expect("write response");
        let mut cursor = Cursor::new(buffer);
        let parsed: Response = read_message(&mut cursor).expect("read response");
        assert_eq!(parsed, response);
    }
}

#[test]
fn message_tags_are_snake_case() {
    assert_eq!(
        serde_json::to_value(Request::ClearForces).expect("json"),
        serde_json::json!({"request": "clear_forces"})
    );
    assert_eq!(
        serde_json::to_value(Request::Force {
            name: "run".to_string(),
            value: true,
            ttl_ms: 0,
        })
        .expect("json"),
        serde_json::json!({"request": "force", "data": {"name": "run", "value": true, "ttl_ms": 0}})
    );
    assert_eq!(
        serde_json::to_value(Response::Bye).expect("json"),
        serde_json::json!({"response": "bye"})
    );
    assert_eq!(
        serde_json::to_value(RunState::Fault).expect("json"),
        serde_json::json!("fault")
    );
    assert_eq!(
        serde_json::to_value(Severity::Warning).expect("json"),
        serde_json::json!("warning")
    );
}

#[test]
fn state_snapshot_serde_round_trip() {
    let original = snapshot();
    let text = serde_json::to_string(&original).expect("serialise");
    let parsed: StateSnapshot = serde_json::from_str(&text).expect("deserialise");
    assert_eq!(parsed, original);
    assert!(text.contains("\"running\""), "{text}");
    assert!(text.contains("\"fault\":null"), "{text}");
}

#[test]
fn oversize_frames_are_rejected_in_both_directions() {
    let oversize = vec![0u8; MAX_FRAME + 1];
    let mut sink = Vec::new();
    assert!(matches!(
        write_frame(&mut sink, &oversize),
        Err(ProtocolError::FrameTooLarge)
    ));
    assert!(sink.is_empty());

    let mut header = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
    header.extend_from_slice(b"{}");
    let mut cursor = Cursor::new(header);
    assert!(matches!(
        read_frame(&mut cursor),
        Err(ProtocolError::FrameTooLarge)
    ));
}

#[test]
fn clean_eof_is_reported_as_eof() {
    let mut empty: &[u8] = &[];
    let result: Result<Response, ProtocolError> = read_message(&mut empty);
    assert!(matches!(result, Err(ProtocolError::Eof)));
}

#[test]
fn truncated_header_is_an_io_error() {
    let mut partial: &[u8] = &[0u8, 0u8];
    let result = read_frame(&mut partial);
    assert!(matches!(result, Err(ProtocolError::Io(_))));
}

#[test]
fn frame_header_is_big_endian_length() {
    let mut buffer = Vec::new();
    write_frame(&mut buffer, b"{}").expect("write frame");
    assert_eq!(&buffer[..4], &2u32.to_be_bytes());
    assert_eq!(&buffer[4..], b"{}");
}

#[test]
fn protocol_error_kinds_are_stable() {
    let io = ProtocolError::Io(std::io::Error::new(std::io::ErrorKind::Other, "boom"));
    assert_eq!(io.kind(), "io");

    let json = serde_json::from_str::<u32>("nope").expect_err("invalid json");
    assert_eq!(ProtocolError::Json(json).kind(), "json");

    assert_eq!(ProtocolError::FrameTooLarge.kind(), "frame_too_large");
    assert_eq!(ProtocolError::Eof.kind(), "eof");
}

#[test]
fn wire_envelope_uses_adjacent_tagging() {
    assert_eq!(
        serde_json::to_value(Request::Hello { protocol: 1 }).expect("json"),
        serde_json::json!({"request": "hello", "data": {"protocol": 1}})
    );
    assert_eq!(
        serde_json::to_value(Request::Start).expect("json"),
        serde_json::json!({"request": "start"})
    );
    assert_eq!(
        serde_json::to_value(Response::State(snapshot())).expect("json"),
        serde_json::json!({
            "response": "state",
            "data": {
                "state": "running",
                "project": "example",
                "hash": "0123456789abcdef",
                "scan_count": 42,
                "scan_time_us": 17,
                "overruns": 0,
                "alive": true,
                "fault": null,
                "vars": [{"name": "run", "value": true, "kind": "output", "forced": false}],
                "forced": [],
            }
        })
    );
    assert_eq!(
        serde_json::to_value(Response::Error {
            code: "fault".to_string(),
            message: "boom".to_string(),
        })
        .expect("json"),
        serde_json::json!({"response": "error", "data": {"code": "fault", "message": "boom"}})
    );

    // The adjacent envelope must also decode.
    let hello: Request =
        serde_json::from_value(serde_json::json!({"request": "hello", "data": {"protocol": 1}}))
            .expect("decode hello");
    assert_eq!(
        hello,
        Request::Hello {
            protocol: PROTOCOL_VERSION
        }
    );
    let start: Request =
        serde_json::from_value(serde_json::json!({"request": "start"})).expect("decode start");
    assert_eq!(start, Request::Start);
}
