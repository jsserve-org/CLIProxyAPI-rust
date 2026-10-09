use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

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
const TOOL_CALL_TYPES: [&str; 2] = ["function_call", "custom_tool_call"];
const TOOL_OUTPUT_TYPES: [&str; 2] = ["function_call_output", "custom_tool_call_output"];

/// Per-socket transcript state used to merge incremental `response.append` /
/// follow-up `response.create` requests into a full Codex transcript.
#[derive(Default)]
struct WsSessionState {
    last_request: Option<Value>,
    last_response_output: Vec<Value>,
}

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
    let mut session = WsSessionState::default();
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
        if let Err(message) =
            handle_turn(&state, &mut socket, &headers, &payload, &mut session).await
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
    session: &mut WsSessionState,
) -> Result<(), String> {
    let request: Value =
        serde_json::from_slice(payload).map_err(|_| "invalid JSON body".to_owned())?;
    if !request.is_object() {
        return Err("JSON body must be an object".to_owned());
    }
    if is_prewarm(session, &request) {
        let normalized = normalize_request(session, &request)?;
        let model = normalized
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        for event in prewarm_payloads(model) {
            if socket
                .send(Message::Text(event.to_string().into()))
                .await
                .is_err()
            {
                return Ok(());
            }
        }
        session.last_request = Some(normalized);
        return Ok(());
    }
    let normalized = normalize_request(session, &request)?;
    let body =
        Bytes::from(serde_json::to_vec(&normalized).map_err(|_| "invalid request".to_owned())?);
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
    let mut completed = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "upstream stream failed".to_owned())?;
        buffer.extend_from_slice(&chunk);
        for data in drain_payloads(&mut buffer)? {
            if capture_completion(session, &data) {
                completed = true;
            }
            let text = String::from_utf8_lossy(&data).into_owned();
            if socket.send(Message::Text(text.into())).await.is_err() {
                return Ok(());
            }
        }
    }
    if !buffer.is_empty() {
        buffer.extend_from_slice(b"\n");
        for data in drain_payloads(&mut buffer)? {
            if capture_completion(session, &data) {
                completed = true;
            }
            let text = String::from_utf8_lossy(&data).into_owned();
            if socket.send(Message::Text(text.into())).await.is_err() {
                return Ok(());
            }
        }
    }
    if completed {
        session.last_request = Some(normalized);
    }
    Ok(())
}

fn capture_completion(session: &mut WsSessionState, data: &[u8]) -> bool {
    let Ok(event) = serde_json::from_slice::<Value>(data) else {
        return false;
    };
    if event.get("type").and_then(Value::as_str) != Some("response.completed") {
        return false;
    }
    session.last_response_output = event
        .get("response")
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    true
}

/// A warm-up `response.create` with `generate:false` before any real turn is
/// acknowledged locally without contacting the upstream.
fn is_prewarm(session: &WsSessionState, request: &Value) -> bool {
    session.last_request.is_none()
        && request.get("type").and_then(Value::as_str) == Some("response.create")
        && request.get("generate").and_then(Value::as_bool) == Some(false)
}

fn prewarm_payloads(model: &str) -> [Value; 2] {
    let id = format!("resp_prewarm_{:032x}", unique_suffix());
    let created_at = unix_seconds();
    let mut created = json!({
        "type": "response.created",
        "sequence_number": 0,
        "response": {
            "id": id,
            "object": "response",
            "created_at": created_at,
            "status": "in_progress",
            "background": false,
            "error": null,
            "output": []
        }
    });
    let mut completed = json!({
        "type": "response.completed",
        "sequence_number": 1,
        "response": {
            "id": id,
            "object": "response",
            "created_at": created_at,
            "status": "completed",
            "background": false,
            "error": null,
            "output": [],
            "usage": {
                "input_tokens": 0,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 0,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 0
            }
        }
    });
    if !model.is_empty() {
        created["response"]["model"] = json!(model);
        completed["response"]["model"] = json!(model);
    }
    [created, completed]
}

fn unique_suffix() -> u128 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos ^ ((count as u128) << 64)
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Normalize a downstream request into a full-transcript Codex request.
fn normalize_request(session: &WsSessionState, request: &Value) -> Result<Value, String> {
    let kind = request
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("response.create")
        .trim();
    if kind != "response.create" && kind != "response.append" {
        return Err(format!("unsupported websocket request type: {kind}"));
    }
    let mut normalized = request.clone();
    if let Some(object) = normalized.as_object_mut() {
        object.remove("type");
        object.remove("previous_response_id");
        object.insert("stream".into(), Value::Bool(true));
    }

    if kind == "response.create" && session.last_request.is_none() {
        if let Some(input) = normalized.get("input") {
            if !input.is_array() {
                return Err("websocket request requires array field: input".to_owned());
            }
        } else {
            normalized["input"] = json!([]);
        }
        if normalized
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .is_none()
        {
            return Err("missing model in response.create request".to_owned());
        }
        return Ok(normalized);
    }

    let Some(last_request) = &session.last_request else {
        return Err("websocket request received before response.create".to_owned());
    };
    let Some(next_input) = normalized.get("input").and_then(Value::as_array).cloned() else {
        return Err("websocket request requires array field: input".to_owned());
    };
    let mut items: Vec<Value> = last_request
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    items.extend(session.last_response_output.iter().cloned());
    items.extend(next_input);
    dedupe_items(&mut items);
    normalized["input"] = Value::Array(items);

    if normalized.get("model").and_then(Value::as_str).is_none()
        && let Some(model) = last_request.get("model")
    {
        normalized["model"] = model.clone();
    }
    if normalized.get("instructions").is_none()
        && let Some(instructions) = last_request.get("instructions")
    {
        normalized["instructions"] = instructions.clone();
    }
    Ok(normalized)
}

/// Drop duplicate tool calls by `call_id`, then duplicate items by `id`,
/// preferring the entry referenced by a tool output.
fn dedupe_items(items: &mut Vec<Value>) {
    let mut seen_calls: HashSet<String> = HashSet::new();
    items.retain(|item| {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let call_id = trimmed(item, "call_id");
        if TOOL_CALL_TYPES.contains(&item_type) && !call_id.is_empty() {
            return seen_calls.insert(call_id);
        }
        true
    });

    let referenced: HashSet<String> = items
        .iter()
        .filter_map(|item| {
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            let call_id = trimmed(item, "call_id");
            (TOOL_OUTPUT_TYPES.contains(&item_type) && !call_id.is_empty()).then_some(call_id)
        })
        .collect();

    let mut keep: HashMap<String, (usize, bool)> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        let id = trimmed(item, "id");
        if id.is_empty() {
            continue;
        }
        let call_id = trimmed(item, "call_id");
        let is_referenced = !call_id.is_empty() && referenced.contains(&call_id);
        match keep.get(&id) {
            Some((_, previous_referenced)) if !is_referenced && *previous_referenced => {}
            _ => {
                keep.insert(id, (index, is_referenced));
            }
        }
    }

    let mut index = 0;
    items.retain(|item| {
        let current = index;
        index += 1;
        let id = trimmed(item, "id");
        if id.is_empty() {
            return true;
        }
        keep.get(&id).is_none_or(|(kept, _)| *kept == current)
    });
}

fn trimmed(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
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
    use super::{WsSessionState, dedupe_items, drain_payloads, normalize_request};
    use bytes::BytesMut;
    use serde_json::json;

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
            json!({"type": "response.created"})
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn waits_for_complete_lines() {
        let mut buffer = BytesMut::from(&b"data: {\"type\":\"part"[..]);
        assert!(drain_payloads(&mut buffer).unwrap().is_empty());
        buffer.extend_from_slice(b"ial\"}\n");
        assert_eq!(drain_payloads(&mut buffer).unwrap().len(), 1);
    }

    #[test]
    fn detects_and_builds_local_prewarm() {
        use super::{is_prewarm, prewarm_payloads};
        let session = WsSessionState::default();
        let warmup = json!({"type":"response.create","model":"gpt-5.6-sol","generate":false});
        assert!(is_prewarm(&session, &warmup));
        assert!(!is_prewarm(
            &session,
            &json!({"type":"response.create","model":"gpt-5.6-sol"})
        ));

        let [created, completed] = prewarm_payloads("gpt-5.6-sol");
        assert_eq!(created["type"], json!("response.created"));
        assert_eq!(completed["type"], json!("response.completed"));
        assert_eq!(created["response"]["id"], completed["response"]["id"]);
        assert_eq!(completed["response"]["status"], json!("completed"));
        assert_eq!(completed["response"]["model"], json!("gpt-5.6-sol"));
        assert_eq!(completed["response"]["usage"]["total_tokens"], json!(0));
        assert!(
            created["response"]["id"]
                .as_str()
                .unwrap()
                .starts_with("resp_prewarm_")
        );
    }

    #[test]
    fn initial_create_strips_type_and_defaults_input() {
        let session = WsSessionState::default();
        let normalized = normalize_request(
            &session,
            &json!({"type":"response.create","model":"gpt-5.6-sol"}),
        )
        .unwrap();
        assert!(normalized.get("type").is_none());
        assert_eq!(normalized["input"], json!([]));
        assert_eq!(normalized["stream"], json!(true));
    }

    #[test]
    fn create_requires_model() {
        let session = WsSessionState::default();
        assert!(normalize_request(&session, &json!({"type":"response.create"})).is_err());
    }

    #[test]
    fn subsequent_request_merges_transcript() {
        let mut session = WsSessionState {
            last_request: Some(json!({
                "model":"gpt-5.6-sol",
                "instructions":"rules",
                "input":[{"type":"message","role":"user","id":"m1","content":"hi"}]
            })),
            last_response_output: vec![json!({
                "type":"message","role":"assistant","id":"m2",
                "content":[{"type":"output_text","text":"hello"}]
            })],
        };
        let normalized = normalize_request(
            &session,
            &json!({
                "type":"response.append",
                "previous_response_id":"resp_1",
                "input":[{"type":"message","role":"user","id":"m3","content":"next"}]
            }),
        )
        .unwrap();
        assert_eq!(normalized["model"], json!("gpt-5.6-sol"));
        assert_eq!(normalized["instructions"], json!("rules"));
        assert!(normalized.get("previous_response_id").is_none());
        assert_eq!(normalized["input"].as_array().unwrap().len(), 3);
        assert_eq!(normalized["input"][1]["id"], json!("m2"));
        assert_eq!(normalized["input"][2]["id"], json!("m3"));

        session.last_request = None;
        assert!(
            normalize_request(&session, &json!({"type":"response.append","input":[]})).is_err()
        );
    }

    #[test]
    fn dedupe_keeps_first_tool_call_and_last_item_by_id() {
        let mut items = vec![
            json!({"type":"function_call","call_id":"call_1","id":"a","name":"x"}),
            json!({"type":"function_call","call_id":"call_1","id":"b","name":"x"}),
            json!({"type":"message","id":"m","content":"old"}),
            json!({"type":"message","id":"m","content":"new"}),
        ];
        dedupe_items(&mut items);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["call_id"], json!("call_1"));
        assert_eq!(items[0]["id"], json!("a"));
        assert_eq!(items[1]["content"], json!("new"));
    }

    #[test]
    fn error_payload_is_websocket_error_event() {
        let payload: serde_json::Value =
            serde_json::from_str(&super::error_payload("boom")).unwrap();
        assert_eq!(payload["type"], json!("error"));
        assert_eq!(payload["error"]["message"], json!("boom"));
    }
}
