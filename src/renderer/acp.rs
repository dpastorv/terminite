//! ACP event handling — route hosted-agent events into session state.

use super::*;

impl Renderer {
    /// Route one ACP event from a hosted agent in `tab_id` into that
    /// tab's session state, then ask for a redraw. The reader thread
    /// dispatches one event per inbound JSON-RPC frame; here we
    /// mutate `Turn`s and respond to fs callbacks.
    pub fn handle_acp_event(&mut self, tab_id: TabId, event: crate::acp::AcpEvent) {
        use crate::acp::AcpEvent;
        let mut tabs: Vec<&mut Tab> = Vec::new();
        self.root
            .as_mut()
            .expect("pane tree present")
            .all_tabs_mut(&mut tabs);
        let Some(tab) = tabs.into_iter().find(|t| t.id == tab_id) else {
            return;
        };
        let Some(session) = tab.acp_session.as_mut() else {
            return;
        };
        match event {
            AcpEvent::Initialized { agent_name, agent_version } => {
                crate::logging::info(&format!(
                    "acp tab {}: initialized ({agent_name}{})",
                    tab_id.0,
                    agent_version
                        .as_deref()
                        .map(|v| format!(" v{v}"))
                        .unwrap_or_default()
                ));
                // Open a session in the project root by default.
                let cwd = std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "/".to_string());
                session.send_new_session(&cwd);
            }
            AcpEvent::SessionCreated { session_id } => {
                crate::logging::info(&format!(
                    "acp tab {}: session {session_id}",
                    tab_id.0
                ));
                session.session_id = Some(session_id);
            }
            AcpEvent::UserMessageChunk(_) => {
                // We already pushed the user turn locally on send;
                // ignore the agent's echo to avoid duplicates.
            }
            AcpEvent::AgentMessageChunk(text) => {
                if let Some(turn) = session.turns.last_mut() {
                    if let crate::acp::Turn::Assistant { text: buf, streaming, .. } = turn {
                        buf.push_str(&text);
                        *streaming = true;
                    }
                }
            }
            AcpEvent::ToolCallStarted { id, title, kind } => {
                if let Some(crate::acp::Turn::Assistant { tool_calls, .. }) =
                    session.turns.last_mut()
                {
                    tool_calls.push(crate::acp::ToolCall {
                        id,
                        title,
                        kind,
                        status: crate::acp::ToolCallStatus::Pending,
                        output: None,
                    });
                }
            }
            AcpEvent::ToolCallUpdated { id, status, output } => {
                if let Some(crate::acp::Turn::Assistant { tool_calls, .. }) =
                    session.turns.last_mut()
                {
                    if let Some(tc) = tool_calls.iter_mut().find(|t| t.id == id) {
                        tc.status = status;
                        if let Some(o) = output {
                            tc.output = Some(o);
                        }
                    }
                }
            }
            AcpEvent::PermissionRequest { request_id, tool_call_title, options } => {
                let kinds: Vec<&str> = options.iter().map(|o| o.kind.as_str()).collect();
                crate::logging::info(&format!(
                    "acp tab {}: permission requested ({}) options=[{}]",
                    tab_id.0,
                    tool_call_title,
                    kinds.join(", "),
                ));
                session.pending_permission = Some(crate::acp::PermissionPrompt {
                    request_id,
                    title: tool_call_title,
                    options,
                });
                // Snap the pane to the bottom so the prompt is visible
                // — render_acp_body appends the permission block at the
                // end, and without this it lands below the viewport in
                // any conversation longer than a screen.
                tab.module_scroll_y = f32::MAX;
            }
            AcpEvent::FsReadRequest { request_id, path, line, limit } => {
                let result = read_text_for_agent(&path, line, limit);
                session.respond_fs_read(request_id, result);
            }
            AcpEvent::FsWriteRequest { request_id, path, content } => {
                // v1: defer to a future permission UI by writing
                // straight through. Real permission gating happens
                // when we add the inline modal — until then the
                // safer move would be to deny by default, but that
                // makes any agent unusable. Tracked as a follow-up.
                let result = write_text_for_agent(&path, &content);
                session.respond_fs_write(request_id, result);
            }
            AcpEvent::ProtocolError(message) => {
                crate::logging::warn(&format!("acp tab {}: {message}", tab_id.0));
            }
            AcpEvent::Stderr(_) => {} // logged in the reader thread already
            AcpEvent::Shutdown => {
                crate::logging::info(&format!("acp tab {}: shutdown", tab_id.0));
                session.awaiting_response = false;
                if let Some(crate::acp::Turn::Assistant { streaming, .. }) =
                    session.turns.last_mut()
                {
                    *streaming = false;
                }
            }
        }
        // Force content buffer rebuild on next render so the new
        // turn / chunk / tool-call status shows up.
        tab.content_buffer = None;
        self.window.request_redraw();
    }

}

// ── helpers moved from mod.rs ──────────────────────

pub(super) fn read_text_for_agent(
    path: &str,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String, String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {path}: {e}"))?;
    if meta.len() > ACP_FS_MAX_BYTES {
        return Err(format!(
            "{path}: {} bytes > cap {}",
            meta.len(),
            ACP_FS_MAX_BYTES
        ));
    }
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut buf = String::with_capacity(meta.len() as usize);
    f.read_to_string(&mut buf).map_err(|e| format!("read {path}: {e}"))?;
    if line.is_none() && limit.is_none() {
        return Ok(buf);
    }
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let mut out = String::new();
    for (i, l) in buf.lines().enumerate().skip(start) {
        if let Some(n) = limit {
            if (i - start) as u32 >= n {
                break;
            }
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(l);
    }
    Ok(out)
}

pub(super) fn write_text_for_agent(path: &str, content: &str) -> Result<(), String> {
    if content.len() as u64 > ACP_FS_MAX_BYTES {
        return Err(format!(
            "{path}: write payload {} bytes > cap {}",
            content.len(),
            ACP_FS_MAX_BYTES
        ));
    }
    let path_buf = std::path::PathBuf::from(path);
    if let Some(parent) = path_buf.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
    }
    std::fs::write(&path_buf, content).map_err(|e| format!("write {path}: {e}"))
}

/// Render an ACP session's turn list as a plain-text body the
/// existing non-shell content_buffer path can shape. Each turn gets
/// a `─── role ───` divider; tool calls show inline with status;
/// a trailing `> draft` line shows what the user is composing.
pub(super) fn render_acp_body(session: &crate::acp::AcpSession) -> String {
    use crate::acp::{ToolCallStatus, Turn};
    let mut out = String::new();
    let agent_label = "Agent";
    for turn in &session.turns {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        match turn {
            Turn::User { text } => {
                out.push_str("─── You ───\n");
                out.push_str(text);
            }
            Turn::Assistant { text, tool_calls, streaming } => {
                let header = if *streaming {
                    format!("─── {agent_label} (streaming…) ───")
                } else {
                    format!("─── {agent_label} ───")
                };
                out.push_str(&header);
                out.push('\n');
                if !text.is_empty() {
                    out.push_str(text);
                }
                for tc in tool_calls {
                    let marker = match tc.status {
                        ToolCallStatus::Pending => "○",
                        ToolCallStatus::InProgress => "◐",
                        ToolCallStatus::Completed => "●",
                        ToolCallStatus::Failed => "✗",
                    };
                    out.push_str(&format!(
                        "\n\n  {marker} [{}] {}",
                        tc.kind, tc.title
                    ));
                    if let Some(o) = &tc.output {
                        for line in o.lines() {
                            out.push_str("\n    ");
                            out.push_str(line);
                        }
                    }
                }
            }
        }
    }
    // Pending permission prompt — inline.
    if let Some(prompt) = &session.pending_permission {
        out.push_str("\n\n─── Permission requested ───\n");
        out.push_str(&prompt.title);
        out.push('\n');
        for (i, opt) in prompt.options.iter().enumerate() {
            let key: &str = match opt.kind.as_str() {
                "allow_once" => "a",
                "allow_always" => "A",
                "reject_once" => "r",
                "reject_always" => "R",
                _ => match i {
                    0 => "1",
                    1 => "2",
                    2 => "3",
                    _ => "4",
                },
            };
            out.push_str(&format!("\n  [{key}] {}", opt.name));
        }
    }
    // Composing draft at the bottom — what the user is typing.
    out.push_str("\n\n> ");
    out.push_str(&session.draft);
    out.push('_'); // cursor marker
    out
}


