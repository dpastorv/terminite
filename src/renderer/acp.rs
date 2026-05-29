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
