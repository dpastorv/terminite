//! Inbound wire decode — JSON-RPC messages -> AcpEvent.

use super::*;

/// Map one inbound JSON-RPC message into zero or more AcpEvents. A
/// `session/update` may carry several semantic events (e.g. a
/// tool_call_update with both status and content); we emit one event
/// per semantic chunk.
pub(super) fn classify_message(msg: &Value) -> Vec<AcpEvent> {
    // Method-bearing message = notification or request from agent.
    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let request_id = msg.get("id").cloned();
        return match method {
            "session/update" => classify_session_update(&params),
            "session/request_permission" => classify_permission_request(request_id, &params),
            "fs/read_text_file" => classify_fs_read(request_id, &params),
            "fs/write_text_file" => classify_fs_write(request_id, &params),
            _ => Vec::new(), // unknown methods silently dropped
        };
    }
    // No method = response to one of our requests.
    if let Some(result) = msg.get("result") {
        return classify_response(result);
    }
    if let Some(error) = msg.get("error") {
        let m = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("(no message)");
        return vec![AcpEvent::ProtocolError(m.to_string())];
    }
    Vec::new()
}

fn classify_response(result: &Value) -> Vec<AcpEvent> {
    // initialize response carries agentInfo + protocolVersion.
    if let Some(info) = result.get("agentInfo") {
        let agent_name = info
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("agent")
            .to_string();
        let agent_version = info
            .get("version")
            .and_then(|v| v.as_str())
            .map(String::from);
        return vec![AcpEvent::Initialized { agent_name, agent_version }];
    }
    if let Some(sid) = result.get("sessionId").and_then(|v| v.as_str()) {
        return vec![AcpEvent::SessionCreated {
            session_id: sid.to_string(),
        }];
    }
    Vec::new()
}

fn classify_session_update(params: &Value) -> Vec<AcpEvent> {
    let Some(update) = params.get("update") else {
        return Vec::new();
    };
    let kind = update
        .get("sessionUpdate")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match kind {
        "user_message_chunk" => {
            extract_content_text(update.get("content"))
                .map(|t| vec![AcpEvent::UserMessageChunk(t)])
                .unwrap_or_default()
        }
        "agent_message_chunk" => {
            extract_content_text(update.get("content"))
                .map(|t| vec![AcpEvent::AgentMessageChunk(t)])
                .unwrap_or_default()
        }
        "tool_call" => {
            let id = update.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let title = update.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let kind_s = update.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
            vec![AcpEvent::ToolCallStarted { id, title, kind: kind_s }]
        }
        "tool_call_update" => {
            let id = update.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let status = match update.get("status").and_then(|v| v.as_str()).unwrap_or("") {
                "pending" => ToolCallStatus::Pending,
                "in_progress" => ToolCallStatus::InProgress,
                "completed" => ToolCallStatus::Completed,
                "failed" => ToolCallStatus::Failed,
                _ => ToolCallStatus::InProgress,
            };
            // `content` is an array of content blocks; concat any text bits.
            let output = update
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    let mut out = String::new();
                    for entry in arr {
                        if let Some(inner) = entry.get("content") {
                            if let Some(text) = extract_content_text(Some(inner)) {
                                if !out.is_empty() { out.push('\n'); }
                                out.push_str(&text);
                            }
                        }
                    }
                    out
                })
                .filter(|s| !s.is_empty());
            vec![AcpEvent::ToolCallUpdated { id, status, output }]
        }
        _ => Vec::new(),
    }
}

fn classify_permission_request(request_id: Option<Value>, params: &Value) -> Vec<AcpEvent> {
    let request_id = request_id.unwrap_or(Value::Null);
    let tool_call_title = params
        .get("toolCall")
        .and_then(|tc| tc.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("Permission requested")
        .to_string();
    let options = params
        .get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(PermissionOption {
                        option_id: o.get("optionId").and_then(|v| v.as_str())?.to_string(),
                        name: o.get("name").and_then(|v| v.as_str())?.to_string(),
                        kind: o.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    vec![AcpEvent::PermissionRequest { request_id, tool_call_title, options }]
}

fn classify_fs_read(request_id: Option<Value>, params: &Value) -> Vec<AcpEvent> {
    let request_id = request_id.unwrap_or(Value::Null);
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let line = params.get("line").and_then(|v| v.as_u64()).map(|n| n as u32);
    let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32);
    vec![AcpEvent::FsReadRequest { request_id, path, line, limit }]
}

fn classify_fs_write(request_id: Option<Value>, params: &Value) -> Vec<AcpEvent> {
    let request_id = request_id.unwrap_or(Value::Null);
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content = params
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    vec![AcpEvent::FsWriteRequest { request_id, path, content }]
}

fn extract_content_text(v: Option<&Value>) -> Option<String> {
    let v = v?;
    let kind = v.get("type").and_then(|x| x.as_str())?;
    if kind == "text" {
        v.get("text").and_then(|x| x.as_str()).map(String::from)
    } else {
        None
    }
}

// ── Available agent presets ─────────────────────────────────────────────

