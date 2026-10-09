//! Rollover：上下文达到预留线后的摘要与会话切换。
//!
//! 不原地改写历史：回合结束后若上下文逼近窗口，由主模型在**同一会话末尾**追加压缩
//! prompt（模型 / 工具 / 推理参数与上一轮完全一致，复用 cache 前缀），摘要作为追加轮次
//! 留在旧会话；随后新建会话承接——复制旧会话的 system 前缀，首条动态内容为摘要
//! reminder，其后是最近 K 个完整用户轮次原文。旧会话写 `superseded_by`，其余内容只增不改。

use std::sync::Arc;

use crate::config::{AgentSpaceConfig, AppConfig};
use crate::harness::{ManagedSession, SessionManager};
use crate::model::{ContentPart, Message, Role, Session, Turn};

/// 追加到旧会话末尾的压缩 prompt
pub const COMPRESS_PROMPT: &str = "\
请对到目前为止的整个对话做一次上下文压缩，产出供后续会话承接的摘要。\
本次回复不得调用任何工具，只输出摘要正文，按以下分节书写（某节没有内容时写“无”）：

## 目标
## 偏好
## 关键事实
## 约束
## 决策
## 待办
## 未决问题

要求：只保留继续工作所需的信息；保留具体的路径、标识符、数值与命令；\
不确定或未经验证的信息标注“未确认”；不要编造，不要复述寒暄。";

/// rollover 完成后新旧会话 id；上层据此切换 binding focus 与 message_index
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rolled {
    pub old_id: String,
    pub new_id: String,
}

/// 当前上下文 token 数：最近一个带 usage 的 assistant 轮次的输入 + 输出
/// （Anthropic 的 input 不含缓存读写，需另行计入）；尚无 usage 时为 `None`
pub fn context_tokens(session: &Session) -> Option<u64> {
    let turn = session
        .turns
        .iter()
        .rev()
        .find(|turn| turn.role == Role::Assistant && turn.usage.is_some())?;
    let usage = turn.usage.as_ref()?;
    let mut tokens = usage.prompt_tokens as u64 + usage.completion_tokens as u64;
    if let Some(anthropic) = &turn.meta.anthropic {
        tokens += usage.cached_tokens.unwrap_or_default() as u64
            + anthropic.cache_creation_input_tokens.unwrap_or_default() as u64;
    }
    Some(tokens)
}

/// 回合结束后的阈值判定：`上下文 tokens >= limit.context - rollover_reserve_tokens`。
/// 模型缺失 `limit.context` 或预留线超过窗口时显式报错，不猜测；已被接替的会话不再触发。
pub fn should_rollover(
    app: &AppConfig,
    config: &AgentSpaceConfig,
    session: &Session,
) -> Result<bool, String> {
    if session.header.superseded_by.is_some() {
        return Ok(false);
    }
    let resolved = app
        .resolve_model(
            &session.header.current_model,
            session.header.current_variant.as_deref(),
        )
        .map_err(|e| e.to_string())?;
    let limit = resolved
        .model_config
        .limit
        .as_ref()
        .ok_or_else(|| {
            format!(
                "模型 {} 未声明 limit.context，无法判定 rollover",
                resolved.full_spec
            )
        })?
        .context as u64;
    let line = limit
        .checked_sub(config.rollover_reserve_tokens)
        .ok_or_else(|| {
            format!(
                "rollover_reserve_tokens ({}) 超过模型 {} 的上下文窗口 ({limit})",
                config.rollover_reserve_tokens, resolved.full_spec
            )
        })?;
    Ok(context_tokens(session).is_some_and(|tokens| tokens >= line))
}

/// 回合结束后调用：达到预留线则 rollover，返回新旧 id；未达到返回 `None`
pub async fn maybe_rollover(
    manager: &Arc<SessionManager>,
    config: &AgentSpaceConfig,
    handle: &Arc<ManagedSession>,
) -> Result<Option<Rolled>, String> {
    let session = handle.snapshot().session.clone();
    if !should_rollover(&manager.app_config(), config, &session)? {
        return Ok(None);
    }
    rollover(manager, config.rollover_keep_turns, handle)
        .await
        .map(Some)
}

/// 压缩并切换到接替会话。失败时旧会话回到调用前的内容（追加的压缩轮次被回退）。
pub async fn rollover(
    manager: &Arc<SessionManager>,
    keep_turns: usize,
    handle: &Arc<ManagedSession>,
) -> Result<Rolled, String> {
    if handle.is_running() {
        return Err("会话正在运行，无法 rollover".into());
    }
    let before = handle.snapshot().session.turns.len();
    let summary = compress(handle, before).await?;
    let old = handle.snapshot().session.clone();
    let successor = manager
        .adopt_session(successor_session(&old, before, &summary, keep_turns))
        .await?;
    if let Err(error) = handle.mark_superseded(&successor.id).await {
        // 旧会话未能标记：撤销新会话，避免出现两个活跃分支
        let _ = manager.delete_session(&successor).await;
        return Err(format!("标记旧会话 superseded_by 失败: {error}"));
    }
    Ok(Rolled {
        old_id: old.header.id.clone(),
        new_id: successor.id.clone(),
    })
}

/// 在旧会话末尾追加压缩 prompt 并取回摘要；任何失败都回退追加的轮次
async fn compress(handle: &Arc<ManagedSession>, before: usize) -> Result<String, String> {
    let outcome = handle.deliver_wait(COMPRESS_PROMPT).await;
    let session = handle.snapshot().session.clone();
    let failure = match outcome {
        Err(error) => Some(format!("压缩请求失败: {error}")),
        Ok(_) if calls_tool(&session.turns[before..]) => {
            Some("压缩请求发起了工具调用，视为失败".into())
        }
        Ok(report) if report.text.trim().is_empty() => Some("压缩请求未返回摘要".into()),
        Ok(report) => return Ok(report.text.trim().to_string()),
    };
    if session.turns.len() > before
        && let Err(error) = handle.revert(before).await
    {
        tracing::warn!("Rollover failed to revert compression turns: {error}");
    }
    Err(failure.expect("failure recorded"))
}

fn calls_tool(turns: &[Turn]) -> bool {
    turns
        .iter()
        .flat_map(|turn| &turn.messages)
        .flat_map(|message| &message.parts)
        .any(|part| matches!(part, ContentPart::ToolCall(_)))
}

/// 注入内容（system 与 reminder）不是用户轮次边界
fn is_round_start(turn: &Turn) -> bool {
    turn.role == Role::User && !crate::system::is_reminder_text(&turn.text_content())
}

/// 构造接替会话（纯函数）：仅读取旧会话压缩前的 `turns[..before]`。
/// 轮次 = system 前缀 + 摘要 reminder + 最近 `keep_turns` 个以用户轮次起始的完整轮次。
/// 轮次边界取在用户轮次之前，工具调用与结果因同属一轮而不被拆散。
pub fn successor_session(
    old: &Session,
    before: usize,
    summary: &str,
    keep_turns: usize,
) -> Session {
    let turns = &old.turns[..before];
    let prefix = turns
        .iter()
        .take_while(|turn| turn.role == Role::System || !is_round_start(turn))
        .count();
    let starts: Vec<usize> = (prefix..before)
        .filter(|&index| is_round_start(&turns[index]))
        .collect();
    let kept_from = if keep_turns == 0 {
        before
    } else {
        starts
            .get(starts.len().saturating_sub(keep_turns))
            .copied()
            .unwrap_or(before)
    };

    let header = &old.header;
    let mut session = Session::with_model(
        header.workspace_root.clone(),
        header.title.clone(),
        header.current_model.clone(),
        header.current_variant.clone(),
        old.toggles(),
    );
    session.header.editor = header.editor;
    session.header.parent = Some(header.id.clone());
    session.header.kind = header.kind;
    session.header.origin = header.origin.clone();
    session.header.topic = header.topic.clone();
    session.header.routed_by = header.routed_by.clone();
    session.header.last_activity = Some(chrono::Utc::now());

    session.turns.extend(turns[..prefix].iter().cloned());
    session.turns.push(Turn::new(
        Role::User,
        vec![Message::text(
            crate::system::format_rollover_summary_reminder(&header.id, summary),
        )],
    ));
    session
        .turns
        .extend(turns[kept_from..].iter().cloned().map(detach_from_old));
    session
}

/// 复制到新会话的轮次脱离旧会话的计费与服务端链：usage 留在旧会话（避免成本重复计入），
/// Responses 的 response_id 链锚点指向旧会话的完整上下文，沿用会让摘要失效
fn detach_from_old(mut turn: Turn) -> Turn {
    turn.usage = None;
    if let Some(response) = &mut turn.meta.response {
        response.response_id = None;
        response.previous_response_id = None;
    }
    turn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelConfig, ProviderConfig, model::ModelLimitConfig};
    use crate::model::{AnthropicTurnMeta, ResponseTurnMeta, TokenUsage, ToolCall, ToolResult};
    use crate::storage::jsonl::JsonlSessionStore;

    fn user(text: &str) -> Turn {
        Turn::user_turn(Message::text(text))
    }

    fn assistant(text: &str, prompt_tokens: u32) -> Turn {
        let mut turn = Turn::assistant_turn(vec![Message::text(text)]);
        turn.usage = Some(TokenUsage {
            prompt_tokens,
            completion_tokens: 10,
            total_tokens: prompt_tokens + 10,
            ..Default::default()
        });
        turn
    }

    /// 带工具调用 / 结果配对的一轮
    fn tool_round(text: &str, id: &str) -> Vec<Turn> {
        let mut call = Turn::assistant_turn(vec![Message::text("working")]);
        call.messages[0]
            .parts
            .push(ContentPart::ToolCall(ToolCall::new(id, "Read", "{}")));
        let result = Turn::new(
            Role::Tool,
            vec![Message::tool_result(ToolResult::success(id, "ok"))],
        );
        vec![user(text), call, result, assistant("done", 100)]
    }

    fn session_with(turns: Vec<Turn>) -> Session {
        let mut session = Session::with_model(
            "/ws",
            Some("t".into()),
            "p/m",
            None,
            crate::model::SessionToggles::default(),
        );
        session.turns = turns;
        session
    }

    fn config(context: Option<usize>) -> AppConfig {
        let mut app = AppConfig::default();
        app.providers.insert(
            "p".into(),
            ProviderConfig {
                models: [(
                    "m".to_string(),
                    ModelConfig {
                        name: "M".into(),
                        limit: context.map(|context| ModelLimitConfig {
                            context,
                            output: 1000,
                        }),
                        ..Default::default()
                    },
                )]
                .into(),
                ..Default::default()
            },
        );
        app
    }

    #[test]
    fn threshold_is_inclusive_and_uses_latest_usage() {
        let cfg = AgentSpaceConfig {
            rollover_reserve_tokens: 200,
            ..Default::default()
        };
        // 上下文 = prompt + completion(10)；线 = 1000 - 200 = 800
        let at = |prompt| session_with(vec![user("q"), assistant("a", prompt)]);
        assert!(!should_rollover(&config(Some(1000)), &cfg, &at(789)).unwrap());
        assert!(should_rollover(&config(Some(1000)), &cfg, &at(790)).unwrap());
        // 只看最近一轮，旧的大 usage 不计
        let session = session_with(vec![
            user("q"),
            assistant("a", 900),
            user("r"),
            assistant("b", 10),
        ]);
        assert!(!should_rollover(&config(Some(1000)), &cfg, &session).unwrap());
        // 尚无 usage 不判定
        assert!(
            !should_rollover(&config(Some(1000)), &cfg, &session_with(vec![user("q")])).unwrap()
        );
    }

    #[test]
    fn anthropic_cache_tokens_count_toward_context() {
        let mut turn = assistant("a", 5);
        turn.usage.as_mut().unwrap().cached_tokens = Some(700);
        turn.meta.anthropic = Some(AnthropicTurnMeta {
            cache_creation_input_tokens: Some(80),
            ..Default::default()
        });
        assert_eq!(
            context_tokens(&session_with(vec![user("q"), turn])),
            Some(5 + 10 + 700 + 80)
        );
    }

    #[test]
    fn missing_limit_or_oversized_reserve_is_explicit_error() {
        let session = session_with(vec![user("q"), assistant("a", 1)]);
        let cfg = AgentSpaceConfig::default();
        assert!(
            should_rollover(&config(None), &cfg, &session)
                .unwrap_err()
                .contains("limit.context")
        );
        let small = config(Some(1000));
        assert!(
            should_rollover(&small, &cfg, &session)
                .unwrap_err()
                .contains("rollover_reserve_tokens")
        );
    }

    #[test]
    fn superseded_session_never_triggers() {
        let mut session = session_with(vec![user("q"), assistant("a", 999)]);
        session.header.superseded_by = Some("sess_new00".into());
        let cfg = AgentSpaceConfig {
            rollover_reserve_tokens: 10,
            ..Default::default()
        };
        assert!(!should_rollover(&config(Some(1000)), &cfg, &session).unwrap());
    }

    fn prefixed(rounds: Vec<Vec<Turn>>) -> Session {
        let mut turns = vec![
            Turn::new(Role::System, vec![Message::text("SYSTEM Date: 2026-01-01")]),
            user("<system-reminder type=\"skills\">\nx\n</system-reminder>"),
        ];
        turns.extend(rounds.into_iter().flatten());
        session_with(turns)
    }

    #[test]
    fn successor_keeps_prefix_summary_and_last_k_rounds_at_user_boundaries() {
        let old = prefixed(vec![
            tool_round("r1", "c1"),
            tool_round("r2", "c2"),
            tool_round("r3", "c3"),
        ]);
        let before = old.turns.len();
        let new = successor_session(&old, before, "SUMMARY", 2);

        assert_eq!(new.header.parent.as_deref(), Some(old.header.id.as_str()));
        assert_ne!(new.header.id, old.header.id);
        // system 前缀逐字复制（日期随之冻结）
        assert_eq!(new.turns[0], old.turns[0]);
        assert_eq!(new.turns[1], old.turns[1]);
        assert!(new.turns[2].text_content().contains("SUMMARY"));
        assert!(
            new.turns[2]
                .text_content()
                .starts_with("<system-reminder type=\"rollover-summary\"")
        );
        // 保留最近 2 轮（r2、r3），首个保留轮次是用户轮次
        let kept = &new.turns[3..];
        assert_eq!(kept.len(), 8);
        assert_eq!(kept[0].text_content(), "r2");
        assert_eq!(kept[4].text_content(), "r3");
        // 工具调用与结果配对完整
        let ids = |role, f: fn(&ContentPart) -> Option<&str>| -> Vec<String> {
            kept.iter()
                .filter(|t| t.role == role)
                .flat_map(|t| &t.messages)
                .flat_map(|m| &m.parts)
                .filter_map(|p| f(p).map(String::from))
                .collect()
        };
        let calls = ids(Role::Assistant, |p| match p {
            ContentPart::ToolCall(c) => Some(&c.id),
            _ => None,
        });
        let results = ids(Role::Tool, |p| match p {
            ContentPart::ToolResult(r) => Some(&r.tool_call_id),
            _ => None,
        });
        assert_eq!(calls, ["c2", "c3"]);
        assert_eq!(calls, results);
        // 旧会话原样未动
        assert_eq!(old.turns.len(), before);
    }

    #[test]
    fn successor_edge_cases_for_keep_count() {
        let old = prefixed(vec![tool_round("r1", "c1"), tool_round("r2", "c2")]);
        let before = old.turns.len();
        // K 大于现有轮次：全部保留；K = 0：仅前缀与摘要
        assert_eq!(
            successor_session(&old, before, "s", 9).turns.len(),
            2 + 1 + 8
        );
        assert_eq!(successor_session(&old, before, "s", 0).turns.len(), 3);
        // 压缩轮次（before 之后）不会被复制
        let mut with_compress = old.clone();
        with_compress.turns.push(user(COMPRESS_PROMPT));
        with_compress.turns.push(assistant("SUMMARY", 1));
        let new = successor_session(&with_compress, before, "s", 1);
        assert!(
            new.turns
                .iter()
                .all(|t| !t.text_content().contains("上下文压缩"))
        );
        assert_eq!(new.turns.last().unwrap().text_content(), "done");
    }

    #[test]
    fn reminder_injections_do_not_split_rounds() {
        // 任务完成 reminder 夹在一轮之内，不是边界
        let mut round = tool_round("r1", "c1");
        round.insert(
            3,
            user("<system-reminder type=\"background-task\">\nfinished\n</system-reminder>"),
        );
        let old = prefixed(vec![tool_round("r0", "c0"), round]);
        let new = successor_session(&old, old.turns.len(), "s", 1);
        assert_eq!(new.turns[3].text_content(), "r1");
        assert_eq!(new.turns.len(), 3 + 5);
    }

    #[test]
    fn copied_turns_drop_usage_and_response_chain_anchor() {
        let mut copied = assistant("a", 123);
        let mut meta = ResponseTurnMeta::new("resp_old");
        meta.previous_response_id = Some("resp_older".into());
        copied.meta.response = Some(meta);
        let old = prefixed(vec![vec![user("q"), copied]]);
        let new = successor_session(&old, old.turns.len(), "s", 1);
        let turn = new.turns.last().unwrap();
        assert!(turn.usage.is_none());
        assert!(turn.openai_response_id().is_none());
        assert!(new.latest_openai_response_id(None).is_none());
        // 旧会话自身的链锚点保持
        assert_eq!(old.latest_openai_response_id(None), Some("resp_old"));
    }

    fn manager(dir: &std::path::Path, app: AppConfig) -> Arc<SessionManager> {
        Arc::new(SessionManager::new(
            Arc::new(JsonlSessionStore::new(dir).unwrap()),
            app,
            dir,
            dir.join("state.json"),
        ))
    }

    /// 端点不可达（压缩请求快速失败，不出本机）
    fn unreachable() -> AppConfig {
        let mut app = config(Some(1000));
        let provider = app.providers.get_mut("p").unwrap();
        provider.api_key = "k".into();
        provider.base_url = "http://127.0.0.1:1".into();
        app.settings.default = "p/m".into();
        app
    }

    #[tokio::test]
    async fn failed_compression_leaves_old_session_unchanged() {
        let dir = std::env::temp_dir().join(format!("vva-rollover-{}", uuid::Uuid::new_v4()));
        let manager = manager(&dir, unreachable());
        let mut old = prefixed(vec![tool_round("r1", "c1")]);
        old.header.workspace_root = dir.clone();
        manager.store().save_header(&old).await.unwrap();
        let handle = manager.get_or_load(&old.header.id).await.unwrap();

        let error = rollover(&manager, 2, &handle).await.unwrap_err();
        assert!(error.contains("压缩请求失败"), "{error}");
        // 内存视图与磁盘都回到压缩前
        assert_eq!(handle.snapshot().session.turns, old.turns);
        let stored = manager
            .store()
            .load_session(&old.header.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.turns, old.turns);
        assert!(stored.header.superseded_by.is_none());
        assert_eq!(manager.store().list_sessions().await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn adopted_successor_is_persisted_and_old_marked_without_cascade_loss() {
        let dir = std::env::temp_dir().join(format!("vva-rollover-{}", uuid::Uuid::new_v4()));
        let manager = manager(&dir, unreachable());
        let mut old = prefixed(vec![tool_round("r1", "c1"), tool_round("r2", "c2")]);
        old.header.workspace_root = dir.clone();
        manager.store().save_header(&old).await.unwrap();
        let handle = manager.get_or_load(&old.header.id).await.unwrap();

        // 模拟压缩已成功追加：摘要轮次留在旧会话
        let before = old.turns.len();
        let successor = successor_session(&old, before, "SUMMARY", 1);
        let fresh = manager.adopt_session(successor).await.unwrap();
        handle.mark_superseded(&fresh.id).await.unwrap();

        let stored_old = manager
            .store()
            .load_session(&old.header.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored_old.header.superseded_by.as_deref(),
            Some(fresh.id.as_str())
        );
        assert_eq!(stored_old.turns, old.turns, "旧会话内容不被改写");
        let stored_new = manager
            .store()
            .load_session(&fresh.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored_new.header.parent.as_deref(),
            Some(old.header.id.as_str())
        );

        // 清理旧会话不得级联删除接替会话
        manager.delete_session(&handle).await.unwrap();
        assert!(
            manager
                .store()
                .load_session(&fresh.id)
                .await
                .unwrap()
                .is_some()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
