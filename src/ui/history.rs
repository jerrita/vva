use crate::config::AppConfig;
use crate::config::model::ModelCostConfig;
use crate::model::Session;
use serde::Serialize;

/// 侧边栏展示的会话全局统计数据
#[derive(Debug, Clone, Default, Serialize)]
pub struct SidebarStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// 会话级输出吞吐：Σ completion_tokens / Σ 模型响应耗时 (tok/s)，无计时数据时为 None
    pub tps: Option<f64>,
    pub total_tokens: u64,
    pub context_tokens: u64,
    pub context_limit: Option<u64>,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub estimated_cost_usd: f64,
    pub total_turns: usize,
}

/// 计算侧边栏统计信息 (Token 消耗、缓存命中与预估美元花费)
pub fn compute_stats(session: &Session, app_config: &AppConfig) -> SidebarStats {
    let mut prompt_tok = 0u64;
    let mut comp_tok = 0u64;
    let mut tot_tok = 0u64;
    let mut context_tok = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;
    let mut gen_tokens = 0u64;
    let mut gen_ms = 0u64;

    for turn in &session.turns {
        if let Some(usage) = &turn.usage {
            prompt_tok += usage.prompt_tokens as u64;
            comp_tok += usage.completion_tokens as u64;
            tot_tok += usage.total_tokens as u64;
            cache_read += usage.cached_tokens.unwrap_or_default() as u64;
            context_tok = usage.total_tokens as u64;
            if let Some(elapsed_ms) = turn.elapsed_ms.filter(|ms| *ms > 0) {
                gen_tokens += usage.completion_tokens as u64;
                gen_ms += elapsed_ms;
            }
        }
        if let Some(anthropic) = &turn.meta.anthropic
            && let Some(w) = anthropic.cache_creation_input_tokens
        {
            cache_write += w as u64;
        }
    }

    let mut context_limit = None;
    let mut cost_cfg: Option<ModelCostConfig> = None;

    if let Ok(resolved) = app_config.resolve_model(
        &session.header.current_model,
        session.header.current_variant.as_deref(),
    ) {
        context_limit = resolved
            .model_config
            .limit
            .as_ref()
            .map(|l| l.context as u64);
        cost_cfg = resolved.model_config.cost.clone();
    }

    let mut estimated_cost = 0.0;
    if let Some(cost) = &cost_cfg {
        for turn in &session.turns {
            let Some(usage) = &turn.usage else { continue };
            let cache_read_turn = usage.cached_tokens.unwrap_or_default() as u64;
            let cache_write_turn = turn
                .meta
                .anthropic
                .as_ref()
                .and_then(|a| a.cache_creation_input_tokens)
                .unwrap_or(0) as u64;
            let prompt = usage.prompt_tokens as u64;
            let (billed_input, context_tokens) = if turn.meta.anthropic.is_some() {
                (prompt, prompt + cache_read_turn + cache_write_turn)
            } else {
                (prompt.saturating_sub(cache_read_turn), prompt)
            };
            estimated_cost += cost.estimate_usd(
                billed_input,
                usage.completion_tokens as u64,
                cache_read_turn,
                cache_write_turn,
                context_tokens,
            );
        }
    }

    SidebarStats {
        prompt_tokens: prompt_tok,
        completion_tokens: comp_tok,
        tps: (gen_ms > 0).then(|| gen_tokens as f64 * 1000.0 / gen_ms as f64),
        total_tokens: tot_tok,
        context_tokens: context_tok,
        context_limit,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        estimated_cost_usd: estimated_cost,
        total_turns: session.turns.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{ContextOver200kCost, ModelConfig, ProviderConfig};
    use crate::model::{AnthropicTurnMeta, TokenUsage, Turn, TurnMeta};
    use std::collections::HashMap;

    fn cost_app_config() -> AppConfig {
        let mut config = AppConfig::default();
        let mut models = HashMap::new();
        models.insert(
            "m".into(),
            ModelConfig {
                name: "M".into(),
                cost: Some(ModelCostConfig {
                    input: 2.0,
                    output: 6.0,
                    cache_read: Some(0.5),
                    cache_write: Some(3.75),
                    context_over_200k: Some(ContextOver200kCost {
                        input: 4.0,
                        output: 12.0,
                        cache_read: Some(1.0),
                        cache_write: Some(7.5),
                    }),
                }),
                ..Default::default()
            },
        );
        config.providers.insert(
            "p".into(),
            ProviderConfig {
                models,
                ..Default::default()
            },
        );
        config
    }

    fn session_with_usage(
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        anthropic: Option<AnthropicTurnMeta>,
    ) -> Session {
        let mut session =
            Session::with_model(std::path::PathBuf::from("/ws"), None, "p/m", None, false);
        let mut turn = Turn::assistant_turn(vec![]);
        turn.usage = Some(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cached_tokens,
            reasoning_tokens: None,
        });
        if let Some(anthropic) = anthropic {
            turn.meta = TurnMeta {
                anthropic: Some(anthropic),
                ..Default::default()
            };
        }
        session.turns.push(turn);
        session
    }

    fn assert_cost_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "cost {actual} != {expected}"
        );
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
        let mut turn = Turn::assistant_turn(vec![]);
        turn.usage = Some(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cached_tokens: Some(80),
            reasoning_tokens: None,
        });
        session.turns.push(turn);

        let stats = compute_stats(&session, &AppConfig::default());
        assert_eq!(stats.prompt_tokens, 100);
        assert_eq!(stats.completion_tokens, 50);
        assert_eq!(stats.total_tokens, 150);
        assert_eq!(stats.context_tokens, 150);
        assert_eq!(stats.cache_read_tokens, 80);
        assert_eq!(stats.total_turns, 1);
        assert_eq!(stats.estimated_cost_usd, 0.0);
    }

    #[test]
    fn context_uses_the_latest_response() {
        let mut session = Session::with_model(
            std::path::PathBuf::from("/ws"),
            None,
            "model_a",
            None,
            false,
        );
        for total_tokens in [150, 300] {
            let mut turn = Turn::assistant_turn(vec![]);
            turn.usage = Some(TokenUsage {
                prompt_tokens: total_tokens - 50,
                completion_tokens: 50,
                total_tokens,
                cached_tokens: None,
                reasoning_tokens: None,
            });
            session.turns.push(turn);
        }

        let stats = compute_stats(&session, &AppConfig::default());
        assert_eq!(stats.total_tokens, 450);
        assert_eq!(stats.context_tokens, 300);
    }

    #[test]
    fn tps_is_generation_time_weighted() {
        let mut session = Session::with_model(
            std::path::PathBuf::from("/ws"),
            None,
            "model_a",
            None,
            false,
        );
        for (completion_tokens, elapsed_ms) in [(50, 500), (150, 1_500)] {
            let mut turn = Turn::assistant_turn(vec![]);
            turn.usage = Some(TokenUsage {
                completion_tokens,
                total_tokens: completion_tokens,
                ..Default::default()
            });
            turn.elapsed_ms = Some(elapsed_ms);
            session.turns.push(turn);
        }
        let mut untimed = Turn::assistant_turn(vec![]);
        untimed.usage = Some(TokenUsage {
            completion_tokens: 1_000,
            total_tokens: 1_000,
            ..Default::default()
        });
        session.turns.push(untimed);

        let stats = compute_stats(&session, &AppConfig::default());
        assert_eq!(stats.completion_tokens, 1_200);
        assert!((stats.tps.unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn responses_cost_excludes_cached_from_input() {
        let session = session_with_usage(100, 50, Some(80), None);
        let stats = compute_stats(&session, &cost_app_config());
        assert_cost_eq(stats.estimated_cost_usd, 0.00038);
    }

    #[test]
    fn anthropic_cost_keeps_input_separate_from_cache() {
        let session = session_with_usage(
            100,
            50,
            Some(80),
            Some(AnthropicTurnMeta {
                cache_creation_input_tokens: Some(20),
                cache_read_input_tokens: Some(80),
                stop_reason: None,
                content_blocks: Vec::new(),
                scope: None,
            }),
        );
        let stats = compute_stats(&session, &cost_app_config());
        assert_cost_eq(stats.estimated_cost_usd, 0.000615);
    }

    #[test]
    fn over_200k_uses_tiered_rates_per_turn() {
        let session = session_with_usage(250_000, 1_000, Some(200_000), None);
        let stats = compute_stats(&session, &cost_app_config());
        assert_cost_eq(stats.estimated_cost_usd, 0.412);
    }
}
