//! Proto (Unix socket) request handling — verb dispatch and handlers.

use super::*;

impl Renderer {
    /// A new module connected — drop any prior subscriber (v1 = single
    /// client; the new one wins).
    pub fn handle_proto_connect(&mut self) {
        self.proto_subscriber = None;
    }

    /// The module disconnected — clear the subscriber slot.
    pub fn handle_proto_disconnect(&mut self) {
        self.proto_subscriber = None;
    }

    /// Handle one parsed request from the proto socket.
    pub fn handle_proto_request(
        &mut self,
        req: crate::proto::Request,
        out: std::sync::mpsc::SyncSender<crate::proto::OutMessage>,
    ) {
        let payload = match req.method.as_str() {
            "list_tabs" => self.proto_list_tabs(),
            "list_blocks" => self.proto_list_blocks(&req.params),
            "get_block" => self.proto_get_block(&req.params),
            "subscribe" => {
                self.proto_subscriber = Some(out.clone());
                crate::proto::OutPayload::Subscribed
            }
            "set_tag" => self.proto_set_tag(&req.params),
            "remove_tag" => self.proto_remove_tag(&req.params),
            "cursor_at" => self.proto_cursor_at(&req.params),
            "cursor_clear" => self.proto_cursor_clear(&req.params),
            "export_tab" => self.proto_export_tab(&req.params),
            "stats" => self.proto_stats(),
            "list_modules" => crate::proto::OutPayload::Modules {
                modules: self.modules.list().to_vec(),
            },
            "reload_modules" => {
                self.reload_modules();
                crate::proto::OutPayload::Modules {
                    modules: self.modules.list().to_vec(),
                }
            }
            other => crate::proto::OutPayload::Error {
                message: format!("unknown method: {other}"),
            },
        };
        let _ = out.try_send(crate::proto::OutMessage { id: req.id, payload });
    }

    fn proto_list_tabs(&self) -> crate::proto::OutPayload {
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let tabs = all
            .iter()
            .map(|t| crate::proto::TabInfo {
                tab_id: t.id.0,
                title: t.title.clone(),
            })
            .collect();
        crate::proto::OutPayload::Tabs { tabs }
    }

    fn proto_list_blocks(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let blocks = tab.blocks.iter().map(block_to_info).collect();
        let cursor = tab.blocks.cursor();
        crate::proto::OutPayload::Blocks { blocks, cursor }
    }

    fn proto_mutate_tab<F>(&mut self, params: &serde_json::Value, f: F) -> crate::proto::OutPayload
    where
        F: FnOnce(&mut Tab) -> crate::proto::OutPayload,
    {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&mut Tab> = Vec::new();
        self.root
            .as_mut()
            .expect("pane tree present")
            .all_tabs_mut(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        f(tab)
    }

    fn proto_set_tag(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let Some(tag) = params.get("tag").and_then(|v| v.as_str()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tag".into(),
            };
        };
        let tag = tag.to_string();
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.add_tag(block_id, &tag) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("could not add tag {tag:?} to block {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    fn proto_remove_tag(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let Some(tag) = params.get("tag").and_then(|v| v.as_str()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tag".into(),
            };
        };
        let tag = tag.to_string();
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.remove_tag(block_id, &tag) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("tag {tag:?} not present on block {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    fn proto_cursor_at(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(block_id) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let block_id = block_id as u32;
        let payload = self.proto_mutate_tab(params, |tab| {
            if tab.blocks.set_cursor(block_id) {
                crate::proto::OutPayload::Ok
            } else {
                crate::proto::OutPayload::Error {
                    message: format!("no block with id {block_id}"),
                }
            }
        });
        self.window.request_redraw();
        payload
    }

    fn proto_cursor_clear(&mut self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let payload = self.proto_mutate_tab(params, |tab| {
            tab.blocks.clear_cursor();
            crate::proto::OutPayload::Ok
        });
        self.window.request_redraw();
        payload
    }

    fn proto_get_block(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        let Some(block_id_u64) = params.get("block_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid block_id".into(),
            };
        };
        let tab_id = TabId(tab_id_u64);
        let block_id = block_id_u64 as u32;
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let Some(block) = tab.blocks.find(block_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no block with id {block_id} in tab {tab_id_u64}"),
            };
        };
        let command = block_command_text(tab, block).unwrap_or_default();
        let output = block_output_text(tab, block).unwrap_or_default();
        crate::proto::OutPayload::Block {
            block: crate::proto::BlockData {
                info: block_to_info(block),
                command,
                output,
            },
        }
    }

    fn proto_export_tab(&self, params: &serde_json::Value) -> crate::proto::OutPayload {
        let Some(tab_id_u64) = params.get("tab_id").and_then(|v| v.as_u64()) else {
            return crate::proto::OutPayload::Error {
                message: "missing or invalid tab_id".into(),
            };
        };
        // Optional `since` — include only blocks with id >= since. Lets
        // the partner stream a session in chunks instead of always
        // exporting from the beginning.
        let since: Option<u32> = params
            .get("since")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let tab_id = TabId(tab_id_u64);
        let mut all: Vec<&Tab> = Vec::new();
        self.root.as_ref().expect("pane tree present").all_tabs(&mut all);
        let Some(tab) = all.into_iter().find(|t| t.id == tab_id) else {
            return crate::proto::OutPayload::Error {
                message: format!("no tab with id {tab_id_u64}"),
            };
        };
        let blocks: Vec<crate::proto::BlockData> = tab
            .blocks
            .iter()
            .filter(|b| since.is_none_or(|s| b.id >= s))
            .map(|b| crate::proto::BlockData {
                info: block_to_info(b),
                command: block_command_text(tab, b).unwrap_or_default(),
                output: block_output_text(tab, b).unwrap_or_default(),
            })
            .collect();
        crate::proto::OutPayload::Export {
            tab_id: tab_id_u64,
            blocks,
        }
    }

    fn proto_stats(&self) -> crate::proto::OutPayload {
        let mut all: Vec<&Tab> = Vec::new();
        self.root
            .as_ref()
            .expect("pane tree present")
            .all_tabs(&mut all);
        let tabs: Vec<crate::proto::TabStats> = all
            .iter()
            .map(|t| crate::proto::TabStats {
                tab_id: t.id.0,
                title: t.title.clone(),
                cols: t.cols,
                rows: t.rows,
                blocks: t.blocks.iter().count(),
                open_block: t.blocks.open_id(),
                cursor_block: t.blocks.cursor(),
                has_image: t.image.is_some(),
            })
            .collect();

        // Frame stats — simple sort to find p99. Sample count caps at
        // `FRAME_TIMER_CAP`, so the sort is O(n log n) on a small n.
        let samples: Vec<f32> = self.frame_samples.iter().copied().collect();
        let (avg_ms, p99_ms, max_ms) = if samples.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let sum: f32 = samples.iter().sum();
            let avg = sum / samples.len() as f32;
            let mut sorted = samples.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let p99_idx = ((sorted.len() as f32 * 0.99) as usize).min(sorted.len() - 1);
            let p99 = sorted[p99_idx];
            let max = sorted[sorted.len() - 1];
            (avg, p99, max)
        };

        crate::proto::OutPayload::Stats(crate::proto::StatsPayload {
            version: env!("CARGO_PKG_VERSION"),
            peak_rss_bytes: process_rss_peak_bytes(),
            frame: crate::proto::FrameStats {
                frames_observed: self.frame_count,
                recent_samples: samples.len(),
                avg_ms,
                p99_ms,
                max_ms,
            },
            tabs,
            subscriber_connected: self.proto_subscriber.is_some(),
        })
    }

    pub(super) fn proto_emit_event(&mut self, event: crate::proto::EventPayload) {
        let Some(out) = self.proto_subscriber.as_ref() else { return };
        let msg = crate::proto::OutMessage {
            id: 0,
            payload: crate::proto::OutPayload::Event(event),
        };
        if out.try_send(msg).is_err() {
            // Disconnected or queue overflowed — drop the subscriber
            // rather than let it stall the main thread.
            self.proto_subscriber = None;
        }
    }
}
