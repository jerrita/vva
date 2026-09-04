use crate::config::AppConfig;
use crate::config::model::ModelCostConfig;
use crate::model::content::{Annotation, ContentPart};
use crate::model::session::Session;
use crate::model::turn::Turn;

/// UI 聊天流条目，由持久化 Turn 数据或流式 AgentEvent 解构映射而来
#[derive(Debug, Clone)]
pub enum ChatEntry {
    UserPrompt {
        text: String,
    },
    Thinking {
        content: String,
    },
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        result: String,
        is_error: bool,
    },
    AssistantMessage {
        text: String,
        citations: Vec<(String, String)>, // title, url
    },
    SystemNotice {
        text: String,
    },
}

/// 侧边栏展示的会话全局统计数据
#[derive(Debug, Clone, Default)]
pub struct SidebarStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub context_limit: Option<u64>,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub estimated_cost_usd: f64,
    pub total_turns: usize,
}

/// 计算侧边栏统计信息 (Token 消耗、缓存命中与预估美元花费)
pub fn compute_stats(session: &Session, app_config: Option<&AppConfig>) -> SidebarStats {
    let mut prompt_tok = 0u64;
    let mut comp_tok = 0u64;
    let mut tot_tok = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;

    for turn in &session.turns {
        if let Some(usage) = &turn.usage {
            prompt_tok += usage.prompt_tokens as u64;
            comp_tok += usage.completion_tokens as u64;
            tot_tok += usage.total_tokens as u64;
        }
        if let Some(anthropic) = &turn.meta.anthropic {
            if let Some(r) = anthropic.cache_read_input_tokens {
                cache_read += r as u64;
            }
            if let Some(w) = anthropic.cache_creation_input_tokens {
                cache_write += w as u64;
            }
        }
    }

    let mut context_limit = None;
    let mut cost_cfg: Option<ModelCostConfig> = None;

    if let Some(cfg) = app_config
        && let Ok(resolved) =
            cfg.resolve_model(&session.current_model, session.current_variant.as_deref())
    {
        context_limit = resolved
            .model_config
            .limit
            .as_ref()
            .map(|l| l.context as u64);
        cost_cfg = resolved.model_config.cost.clone();
    }

    let mut estimated_cost = 0.0;
    if let Some(cost) = cost_cfg {
        let in_cost = (prompt_tok as f64) * (cost.input / 1_000_000.0);
        let out_cost = (comp_tok as f64) * (cost.output / 1_000_000.0);
        let read_cost = (cache_read as f64) * (cost.cache_read.unwrap_or(0.0) / 1_000_000.0);
        let write_cost = (cache_write as f64) * (cost.cache_write.unwrap_or(0.0) / 1_000_000.0);
        estimated_cost = in_cost + out_cost + read_cost + write_cost;
    }

    SidebarStats {
        prompt_tokens: prompt_tok,
        completion_tokens: comp_tok,
        total_tokens: tot_tok,
        context_limit,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        estimated_cost_usd: estimated_cost,
        total_turns: session.turns.len(),
    }
}

/// 将历史持久化 Turn 数据投影转换为 UI 对话展现列表
pub fn entries_from_turns(turns: &[Turn]) -> Vec<ChatEntry> {
    let mut entries = Vec::new();

    for turn in turns {
        match turn.role {
            crate::model::Role::User => {
                for msg in &turn.messages {
                    let mut text_acc = String::new();
                    for part in &msg.parts {
                        if let ContentPart::Text { text, .. } = part {
                            if !text_acc.is_empty() {
                                text_acc.push('\n');
                            }
                            text_acc.push_str(text);
                        }
                    }
                    if !text_acc.is_empty() {
                        entries.push(ChatEntry::UserPrompt { text: text_acc });
                    }
                }
            }
            crate::model::Role::Assistant => {
                for msg in &turn.messages {
                    for part in &msg.parts {
                        match part {
                            ContentPart::Reasoning { content, .. } => {
                                entries.push(ChatEntry::Thinking {
                                    content: content.clone(),
                                });
                            }
                            ContentPart::Text { text, annotations } => {
                                let mut citations = Vec::new();
                                for ann in annotations {
                                    if let Annotation::UrlCitation { title, url, .. } = ann {
                                        citations.push((title.clone(), url.clone()));
                                    }
                                }
                                entries.push(ChatEntry::AssistantMessage {
                                    text: text.clone(),
                                    citations,
                                });
                            }
                            ContentPart::ToolCall(call) => {
                                entries.push(ChatEntry::ToolCall {
                                    name: call.name.clone(),
                                    args: call.arguments.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            crate::model::Role::Tool => {
                for msg in &turn.messages {
                    for part in &msg.parts {
                        if let ContentPart::ToolResult(res) = part {
                            entries.push(ChatEntry::ToolResult {
                                name: res.tool_call_id.clone(),
                                result: res.content.clone(),
                                is_error: res.is_error,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;
    use crate::model::message::Message;
    use crate::model::tool::{ToolCall, ToolResult};
    use crate::model::turn::TokenUsage;

    #[test]
    fn test_entries_from_turns_projection() {
        let m1 = Message::user("Hello world");
        let t1 = Turn::user_turn(m1, None);

        let m2 = Message::assistant(vec![
            ContentPart::reasoning("Thinking about greeting", None),
            ContentPart::Text {
                text: "Hi there!".into(),
                annotations: vec![Annotation::UrlCitation {
                    url: "https://example.com".into(),
                    title: "Example".into(),
                    start_index: 0,
                    end_index: 2,
                }],
            },
            ContentPart::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "test_tool".into(),
                arguments: "{}".into(),
            }),
        ]);
        let t2 = Turn::assistant_turn(vec![m2], None);

        let m3 = Message::new(
            Role::Tool,
            vec![ContentPart::ToolResult(ToolResult {
                tool_call_id: "call_1".into(),
                content: "done".into(),
                is_error: false,
            })],
        );
        let t3 = Turn::new(Role::Tool, vec![m3], None);

        let entries = entries_from_turns(&[t1, t2, t3]);
        assert_eq!(entries.len(), 5);

        match &entries[0] {
            ChatEntry::UserPrompt { text } => assert_eq!(text, "Hello world"),
            _ => panic!("Expected UserPrompt"),
        }
        match &entries[1] {
            ChatEntry::Thinking { content } => assert_eq!(content, "Thinking about greeting"),
            _ => panic!("Expected Thinking"),
        }
        match &entries[2] {
            ChatEntry::AssistantMessage { text, citations } => {
                assert_eq!(text, "Hi there!");
                assert_eq!(citations.len(), 1);
                assert_eq!(citations[0].0, "Example");
            }
            _ => panic!("Expected AssistantMessage"),
        }
        match &entries[3] {
            ChatEntry::ToolCall { name, args } => {
                assert_eq!(name, "test_tool");
                assert_eq!(args, "{}");
            }
            _ => panic!("Expected ToolCall"),
        }
        match &entries[4] {
            ChatEntry::ToolResult {
                name,
                result,
                is_error,
            } => {
                assert_eq!(name, "call_1");
                assert_eq!(result, "done");
                assert!(!is_error);
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn test_compute_stats_calculation() {
        let mut session = Session::with_model(
            std::path::PathBuf::from("/ws"),
            None,
            "model_a",
            None,
            false,
        );
        let mut turn = Turn::assistant_turn(vec![], None);
        turn.usage = Some(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cached_tokens: None,
            reasoning_tokens: None,
        });
        session.turns.push(turn);

        let stats = compute_stats(&session, None);
        assert_eq!(stats.prompt_tokens, 100);
        assert_eq!(stats.completion_tokens, 50);
        assert_eq!(stats.total_tokens, 150);
        assert_eq!(stats.total_turns, 1);
        assert_eq!(stats.estimated_cost_usd, 0.0);
    }
}
