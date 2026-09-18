use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, Method},
    response::Response,
};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::{AppState, proxy};

const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// WebSocket transport for the Responses API (`GET /v1/responses`). Each
/// downstream message is a Responses request; upstream events are forwarded
/// back as JSON text frames. Multiple turns are supported on one socket.
pub async fn responses_websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(state, socket, headers))
}

async fn handle_socket(state: AppState, mut socket: WebSocket, headers: HeaderMap) {
    while let Some(message) = socket.recv().await {
        let payload = match message {
            Ok(Message::Text(text)) => text.as_str().as_bytes().to_vec(),
            Ok(Message::Binary(bytes)) => bytes.to_vec(),
            Ok(Message::Ping(payload)) => {
                let _ = socket.send(Message::Pong(payload)).await;
                continue;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        if let Err(message) = handle_turn(&state, &mut socket, &headers, &payload).await
            && socket
                .send(Message::Text(error_payload(&message).into()))
                .await
                .is_err()
        {
            break;
        }
    }
}

async fn handle_turn(
    state: &AppState,
    socket: &mut WebSocket,
    headers: &HeaderMap,
    payload: &[u8],
) -> Result<(), String> {
    let mut request: Value =
        serde_json::from_slice(payload).map_err(|_| "invalid JSON body".to_owned())?;
    if !request.is_object() {
        return Err("JSON body must be an object".to_owned());
    }
    if let Some(object) = request.as_object_mut() {
        object.insert("stream".into(), Value::Bool(true));
    }
    let body = Bytes::from(serde_json::to_vec(&request).map_err(|_| "invalid request".to_owned())?);
    let upstream = proxy::execute_codex(state, &Method::POST, headers, body, "responses")
        .await
        .map_err(|error| error.message().to_owned())?;
    if !upstream.status().is_success() {
        return Err(format!(
            "Codex upstream returned HTTP {}",
            upstream.status()
        ));
    }

    let mut stream = upstream.bytes_stream();
    let mut buffer = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "upstream stream failed".to_owned())?;
        buffer.extend_from_slice(&chunk);
        for data in drain_payloads(&mut buffer)? {
            let text = String::from_utf8_lossy(&data).into_owned();
            if socket.send(Message::Text(text.into())).await.is_err() {
                return Ok(());
            }
        }
    }
    if !buffer.is_empty() {
        buffer.extend_from_slice(b"\n");
        for data in drain_payloads(&mut buffer)? {
            let text = String::from_utf8_lossy(&data).into_owned();
            if socket.send(Message::Text(text.into())).await.is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Extract complete `data:` payload lines that hold valid JSON, dropping
/// `event:` lines, blank lines and the `[DONE]` marker.
fn drain_payloads(buffer: &mut BytesMut) -> Result<Vec<Vec<u8>>, String> {
    if buffer.len() > MAX_EVENT_BYTES && !buffer.contains(&b'\n') {
        return Err("upstream SSE event exceeds limit".to_owned());
    }
    let mut payloads = Vec::new();
    while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
        let line = buffer.split_to(position + 1);
        let line = line.strip_suffix(b"\n").unwrap_or(&line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let data = line
            .strip_prefix(b"data:")
            .map(|data| data.strip_prefix(b" ").unwrap_or(data))
            .unwrap_or(line);
        let data = data.trim_ascii();
        if data.is_empty() || data == b"[DONE]" {
            continue;
        }
        if serde_json::from_slice::<Value>(data).is_ok() {
            payloads.push(data.to_vec());
        }
    }
    Ok(payloads)
}

fn error_payload(message: &str) -> String {
    json!({"type":"error","error":{"type":"api_error","message":message}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::drain_payloads;
    use bytes::BytesMut;

    #[test]
    fn extracts_json_payloads_and_skips_markers() {
        let mut buffer = BytesMut::from(
            &b"event: response.created\ndata: {\"type\":\"response.created\"}\n\ndata: [DONE]\n\n"
                [..],
        );
        let payloads = drain_payloads(&mut buffer).unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&payloads[0]).unwrap(),
            serde_json::json!({"type": "response.created"})
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn waits_for_complete_lines() {
        let mut buffer = BytesMut::from(&b"data: {\"type\":\"part"[..]);
        assert!(drain_payloads(&mut buffer).unwrap().is_empty());
        buffer.extend_from_slice(b"ial\"}\n");
        let payloads = drain_payloads(&mut buffer).unwrap();
        assert_eq!(payloads.len(), 1);
    }

    #[test]
    fn error_payload_is_websocket_error_event() {
        let payload: serde_json::Value =
            serde_json::from_str(&super::error_payload("boom")).unwrap();
        assert_eq!(payload["type"], serde_json::json!("error"));
        assert_eq!(payload["error"]["message"], serde_json::json!("boom"));
    }
}
