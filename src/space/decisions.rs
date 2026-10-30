//! OpenRouter Decisions / OpenCode System One 路由后端：判断是否新建、切换 focus 与 TTL，
//! 答案回填为 `RouteDecision`，其后的校验与日志与小模型后端共用。

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::router::{
    Action, DecisionError, ModelReply, Prompt, RouteDecision, RouteInput, RouteModel, SessionTtl,
};
use crate::config::DecisionsRouterConfig;
use crate::model::TokenUsage;
use crate::provider::common::header_value;
use crate::provider::http::post_json;

pub struct DecisionsRouteModel {
    config: DecisionsRouterConfig,
    api_key: String,
}

impl DecisionsRouteModel {
    pub fn new(config: DecisionsRouterConfig, api_key: String) -> Self {
        Self { config, api_key }
    }
}

#[async_trait]
impl RouteModel for DecisionsRouteModel {
    async fn decide(&self, input: &RouteInput) -> Result<ModelReply, String> {
        let body = build_request(&self.config.model, input);
        let mut headers = HeaderMap::new();
        let bearer =
            header_value(&format!("Bearer {}", self.api_key)).map_err(|e| e.to_string())?;
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let response = post_json(self.config.endpoint(), headers, &body)
            .await
            .map_err(|e| e.to_string())?;
        parse_response(response, self.config.min_confidence)
    }
}

/// 请求体：只提供当前 focus，所有决策均使用 choice 题。
fn build_request(model: &str, input: &RouteInput) -> Value {
    json!({
        "model": model,
        "state": Prompt::new(input),
        "questions": {
            "action": {
                "type": "choice",
                "instructions": "判断 user_text 是否需要新建会话。不要回答用户的问题。默认 existing，始终续接当前 focus，运行状态由程序处理。",
                "criteria": {
                    "existing": "承接、追问、改写、继续前文、索取先前的结果或文件，或无法判断是否新话题",
                    "new": "明确开启与当前 focus 无关的独立话题或任务"
                }
            },
            "focus": {
                "type": "choice",
                "instructions": "仅在 action=new 时判断是否把默认续接目标切换到新会话。",
                "criteria": {
                    "switch": "预期后续持续讨论的新话题，切换 focus 到新会话",
                    "keep": "oneshot 一次性任务，保留原 focus"
                }
            },
            "ttl": {
                "type": "choice",
                "instructions": "仅在 action=new 时选择新会话的空闲 TTL，与是否切换 focus 分开判断。",
                "criteria": {
                    "3h": "一次性、短期价值的任务，空闲 3 小时后清理",
                    "12h": "正常会话，空闲 12 小时后清理"
                }
            }
        }
    })
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Deserialize)]
struct ChoiceAnswer {
    choice: String,
    confidence: Option<f64>,
}

/// `focus` / `ttl` 仅在 `action=new` 时参与，故延迟解析。
#[derive(Deserialize)]
struct Answers {
    action: Value,
    focus: Value,
    ttl: Value,
}

#[derive(Deserialize)]
struct DecisionsResponse {
    answers: Answers,
    usage: Usage,
}

fn answer<T: DeserializeOwned>(name: &str, v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| format!("Decisions 题目 {name} 答案非法: {e}"))
}

/// 严格解析 choice 题，返回 (选项, 置信度)。
fn choice_answer(name: &str, v: Value) -> Result<(String, f64), DecisionError> {
    let a: ChoiceAnswer = answer(name, v)?;
    let c = a
        .confidence
        .ok_or_else(|| format!("Decisions 题目 {name} 缺少 confidence"))?;
    if !c.is_finite() || !(0.0..=1.0).contains(&c) {
        return Err(format!("Decisions 题目 {name} confidence 须在 0–1").into());
    }
    Ok((a.choice, c))
}

fn require_confidence(
    question: &'static str,
    choice: &str,
    confidence: f64,
    minimum: f64,
) -> Result<(), DecisionError> {
    if confidence < minimum {
        return Err(DecisionError::LowConfidence {
            question,
            choice: choice.into(),
            confidence,
            minimum,
        });
    }
    Ok(())
}

/// 响应结构非法视为调用失败；答案不合格时 usage 仍随回复带回，供 router.jsonl 记账
fn parse_response(response: Value, min_confidence: f64) -> Result<ModelReply, String> {
    let r: DecisionsResponse =
        serde_json::from_value(response).map_err(|e| format!("Decisions 响应非法: {e}"))?;
    let Usage {
        input_tokens,
        output_tokens,
    } = r.usage;
    Ok(ModelReply {
        decision: decide(r.answers, min_confidence),
        usage: Some(TokenUsage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            total_tokens: input_tokens + output_tokens,
            ..TokenUsage::default()
        }),
    })
}

fn decide(answers: Answers, min_confidence: f64) -> Result<RouteDecision, DecisionError> {
    let (choice, confidence) = choice_answer("action", answers.action)?;
    let action = match choice.as_str() {
        "existing" => Action::Existing,
        "new" => Action::New,
        other => return Err(format!("Decisions 返回未知 action: {other}").into()),
    };
    require_confidence("action", &choice, confidence, min_confidence)?;
    let (focus, ttl) = if action == Action::New {
        let (focus, confidence) = choice_answer("focus", answers.focus)?;
        let focus_value = match focus.as_str() {
            "switch" => true,
            "keep" => false,
            other => return Err(format!("Decisions 返回未知 focus: {other}").into()),
        };
        require_confidence("focus", &focus, confidence, min_confidence)?;
        let (ttl, confidence) = choice_answer("ttl", answers.ttl)?;
        let ttl_value = match ttl.as_str() {
            "3h" => SessionTtl::Short,
            "12h" => SessionTtl::Normal,
            other => return Err(format!("Decisions 返回未知 ttl: {other}").into()),
        };
        require_confidence("ttl", &ttl, confidence, min_confidence)?;
        (focus_value, Some(ttl_value))
    } else {
        (false, None)
    };
    Ok(RouteDecision {
        action,
        focus,
        ttl,
        reason: format!("decisions: action={choice} ({confidence:.2})"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::router::Candidate;

    fn input() -> RouteInput {
        RouteInput {
            binding: "qq:c2c:u1".into(),
            text: "再看看".into(),
            reply_to: None,
            files: vec![],
            candidates: vec![
                Candidate {
                    id: "sess_b".into(),
                    title: String::new(),
                    running: true,
                    focus: false,
                },
                Candidate {
                    id: "sess_a".into(),
                    title: "整理笔记".into(),
                    running: false,
                    focus: true,
                },
            ],
        }
    }

    fn response(action: &str, conf: f64, focus: &str, ttl: &str) -> Value {
        json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": {
                "action": {"type": "choice", "choice": action, "confidence": conf,
                           "probabilities": {"existing": 1, "new": 0}},
                "focus": {"type": "choice", "choice": focus, "confidence": 1,
                          "probabilities": {"keep": 1, "switch": 0}},
                "ttl": {"type": "choice", "choice": ttl, "confidence": 1,
                        "probabilities": {"3h": 1, "12h": 0}}
            },
            "usage": {"input_tokens": 522, "output_tokens": 70, "cost": 0.000021924},
            "id": "gen-dec-1", "provider": "TypeSafe"
        })
    }

    fn decision(v: Value) -> Result<RouteDecision, DecisionError> {
        parse_response(v, 0.6).unwrap().decision
    }

    #[test]
    fn request_only_exposes_focus_and_asks_creation_options() {
        let body = build_request("typesafe/jev-1.13", &input());
        let questions = body["questions"].as_object().unwrap();
        assert_eq!(questions.len(), 3);
        assert!(questions.contains_key("action"));
        assert!(questions.contains_key("focus"));
        assert!(questions.contains_key("ttl"));
        assert_eq!(body["state"]["focus"]["id"], "sess_a");
        assert!(!body["state"].to_string().contains("sess_b"));
        assert_eq!(body["state"]["user_text"], "再看看");
    }

    #[test]
    fn answers_map_to_independent_creation_options() {
        for (focus, want_focus) in [("keep", false), ("switch", true)] {
            for (ttl, want_ttl) in [("3h", SessionTtl::Short), ("12h", SessionTtl::Normal)] {
                let r = parse_response(response("new", 0.95, focus, ttl), 0.6).unwrap();
                assert_eq!(r.usage.unwrap().total_tokens, 592);
                let d = r.decision.unwrap();
                assert_eq!(d.action, Action::New);
                assert_eq!(d.focus, want_focus);
                assert_eq!(d.ttl, Some(want_ttl));
            }
        }
        // 续接只解析 action；新建选项不参与。
        let mut v = response("existing", 0.9, "keep", "3h");
        v["answers"]["focus"] = Value::Null;
        v["answers"]["ttl"] = Value::Null;
        let d = decision(v).unwrap();
        assert_eq!(d.action, Action::Existing);
        assert!(!d.focus);
        assert_eq!(d.ttl, None);
    }

    /// System One 直通响应无需 OpenRouter 的 id / provider / cost 外层字段。
    #[test]
    fn system_one_response_maps_answers_and_usage() {
        let mut v = response("new", 1.0, "switch", "12h");
        let obj = v.as_object_mut().unwrap();
        obj.remove("id");
        obj.remove("provider");
        v["model"] = json!("jev-1.13.0");
        v["usage"] = json!({"input_tokens": 428, "output_tokens": 73});
        let r = parse_response(v, 0.6).unwrap();
        let d = r.decision.unwrap();
        assert_eq!(d.action, Action::New);
        assert!(d.focus);
        assert_eq!(d.ttl, Some(SessionTtl::Normal));
        assert_eq!(r.usage.unwrap().total_tokens, 501);
    }

    #[test]
    fn low_confidence_is_uncertain_and_missing_confidence_is_invalid() {
        for question in ["action", "focus", "ttl"] {
            let mut v = response("new", 1.0, "keep", "3h");
            v["answers"][question]["confidence"] = json!(0.4);
            let r = parse_response(v, 0.6).unwrap();
            assert!(r.usage.is_some());
            assert!(
                matches!(r.decision, Err(DecisionError::LowConfidence { question: q, .. }) if q == question)
            );

            let mut v = response("new", 1.0, "keep", "3h");
            v["answers"][question]
                .as_object_mut()
                .unwrap()
                .remove("confidence");
            assert!(
                matches!(decision(v), Err(DecisionError::Invalid(e)) if e.contains("缺少 confidence"))
            );
        }
    }

    #[test]
    fn unknown_choices_and_invalid_scores_are_rejected() {
        for (action, focus, ttl) in [
            ("unknown", "keep", "3h"),
            ("new", "unknown", "3h"),
            ("new", "keep", "0h"),
        ] {
            assert!(matches!(
                decision(response(action, 1.0, focus, ttl)),
                Err(DecisionError::Invalid(_))
            ));
        }
        for question in ["action", "focus", "ttl"] {
            for confidence in [-0.1, 1.1] {
                let mut v = response("new", 1.0, "keep", "3h");
                v["answers"][question]["confidence"] = json!(confidence);
                assert!(matches!(decision(v), Err(DecisionError::Invalid(_))));
            }
        }
        assert!(parse_response(json!({"answers": {}}), 0.6).is_err());
    }
}
