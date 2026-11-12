//! Claude through the Anthropic Messages API (`POST /v1/messages`), over raw
//! HTTP: Rust has no official Anthropic SDK.
//!
//! Conversations are kept in the app's own message format (the shape the
//! OpenAI-compatible provider uses) and converted per request. Assistant
//! turns produced by Claude also carry the exact content blocks the API
//! returned (`anthropic_content`), which are replayed verbatim: thinking
//! blocks are bound to the conversation that produced them, and any change to
//! an earlier turn invalidates them.

use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};
use tauri::ipc::Channel;

use super::{
    config::Config,
    metadata::{Model, ReasoningEffort},
    provider::{cancellable, checked, client},
    Event,
};

const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Room for long answers; streaming keeps a large cap safe from timeouts.
const MAX_OUTPUT_TOKENS: u64 = 64_000;
/// Where assistant turns keep the content blocks Claude returned.
pub const CONTENT_KEY: &str = "anthropic_content";
/// Block types that may appear in `anthropic_content`.
const STORED_BLOCKS: &[&str] = &[
    "text",
    "thinking",
    "redacted_thinking",
    "tool_use",
    "fallback",
];

/// Models that can decline a request in a way another model can serve. The
/// API then retries on the model Anthropic recommends for the category.
fn supports_server_fallback(model: &str) -> bool {
    [
        "claude-fable-5-2",
        "claude-fable-5-1",
        "claude-opus-5-5",
        "claude-opus-5",
        "claude-sonnet-5-5",
    ]
    .contains(&model)
}

/// The API base without a trailing endpoint, e.g. `https://api.anthropic.com`.
pub fn base(config: &Config) -> String {
    let url = config.base_url.trim_end_matches('/');
    url.strip_suffix("/v1/messages")
        .or_else(|| url.strip_suffix("/v1/models"))
        .or_else(|| url.strip_suffix("/v1"))
        .unwrap_or(url)
        .to_string()
}

fn request(
    config: &Config,
    method: reqwest::Method,
    path: &str,
    beta: Option<&str>,
) -> Result<reqwest::RequestBuilder, String> {
    let mut request = client()?
        .request(method, format!("{}{path}", base(config)))
        .header("anthropic-version", API_VERSION)
        .header("x-api-key", &config.api_key);
    if let Some(beta) = beta {
        request = request.header("anthropic-beta", beta);
    }
    Ok(request)
}

// ===== models =====

/// A model from `GET /v1/models`, with the effort levels it supports.
pub fn model_from_api(value: &Value) -> Option<Model> {
    let id = value["id"].as_str()?.trim();
    if id.is_empty() || id.len() > 200 {
        return None;
    }
    let capabilities = &value["capabilities"];
    let supported = |path: &str| capabilities.pointer(path).and_then(Value::as_bool) == Some(true);
    let reasoning_efforts = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .filter(|level| supported(&format!("/effort/{level}/supported")))
        .map(|level| ReasoningEffort {
            value: (*level).into(),
            label: (*level).into(),
            description: String::new(),
        })
        .collect();
    Some(Model {
        id: id.into(),
        name: value["display_name"]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(id)
            .chars()
            .take(200)
            .collect(),
        reasoning_efforts,
        default_reasoning_effort: None,
        context_window: super::metadata::context_window(value),
        adaptive_thinking: Some(supported("/thinking/types/adaptive/supported")),
        max_output_tokens: value["max_tokens"].as_u64(),
    })
}

pub async fn models(config: &Config, cancel: Option<&AtomicBool>) -> Result<Vec<Model>, String> {
    let request = request(config, reqwest::Method::GET, "/v1/models?limit=1000", None)?
        .timeout(Duration::from_secs(30));
    let response = match cancel {
        Some(cancel) => cancellable(cancel, request.send()).await?,
        None => request.send().await.map_err(|e| e.to_string())?,
    };
    let response = checked(response, config, cancel).await?;
    let body = match cancel {
        Some(cancel) => cancellable(cancel, response.json::<Value>()).await?,
        None => response
            .json::<Value>()
            .await
            .map_err(|e| format!("模型列表无法解析：{e}"))?,
    };
    let models: Vec<Model> = body["data"]
        .as_array()
        .ok_or("服务未返回模型列表")?
        .iter()
        .filter_map(model_from_api)
        .take(500)
        .collect();
    Ok(models)
}

// ===== requests =====

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// A user turn: its stored context note (if any) and the user's text.
fn user_blocks(message: &Value) -> Vec<Value> {
    let mut blocks = Vec::new();
    if let Some(context) = message["context"].as_str().filter(|c| !c.is_empty()) {
        blocks.push(text_block(context));
    }
    let text = message["content"].as_str().unwrap_or_default();
    if !text.is_empty() || blocks.is_empty() {
        blocks.push(text_block(text));
    }
    blocks
}

/// An assistant turn that Claude did not produce (an earlier OpenAI-compatible
/// turn, or a compaction artifact), rebuilt from its text and tool calls.
fn assistant_blocks(message: &Value) -> Vec<Value> {
    let mut blocks = Vec::new();
    if let Some(text) = message["content"].as_str().filter(|t| !t.is_empty()) {
        blocks.push(text_block(text));
    }
    for call in message["tool_calls"].as_array().into_iter().flatten() {
        let input = call["function"]["arguments"]
            .as_str()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        blocks.push(json!({
            "type": "tool_use",
            "id": call["id"],
            "name": call["function"]["name"],
            "input": input,
        }));
    }
    blocks
}

fn tool_result_block(message: &Value) -> Value {
    let content = message["content"].as_str().unwrap_or_default();
    let failed = serde_json::from_str::<Value>(content)
        .ok()
        .is_some_and(|result| !result["error"].is_null());
    json!({
        "type": "tool_result",
        "tool_use_id": message["tool_call_id"],
        "content": content,
        "is_error": failed,
    })
}

/// The app's messages as Messages API turns. Tool results that follow one
/// assistant turn go back together in a single user turn.
pub fn convert_messages(messages: &[Value]) -> Vec<Value> {
    let mut turns: Vec<Value> = Vec::new();
    for message in messages {
        let (role, blocks) = match message["role"].as_str() {
            Some("assistant") => (
                "assistant",
                match message[CONTENT_KEY].as_array() {
                    Some(stored) if !stored.is_empty() => stored.clone(),
                    _ => assistant_blocks(message),
                },
            ),
            Some("tool") => ("user", vec![tool_result_block(message)]),
            _ => ("user", user_blocks(message)),
        };
        if blocks.is_empty() {
            continue;
        }
        // Tool results join the user turn holding the other results of the
        // same assistant turn. Other turns stand alone, as sent.
        let joins = message["role"] == "tool"
            && turns.last().is_some_and(|last| {
                last["role"] == "user"
                    && last["content"]
                        .as_array()
                        .is_some_and(|c| c.iter().all(|b| b["type"] == "tool_result"))
            });
        if joins {
            let last = turns.last_mut().expect("checked above");
            last["content"]
                .as_array_mut()
                .expect("checked above")
                .extend(blocks);
        } else {
            turns.push(json!({"role": role, "content": blocks}));
        }
    }
    turns
}

/// The app's tool definitions (OpenAI function format) as Messages API tools.
/// Inputs stream as they are generated; they are validated before any tool
/// runs, since a streamed input may be incomplete.
pub fn convert_tools(definitions: &Value) -> Vec<Value> {
    definitions
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| {
            json!({
                "name": tool["function"]["name"],
                "description": tool["function"]["description"],
                "input_schema": tool["function"]["parameters"],
                "eager_input_streaming": true,
            })
        })
        .collect()
}

pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub messages: &'a [Value],
    pub tools: Option<&'a Value>,
    pub effort: Option<&'a str>,
    /// From the model's metadata; `None` when unknown (a manual model ID).
    pub metadata: Option<&'a Model>,
}

/// The request body, and the beta header it needs.
pub fn request_body(request: &Request) -> (Value, Option<&'static str>) {
    let max_tokens = request
        .metadata
        .and_then(|model| model.max_output_tokens)
        .map_or(MAX_OUTPUT_TOKENS, |max| max.min(MAX_OUTPUT_TOKENS));
    let mut body = json!({
        "model": request.model,
        "max_tokens": max_tokens,
        "stream": true,
        "system": request.system,
        "messages": convert_messages(request.messages),
        // Cache the conversation prefix; the breakpoint follows the last block.
        "cache_control": {"type": "ephemeral"},
    });
    // Current models think adaptively; summaries make the reasoning visible.
    // Older ones reject the setting, so it is sent only where supported or
    // where nothing is known about the model.
    if request
        .metadata
        .and_then(|model| model.adaptive_thinking)
        .unwrap_or(true)
    {
        body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
    }
    if let Some(effort) = request.effort {
        body["output_config"] = json!({"effort": effort});
    }
    if let Some(tools) = request.tools {
        body["tools"] = json!(convert_tools(tools));
        body["tool_choice"] = json!({"type": "auto"});
    }
    if supports_server_fallback(request.model) {
        body["fallbacks"] = json!("default");
        return (body, Some(FALLBACK_BETA));
    }
    (body, None)
}

// ===== responses =====

/// Assembles a streamed response.
#[derive(Default)]
pub struct Accumulator {
    blocks: BTreeMap<usize, Value>,
    tool_inputs: BTreeMap<usize, String>,
    size: usize,
    stop_reason: Option<String>,
    stop_details: Value,
    stopped: bool,
}

impl Accumulator {
    fn grow(&mut self, added: usize) -> Result<(), String> {
        self.size += added;
        if self.size > 2 * 1_048_576 {
            return Err("模型响应超过 2 MB，请缩小任务范围".into());
        }
        Ok(())
    }

    pub fn ingest(&mut self, event: &Value, emit: &impl Fn(Event)) -> Result<(), String> {
        let index = event["index"].as_u64().unwrap_or(0) as usize;
        match event["type"].as_str().unwrap_or_default() {
            "error" => {
                return Err(format!(
                    "模型响应错误：{}",
                    event["error"]["message"].as_str().unwrap_or("未知错误")
                ))
            }
            "content_block_start" => {
                if index >= 256 {
                    return Err("模型响应内容块过多".into());
                }
                let block = event["content_block"].clone();
                if block["type"] == "fallback" {
                    let model = block["to"]["model"].as_str().unwrap_or("备用模型");
                    emit(Event::new(
                        "text",
                        json!({"text": format!("\n\n[原模型未继续此请求，已由 {model} 接手]\n\n")}),
                    ));
                }
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                let Some(block) = self.blocks.get_mut(&index) else {
                    return Err("模型流式响应顺序异常".into());
                };
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        let joined =
                            format!("{}{text}", block["text"].as_str().unwrap_or_default());
                        block["text"] = json!(joined);
                        emit(Event::new("text", json!({"text": text})));
                        self.grow(text.len())?;
                    }
                    "thinking_delta" => {
                        let text = delta["thinking"].as_str().unwrap_or_default();
                        let joined =
                            format!("{}{text}", block["thinking"].as_str().unwrap_or_default());
                        block["thinking"] = json!(joined);
                        emit(Event::new("thinking", json!({"text": text})));
                        self.grow(text.len())?;
                    }
                    "signature_delta" => {
                        let part = delta["signature"].as_str().unwrap_or_default();
                        let joined =
                            format!("{}{part}", block["signature"].as_str().unwrap_or_default());
                        block["signature"] = json!(joined);
                    }
                    "input_json_delta" => {
                        let part = delta["partial_json"].as_str().unwrap_or_default();
                        let input = self.tool_inputs.entry(index).or_default();
                        if input.len() + part.len() > 524_288 {
                            return Err("工具参数超过 512 KB".into());
                        }
                        input.push_str(part);
                        self.size += part.len();
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.into());
                }
                if !event["delta"]["stop_details"].is_null() {
                    self.stop_details = event["delta"]["stop_details"].clone();
                }
            }
            "message_stop" => self.stopped = true,
            _ => {}
        }
        Ok(())
    }

    /// The assistant turn in the app's format. Nothing becomes a tool call
    /// unless the response finished normally.
    pub fn finish(mut self) -> Result<Value, String> {
        if !self.stopped {
            return Err("模型响应意外中断，未执行本轮工具".into());
        }
        match self.stop_reason.as_deref() {
            Some("end_turn" | "tool_use" | "stop_sequence") => {}
            Some("refusal") => {
                let category = self.stop_details["category"]
                    .as_str()
                    .map(|c| format!("（类别：{c}）"))
                    .unwrap_or_default();
                return Err(format!(
                    "模型拒绝了此请求{category}，未执行本轮工具。可换一种说法或改用其他模型。"
                ));
            }
            Some("max_tokens") => {
                return Err("模型输出达到长度上限，未完整生成，未执行本轮工具".into())
            }
            other => {
                return Err(format!(
                    "模型未完整生成（{}），未执行本轮工具",
                    other.unwrap_or("无结束原因")
                ))
            }
        }
        // Tool inputs were streamed as text; parse them now. A malformed input
        // is kept raw so the tool call is reported as invalid, not run.
        let mut raw_inputs = BTreeMap::new();
        for (index, raw) in std::mem::take(&mut self.tool_inputs) {
            if let Some(block) = self.blocks.get_mut(&index) {
                block["input"] = serde_json::from_str::<Value>(&raw)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
            }
            raw_inputs.insert(index, raw);
        }
        let blocks: Vec<(usize, Value)> = self.blocks.into_iter().collect();

        // After a fallback mid-answer, what came before the last `fallback`
        // block belongs to the attempt that was declined: only its text is
        // echoed back and none of its tool calls run.
        let boundary = blocks
            .iter()
            .rposition(|(_, block)| block["type"] == "fallback");
        let mut stored = Vec::new();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut tool_calls = Vec::new();
        for (position, (index, block)) in blocks.into_iter().enumerate() {
            let before_boundary = boundary.is_some_and(|b| position < b);
            let kind = block["type"].as_str().unwrap_or_default().to_string();
            if before_boundary && kind != "text" {
                continue;
            }
            if !STORED_BLOCKS.contains(&kind.as_str()) {
                continue;
            }
            if !before_boundary {
                match kind.as_str() {
                    "text" => text.push_str(block["text"].as_str().unwrap_or_default()),
                    "thinking" => thinking.push_str(block["thinking"].as_str().unwrap_or_default()),
                    "tool_use" => {
                        let raw = raw_inputs
                            .get(&index)
                            .cloned()
                            .filter(|raw| !raw.trim().is_empty())
                            .unwrap_or_else(|| block["input"].to_string());
                        tool_calls.push(json!({
                            "id": block["id"],
                            "type": "function",
                            "function": {"name": block["name"], "arguments": raw},
                        }));
                    }
                    _ => {}
                }
            }
            stored.push(block);
        }
        for call in &tool_calls {
            if call["id"].as_str().unwrap_or_default().is_empty()
                || call["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty()
            {
                return Err("模型返回了不完整的工具调用".into());
            }
        }
        if text.is_empty() && tool_calls.is_empty() {
            return Err("模型返回了空回复".into());
        }
        let mut message = json!({"role": "assistant", "content": text});
        if !thinking.is_empty() {
            message["reasoning_content"] = json!(thinking);
        }
        if !tool_calls.is_empty() {
            message["tool_calls"] = json!(tool_calls);
        }
        message[CONTENT_KEY] = json!(stored);
        Ok(message)
    }
}

/// Whether an API error says replayed thinking blocks no longer match the
/// conversation, so that stripping them and retrying once can succeed.
pub fn is_thinking_signature_error(error: &str) -> bool {
    error.contains("Invalid `signature` in `thinking` block")
        || error.contains("Invalid `signature` in `redacted_thinking` block")
}

/// Remove every thinking block from the stored assistant turns. Removing all
/// of them keeps the history valid; done once, at a compaction or after the
/// API rejected an edited history.
pub fn strip_thinking(messages: &mut [Value]) {
    for message in messages {
        // `get_mut`, not indexing: indexing a missing key would insert it.
        if let Some(blocks) = message.get_mut(CONTENT_KEY).and_then(Value::as_array_mut) {
            blocks.retain(|block| {
                !matches!(
                    block["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            });
        }
    }
}

/// Check stored content blocks that came back from the frontend: only
/// assistant turns may carry them, and only of the kinds Claude returns.
pub fn validate_stored(message: &Value) -> Result<(), String> {
    let Some(stored) = message.get(CONTENT_KEY) else {
        return Ok(());
    };
    let blocks = stored.as_array().ok_or("对话内容格式无效")?;
    if message["role"] != "assistant"
        || blocks
            .iter()
            .any(|block| !STORED_BLOCKS.contains(&block["type"].as_str().unwrap_or_default()))
    {
        return Err("对话内容格式无效".into());
    }
    Ok(())
}

pub async fn completion(
    config: &Config,
    request: &Request<'_>,
    cancel: Arc<AtomicBool>,
    events: &Channel<Event>,
) -> Result<Value, String> {
    let (body, beta) = request_body(request);
    let http = self::request(config, reqwest::Method::POST, "/v1/messages", beta)?.json(&body);
    let response = cancellable(&cancel, http.send()).await?;
    let mut response = checked(response, config, Some(&cancel)).await?;
    let emit = |event| {
        let _ = events.send(event);
    };
    let mut acc = Accumulator::default();
    let mut pending = Vec::new();
    while let Some(chunk) = cancellable(&cancel, response.chunk()).await? {
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line = pending.drain(..=end).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data.is_empty() {
                continue;
            }
            let event: Value =
                serde_json::from_str(data).map_err(|_| "模型流式响应不是有效 JSON".to_string())?;
            acc.ingest(&event, &emit)?;
        }
        if pending.len() > 1_048_576 {
            return Err("模型响应单行过长".into());
        }
    }
    acc.finish()
}

/// For tests: a copy without the keys only this module reads.
#[cfg(test)]
fn without_stored(message: &Value) -> Value {
    let mut copy = message.as_object().cloned().unwrap_or_default();
    copy.remove(CONTENT_KEY);
    Value::Object(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(events: &[Value]) -> Accumulator {
        let mut acc = Accumulator::default();
        for event in events {
            acc.ingest(event, &|_| {}).unwrap();
        }
        acc
    }

    fn start(index: usize, block: Value) -> Value {
        json!({"type": "content_block_start", "index": index, "content_block": block})
    }

    fn delta(index: usize, delta: Value) -> Value {
        json!({"type": "content_block_delta", "index": index, "delta": delta})
    }

    fn end(reason: &str) -> [Value; 2] {
        [
            json!({"type": "message_delta", "delta": {"stop_reason": reason}}),
            json!({"type": "message_stop"}),
        ]
    }

    #[test]
    fn assembles_thinking_text_and_tool_calls_and_keeps_blocks_verbatim() {
        let mut events = vec![
            start(
                0,
                json!({"type": "thinking", "thinking": "", "signature": ""}),
            ),
            delta(0, json!({"type": "thinking_delta", "thinking": "先看目录"})),
            delta(
                0,
                json!({"type": "signature_delta", "signature": "sig-abc"}),
            ),
            start(1, json!({"type": "text", "text": ""})),
            delta(1, json!({"type": "text_delta", "text": "检查中"})),
            start(
                2,
                json!({"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {}}),
            ),
            delta(
                2,
                json!({"type": "input_json_delta", "partial_json": "{\"command\":"}),
            ),
            delta(
                2,
                json!({"type": "input_json_delta", "partial_json": "\"pwd\"}"}),
            ),
        ];
        events.extend(end("tool_use"));
        let message = stream(&events).finish().unwrap();
        assert_eq!(message["content"], "检查中");
        assert_eq!(message["reasoning_content"], "先看目录");
        assert_eq!(message["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"pwd\"}"
        );
        let stored = &message[CONTENT_KEY];
        assert_eq!(
            stored[0],
            json!({"type": "thinking", "thinking": "先看目录", "signature": "sig-abc"})
        );
        assert_eq!(stored[2]["input"], json!({"command": "pwd"}));
    }

    #[test]
    fn truncated_refused_or_unfinished_responses_never_become_tool_calls() {
        let partial_tool = [
            start(
                0,
                json!({"type": "tool_use", "id": "t", "name": "shell", "input": {}}),
            ),
            delta(
                0,
                json!({"type": "input_json_delta", "partial_json": "{\"command\":\"rm"}),
            ),
        ];
        for reason in ["max_tokens", "refusal", "pause_turn"] {
            let mut events = partial_tool.to_vec();
            events.extend(end(reason));
            assert!(stream(&events).finish().is_err(), "{reason}");
        }
        // The stream broke off before message_stop.
        assert!(stream(&partial_tool).finish().is_err());

        let mut refusal = vec![
            json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}}}),
        ];
        refusal.push(json!({"type": "message_stop"}));
        assert!(stream(&refusal).finish().unwrap_err().contains("cyber"));

        let mut acc = Accumulator::default();
        assert!(acc
            .ingest(&json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}), &|_| {})
            .unwrap_err()
            .contains("Overloaded"));
    }

    #[test]
    fn a_mid_answer_fallback_drops_the_declined_attempts_tool_calls() {
        let mut events = vec![
            start(
                0,
                json!({"type": "thinking", "thinking": "", "signature": ""}),
            ),
            delta(
                0,
                json!({"type": "signature_delta", "signature": "declined-sig"}),
            ),
            start(1, json!({"type": "text", "text": ""})),
            delta(1, json!({"type": "text_delta", "text": "部分"})),
            start(
                2,
                json!({"type": "tool_use", "id": "declined", "name": "shell", "input": {}}),
            ),
            delta(
                2,
                json!({"type": "input_json_delta", "partial_json": "{\"command\":\"whoami\"}"}),
            ),
            start(
                3,
                json!({"type": "fallback", "from": {"model": "claude-opus-5-5"}, "to": {"model": "claude-opus-5"}}),
            ),
            start(4, json!({"type": "text", "text": ""})),
            delta(4, json!({"type": "text_delta", "text": "继续"})),
            start(
                5,
                json!({"type": "tool_use", "id": "kept", "name": "shell", "input": {}}),
            ),
            delta(
                5,
                json!({"type": "input_json_delta", "partial_json": "{\"command\":\"pwd\"}"}),
            ),
        ];
        events.extend(end("tool_use"));
        let message = stream(&events).finish().unwrap();
        let calls = message["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "kept");
        assert_eq!(message["content"], "继续");
        let kinds: Vec<_> = message[CONTENT_KEY]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["type"].as_str().unwrap().to_string())
            .collect();
        // Text before the boundary stays, the fallback block keeps its place,
        // the declined thinking and tool call are gone.
        assert_eq!(kinds, ["text", "fallback", "text", "tool_use"]);
    }

    #[test]
    fn a_malformed_streamed_input_is_reported_not_run() {
        let mut events = vec![
            start(
                0,
                json!({"type": "tool_use", "id": "t", "name": "shell", "input": {}}),
            ),
            delta(
                0,
                json!({"type": "input_json_delta", "partial_json": "{\"command\": \"ls"}),
            ),
        ];
        events.extend(end("tool_use"));
        let message = stream(&events).finish().unwrap();
        let raw = message["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        assert!(
            serde_json::from_str::<Value>(raw).is_err(),
            "the caller rejects it"
        );
        assert_eq!(message[CONTENT_KEY][0]["input"], json!({}));
    }

    #[test]
    fn conversations_convert_with_tool_results_grouped_and_stored_turns_verbatim() {
        let stored = json!([
            {"type": "thinking", "thinking": "t", "signature": "s"},
            {"type": "tool_use", "id": "a", "name": "shell", "input": {"command": "ls"}},
            {"type": "tool_use", "id": "b", "name": "shell", "input": {"command": "pwd"}}
        ]);
        let messages = vec![
            json!({"role": "user", "content": "检查", "context": "当前权限模式：auto"}),
            json!({"role": "assistant", "content": "", "tool_calls": [], CONTENT_KEY: stored.clone()}),
            json!({"role": "tool", "tool_call_id": "a", "content": "{\"exitCode\":0,\"error\":null}"}),
            json!({"role": "tool", "tool_call_id": "b", "content": "{\"error\":\"denied\"}"}),
            json!({"role": "user", "content": "继续"}),
            // An assistant turn from the OpenAI-compatible provider.
            json!({"role": "assistant", "content": "好的", "tool_calls": [{"id": "c", "type": "function", "function": {"name": "shell", "arguments": "{\"command\":\"id\"}"}}]}),
            json!({"role": "tool", "tool_call_id": "c", "content": "{}"}),
        ];
        let turns = convert_messages(&messages);
        let roles: Vec<_> = turns.iter().map(|t| t["role"].as_str().unwrap()).collect();
        assert_eq!(
            roles,
            ["user", "assistant", "user", "user", "assistant", "user"]
        );
        assert_eq!(turns[0]["content"][0]["text"], "当前权限模式：auto");
        assert_eq!(turns[0]["content"][1]["text"], "检查");
        assert_eq!(turns[1]["content"], stored);
        let results = turns[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            (
                results[0]["is_error"].clone(),
                results[1]["is_error"].clone()
            ),
            (json!(false), json!(true))
        );
        assert_eq!(turns[3]["content"][0]["text"], "继续");
        assert_eq!(turns[4]["content"][1]["input"], json!({"command": "id"}));
        // Converting the same history twice gives identical bytes.
        assert_eq!(convert_messages(&messages), turns);
    }

    #[test]
    fn request_bodies_follow_the_model() {
        let messages = [json!({"role": "user", "content": "hi"})];
        let tools = super::super::tools::definitions();
        let (body, beta) = request_body(&Request {
            model: "claude-opus-5-5",
            system: "sys",
            messages: &messages,
            tools: Some(&tools),
            effort: Some("high"),
            metadata: None,
        });
        assert_eq!(beta, Some(FALLBACK_BETA));
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["max_tokens"], MAX_OUTPUT_TOKENS);
        assert_eq!(body["tool_choice"], json!({"type": "auto"}));
        assert_eq!(body["tools"][0]["name"], "shell");
        assert_eq!(body["tools"][0]["eager_input_streaming"], true);
        assert!(body.get("temperature").is_none());

        let old = Model {
            id: "claude-haiku-4-5".into(),
            name: "Haiku 4.5".into(),
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
            context_window: None,
            adaptive_thinking: Some(false),
            max_output_tokens: Some(8_000),
        };
        let (body, beta) = request_body(&Request {
            model: "claude-haiku-4-5",
            system: "sys",
            messages: &messages,
            tools: None,
            effort: None,
            metadata: Some(&old),
        });
        assert_eq!(beta, None);
        assert!(body.get("thinking").is_none() && body.get("fallbacks").is_none());
        assert!(body.get("tools").is_none() && body.get("output_config").is_none());
        assert_eq!(body["max_tokens"], 8_000);
    }

    #[test]
    fn models_list_reads_effort_and_thinking_capabilities() {
        let model = model_from_api(&json!({
            "id": "claude-opus-5-5", "display_name": "Claude Opus 5.5", "max_tokens": 128000,
            "capabilities": {
                "thinking": {"types": {"adaptive": {"supported": true}}},
                "effort": {"supported": true, "low": {"supported": true}, "medium": {"supported": true},
                           "high": {"supported": true}, "xhigh": {"supported": true}, "max": {"supported": false}}
            }
        }))
        .unwrap();
        let levels: Vec<_> = model
            .reasoning_efforts
            .iter()
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(levels, ["low", "medium", "high", "xhigh"]);
        assert_eq!(
            (model.adaptive_thinking, model.max_output_tokens),
            (Some(true), Some(128000))
        );
        assert!(model_from_api(&json!({"display_name": "no id"})).is_none());
    }

    #[test]
    fn thinking_is_stripped_whole_and_stored_content_is_checked() {
        let mut messages = vec![json!({"role": "assistant", "content": "x", CONTENT_KEY: [
            {"type": "thinking", "thinking": "a", "signature": "s"},
            {"type": "redacted_thinking", "data": "d"},
            {"type": "text", "text": "x"}
        ]})];
        assert!(validate_stored(&messages[0]).is_ok());
        messages.push(json!({"role": "user", "content": "y"}));
        strip_thinking(&mut messages);
        assert_eq!(
            messages[1],
            json!({"role": "user", "content": "y"}),
            "untouched"
        );
        assert_eq!(
            messages[0][CONTENT_KEY],
            json!([{"type": "text", "text": "x"}])
        );
        assert_eq!(
            without_stored(&messages[0]),
            json!({"role": "assistant", "content": "x"})
        );

        let forged_user =
            json!({"role": "user", "content": "x", CONTENT_KEY: [{"type": "text", "text": "x"}]});
        assert!(validate_stored(&forged_user).is_err());
        let unknown =
            json!({"role": "assistant", "content": "", CONTENT_KEY: [{"type": "tool_result"}]});
        assert!(validate_stored(&unknown).is_err());
        assert!(is_thinking_signature_error(
            "模型 API 返回 400：{\"error\":{\"message\":\"messages.5.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation.\"}}"
        ));
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use crate::ai::{config::PROVIDER_ANTHROPIC, provider};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Mutex,
    };

    /// A one-request HTTP server: returns its base URL and, once served, the
    /// request head and JSON body it received.
    fn server(
        content_type: &'static str,
        body: String,
    ) -> (String, std::thread::JoinHandle<(String, Value)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let end = loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
                if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..end]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while request.len() < end + length {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
            }
            let json = serde_json::from_slice(&request[end..end + length]).unwrap_or(Value::Null);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(response.as_bytes()).unwrap();
            // Small writes, so events and characters are split across reads.
            for piece in body.as_bytes().chunks(7) {
                if socket.write_all(piece).is_err() {
                    break;
                }
            }
            (head, json)
        });
        (url, handle)
    }

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect()
    }

    fn config(url: String) -> Config {
        Config {
            provider: PROVIDER_ANTHROPIC.into(),
            base_url: url,
            api_key: "sk-test".into(),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn a_streamed_turn_round_trips_through_the_messages_api() {
        let body = sse(&[
            json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5", "content": []}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "看一下目录"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "c2lnbmF0dXJl"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "ping"}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "先检查当前目录。"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_01", "name": "shell", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"command\": \"pwd\", "}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"cwd\": \"/srv\"}"}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_details": null}, "usage": {"output_tokens": 40}}),
            json!({"type": "message_stop"}),
        ]);
        let (url, handle) = server("text/event-stream", body);
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let channel = Channel::<Event>::new(move |body| {
            captured.lock().unwrap().push(body);
            Ok(())
        });
        let tools = super::super::tools::definitions();
        let history =
            [json!({"role": "user", "content": "部署", "context": "[当前权限模式：auto]"})];
        let request = provider::ChatRequest {
            model: "claude-opus-5-5",
            system: "你是运维助手",
            messages: &history,
            tools: Some(&tools),
            effort: Some("high"),
            metadata: None,
        };
        let message = provider::chat(
            &config(url),
            &request,
            Arc::new(AtomicBool::new(false)),
            &channel,
        )
        .await
        .unwrap();

        let (head, sent) = handle.join().unwrap();
        let head = head.to_lowercase();
        assert!(head.starts_with("post /v1/messages "), "{head}");
        assert!(head.contains("x-api-key: sk-test"));
        assert!(head.contains("anthropic-version: 2026-10-21"));
        assert!(head.contains("anthropic-beta: server-side-fallback-2026-07-01"));
        assert!(!head.contains("authorization:"));
        assert_eq!(sent["system"], "你是运维助手");
        assert_eq!(
            sent["messages"][0]["content"][0]["text"],
            "[当前权限模式：auto]"
        );
        assert_eq!(sent["messages"][0]["content"][1]["text"], "部署");
        assert_eq!(sent["output_config"]["effort"], "high");

        assert_eq!(message["content"], "先检查当前目录。");
        let arguments: Value = serde_json::from_str(
            message["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(arguments, json!({"command": "pwd", "cwd": "/srv"}));
        assert_eq!(message[CONTENT_KEY][0]["signature"], "c2lnbmF0dXJl");
        // The turn replays exactly as received.
        let mut next = history.to_vec();
        next.push(message.clone());
        assert_eq!(convert_messages(&next)[1]["content"], message[CONTENT_KEY]);
        assert!(
            events.lock().unwrap().len() >= 2,
            "thinking and text streamed to the UI"
        );
    }

    #[tokio::test]
    async fn the_model_list_comes_from_v1_models() {
        let body = json!({"data": [
            {"id": "claude-opus-5-5", "display_name": "Claude Opus 5.5", "max_tokens": 128000,
             "capabilities": {"thinking": {"types": {"adaptive": {"supported": true}}},
                              "effort": {"high": {"supported": true}, "max": {"supported": true}}}},
            {"id": "claude-haiku-4-5", "display_name": "Claude Haiku 4.5", "max_tokens": 64000,
             "capabilities": {"thinking": {"types": {"adaptive": {"supported": false}}}}}
        ], "has_more": false})
        .to_string();
        let (url, handle) = server("application/json", body);
        let models = provider::models_cancellable(&config(url), None)
            .await
            .unwrap();
        let (head, _) = handle.join().unwrap();
        assert!(head.starts_with("GET /v1/models?limit=1000 "), "{head}");
        assert!(head.to_lowercase().contains("x-api-key: sk-test"));
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].reasoning_efforts.len(), 2);
        assert_eq!(models[1].adaptive_thinking, Some(false));
    }
}
