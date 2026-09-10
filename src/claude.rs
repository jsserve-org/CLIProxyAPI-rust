use std::collections::HashSet;

use async_stream::stream;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use crate::{AppState, error::AppError, proxy};

const MAX_SSE_LINE: usize = 4 * 1024 * 1024;

pub async fn messages(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, AppError> {
    let (parts, body) = request.into_parts();
    let input = axum::body::to_bytes(body, state.config.max_body_bytes)
        .await
        .map_err(|_| AppError::bad_request("request body exceeds limit"))?;
    let payload: Value =
        serde_json::from_slice(&input).map_err(|_| AppError::bad_request("invalid JSON body"))?;
    if payload.get("stream").and_then(Value::as_bool) == Some(false) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("gpt-5.6-sol")
        .to_owned();
    let translated = translate_request(payload)?;
    let upstream = proxy::execute_codex(
        &state,
        &Method::POST,
        &parts.headers,
        Bytes::from(
            serde_json::to_vec(&translated)
                .map_err(|_| AppError::bad_request("invalid request"))?,
        ),
        "responses",
    )
    .await?;
    if !upstream.status().is_success() {
        let status = upstream.status();
        return Err(AppError::new(
            status,
            format!("Codex upstream returned HTTP {status}"),
        ));
    }
    let body = translate_stream(upstream, model);
    let mut response = Response::new(body);
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-cache"));
    Ok(response)
}

fn translate_request(input: Value) -> Result<Value, AppError> {
    let object = input
        .as_object()
        .ok_or_else(|| AppError::bad_request("JSON body must be an object"))?;
    let model = object
        .get("model")
        .cloned()
        .unwrap_or_else(|| Value::String("gpt-5.6-sol".into()));
    let mut output = Map::new();
    output.insert("model".into(), model);
    output.insert("stream".into(), Value::Bool(true));
    output.insert("store".into(), Value::Bool(false));
    output.insert(
        "instructions".into(),
        Value::String(system_text(object.get("system"))),
    );
    output.insert("input".into(), translate_messages(object.get("messages"))?);
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        output.insert(
            "tools".into(),
            Value::Array(tools.iter().filter_map(translate_tool).collect()),
        );
        output.insert(
            "parallel_tool_calls".into(),
            object
                .get("disable_parallel_tool_use")
                .and_then(Value::as_bool)
                .map(|value| Value::Bool(!value))
                .unwrap_or(Value::Bool(true)),
        );
    }
    if let Some(choice) = object.get("tool_choice") {
        let translated = match choice.get("type").and_then(Value::as_str) {
            Some("any") => Value::String("required".into()),
            Some("tool") => {
                json!({"type":"function","name":choice.get("name").and_then(Value::as_str).unwrap_or_default()})
            }
            _ => Value::String("auto".into()),
        };
        output.insert("tool_choice".into(), translated);
    }
    if let Some(max) = object.get("max_tokens") {
        output.insert("max_output_tokens".into(), max.clone());
    }
    if let Some(key) = object.get("metadata").and_then(|v| v.get("user_id")) {
        output.insert("prompt_cache_key".into(), key.clone());
    }
    if let Some(thinking) = object.get("thinking") {
        let effort = if thinking.get("type").and_then(Value::as_str) == Some("enabled") {
            "high"
        } else {
            "none"
        };
        output.insert(
            "reasoning".into(),
            json!({"effort":effort,"summary":"auto"}),
        );
    }
    Ok(Value::Object(output))
}

fn system_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn translate_messages(value: Option<&Value>) -> Result<Value, AppError> {
    let messages = value
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::bad_request("messages is required"))?;
    let mut items = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match message.get("content") {
            Some(Value::String(text)) => items.push(json!({"type":"message","role":role,"content":[{
                "type": if role == "assistant" { "output_text" } else { "input_text" }, "text":text
            }]})),
            Some(Value::Array(parts)) => translate_parts(role, parts, &mut items),
            _ => {}
        }
    }
    Ok(Value::Array(items))
}

fn translate_parts(role: &str, parts: &[Value], items: &mut Vec<Value>) {
    let mut content = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => content.push(json!({"type":if role == "assistant" {"output_text"} else {"input_text"},"text":part.get("text").and_then(Value::as_str).unwrap_or_default()})),
            Some("image") => {
                if let Some(source) = part.get("source") {
                    let image_url = match source.get("type").and_then(Value::as_str) {
                        Some("base64") => format!("data:{};base64,{}", source.get("media_type").and_then(Value::as_str).unwrap_or("image/png"), source.get("data").and_then(Value::as_str).unwrap_or_default()),
                        Some("url") => source.get("url").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        _ => String::new(),
                    };
                    if !image_url.is_empty() { content.push(json!({"type":"input_image","image_url":image_url})); }
                }
            }
            Some("tool_use") => {
                flush_content(role, &mut content, items);
                items.push(json!({"type":"function_call","call_id":part.get("id").and_then(Value::as_str).unwrap_or_default(),"name":part.get("name").and_then(Value::as_str).unwrap_or_default(),"arguments":serde_json::to_string(part.get("input").unwrap_or(&Value::Null)).unwrap_or_else(|_| "{}".into())}));
            }
            Some("tool_result") => {
                flush_content(role, &mut content, items);
                items.push(json!({"type":"function_call_output","call_id":part.get("tool_use_id").and_then(Value::as_str).unwrap_or_default(),"output":tool_result_text(part.get("content"))}));
            }
            _ => {}
        }
    }
    flush_content(role, &mut content, items);
}

fn flush_content(role: &str, content: &mut Vec<Value>, items: &mut Vec<Value>) {
    if !content.is_empty() {
        items.push(json!({"type":"message","role":role,"content":std::mem::take(content)}));
    }
}

fn tool_result_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|v| v.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
        None => String::new(),
    }
}

fn translate_tool(tool: &Value) -> Option<Value> {
    Some(
        json!({"type":"function","name":tool.get("name")?.as_str()?,"description":tool.get("description").and_then(Value::as_str).unwrap_or_default(),"parameters":tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type":"object"})),"strict":false}),
    )
}

fn translate_stream(upstream: reqwest::Response, requested_model: String) -> Body {
    let mut input = upstream.bytes_stream();
    let output = stream! {
        let mut buffer = BytesMut::new();
        let mut state = StreamState::new(requested_model);
        while let Some(chunk) = input.next().await {
            let chunk = match chunk { Ok(value) => value, Err(error) => { yield Err(std::io::Error::other(error)); break; } };
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_SSE_LINE && !buffer.contains(&b'\n') {
                yield Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "upstream SSE event exceeds limit")); break;
            }
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.split_to(position + 1);
                let line = line.strip_suffix(b"\n").unwrap_or(&line);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let Some(data) = line.strip_prefix(b"data:") else { continue; };
                let data = data.strip_prefix(b" ").unwrap_or(data);
                if data == b"[DONE]" { continue; }
                if let Ok(event) = serde_json::from_slice::<Value>(data) {
                    for translated in state.event(&event) { yield Ok(Bytes::from(translated)); }
                }
            }
        }
    };
    Body::from_stream(output)
}

struct StreamState {
    model: String,
    id: String,
    started: HashSet<usize>,
    stopped: HashSet<usize>,
    tool_blocks: HashSet<usize>,
}

impl StreamState {
    fn new(model: String) -> Self {
        Self {
            model,
            id: String::new(),
            started: HashSet::new(),
            stopped: HashSet::new(),
            tool_blocks: HashSet::new(),
        }
    }
    fn event(&mut self, event: &Value) -> Vec<String> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let mut out = Vec::new();
        match kind {
            "response.created" | "response.in_progress" if self.id.is_empty() => {
                self.id = event.pointer("/response/id").and_then(Value::as_str).unwrap_or("msg_rust").to_owned();
                out.push(sse("message_start", json!({"type":"message_start","message":{"id":self.id,"type":"message","role":"assistant","model":self.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}})));
            }
            "response.output_item.added" => {
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") && self.started.insert(index) {
                    self.tool_blocks.insert(index);
                    out.push(sse("content_block_start", json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":event.pointer("/item/call_id").and_then(Value::as_str).unwrap_or_default(),"name":event.pointer("/item/name").and_then(Value::as_str).unwrap_or_default(),"input":{}}})));
                }
            }
            "response.content_part.added" if self.started.insert(index) => out.push(sse("content_block_start", json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}))),
            "response.output_text.delta" => {
                if self.started.insert(index) { out.push(sse("content_block_start", json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}))); }
                out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":event.get("delta").and_then(Value::as_str).unwrap_or_default()}})));
            }
            "response.function_call_arguments.delta" => out.push(sse("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":event.get("delta").and_then(Value::as_str).unwrap_or_default()}}))),
            "response.content_part.done" | "response.output_item.done" if self.started.contains(&index) && self.stopped.insert(index) => out.push(sse("content_block_stop", json!({"type":"content_block_stop","index":index}))),
            "response.completed" => {
                for open in self.started.clone() { if self.stopped.insert(open) { out.push(sse("content_block_stop", json!({"type":"content_block_stop","index":open}))); } }
                let usage = event.pointer("/response/usage").unwrap_or(&Value::Null);
                let stop = if self.tool_blocks.is_empty() { "end_turn" } else { "tool_use" };
                out.push(sse("message_delta", json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":{"input_tokens":usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),"output_tokens":usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0)}})));
                out.push(sse("message_stop", json!({"type":"message_stop"})));
            }
            "error" | "response.failed" => out.push(sse("error", json!({"type":"error","error":{"type":"api_error","message":event.pointer("/error/message").or_else(|| event.pointer("/response/error/message")).and_then(Value::as_str).unwrap_or("upstream response failed")}}))),
            _ => {}
        }
        out
    }
}

fn sse(name: &str, payload: Value) -> String {
    format!("event: {name}\ndata: {payload}\n\n")
}

#[cfg(test)]
mod tests {
    use super::translate_request;
    use serde_json::json;
    #[test]
    fn translates_tool_round_trip_shape() {
        let request = json!({"model":"gpt-5.6-sol","stream":true,"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"shell","input":{"cmd":"pwd"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"ok"}]}],"tools":[{"name":"shell","description":"run","input_schema":{"type":"object"}}]});
        let output = translate_request(request).unwrap();
        assert_eq!(
            output.pointer("/input/0/type").and_then(|v| v.as_str()),
            Some("function_call")
        );
        assert_eq!(
            output.pointer("/input/1/type").and_then(|v| v.as_str()),
            Some("function_call_output")
        );
    }
}
