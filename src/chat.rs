use std::collections::{HashMap, HashSet};

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
use sha2::{Digest, Sha256};

use crate::{AppState, error::AppError, proxy};

const MAX_SSE_LINE: usize = 4 * 1024 * 1024;
const TOOL_NAME_LIMIT: usize = 64;

pub async fn chat_completions(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, AppError> {
    serve(state, request, false).await
}

pub async fn completions(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, AppError> {
    let (parts, body) = request.into_parts();
    let max = state.config().max_body_bytes;
    let bytes = axum::body::to_bytes(body, max)
        .await
        .map_err(|_| AppError::bad_request("request body exceeds limit"))?;
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|_| AppError::bad_request("invalid JSON body"))?;
    let chat = legacy_to_chat(&payload);
    let rewritten =
        serde_json::to_vec(&chat).map_err(|_| AppError::bad_request("invalid request"))?;
    serve(
        state,
        Request::from_parts(parts, Body::from(rewritten)),
        true,
    )
    .await
}

async fn serve(state: AppState, request: Request, legacy: bool) -> Result<Response, AppError> {
    let (parts, body) = request.into_parts();
    let max = state.config().max_body_bytes;
    let bytes = axum::body::to_bytes(body, max)
        .await
        .map_err(|_| AppError::bad_request("request body exceeds limit"))?;
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|_| AppError::bad_request("invalid JSON body"))?;
    if !payload.is_object() {
        return Err(AppError::bad_request("JSON body must be an object"));
    }
    let stream = payload.get("stream").and_then(Value::as_bool) == Some(true);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !legacy && state.provider_for(&model) == crate::Provider::Copilot {
        let body = Bytes::from(
            serde_json::to_vec(&payload).map_err(|_| AppError::bad_request("invalid request"))?,
        );
        let upstream =
            proxy::execute_copilot(&state, &Method::POST, body, "chat/completions").await?;
        if !upstream.status().is_success() {
            let status = upstream.status();
            return Err(AppError::new(
                status,
                format!("Copilot upstream returned HTTP {status}"),
            ));
        }
        return proxy::to_axum(upstream);
    }
    // The Codex backend always streams; non-streaming clients are served by
    // aggregating the terminal `response.completed` event locally.
    let translated = convert_openai_request_to_codex(&model, &payload, true);
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
    if stream {
        let body = translate_stream(upstream, payload, legacy);
        let mut response = Response::new(body);
        response.headers_mut().insert(
            "content-type",
            HeaderValue::from_static("text/event-stream"),
        );
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-cache"));
        Ok(response)
    } else {
        translate_non_stream(upstream, &payload, max, legacy).await
    }
}

fn legacy_to_chat(payload: &Value) -> Value {
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("Complete this:");
    let mut out = json!({"model":"","messages":[{"role":"user","content":""}]});
    if let Some(model) = payload.get("model") {
        out["model"] = model.clone();
    }
    out["messages"][0]["content"] = Value::String(prompt.to_owned());
    for key in [
        "max_tokens",
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
        "stop",
        "stream",
        "logprobs",
        "top_logprobs",
        "echo",
    ] {
        if let Some(value) = payload.get(key) {
            out[key] = value.clone();
        }
    }
    out
}

fn pointer_string(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn collect_request_tool_names(payload: &Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let add = |name: String, names: &mut Vec<String>, seen: &mut HashSet<String>| {
        if name.is_empty() || !seen.insert(name.clone()) {
            return;
        }
        names.push(name);
    };
    if let Some(tools) = payload.get("tools").and_then(Value::as_array) {
        for tool in tools {
            match tool.get("type").and_then(Value::as_str) {
                Some("function") => add(
                    pointer_string(tool, "/function/name"),
                    &mut names,
                    &mut seen,
                ),
                Some("custom") => add(pointer_string(tool, "/name"), &mut names, &mut seen),
                _ => {}
            }
        }
    }
    if let Some(choice) = payload.get("tool_choice").filter(|v| v.is_object()) {
        match choice
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "function" => {
                let mut name = pointer_string(choice, "/function/name");
                if name.is_empty() {
                    name = pointer_string(choice, "/name");
                }
                add(name, &mut names, &mut seen);
            }
            "custom" => add(pointer_string(choice, "/name"), &mut names, &mut seen),
            _ => {}
        }
    }
    if let Some(messages) = payload.get("messages").and_then(Value::as_array) {
        for message in messages {
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let name = match call.get("function").and_then(|f| f.get("name")) {
                        Some(name) => name.as_str().unwrap_or_default().to_owned(),
                        None => pointer_string(call, "/custom/name"),
                    };
                    add(name, &mut names, &mut seen);
                }
            }
        }
    }
    names
}

fn sanitize_tool_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn shorten_name_if_needed(name: &str) -> String {
    let sanitized = sanitize_tool_name(name);
    if sanitized.len() <= TOOL_NAME_LIMIT {
        return sanitized;
    }
    if sanitized.starts_with("mcp__")
        && let Some(index) = sanitized.rfind("__")
        && index > 0
    {
        let candidate = format!("mcp__{}", &sanitized[index + 2..]);
        return candidate.chars().take(TOOL_NAME_LIMIT).collect();
    }
    sanitized.chars().take(TOOL_NAME_LIMIT).collect()
}

fn build_short_name_map(names: &[String]) -> HashMap<String, String> {
    let mut used: HashSet<String> = HashSet::new();
    let mut map = HashMap::new();
    for name in names {
        let base = shorten_name_if_needed(name);
        let mut unique = base.clone();
        let mut suffix = 1;
        while !used.insert(unique.clone()) {
            let mark = format!("_{suffix}");
            let allowed = TOOL_NAME_LIMIT.saturating_sub(mark.len());
            let mut trimmed: String = base.chars().take(allowed).collect();
            trimmed.push_str(&mark);
            unique = trimmed;
            suffix += 1;
        }
        map.insert(name.clone(), unique);
    }
    map
}

fn mapped_tool_name(short_names: &HashMap<String, String>, name: &str) -> String {
    short_names
        .get(name)
        .cloned()
        .unwrap_or_else(|| shorten_name_if_needed(name))
}

pub(crate) fn build_reverse_map_from_original(payload: &Value) -> HashMap<String, String> {
    let names = collect_request_tool_names(payload);
    let mut reverse = HashMap::new();
    if !names.is_empty() {
        for (original, short) in build_short_name_map(&names) {
            reverse.insert(short, original);
        }
    }
    reverse
}

struct PendingToolCall {
    call_id: String,
    source_call_id: String,
    call_type: String,
    consumed: bool,
}

fn resolve_tool_call(
    tool_call: &Value,
    custom_tool_names: &HashSet<String>,
) -> Option<(String, String, String)> {
    match tool_call.get("type").and_then(Value::as_str) {
        Some("custom") => Some((
            "custom".to_owned(),
            pointer_string(tool_call, "/custom/name"),
            pointer_string(tool_call, "/custom/input"),
        )),
        Some("function") => {
            let name = pointer_string(tool_call, "/function/name");
            let call_type = if custom_tool_names.contains(&name) {
                "custom"
            } else {
                "function"
            };
            Some((
                call_type.to_owned(),
                name,
                pointer_string(tool_call, "/function/arguments"),
            ))
        }
        _ => None,
    }
}

pub(crate) fn convert_openai_request_to_codex(
    model_name: &str,
    input: &Value,
    stream: bool,
) -> Value {
    let mut out = Map::new();
    out.insert("instructions".into(), Value::String(String::new()));
    out.insert("stream".into(), Value::Bool(stream));
    let effort = input
        .get("reasoning_effort")
        .cloned()
        .unwrap_or_else(|| Value::String("medium".into()));
    out.insert("reasoning".into(), json!({"effort": effort}));
    out.insert("parallel_tool_calls".into(), Value::Bool(true));
    out.insert("include".into(), json!(["reasoning.encrypted_content"]));
    out.insert("model".into(), Value::String(model_name.to_owned()));

    let tools: Vec<Value> = input
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut custom_tool_names: HashSet<String> = HashSet::new();
    let mut function_tool_names: HashSet<String> = HashSet::new();
    for tool in &tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => {
                function_tool_names.insert(pointer_string(tool, "/function/name"));
            }
            Some("custom") => {
                custom_tool_names.insert(pointer_string(tool, "/name"));
            }
            _ => {}
        }
    }
    for name in &function_tool_names {
        custom_tool_names.remove(name);
    }
    let all_names = collect_request_tool_names(input);
    let short_names = build_short_name_map(&all_names);

    let mut input_items: Vec<Value> = Vec::new();
    let mut pending: Vec<PendingToolCall> = Vec::new();
    let mut ambiguous: HashSet<String> = HashSet::new();

    if let Some(messages) = input.get("messages").and_then(Value::as_array) {
        for (i, message) in messages.iter().enumerate() {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default();

            if role == "tool" {
                let raw_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !raw_id.is_empty() && ambiguous.contains(&raw_id) {
                    continue;
                }
                let found = pending.iter().position(|call| {
                    !call.consumed
                        && (raw_id.is_empty()
                            || call.source_call_id == raw_id
                            || call.call_id == raw_id)
                });
                let Some(index) = found else { continue };
                pending[index].consumed = true;
                let call_id = pending[index].call_id.clone();
                let output_type = if pending[index].call_type == "custom" {
                    "custom_tool_call_output"
                } else {
                    "function_call_output"
                };
                let mut item = json!({"type": output_type, "call_id": call_id});
                set_tool_call_output_content(&mut item, message.get("content"));
                input_items.push(item);
                continue;
            }

            // A new conversational message starts a new tool-call batch.
            pending.clear();
            ambiguous.clear();

            let out_role = if role == "system" { "developer" } else { role };
            let mut content_items: Vec<Value> = Vec::new();
            let content = message.get("content");
            if let Some(text) = content.and_then(Value::as_str) {
                if !text.is_empty() {
                    let part_type = if role == "assistant" {
                        "output_text"
                    } else {
                        "input_text"
                    };
                    content_items.push(json!({"type": part_type, "text": text}));
                }
            } else if let Some(items) = content.and_then(Value::as_array) {
                for item in items {
                    match item.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let part_type = if role == "assistant" {
                                "output_text"
                            } else {
                                "input_text"
                            };
                            content_items.push(json!({
                                "type": part_type,
                                "text": item.get("text").and_then(Value::as_str).unwrap_or_default()
                            }));
                        }
                        Some("image_url") if role == "user" => {
                            let mut part = json!({"type": "input_image"});
                            if let Some(url) = item.pointer("/image_url/url") {
                                part["image_url"] = url.clone();
                            }
                            content_items.push(part);
                        }
                        Some("file") if role == "user" => {
                            let file_data = pointer_string(item, "/file/file_data");
                            if !file_data.is_empty() {
                                let filename = pointer_string(item, "/file/filename");
                                let mut part =
                                    json!({"type": "input_file", "file_data": file_data});
                                if !filename.is_empty() {
                                    part["filename"] = Value::String(filename);
                                }
                                content_items.push(part);
                            }
                        }
                        Some("input_audio") if role == "user" => {
                            let data = pointer_string(item, "/input_audio/data");
                            if !data.is_empty() {
                                let format = pointer_string(item, "/input_audio/format");
                                let mut part = json!({"type": "input_audio", "data": data});
                                if !format.is_empty() {
                                    part["format"] = Value::String(format);
                                }
                                content_items.push(part);
                            }
                        }
                        _ => {}
                    }
                }
            }

            if role != "assistant" || !content_items.is_empty() {
                input_items.push(json!({
                    "type": "message",
                    "role": out_role,
                    "content": Value::Array(content_items)
                }));
            }

            if role == "assistant" {
                let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
                    continue;
                };
                let mut call_id_counts: HashMap<String, u32> = HashMap::new();
                let mut used_call_ids: HashSet<String> = HashSet::new();
                for tool_call in tool_calls {
                    if resolve_tool_call(tool_call, &custom_tool_names).is_some() {
                        let call_id = tool_call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        if !call_id.is_empty() {
                            *call_id_counts.entry(call_id.clone()).or_insert(0) += 1;
                            used_call_ids.insert(call_id);
                        }
                    }
                }
                for (call_id, count) in &call_id_counts {
                    if *count > 1 {
                        ambiguous.insert(call_id.clone());
                    }
                }

                for (j, tool_call) in tool_calls.iter().enumerate() {
                    let Some((tool_type, tool_name, tool_input)) =
                        resolve_tool_call(tool_call, &custom_tool_names)
                    else {
                        continue;
                    };
                    let source_call_id = tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if !source_call_id.is_empty() && ambiguous.contains(&source_call_id) {
                        continue;
                    }
                    let call_id = if source_call_id.is_empty() {
                        let base = format!("call_missing_{i}_{j}");
                        let mut candidate = base.clone();
                        let mut suffix = 1;
                        while used_call_ids.contains(&candidate) {
                            candidate = format!("{base}_{suffix}");
                            suffix += 1;
                        }
                        used_call_ids.insert(candidate.clone());
                        candidate
                    } else {
                        source_call_id.clone()
                    };
                    pending.push(PendingToolCall {
                        call_id: call_id.clone(),
                        source_call_id,
                        call_type: tool_type.clone(),
                        consumed: false,
                    });
                    let name = mapped_tool_name(&short_names, &tool_name);
                    match tool_type.as_str() {
                        "function" => input_items.push(json!({
                            "type": "function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": tool_input
                        })),
                        "custom" => input_items.push(json!({
                            "type": "custom_tool_call",
                            "call_id": call_id,
                            "name": name,
                            "input": tool_input
                        })),
                        _ => {}
                    }
                }
            }
        }
    }
    out.insert("input".into(), Value::Array(input_items));

    let response_format = input.get("response_format");
    let text = input.get("text");
    if let Some(response_format) = response_format {
        if !out.contains_key("text") {
            out.insert("text".into(), json!({}));
        }
        match response_format.get("type").and_then(Value::as_str) {
            Some("text") => {
                set_nested(&mut out, &["text", "format", "type"], json!("text"));
            }
            Some("json_schema") => {
                if let Some(schema) = response_format.get("json_schema") {
                    set_nested(&mut out, &["text", "format", "type"], json!("json_schema"));
                    if let Some(value) = schema.get("name") {
                        set_nested(&mut out, &["text", "format", "name"], value.clone());
                    }
                    if let Some(value) = schema.get("strict") {
                        set_nested(&mut out, &["text", "format", "strict"], value.clone());
                    }
                    if let Some(value) = schema.get("schema") {
                        set_nested(&mut out, &["text", "format", "schema"], value.clone());
                    }
                }
            }
            _ => {}
        }
        if let Some(text) = text
            && let Some(verbosity) = text.get("verbosity")
        {
            set_nested(&mut out, &["text", "verbosity"], verbosity.clone());
        }
    } else if let Some(text) = text
        && let Some(verbosity) = text.get("verbosity")
    {
        if !out.contains_key("text") {
            out.insert("text".into(), json!({}));
        }
        set_nested(&mut out, &["text", "verbosity"], verbosity.clone());
    }

    if !tools.is_empty() {
        let mut tool_items: Vec<Value> = Vec::with_capacity(tools.len());
        for tool in &tools {
            let tool_type = tool.get("type").and_then(Value::as_str).unwrap_or_default();
            if tool_type == "custom" {
                let mut item = tool.clone();
                let name = pointer_string(tool, "/name");
                item["name"] = Value::String(mapped_tool_name(&short_names, &name));
                tool_items.push(item);
                continue;
            }
            if !tool_type.is_empty() && tool_type != "function" && tool.is_object() {
                tool_items.push(tool.clone());
                continue;
            }
            if tool_type == "function" {
                let mut item = Map::new();
                item.insert("type".into(), json!("function"));
                if let Some(function) = tool.get("function") {
                    if let Some(value) = function.get("name") {
                        let name = value.as_str().unwrap_or_default();
                        item.insert(
                            "name".into(),
                            Value::String(mapped_tool_name(&short_names, name)),
                        );
                    }
                    if let Some(value) = function.get("description") {
                        item.insert("description".into(), value.clone());
                    }
                    if let Some(value) = function.get("parameters") {
                        item.insert("parameters".into(), value.clone());
                    }
                    item.insert(
                        "strict".into(),
                        function
                            .get("strict")
                            .cloned()
                            .unwrap_or(Value::Bool(false)),
                    );
                }
                tool_items.push(Value::Object(item));
            }
        }
        out.insert("tools".into(), Value::Array(tool_items));
    }

    if let Some(tool_choice) = input.get("tool_choice") {
        match tool_choice {
            Value::String(value) => {
                out.insert("tool_choice".into(), Value::String(value.clone()));
            }
            value if value.is_object() => {
                let mut choice_type = value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if choice_type == "function" || choice_type == "custom" {
                    let mut name = if choice_type == "function" {
                        pointer_string(value, "/function/name")
                    } else {
                        pointer_string(value, "/name")
                    };
                    if choice_type == "function" && custom_tool_names.contains(&name) {
                        choice_type = "custom".to_owned();
                    }
                    if !name.is_empty() {
                        name = mapped_tool_name(&short_names, &name);
                    }
                    let mut choice = Map::new();
                    choice.insert("type".into(), Value::String(choice_type));
                    if !name.is_empty() {
                        choice.insert("name".into(), Value::String(name));
                    }
                    out.insert("tool_choice".into(), Value::Object(choice));
                } else if !choice_type.is_empty() {
                    out.insert("tool_choice".into(), value.clone());
                }
            }
            _ => {}
        }
    }

    out.insert("store".into(), Value::Bool(false));
    Value::Object(out)
}

fn set_nested(root: &mut Map<String, Value>, path: &[&str], value: Value) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        root.insert(path[0].to_owned(), value);
        return;
    }
    let entry = root.entry(path[0].to_owned()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    set_nested(entry.as_object_mut().unwrap(), &path[1..], value);
}

fn set_tool_call_output_content(item: &mut Value, content: Option<&Value>) {
    match content {
        Some(Value::String(raw)) => {
            if let Ok(structured) = serde_json::from_str::<Value>(raw)
                && has_tool_output_image_part(&structured)
            {
                set_tool_call_output_content(item, Some(&structured));
                return;
            }
            item["output"] = Value::String(raw.clone());
        }
        Some(Value::Array(parts)) => {
            let output: Vec<Value> = parts.iter().map(tool_output_content_part).collect();
            item["output"] = Value::Array(output);
        }
        Some(other) => {
            let raw = other.to_string();
            let fallback = if raw.is_empty() {
                other.as_str().unwrap_or_default().to_owned()
            } else {
                raw
            };
            item["output"] = Value::String(fallback);
        }
        None => {
            item["output"] = Value::String(String::new());
        }
    }
}

fn tool_output_content_part(item: &Value) -> Value {
    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        "text" | "input_text" | "output_text" => json!({
            "type": "input_text",
            "text": item.get("text").and_then(Value::as_str).unwrap_or_default()
        }),
        "image_url" | "input_image" => {
            let is_input_image = item.get("type").and_then(Value::as_str) == Some("input_image");
            let image_url = if is_input_image {
                item.get("image_url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            } else {
                pointer_string(item, "/image_url/url")
            };
            let file_id = if is_input_image {
                pointer_string(item, "/file_id")
            } else {
                pointer_string(item, "/image_url/file_id")
            };
            if image_url.is_empty() && file_id.is_empty() {
                return tool_output_fallback_part(item);
            }
            let mut part = json!({"type": "input_image"});
            if !image_url.is_empty() {
                part["image_url"] = Value::String(image_url);
            }
            if !file_id.is_empty() {
                part["file_id"] = Value::String(file_id);
            }
            let detail = if is_input_image {
                pointer_string(item, "/detail")
            } else {
                pointer_string(item, "/image_url/detail")
            };
            if !detail.is_empty() {
                part["detail"] = Value::String(detail);
            }
            part
        }
        "file" => {
            let file_id = pointer_string(item, "/file/file_id");
            let file_data = pointer_string(item, "/file/file_data");
            let file_url = pointer_string(item, "/file/file_url");
            if file_id.is_empty() && file_data.is_empty() && file_url.is_empty() {
                return tool_output_fallback_part(item);
            }
            let mut part = json!({"type": "input_file"});
            if !file_id.is_empty() {
                part["file_id"] = Value::String(file_id);
            }
            if !file_data.is_empty() {
                part["file_data"] = Value::String(file_data);
            }
            if !file_url.is_empty() {
                part["file_url"] = Value::String(file_url);
            }
            let filename = pointer_string(item, "/file/filename");
            if !filename.is_empty() {
                part["filename"] = Value::String(filename);
            }
            part
        }
        _ => tool_output_fallback_part(item),
    }
}

fn has_tool_output_image_part(content: &Value) -> bool {
    let Some(items) = content.as_array() else {
        return false;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("image_url") => {
                if !pointer_string(item, "/image_url/url").is_empty()
                    || !pointer_string(item, "/image_url/file_id").is_empty()
                {
                    return true;
                }
            }
            Some("input_image") => {
                if !pointer_string(item, "/image_url").is_empty()
                    || !pointer_string(item, "/file_id").is_empty()
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn tool_output_fallback_part(item: &Value) -> Value {
    let raw = item.to_string();
    let text = if raw.is_empty() {
        item.as_str().unwrap_or_default().to_owned()
    } else {
        raw
    };
    json!({"type": "input_text", "text": text})
}

fn translate_stream(upstream: proxy::UpstreamResponse, original: Value, legacy: bool) -> Body {
    let mut input = upstream.bytes_stream();
    let output = stream! {
        let mut buffer = BytesMut::new();
        let mut state = StreamState::new(original);
        while let Some(chunk) = input.next().await {
            let chunk = match chunk {
                Ok(value) => value,
                Err(error) => {
                    yield Err(error);
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_SSE_LINE && !buffer.contains(&b'\n') {
                yield Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "upstream SSE event exceeds limit",
                ));
                break;
            }
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.split_to(position + 1);
                let line = line.strip_suffix(b"\n").unwrap_or(&line);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let Some(data) = line.strip_prefix(b"data:") else { continue; };
                let data = data.strip_prefix(b" ").unwrap_or(data);
                if data == b"[DONE]" { continue; }
                if let Ok(event) = serde_json::from_slice::<Value>(data) {
                    for chunk in state.event(&event) {
                        let converted = if legacy {
                            chat_chunk_to_completions(&chunk)
                        } else {
                            Some(chunk)
                        };
                        if let Some(converted) = converted {
                            let text = serde_json::to_string(&converted).unwrap_or_default();
                            yield Ok(Bytes::from(format!("data: {text}\n\n")));
                        }
                    }
                }
            }
        }
        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
    };
    Body::from_stream(output)
}

async fn translate_non_stream(
    upstream: proxy::UpstreamResponse,
    original: &Value,
    max_bytes: usize,
    legacy: bool,
) -> Result<Response, AppError> {
    let mut input = upstream.bytes_stream();
    let mut buffer = BytesMut::new();
    let mut received = 0_usize;
    let mut completed: Option<Value> = None;
    let mut upstream_error: Option<String> = None;

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
            absorb_terminal_event(&event, &mut completed, &mut upstream_error);
        })?;
    }
    if !buffer.is_empty() {
        buffer.extend_from_slice(b"\n");
        consume_sse_lines(&mut buffer, |event| {
            absorb_terminal_event(&event, &mut completed, &mut upstream_error);
        })?;
    }
    if let Some(message) = upstream_error {
        return Err(AppError::bad_gateway(message));
    }
    let response = completed
        .ok_or_else(|| AppError::bad_gateway("upstream response ended before completion"))?;
    let chat = build_chat_completion(&response, original);
    if legacy {
        Ok(axum::Json(chat_to_completions_response(&chat)).into_response())
    } else {
        Ok(axum::Json(chat).into_response())
    }
}

fn absorb_terminal_event(event: &Value, completed: &mut Option<Value>, error: &mut Option<String>) {
    match event.get("type").and_then(Value::as_str) {
        Some("response.completed") | Some("response.incomplete") => {
            if let Some(response) = event.get("response") {
                *completed = Some(response.clone());
            }
        }
        Some("error") | Some("response.failed") => {
            *error = Some(
                event
                    .pointer("/error/message")
                    .or_else(|| event.pointer("/response/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("upstream response failed")
                    .to_owned(),
            );
        }
        _ => {}
    }
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

fn apply_usage(template: &mut Value, usage: &Value) {
    let mut map = Map::new();
    if let Some(value) = usage.get("output_tokens") {
        map.insert("completion_tokens".into(), value.clone());
    }
    if let Some(value) = usage.get("total_tokens") {
        map.insert("total_tokens".into(), value.clone());
    }
    if let Some(value) = usage.get("input_tokens") {
        map.insert("prompt_tokens".into(), value.clone());
    }
    let mut prompt_details = Map::new();
    if let Some(value) = usage.pointer("/input_tokens_details/cached_tokens") {
        prompt_details.insert("cached_tokens".into(), value.clone());
    }
    if let Some(value) = codex_cache_write_tokens(usage) {
        prompt_details.insert("cache_write_tokens".into(), value.clone());
        prompt_details.insert("cached_creation_tokens".into(), value);
    }
    if !prompt_details.is_empty() {
        map.insert(
            "prompt_tokens_details".into(),
            Value::Object(prompt_details),
        );
    }
    let mut completion_details = Map::new();
    if let Some(value) = usage.pointer("/output_tokens_details/reasoning_tokens") {
        completion_details.insert("reasoning_tokens".into(), value.clone());
    }
    if !completion_details.is_empty() {
        map.insert(
            "completion_tokens_details".into(),
            Value::Object(completion_details),
        );
    }
    if !map.is_empty() {
        template
            .as_object_mut()
            .unwrap()
            .insert("usage".into(), Value::Object(map));
    }
}

fn codex_cache_write_tokens(usage: &Value) -> Option<Value> {
    let value = usage.pointer("/input_tokens_details/cache_write_tokens")?;
    if value.is_null() {
        return None;
    }
    let number = value.as_u64()?;
    Some(Value::Number(number.into()))
}

fn codex_response_service_tier(response: Option<&Value>) -> Option<String> {
    let tier = response?.get("service_tier")?.as_str()?;
    let trimmed = tier.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn codex_tool_call_arguments(item: &Value) -> String {
    if item.get("type").and_then(Value::as_str) == Some("custom_tool_call") {
        item.get("input")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    } else {
        item.get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}

fn is_codex_tool_call_type(item_type: &str) -> bool {
    item_type == "function_call" || item_type == "custom_tool_call"
}

fn mime_type_from_codex_output_format(output_format: &str) -> String {
    if output_format.is_empty() {
        return "image/png".to_owned();
    }
    if output_format.contains('/') {
        return output_format.to_owned();
    }
    match output_format.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/png",
    }
    .to_owned()
}

fn build_chat_completion(response: &Value, original: &Value) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let mut template = json!({
        "id":"",
        "object":"chat.completion",
        "created":123456,
        "model":"model",
        "choices":[{
            "index":0,
            "message":{"role":"assistant","content":null,"reasoning_content":null,"tool_calls":null},
            "finish_reason":null,
            "native_finish_reason":null
        }]
    });

    if let Some(tier) = codex_response_service_tier(Some(response)) {
        template["service_tier"] = Value::String(tier);
    }
    if let Some(model) = response.get("model") {
        template["model"] = model.clone();
    }
    template["created"] = response
        .get("created_at")
        .and_then(Value::as_i64)
        .map(Value::from)
        .unwrap_or_else(|| Value::from(now));
    if let Some(id) = response.get("id") {
        template["id"] = id.clone();
    }
    if let Some(usage) = response.get("usage") {
        apply_usage(&mut template, usage);
    }

    let mut tool_calls: Vec<Value> = Vec::new();
    let mut images: Vec<Value> = Vec::new();
    let mut has_tool_calls = false;
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        let mut content_text = String::new();
        let mut reasoning_text = String::new();
        for item in output {
            match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                "reasoning" => {
                    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                        for entry in summary {
                            if entry.get("type").and_then(Value::as_str) == Some("summary_text") {
                                if let Some(text) = entry.get("text").and_then(Value::as_str)
                                    && !text.is_empty()
                                {
                                    reasoning_text.push_str(text);
                                }
                                break;
                            }
                        }
                    }
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for entry in content {
                            if entry.get("type").and_then(Value::as_str) == Some("reasoning_text")
                                && let Some(text) = entry.get("text").and_then(Value::as_str)
                                && !text.is_empty()
                            {
                                reasoning_text.push_str(text);
                            }
                        }
                    }
                }
                "message" => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for entry in content {
                            if entry.get("type").and_then(Value::as_str) == Some("output_text") {
                                if let Some(text) = entry.get("text").and_then(Value::as_str)
                                    && !text.is_empty()
                                {
                                    content_text.push_str(text);
                                }
                                break;
                            }
                        }
                    }
                }
                "function_call" | "custom_tool_call" => {
                    let mut call =
                        json!({"id":"","type":"function","function":{"name":"","arguments":""}});
                    if let Some(id) = item.get("call_id") {
                        call["id"] = id.clone();
                    }
                    if let Some(name) = item.get("name").and_then(Value::as_str) {
                        let mut restored = name.to_owned();
                        let reverse = build_reverse_map_from_original(original);
                        if let Some(original_name) = reverse.get(&restored) {
                            restored = original_name.clone();
                        }
                        call["function"]["name"] = Value::String(restored);
                    }
                    call["function"]["arguments"] = Value::String(codex_tool_call_arguments(item));
                    tool_calls.push(call);
                }
                "image_generation_call" => {
                    let data = item
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if data.is_empty() {
                        continue;
                    }
                    let output_format = item
                        .get("output_format")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let mime = mime_type_from_codex_output_format(output_format);
                    let image_url = format!("data:{mime};base64,{data}");
                    let index = images.len();
                    images.push(json!({
                        "type":"image_url","index":index,
                        "image_url":{"url":image_url}
                    }));
                }
                _ => {}
            }
        }
        if !content_text.is_empty() {
            template["choices"][0]["message"]["content"] = Value::String(content_text);
        }
        if !reasoning_text.is_empty() {
            template["choices"][0]["message"]["reasoning_content"] = Value::String(reasoning_text);
        }
        has_tool_calls = !tool_calls.is_empty();
        if has_tool_calls {
            template["choices"][0]["message"]["tool_calls"] = Value::Array(tool_calls);
        }
        if !images.is_empty() {
            template["choices"][0]["message"]["images"] = Value::Array(images);
        }
    }

    if let Some(status) = response.get("status").and_then(Value::as_str) {
        let mut finish_reason = "";
        let mut native_finish_reason = "";
        match status {
            "completed" => {
                finish_reason = "stop";
                native_finish_reason = "stop";
                if has_tool_calls {
                    finish_reason = "tool_calls";
                    native_finish_reason = "tool_calls";
                }
            }
            "incomplete" => {
                native_finish_reason = response
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                finish_reason = match native_finish_reason {
                    "max_tokens" | "max_output_tokens" => "length",
                    "content_filter" => "content_filter",
                    _ => "stop",
                };
            }
            _ => {}
        }
        if !finish_reason.is_empty() {
            template["choices"][0]["finish_reason"] = Value::String(finish_reason.to_owned());
            template["choices"][0]["native_finish_reason"] =
                Value::String(native_finish_reason.to_owned());
        }
    }

    template
}

#[derive(Clone, Copy)]
struct ToolCallState {
    index: i64,
    arguments_emitted: bool,
    done: bool,
}

struct StreamState {
    original: Value,
    model: String,
    response_id: String,
    created_at: i64,
    service_tier: String,
    function_call_index: i64,
    states: Vec<ToolCallState>,
    keys: HashMap<String, usize>,
    current: Option<usize>,
    last_image_hash: HashMap<String, [u8; 32]>,
}

impl StreamState {
    fn new(original: Value) -> Self {
        Self {
            original,
            model: String::new(),
            response_id: String::new(),
            created_at: 0,
            service_tier: String::new(),
            function_call_index: -1,
            states: Vec::new(),
            keys: HashMap::new(),
            current: None,
            last_image_hash: HashMap::new(),
        }
    }

    fn register(&mut self, event: &Value, item: &Value, index: usize) {
        if let Some(item_id) = event.get("item_id").and_then(Value::as_str)
            && !item_id.is_empty()
        {
            self.keys.insert(format!("item:{item_id}"), index);
        }
        if let Some(item_id) = item.get("id").and_then(Value::as_str)
            && !item_id.is_empty()
        {
            self.keys.insert(format!("item:{item_id}"), index);
        }
        if let Some(output_index) = event.get("output_index") {
            self.keys.insert(format!("output:{output_index}"), index);
        }
        self.current = Some(index);
    }

    fn find(&self, event: &Value, item: &Value) -> Option<usize> {
        if let Some(item_id) = event.get("item_id").and_then(Value::as_str)
            && !item_id.is_empty()
            && let Some(index) = self.keys.get(&format!("item:{item_id}"))
        {
            return Some(*index);
        }
        if let Some(item_id) = item.get("id").and_then(Value::as_str)
            && !item_id.is_empty()
            && let Some(index) = self.keys.get(&format!("item:{item_id}"))
        {
            return Some(*index);
        }
        if let Some(output_index) = event.get("output_index")
            && let Some(index) = self.keys.get(&format!("output:{output_index}"))
        {
            return Some(*index);
        }
        self.current
    }

    fn push_image(
        &mut self,
        template: &mut Value,
        item_id: &str,
        data: &str,
        format: &str,
    ) -> bool {
        if data.is_empty() {
            return false;
        }
        if !item_id.is_empty() {
            let hash: [u8; 32] = Sha256::digest(data.as_bytes()).into();
            if let Some(last) = self.last_image_hash.get(item_id)
                && *last == hash
            {
                return false;
            }
            self.last_image_hash.insert(item_id.to_owned(), hash);
        }
        let mime = mime_type_from_codex_output_format(format);
        let image_url = format!("data:{mime};base64,{data}");
        if !template["choices"][0]["delta"]["images"].is_array() {
            template["choices"][0]["delta"]["images"] = json!([]);
        }
        let index = template["choices"][0]["delta"]["images"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        template["choices"][0]["delta"]["images"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"image_url","index":index,"image_url":{"url":image_url}}));
        template["choices"][0]["delta"]["role"] = json!("assistant");
        true
    }

    fn event(&mut self, root: &Value) -> Vec<Value> {
        if let Some(tier) = codex_response_service_tier(root.get("response"))
            .or_else(|| codex_response_service_tier(Some(root)))
        {
            self.service_tier = tier;
        }

        let data_type = root.get("type").and_then(Value::as_str).unwrap_or_default();
        if data_type == "response.created" {
            self.response_id = pointer_string(root, "/response/id");
            self.created_at = root
                .pointer("/response/created_at")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            self.model = pointer_string(root, "/response/model");
            return Vec::new();
        }

        let mut template = json!({
            "id":"",
            "object":"chat.completion.chunk",
            "created":12345,
            "model":"model",
            "choices":[{"index":0,"delta":{},"finish_reason":null,"native_finish_reason":null}]
        });
        if !self.service_tier.is_empty() {
            template["service_tier"] = Value::String(self.service_tier.clone());
        }
        if let Some(model) = root.get("model").and_then(Value::as_str) {
            template["model"] = Value::String(model.to_owned());
        } else if !self.model.is_empty() {
            template["model"] = Value::String(self.model.clone());
        }
        template["created"] = Value::from(self.created_at);
        template["id"] = Value::String(self.response_id.clone());
        if let Some(usage) = root.pointer("/response/usage") {
            apply_usage(&mut template, usage);
        }

        match data_type {
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = root.get("delta") {
                    template["choices"][0]["delta"]["role"] = json!("assistant");
                    template["choices"][0]["delta"]["reasoning_content"] =
                        Value::String(delta.as_str().unwrap_or_default().to_owned());
                }
            }
            "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
                template["choices"][0]["delta"]["role"] = json!("assistant");
                template["choices"][0]["delta"]["reasoning_content"] = json!("\n\n");
            }
            "response.output_text.delta" => {
                if let Some(delta) = root.get("delta") {
                    template["choices"][0]["delta"]["role"] = json!("assistant");
                    template["choices"][0]["delta"]["content"] =
                        Value::String(delta.as_str().unwrap_or_default().to_owned());
                }
            }
            "response.image_generation_call.partial_image" => {
                let item_id = pointer_string(root, "/item_id");
                let data = pointer_string(root, "/partial_image_b64");
                let format = pointer_string(root, "/output_format");
                if !self.push_image(&mut template, &item_id, &data, &format) {
                    return Vec::new();
                }
            }
            "response.completed" | "response.incomplete" => {
                let mut finish_reason = "stop";
                let mut native_finish_reason = "stop";
                if data_type == "response.incomplete" {
                    native_finish_reason = root
                        .pointer("/response/incomplete_details/reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    finish_reason = match native_finish_reason {
                        "max_tokens" | "max_output_tokens" => "length",
                        "content_filter" => "content_filter",
                        _ => "stop",
                    };
                } else if self.function_call_index != -1 {
                    finish_reason = "tool_calls";
                    native_finish_reason = "tool_calls";
                }
                template["choices"][0]["finish_reason"] = json!(finish_reason);
                template["choices"][0]["native_finish_reason"] = json!(native_finish_reason);
            }
            "response.output_item.added" => {
                let Some(item) = root.get("item") else {
                    return Vec::new();
                };
                if !is_codex_tool_call_type(
                    item.get("type").and_then(Value::as_str).unwrap_or_default(),
                ) {
                    return Vec::new();
                }
                self.function_call_index += 1;
                let index = self.function_call_index;
                let state_index = self.states.len();
                self.states.push(ToolCallState {
                    index,
                    arguments_emitted: false,
                    done: false,
                });
                self.register(root, item, state_index);

                let mut call = json!({"index":index,"id":"","type":"function","function":{"name":"","arguments":""}});
                call["id"] = Value::String(
                    item.get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                );
                let mut name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let reverse = build_reverse_map_from_original(&self.original);
                if let Some(original_name) = reverse.get(&name) {
                    name = original_name.clone();
                }
                call["function"]["name"] = Value::String(name);
                template["choices"][0]["delta"]["role"] = json!("assistant");
                template["choices"][0]["delta"]["tool_calls"] = Value::Array(vec![call]);
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let Some(state_index) = self.find(root, &Value::Null) else {
                    return Vec::new();
                };
                let delta = root
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if self.states[state_index].done || delta.is_empty() {
                    return Vec::new();
                }
                self.states[state_index].arguments_emitted = true;
                let index = self.states[state_index].index;
                template["choices"][0]["delta"]["tool_calls"] = json!([
                    {"index":index,"function":{"arguments":delta}}
                ]);
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                let Some(state_index) = self.find(root, &Value::Null) else {
                    return Vec::new();
                };
                if self.states[state_index].done || self.states[state_index].arguments_emitted {
                    return Vec::new();
                }
                self.states[state_index].arguments_emitted = true;
                let field = if data_type == "response.custom_tool_call_input.done" {
                    "input"
                } else {
                    "arguments"
                };
                let full = root.get(field).and_then(Value::as_str).unwrap_or_default();
                if full.is_empty() {
                    return Vec::new();
                }
                let index = self.states[state_index].index;
                template["choices"][0]["delta"]["tool_calls"] = json!([
                    {"index":index,"function":{"arguments":full}}
                ]);
            }
            "response.output_item.done" => {
                let Some(item) = root.get("item") else {
                    return Vec::new();
                };
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
                if item_type == "image_generation_call" {
                    let item_id = pointer_string(item, "/id");
                    let data = pointer_string(item, "/result");
                    let format = pointer_string(item, "/output_format");
                    if !self.push_image(&mut template, &item_id, &data, &format) {
                        return Vec::new();
                    }
                    return vec![template];
                }
                if !is_codex_tool_call_type(item_type) {
                    return Vec::new();
                }
                if let Some(state_index) = self.find(root, item) {
                    if self.states[state_index].done {
                        return Vec::new();
                    }
                    self.states[state_index].done = true;
                    if self.states[state_index].arguments_emitted {
                        return Vec::new();
                    }
                    self.states[state_index].arguments_emitted = true;
                    let full = codex_tool_call_arguments(item);
                    if full.is_empty() {
                        return Vec::new();
                    }
                    let index = self.states[state_index].index;
                    template["choices"][0]["delta"]["tool_calls"] = json!([
                        {"index":index,"function":{"arguments":full}}
                    ]);
                    return vec![template];
                }
                self.function_call_index += 1;
                let index = self.function_call_index;
                let state_index = self.states.len();
                self.states.push(ToolCallState {
                    index,
                    arguments_emitted: true,
                    done: true,
                });
                self.register(root, item, state_index);

                let mut call = json!({"index":index,"id":"","type":"function","function":{"name":"","arguments":""}});
                call["id"] = Value::String(
                    item.get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                );
                let mut name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let reverse = build_reverse_map_from_original(&self.original);
                if let Some(original_name) = reverse.get(&name) {
                    name = original_name.clone();
                }
                call["function"]["name"] = Value::String(name);
                call["function"]["arguments"] = Value::String(codex_tool_call_arguments(item));
                template["choices"][0]["delta"]["role"] = json!("assistant");
                template["choices"][0]["delta"]["tool_calls"] = Value::Array(vec![call]);
            }
            _ => return Vec::new(),
        }
        vec![template]
    }
}

fn chat_to_completions_response(chat: &Value) -> Value {
    let mut out = json!({"id":"","object":"text_completion","created":0,"model":"","choices":[]});
    if let Some(id) = chat.get("id") {
        out["id"] = id.clone();
    }
    if let Some(created) = chat.get("created") {
        out["created"] = created.clone();
    }
    if let Some(model) = chat.get("model") {
        out["model"] = model.clone();
    }
    if let Some(usage) = chat.get("usage") {
        out["usage"] = usage.clone();
    }
    let mut choices: Vec<Value> = Vec::new();
    if let Some(chat_choices) = chat.get("choices").and_then(Value::as_array) {
        for choice in chat_choices {
            let mut converted = Map::new();
            converted.insert(
                "index".into(),
                choice.get("index").cloned().unwrap_or_else(|| json!(0)),
            );
            let text = if let Some(message) = choice.get("message") {
                message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            } else if let Some(delta) = choice.get("delta") {
                delta
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            } else {
                ""
            };
            converted.insert("text".into(), Value::String(text.to_owned()));
            if let Some(finish_reason) = choice.get("finish_reason") {
                converted.insert("finish_reason".into(), finish_reason.clone());
            }
            if let Some(logprobs) = choice.get("logprobs") {
                converted.insert("logprobs".into(), logprobs.clone());
            }
            choices.push(Value::Object(converted));
        }
    }
    if !choices.is_empty() {
        out["choices"] = Value::Array(choices);
    }
    out
}

fn chat_chunk_to_completions(chat: &Value) -> Option<Value> {
    let has_usage = chat.get("usage").is_some();
    let mut has_content = false;
    if let Some(choices) = chat.get("choices").and_then(Value::as_array) {
        for choice in choices {
            if let Some(content) = choice
                .get("delta")
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
                && !content.is_empty()
            {
                has_content = true;
                break;
            }
            if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str)
                && !finish_reason.is_empty()
                && finish_reason != "null"
            {
                has_content = true;
                break;
            }
        }
    }
    if !has_content && !has_usage {
        return None;
    }

    let mut out = json!({"id":"","object":"text_completion","created":0,"model":"","choices":[]});
    if let Some(id) = chat.get("id") {
        out["id"] = id.clone();
    }
    if let Some(created) = chat.get("created") {
        out["created"] = created.clone();
    }
    if let Some(model) = chat.get("model") {
        out["model"] = model.clone();
    }
    let mut choices: Vec<Value> = Vec::new();
    if let Some(chat_choices) = chat.get("choices").and_then(Value::as_array) {
        for choice in chat_choices {
            let mut converted = Map::new();
            converted.insert(
                "index".into(),
                choice.get("index").cloned().unwrap_or_else(|| json!(0)),
            );
            let text = choice
                .get("delta")
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            converted.insert("text".into(), Value::String(text.to_owned()));
            if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str)
                && finish_reason != "null"
            {
                converted.insert("finish_reason".into(), json!(finish_reason));
            }
            if let Some(logprobs) = choice.get("logprobs") {
                converted.insert("logprobs".into(), logprobs.clone());
            }
            choices.push(Value::Object(converted));
        }
    }
    if !choices.is_empty() {
        out["choices"] = Value::Array(choices);
    }
    if let Some(usage) = chat.get("usage") {
        out["usage"] = usage.clone();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{
        StreamState, build_chat_completion, chat_chunk_to_completions,
        chat_to_completions_response, convert_openai_request_to_codex,
    };
    use serde_json::json;

    #[test]
    fn request_maps_tools_and_tool_round_trip() {
        let request = json!({
            "model":"gpt-5.6-sol",
            "messages":[
                {"role":"assistant","content":null,"tool_calls":[
                    {"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"x\"}"}}
                ]},
                {"role":"tool","tool_call_id":"call_1","content":"result"}
            ],
            "tools":[{"type":"function","function":{"name":"lookup","description":"d","parameters":{"type":"object"}}}],
            "tool_choice":{"type":"function","function":{"name":"lookup"}}
        });
        let translated = convert_openai_request_to_codex("gpt-5.6-sol", &request, true);
        assert_eq!(translated["stream"], json!(true));
        assert_eq!(translated["reasoning"]["effort"], json!("medium"));
        assert_eq!(translated["input"][0]["type"], json!("function_call"));
        assert_eq!(
            translated["input"][1]["type"],
            json!("function_call_output")
        );
        assert_eq!(translated["input"][1]["call_id"], json!("call_1"));
        assert_eq!(translated["tools"][0]["type"], json!("function"));
        assert_eq!(translated["tools"][0]["strict"], json!(false));
        assert_eq!(translated["tool_choice"]["name"], json!("lookup"));
        assert_eq!(translated["store"], json!(false));
    }

    #[test]
    fn request_shortens_long_tool_names() {
        let long_name = format!("mcp__{}{}", "a".repeat(80), "tail");
        let request = json!({
            "messages":[{"role":"user","content":"hi"}],
            "tools":[{"type":"function","function":{"name":long_name,"parameters":{}}}]
        });
        let translated = convert_openai_request_to_codex("gpt-5.6-sol", &request, true);
        let mapped = translated["tools"][0]["name"].as_str().unwrap();
        assert!(mapped.len() <= 64);
        assert!(mapped.starts_with("mcp__"));
    }

    #[test]
    fn non_stream_builds_chat_completion() {
        let response = json!({
            "id":"resp_123",
            "model":"gpt-5.6-sol",
            "created_at":1700000000,
            "status":"completed",
            "service_tier":"default",
            "usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120,
                "input_tokens_details":{"cached_tokens":30,"cache_write_tokens":40},
                "output_tokens_details":{"reasoning_tokens":5}},
            "output":[
                {"type":"reasoning","summary":[{"type":"summary_text","text":"think"}]},
                {"type":"message","content":[{"type":"output_text","text":"hello"}]},
                {"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{}"}
            ]
        });
        let out = build_chat_completion(&response, &json!({}));
        assert_eq!(out["id"], json!("resp_123"));
        assert_eq!(out["object"], json!("chat.completion"));
        assert_eq!(out["service_tier"], json!("default"));
        assert_eq!(out["choices"][0]["message"]["content"], json!("hello"));
        assert_eq!(
            out["choices"][0]["message"]["reasoning_content"],
            json!("think")
        );
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["id"],
            json!("call_1")
        );
        assert_eq!(out["choices"][0]["finish_reason"], json!("tool_calls"));
        assert_eq!(out["usage"]["prompt_tokens"], json!(100));
        assert_eq!(out["usage"]["completion_tokens"], json!(20));
        assert_eq!(out["usage"]["total_tokens"], json!(120));
        assert_eq!(
            out["usage"]["prompt_tokens_details"]["cached_tokens"],
            json!(30)
        );
        assert_eq!(
            out["usage"]["prompt_tokens_details"]["cache_write_tokens"],
            json!(40)
        );
        assert_eq!(
            out["usage"]["prompt_tokens_details"]["cached_creation_tokens"],
            json!(40)
        );
        assert_eq!(
            out["usage"]["completion_tokens_details"]["reasoning_tokens"],
            json!(5)
        );
    }

    #[test]
    fn streaming_emits_usage_and_finish_reason() {
        let mut state = StreamState::new(json!({}));
        state.event(&json!({
            "type":"response.created",
            "response":{"id":"resp_123","created_at":1700000000,"model":"gpt-5.6-sol"}
        }));
        let chunks = state.event(&json!({
            "type":"response.completed",
            "response":{
                "id":"resp_123",
                "usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120,
                    "input_tokens_details":{"cached_tokens":30,"cache_write_tokens":0},
                    "output_tokens_details":{"reasoning_tokens":5}}
            }
        }));
        assert_eq!(chunks.len(), 1);
        let chunk = &chunks[0];
        assert_eq!(chunk["object"], json!("chat.completion.chunk"));
        assert_eq!(chunk["id"], json!("resp_123"));
        assert_eq!(chunk["choices"][0]["finish_reason"], json!("stop"));
        assert_eq!(chunk["usage"]["total_tokens"], json!(120));
        assert_eq!(
            chunk["usage"]["prompt_tokens_details"]["cache_write_tokens"],
            json!(0)
        );
    }

    #[test]
    fn legacy_response_conversion() {
        let chat = json!({
            "id":"resp_123",
            "created":1700000000,
            "model":"gpt-5.6-sol",
            "usage":{"total_tokens":5},
            "choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}]
        });
        let out = chat_to_completions_response(&chat);
        assert_eq!(out["object"], json!("text_completion"));
        assert_eq!(out["choices"][0]["text"], json!("hello"));
        assert_eq!(out["choices"][0]["finish_reason"], json!("stop"));

        let chunk = json!({
            "id":"resp_123",
            "created":1700000000,
            "model":"gpt-5.6-sol",
            "choices":[{"index":0,"delta":{"content":"hi"}}]
        });
        let converted = chat_chunk_to_completions(&chunk).unwrap();
        assert_eq!(converted["choices"][0]["text"], json!("hi"));
        assert!(
            chat_chunk_to_completions(&json!({
                "choices":[{"index":0,"delta":{}}]
            }))
            .is_none()
        );
    }
}
