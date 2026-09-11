use std::{collections::HashSet, sync::OnceLock};

use async_stream::stream;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, Method},
    response::{IntoResponse, Response},
};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use tiktoken_rs::CoreBPE;

use crate::{AppState, error::AppError, proxy};

const MAX_SSE_LINE: usize = 4 * 1024 * 1024;
static CLAUDE_TOKENIZER: OnceLock<Result<CoreBPE, String>> = OnceLock::new();

pub async fn count_tokens(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, AppError> {
    let bytes = axum::body::to_bytes(request.into_body(), state.config.max_body_bytes)
        .await
        .map_err(|_| AppError::bad_request("request body exceeds limit"))?;
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|_| AppError::bad_request("invalid JSON body"))?;
    if !payload.is_object() {
        return Err(AppError::bad_request("JSON body must be an object"));
    }
    let count = tokio::task::spawn_blocking(move || count_claude_input_tokens(&payload))
        .await
        .map_err(|_| AppError::bad_gateway("token counting task failed"))??;
    Ok(axum::Json(json!({"input_tokens": count})).into_response())
}

fn count_claude_input_tokens(payload: &Value) -> Result<usize, AppError> {
    let tokenizer = CLAUDE_TOKENIZER.get_or_init(|| {
        tiktoken_rs::o200k_base().map_err(|error| format!("initialize tokenizer: {error}"))
    });
    let tokenizer = tokenizer
        .as_ref()
        .map_err(|error| AppError::bad_gateway(error.clone()))?;
    let segments = collect_claude_input_token_segments(payload);
    if segments.is_empty() {
        return Ok(0);
    }
    Ok(tokenizer.encode_ordinary(&segments.join("\n")).len())
}

fn collect_claude_input_token_segments(payload: &Value) -> Vec<String> {
    let mut segments = Vec::with_capacity(32);
    collect_system_segments(payload.get("system"), &mut segments);
    collect_message_segments(payload.get("messages"), &mut segments);
    collect_tool_segments(payload.get("tools"), &mut segments);
    collect_tool_choice_segments(payload.get("tool_choice"), &mut segments);
    segments
}

fn collect_system_segments(value: Option<&Value>, segments: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => push_string(segments, text),
        Some(Value::Array(parts)) => {
            for part in parts {
                match part {
                    Value::String(text) => push_string(segments, text),
                    Value::Object(object)
                        if object.get("type").and_then(Value::as_str) == Some("text") =>
                    {
                        push_value_string(segments, object.get("text"));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn collect_message_segments(value: Option<&Value>, segments: &mut Vec<String>) {
    let Some(messages) = value.and_then(Value::as_array) else {
        return;
    };
    for message in messages {
        push_value_string(segments, message.get("role"));
        collect_content_segments(message.get("content"), segments);
    }
}

fn collect_content_segments(value: Option<&Value>, segments: &mut Vec<String>) {
    let Some(value) = value else { return };
    match value {
        Value::String(text) => push_string(segments, text),
        Value::Array(parts) => {
            for part in parts {
                collect_content_segments(Some(part), segments);
            }
        }
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("text") => push_value_string(segments, object.get("text")),
            Some("thinking") => push_value_string(segments, object.get("thinking")),
            Some("document") => collect_document_segments(object, segments),
            Some("tool_use" | "server_tool_use" | "mcp_tool_use") => {
                push_value_string(segments, object.get("id"));
                push_value_string(segments, object.get("name"));
                push_json(segments, object.get("input"));
            }
            Some(
                "tool_result"
                | "mcp_tool_result"
                | "web_search_tool_result"
                | "web_fetch_tool_result"
                | "code_execution_tool_result"
                | "bash_code_execution_tool_result"
                | "text_editor_code_execution_tool_result",
            ) => {
                push_value_string(segments, object.get("tool_use_id"));
                push_value_string(segments, object.get("tool_call_id"));
                collect_content_segments(object.get("content"), segments);
            }
            Some("web_search_result" | "search_result") => {
                push_value_string(segments, object.get("source"));
                for key in ["title", "url", "page_age"] {
                    push_value_string(segments, object.get(key));
                }
                collect_content_segments(object.get("content"), segments);
            }
            Some("web_fetch_result") => {
                push_value_string(segments, object.get("url"));
                push_value_string(segments, object.get("retrieved_at"));
                collect_content_segments(object.get("content"), segments);
            }
            Some(
                "code_execution_result"
                | "bash_code_execution_result"
                | "text_editor_code_execution_result",
            ) => {
                for key in ["stdout", "stderr", "return_code"] {
                    push_value_string(segments, object.get(key));
                }
                collect_content_segments(object.get("content"), segments);
                collect_content_segments(object.get("output"), segments);
            }
            Some("tool_reference") => push_value_string(segments, object.get("tool_name")),
            Some("image" | "input_audio" | "audio" | "video" | "redacted_thinking") => {}
            None => push_json(segments, Some(value)),
            _ => push_value_string(segments, object.get("text")),
        },
        _ => {}
    }
}

fn collect_document_segments(object: &Map<String, Value>, segments: &mut Vec<String>) {
    let Some(source) = object.get("source").and_then(Value::as_object) else {
        return;
    };
    if source.get("type").and_then(Value::as_str) != Some("text") {
        return;
    }
    push_value_string(segments, object.get("title"));
    push_value_string(segments, object.get("context"));
    push_value_string(segments, source.get("data"));
    push_value_string(segments, source.get("content"));
}

fn collect_tool_segments(value: Option<&Value>, segments: &mut Vec<String>) {
    let Some(tools) = value.and_then(Value::as_array) else {
        return;
    };
    for tool in tools {
        for key in ["type", "name", "description"] {
            push_value_string(segments, tool.get(key));
        }
        push_json(segments, tool.get("input_schema"));
    }
}

fn collect_tool_choice_segments(value: Option<&Value>, segments: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => push_string(segments, text),
        Some(value) => {
            push_value_string(segments, value.get("type"));
            push_value_string(segments, value.get("name"));
        }
        None => {}
    }
}

fn push_value_string(segments: &mut Vec<String>, value: Option<&Value>) {
    if let Some(text) = value.and_then(Value::as_str) {
        push_string(segments, text);
    }
}

fn push_string(segments: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_owned());
    }
}

fn push_json(segments: &mut Vec<String>, value: Option<&Value>) {
    let Some(value) = value else { return };
    match value {
        Value::String(text) => push_string(segments, text),
        value => {
            if let Ok(encoded) = serde_json::to_string(value) {
                push_string(segments, &encoded);
            }
        }
    }
}

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
    let stream = payload.get("stream").and_then(Value::as_bool) != Some(false);
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
    if !stream {
        return translate_non_stream(upstream, model, state.config.max_body_bytes).await;
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

async fn translate_non_stream(
    upstream: reqwest::Response,
    requested_model: String,
    max_bytes: usize,
) -> Result<Response, AppError> {
    let mut input = upstream.bytes_stream();
    let mut buffer = BytesMut::new();
    let mut received = 0_usize;
    let mut completed = None;
    let mut upstream_error = None;

    while let Some(chunk) = input.next().await {
        let chunk = chunk.map_err(|_| AppError::bad_gateway("upstream stream failed"))?;
        received = received
            .checked_add(chunk.len())
            .ok_or_else(|| AppError::bad_gateway("upstream response exceeds limit"))?;
        if received > max_bytes {
            return Err(AppError::bad_gateway("upstream response exceeds limit"));
        }
        buffer.extend_from_slice(&chunk);
        consume_sse_lines(&mut buffer, |event| {
            match event.get("type").and_then(Value::as_str) {
                Some("response.completed") => completed = event.get("response").cloned(),
                Some("error") | Some("response.failed") => {
                    upstream_error = Some(
                        event
                            .pointer("/error/message")
                            .or_else(|| event.pointer("/response/error/message"))
                            .and_then(Value::as_str)
                            .unwrap_or("upstream response failed")
                            .to_owned(),
                    )
                }
                _ => {}
            }
        })?;
    }
    if !buffer.is_empty() {
        buffer.extend_from_slice(b"\n");
        consume_sse_lines(&mut buffer, |event| {
            if event.get("type").and_then(Value::as_str) == Some("response.completed") {
                completed = event.get("response").cloned();
            }
        })?;
    }
    if let Some(message) = upstream_error {
        return Err(AppError::bad_gateway(message));
    }
    let completed = completed
        .ok_or_else(|| AppError::bad_gateway("upstream response ended before completion"))?;
    Ok(axum::Json(translate_completed_response(&completed, &requested_model)?).into_response())
}

fn consume_sse_lines(
    buffer: &mut BytesMut,
    mut consume: impl FnMut(Value),
) -> Result<(), AppError> {
    if buffer.len() > MAX_SSE_LINE && !buffer.contains(&b'\n') {
        return Err(AppError::bad_gateway("upstream SSE event exceeds limit"));
    }
    while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
        let line = buffer.split_to(position + 1);
        let line = line.strip_suffix(b"\n").unwrap_or(&line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(data) = line.strip_prefix(b"data:") else {
            continue;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if data == b"[DONE]" {
            continue;
        }
        let event = serde_json::from_slice(data)
            .map_err(|_| AppError::bad_gateway("invalid upstream SSE event"))?;
        consume(event);
    }
    Ok(())
}

fn translate_completed_response(
    response: &Value,
    requested_model: &str,
) -> Result<Value, AppError> {
    let mut content = Vec::new();
    let mut used_tool = false;
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if part.get("type").and_then(Value::as_str) == Some("output_text") {
                        content.push(json!({
                            "type": "text",
                            "text": part.get("text").and_then(Value::as_str).unwrap_or_default()
                        }));
                    }
                }
            }
            Some("function_call") => {
                used_tool = true;
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let input = serde_json::from_str::<Value>(arguments).map_err(|_| {
                    AppError::bad_gateway("upstream returned invalid tool arguments")
                })?;
                content.push(json!({
                    "type": "tool_use",
                    "id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                    "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "input": input
                }));
            }
            _ => {}
        }
    }
    let usage = response.get("usage").unwrap_or(&Value::Null);
    Ok(json!({
        "id": response.get("id").and_then(Value::as_str).unwrap_or("msg_rust"),
        "type": "message",
        "role": "assistant",
        "model": requested_model,
        "content": content,
        "stop_reason": if used_tool { "tool_use" } else { "end_turn" },
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
            "output_tokens": usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0)
        }
    }))
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
    use super::{
        collect_claude_input_token_segments, count_claude_input_tokens,
        translate_completed_response, translate_request,
    };
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

    #[test]
    fn translates_completed_response_with_text_and_tool_use() {
        let response = json!({
            "id": "resp_123",
            "output": [
                {"type":"message","content":[{"type":"output_text","text":"hello"}]},
                {"type":"function_call","call_id":"call_1","name":"shell","arguments":"{\"cmd\":\"pwd\"}"}
            ],
            "usage": {"input_tokens": 12, "output_tokens": 7}
        });
        let translated = translate_completed_response(&response, "gpt-test").unwrap();
        assert_eq!(translated["id"], "resp_123");
        assert_eq!(translated["model"], "gpt-test");
        assert_eq!(translated["stop_reason"], "tool_use");
        assert_eq!(translated["content"][0]["text"], "hello");
        assert_eq!(translated["content"][1]["input"]["cmd"], "pwd");
        assert_eq!(translated["usage"]["output_tokens"], 7);
    }

    #[test]
    fn token_segments_match_pinned_go_fixture() {
        let payload = json!({
            "model":"claude-test",
            "system":[
                {"type":"text","text":"Follow repository rules.","cache_control":{"type":"ephemeral"}},
                {"type":"image","source":{"type":"base64","data":"ignored-system-image"}}
            ],
            "messages":[
                {"role":"user","content":[
                    {"type":"text","text":"Review the implementation."},
                    {"type":"document","source":{"type":"text","data":"Reference document text."}},
                    {"type":"image","source":{"type":"base64","data":"ignored-image"}}
                ]},
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"Inspect the relevant files.","signature":"ignored-signature"},
                    {"type":"tool_use","id":"toolu_1","name":"read_file","input":{"path":"main.go"}}
                ]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[
                    {"type":"text","text":"package main"},
                    {"type":"image","source":{"type":"base64","data":"ignored-tool-image"}}
                ]}]}
            ],
            "tools":[{"name":"read_file","description":"Reads a repository file.","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}],
            "tool_choice":{"type":"tool","name":"read_file"},
            "metadata":{"user_id":"ignored-metadata"},
            "max_tokens":4096,
            "stream":true
        });
        assert_eq!(
            collect_claude_input_token_segments(&payload),
            vec![
                "Follow repository rules.",
                "user",
                "Review the implementation.",
                "Reference document text.",
                "assistant",
                "Inspect the relevant files.",
                "toolu_1",
                "read_file",
                "{\"path\":\"main.go\"}",
                "user",
                "toolu_1",
                "package main",
                "read_file",
                "Reads a repository file.",
                "{\"type\":\"object\",\"properties\":{\"path\":{\"type\":\"string\"}}}",
                "tool",
                "read_file"
            ]
        );
    }

    #[test]
    fn token_count_excludes_multimedia_and_control_fields() {
        let base = json!({
            "system":"System text.",
            "messages":[{"role":"user","content":[{"type":"text","text":"User text."}]}],
            "tools":[{"name":"lookup","description":"Looks up data.","input_schema":{"type":"object"}}]
        });
        let extended = json!({
            "model":"claude-test",
            "system":"System text.",
            "messages":[{"role":"user","content":[
                {"type":"text","text":"User text."},
                {"type":"image","source":{"type":"base64","data":"very-large-image-data"}},
                {"type":"input_audio","source":{"type":"base64","data":"very-large-audio-data"}},
                {"type":"video","source":{"type":"url","url":"https://example.com/video.mp4"}},
                {"type":"document","source":{"type":"base64","data":"very-large-pdf-data"}}
            ]}],
            "tools":[{"name":"lookup","description":"Looks up data.","input_schema":{"type":"object"},"cache_control":{"type":"ephemeral"}}],
            "metadata":{"large_wrapper":"ignored"},
            "max_tokens":8192,
            "temperature":0.8,
            "thinking":{"type":"enabled","budget_tokens":4096},
            "stream":true
        });
        assert_eq!(
            count_claude_input_tokens(&base).unwrap(),
            count_claude_input_tokens(&extended).unwrap()
        );
    }
}
