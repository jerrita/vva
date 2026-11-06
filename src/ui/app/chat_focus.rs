//! 键盘聊天浏览：按组件身份定位，重排与流式追加不改变操作目标。

use std::ops::Range;

use super::{App, ToolExpand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatTarget {
    User(usize),
    Response(String),
    Tool(String),
    Thought(String),
    Citations(String),
}

pub(crate) struct ChatItem {
    pub target: ChatTarget,
    pub lines: Range<usize>,
}

impl App {
    pub(crate) fn chat_items(&self) -> Vec<ChatItem> {
        let cache = &self.chat_cache;
        let mut items: Vec<ChatItem> = Vec::new();
        for hit in &cache.response_hits {
            let target = ChatTarget::Response(hit.id.clone());
            let end = if hit.collapsed {
                cache
                    .user_hits
                    .iter()
                    .find(|user| user.line_start > hit.line_start)
                    .map_or(self.chat.lines.len(), |user| user.line_start)
            } else {
                hit.line_end
            };
            if let Some(item) = items.last_mut().filter(|item| item.target == target) {
                item.lines.end = end;
            } else {
                items.push(ChatItem {
                    target,
                    lines: hit.line_start..end,
                });
            }
        }
        items.extend(cache.user_hits.iter().map(|hit| ChatItem {
            target: ChatTarget::User(hit.turn_index),
            lines: hit.line_start..hit.line_end,
        }));
        items.extend(cache.tool_hits.iter().map(|hit| ChatItem {
            target: ChatTarget::Tool(hit.id.clone()),
            lines: hit.header_start..hit.header_end.max(hit.block_end),
        }));
        items.extend(cache.thought_hits.iter().map(|hit| ChatItem {
            target: ChatTarget::Thought(hit.id.clone()),
            lines: hit.header_start..hit.header_end.max(hit.body_end),
        }));
        items.extend(cache.citations_hits.iter().map(|hit| ChatItem {
            target: ChatTarget::Citations(hit.id.clone()),
            lines: hit.header_start..hit.block_end,
        }));
        items.retain(|item| !item.lines.is_empty());
        items.sort_by_key(|item| item.lines.start);
        items
    }

    pub(crate) fn focused_chat_item(&self) -> Option<ChatItem> {
        let target = self.chat_focus.as_ref()?;
        self.chat_items()
            .into_iter()
            .find(|item| &item.target == target)
    }

    pub(crate) fn move_chat_focus(&mut self, forward: bool) {
        let items = self.chat_items();
        let index = items
            .iter()
            .position(|item| Some(&item.target) == self.chat_focus.as_ref())
            .expect("chat focus points to a rendered item");
        let next = if forward {
            (index + 1).min(items.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.chat_focus = Some(items[next].target.clone());
        self.chat_focus_needs_reveal = true;
    }

    pub(crate) fn activate_chat_item(&mut self, item: &ChatItem, collapse: bool) {
        match &item.target {
            ChatTarget::User(_) if !collapse => self.open_message_action_at_line(item.lines.start),
            ChatTarget::User(_) => {}
            ChatTarget::Response(id) => {
                let hit = self
                    .chat_cache
                    .response_hits
                    .iter()
                    .find(|hit| &hit.id == id)
                    .expect("focused response is rendered");
                if hit.collapsible {
                    self.response_expand.insert(id.clone(), !collapse);
                }
            }
            ChatTarget::Tool(id) => {
                let hit = self
                    .chat_cache
                    .tool_hits
                    .iter()
                    .find(|hit| &hit.id == id)
                    .expect("focused tool is rendered");
                let mode = self.tool_expand_at(id, hit.full_only);
                if collapse {
                    self.collapse_tool(id, hit.full_only);
                } else if mode == ToolExpand::Preview && hit.truncated {
                    self.handle_tool_click(hit.block_start);
                } else if mode == ToolExpand::Hidden {
                    self.handle_tool_click(hit.header_start);
                }
            }
            ChatTarget::Thought(id) => {
                self.thought_expand.insert(id.clone(), !collapse);
            }
            ChatTarget::Citations(id) => {
                self.citations_expand.insert(id.clone(), !collapse);
            }
        }
    }
}
