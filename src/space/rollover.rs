//! Rollover：上下文达到预留线后的摘要与会话切换。
//!
//! 不原地改写历史：回合结束后若上下文逼近窗口，由主模型在**同一会话末尾**追加压缩
//! prompt（模型 / 工具 / 推理参数与上一轮完全一致，复用 cache 前缀），摘要作为追加轮次
//! 留在旧会话；随后新建会话承接——复制旧会话的 system 前缀，其后仅有摘要 reminder，
//! 不复制任何对话轮次。旧会话写 `superseded_by`，其余内容只增不改。

use std::sync::Arc;

use crate::config::{AgentSpaceConfig, AppConfig};
use crate::harness::{ManagedSession, SessionManager};
use crate::model::{ContentPart, Message, Role, Session, TokenUsage, Turn};

/// 追加到旧会话末尾的压缩 prompt
const COMPRESS_PROMPT: &str = "\
请对到目前为止的整个对话做一次上下文压缩，产出供后续会话承接的摘要。\
该摘要将是新会话中仅有的历史，不会附带任何原始对话，因此必须自足。\
本次回复不得调用任何工具，只输出摘要正文，按以下分节书写（某节没有内容时写“无”）：

## 目标
## 偏好
## 关键事实
## 约束
## 决策
## 待办
## 未决问题
## 当前进展

“当前进展”写明最近一条用户请求的原意、已完成与正在进行的步骤。

要求：只保留继续工作所需的信息；保留具体的路径、标识符、数值与命令；\
不确定或未经验证的信息标注“未确认”；不要编造，不要复述寒暄。";

/// rollover 完成后新旧会话 id；上层据此切换 binding focus 与 message_index
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rolled {
    pub old_id: String,
    pub new_id: String,
}

/// 一次请求的总输入 token（含缓存读写；Anthropic 的 input 不含缓存读写，需另行计入）
fn input_tokens(turn: &Turn, usage: &TokenUsage) -> u64 {
    let mut tokens = usage.prompt_tokens as u64;
    if let Some(anthropic) = &turn.meta.anthropic {
        tokens += usage.cached_tokens.unwrap_or_default() as u64
            + anthropic.cache_creation_input_tokens.unwrap_or_default() as u64;
    }
    tokens
}

/// 当前上下文 token 数：最近一个带 usage 的 assistant 轮次的输入 + 输出；尚无 usage 时为 `None`
pub fn context_tokens(session: &Session) -> Option<u64> {
    session
        .turns
        .iter()
        .rev()
        .filter(|turn| turn.role == Role::Assistant)
        .find_map(|turn| {
            let usage = turn.usage.as_ref()?;
            Some(input_tokens(turn, usage) + usage.completion_tokens as u64)
        })
}

/// 全会话 cache 命中率（读缓存 token / 总输入 token）；尚无输入时为 `None`
pub fn cache_hit_rate(session: &Session) -> Option<f64> {
    let (hit, total) = session
        .turns
        .iter()
        .filter(|turn| turn.role == Role::Assistant)
        .filter_map(|turn| Some((turn.usage.as_ref()?, turn)))
        .fold((0u64, 0u64), |(hit, total), (usage, turn)| {
            (
                hit + usage.cached_tokens.unwrap_or_default() as u64,
                total + input_tokens(turn, usage),
            )
        });
    (total > 0).then(|| hit as f64 / total as f64)
}

/// 回合结束后的阈值判定：`上下文 tokens >= limit.context - rollover_reserve_tokens`。
/// 模型缺失 `limit.context` 或预留线超过窗口时显式报错，不猜测；已被接替的会话不再触发。
fn should_rollover(
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
    tracing::info!(
        "rollover 触发: session={} 上下文 {} tokens",
        handle.id,
        context_tokens(&session).unwrap_or_default()
    );
    rollover(manager, handle).await.map(Some)
}

/// 压缩并切换到接替会话。失败时旧会话回到调用前的内容（追加的压缩轮次被回退）。
async fn rollover(
    manager: &Arc<SessionManager>,
    handle: &Arc<ManagedSession>,
) -> Result<Rolled, String> {
    if handle.is_running() {
        return Err("会话正在运行，无法 rollover".into());
    }
    // 压缩前读取：索引读取失败时不留下压缩轮次
    let memory = super::curator::memory_reminder(manager.workspace())?;
    let before = handle.snapshot().session.turns.len();
    let summary = compress(handle, before).await?;
    let old = handle.snapshot().session.clone();
    let successor = manager
        .adopt_session(successor_session(&old, before, &summary, memory.as_deref()))
        .await?;
    if let Err(error) = handle.mark_superseded(&successor.id).await {
        // 旧会话未能标记：撤销新会话，避免出现两个活跃分支
        let _ = manager.delete_session(&successor).await;
        return Err(format!("标记旧会话 superseded_by 失败: {error}"));
    }
    tracing::info!("rollover 完成: {} -> {}", handle.id, successor.id);
    Ok(Rolled {
        old_id: handle.id.clone(),
        new_id: successor.id.clone(),
    })
}

/// 在旧会话末尾追加压缩 prompt 并取回摘要；任何失败都回退追加的轮次
async fn compress(handle: &Arc<ManagedSession>, before: usize) -> Result<String, String> {
    tracing::info!("rollover 发出压缩请求: session={}", handle.id);
    let outcome = handle.deliver_wait(COMPRESS_PROMPT).await;
    let session = handle.snapshot().session.clone();
    let failure = match outcome {
        Err(error) => format!("压缩请求失败: {error}"),
        Ok(_) if calls_tool(&session.turns[before..]) => "压缩请求发起了工具调用，视为失败".into(),
        Ok(report) if report.text.trim().is_empty() => "压缩请求未返回摘要".into(),
        Ok(report) => {
            let summary = report.text.trim().to_string();
            tracing::info!(
                "rollover 摘要已生成: session={} {} 字符",
                handle.id,
                summary.chars().count()
            );
            return Ok(summary);
        }
    };
    tracing::warn!("rollover 压缩失败: session={}: {failure}", handle.id);
    if session.turns.len() > before
        && let Err(error) = handle.revert(before).await
    {
        return Err(format!("{failure}；回退压缩轮次失败: {error}"));
    }
    Err(failure)
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

fn is_memory_reminder(turn: &Turn) -> bool {
    turn.role == Role::User
        && turn
            .text_content()
            .trim_start()
            .starts_with("<system-reminder type=\"memory\"")
}

/// 构造接替会话（纯函数）：仅读取旧会话压缩前的 `turns[..before]`。
/// 轮次 = system 前缀（日期随之冻结）+ 摘要 reminder；交接只留摘要，不复制任何对话轮次。
/// 前缀中旧的记忆索引快照已过时：`memory` 为当前索引 reminder，原位替换
/// （旧前缀没有则不加；当前无索引则丢弃旧快照）。
fn successor_session(old: &Session, before: usize, summary: &str, memory: Option<&str>) -> Session {
    let turns = &old.turns[..before];
    let prefix = turns
        .iter()
        .take_while(|turn| !is_round_start(turn))
        .count();

    let header = &old.header;
    let mut session = Session::with_model(
        header.workspace_root.clone(),
        header.title.clone(),
        header.current_model.clone(),
        header.current_variant.clone(),
        old.toggles(),
    );
    session.header.editor = header.editor;
    session.header.continued_from = Some(header.id.clone());
    session.header.kind = header.kind;
    session.header.origin = header.origin.clone();
    session.header.routed_by = header.routed_by.clone();
    session.header.ttl_secs = header.ttl_secs;
    session.header.last_activity = Some(chrono::Utc::now());

    for turn in &turns[..prefix] {
        if !is_memory_reminder(turn) {
            session.turns.push(turn.clone());
        } else if let Some(memory) = memory {
            session
                .turns
                .push(Turn::new(Role::User, vec![Message::text(memory)]));
        }
    }
    session.turns.push(Turn::new(
        Role::User,
        vec![Message::text(
            crate::system::format_rollover_summary_reminder(&header.id, summary),
        )],
    ));
    session
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelConfig, ProviderConfig, model::ModelLimitConfig};
    use crate::model::{AnthropicTurnMeta, TokenUsage, ToolCall, ToolResult};
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
            .push(ContentPart::ToolCall(ToolCall::new(
                id,
                crate::tools::names::READ,
                "{}",
            )));
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
        // 已被接替的会话不再触发
        let mut superseded = at(999);
        superseded.header.superseded_by = Some("sess_new00".into());
        assert!(!should_rollover(&config(Some(1000)), &cfg, &superseded).unwrap());
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

    fn prefixed(rounds: Vec<Vec<Turn>>) -> Session {
        let mut turns = vec![
            Turn::new(Role::System, vec![Message::text("SYSTEM Date: 2026-01-01")]),
            user("<system-reminder type=\"skills\">\nx\n</system-reminder>"),
        ];
        turns.extend(rounds.into_iter().flatten());
        session_with(turns)
    }

    #[test]
    fn successor_holds_only_prefix_and_summary() {
        let mut old = prefixed(vec![tool_round("r1", "c1"), tool_round("r2", "c2")]);
        let before = old.turns.len();
        old.header.curation_requested = true;
        old.header.ttl_secs = Some(10_800);
        let new = successor_session(&old, before, "SUMMARY", None);
        assert!(old.header.curation_requested);
        assert!(!new.header.curation_requested);
        assert_eq!(new.header.ttl_secs, old.header.ttl_secs);

        assert_eq!(
            new.header.continued_from.as_deref(),
            Some(old.header.id.as_str())
        );
        assert!(new.header.parent.is_none(), "接替会话不是 subagent");
        assert_ne!(new.header.id, old.header.id);
        // system 前缀逐字复制（日期随之冻结），其后仅有摘要，不含任何对话轮次
        assert_eq!(new.turns.len(), 3);
        assert_eq!(new.turns[0], old.turns[0]);
        assert_eq!(new.turns[1], old.turns[1]);
        assert!(
            new.turns[2]
                .text_content()
                .starts_with("<system-reminder type=\"rollover-summary\"")
        );
        assert!(new.turns[2].text_content().contains("SUMMARY"));
        // 旧会话原样未动
        assert_eq!(old.turns.len(), before);
    }

    #[test]
    fn successor_replaces_stale_memory_snapshot_with_current() {
        let mut old = prefixed(vec![tool_round("r1", "c1")]);
        old.turns.insert(
            2,
            user("<system-reminder type=\"memory\">\nOLD INDEX\n</system-reminder>"),
        );
        let before = old.turns.len();
        let fresh = "<system-reminder type=\"memory\">\nNEW INDEX\n</system-reminder>";

        let new = successor_session(&old, before, "S", Some(fresh));
        assert_eq!(new.turns.len(), 4);
        assert_eq!(new.turns[2].text_content(), fresh);
        assert!(
            new.turns
                .iter()
                .all(|t| !t.text_content().contains("OLD INDEX"))
        );

        // 当前无索引：旧快照丢弃，不留过期内容
        let new = successor_session(&old, before, "S", None);
        assert_eq!(new.turns.len(), 3);
        assert!(
            new.turns
                .iter()
                .all(|t| !t.text_content().contains("OLD INDEX"))
        );

        // 旧前缀没有 memory reminder：不新增
        let plain = prefixed(vec![tool_round("r1", "c1")]);
        let new = successor_session(&plain, plain.turns.len(), "S", Some(fresh));
        assert_eq!(new.turns.len(), 3);
        assert!(
            new.turns
                .iter()
                .all(|t| !t.text_content().contains("NEW INDEX"))
        );
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

        let error = rollover(&manager, &handle).await.unwrap_err();
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
        let successor = successor_session(&old, before, "SUMMARY", None);
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
            stored_new.header.continued_from.as_deref(),
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
