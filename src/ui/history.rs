use crate::config::AppConfig;
use crate::config::model::ModelCostConfig;
use crate::model::Session;

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
pub fn compute_stats(session: &Session, app_config: &AppConfig) -> SidebarStats {
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

    if let Ok(resolved) =
        app_config.resolve_model(&session.current_model, session.current_variant.as_deref())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TokenUsage, Turn};
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
            cached_tokens: None,
            reasoning_tokens: None,
        });
        session.turns.push(turn);

        let stats = compute_stats(&session, &AppConfig::default());
        assert_eq!(stats.prompt_tokens, 100);
        assert_eq!(stats.completion_tokens, 50);
        assert_eq!(stats.total_tokens, 150);
        assert_eq!(stats.total_turns, 1);
        assert_eq!(stats.estimated_cost_usd, 0.0);
    }
}
