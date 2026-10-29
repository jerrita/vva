//! OpenRouter Decisions / OpenCode System One 路由后端：把候选表映射为 choice / noul 题目，
//! 答案回填为 `RouteDecision`，其后的校验与日志与小模型后端共用。

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::router::{
    Action, DecisionError, ModelReply, Prompt, RouteDecision, RouteInput, RouteModel, RunningIntent,
};
use crate::config::DecisionsRouterConfig;
use crate::model::TokenUsage;
use crate::provider::common::header_value;
use crate::provider::http::post_json;

/// 空标题候选在选项说明中的占位
const UNTITLED: &str = "(无标题)";
/// `guide` noul 答案不低于该值视为插话
const GUIDE_THRESHOLD: f64 = 0.5;

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

/// 请求体：`state` 与小模型提示词同构（候选表按 id 排序，用户消息置尾）
fn build_request(model: &str, input: &RouteInput) -> Value {
    let state = Prompt::new(input);
    let sessions: serde_json::Map<String, Value> = state
        .candidates
        .iter()
        .map(|c| {
            let title = if c.title.is_empty() {
                UNTITLED
            } else {
                &c.title
            };
            (c.id.clone(), json!(title))
        })
        .collect();
    json!({
        "model": model,
        "state": state,
        "questions": {
            "action": {
                "type": "choice",
                "instructions": "判断 user_text 应如何路由。不要回答用户的问题。有疑问时选 existing，续接 focus=true 的会话。",
                "criteria": {
                    "existing": "承接、追问、改写、继续前文、索取先前的结果或文件，或无法判断是否新话题",
                    "new": "明确开启与所有候选会话主题完全无关的独立话题或任务"
                }
            },
            "session": {
                "type": "choice",
                "instructions": "若承接已有会话，user_text 属于哪个会话（值为会话标题）。难以判断时选 focus=true 的会话；title 明显指向其他会话时选那个。",
                "criteria": sessions
            },
            "guide": {
                "type": "noul",
                "instructions": "user_text 是否是对某个 running=true 会话的补充、修正、打断或改变方向",
                "criteria": {
                    "true": "是对正在生成的会话的补充、修正、打断或改变方向",
                    "false": "不是"
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

#[derive(Deserialize)]
struct NoulAnswer {
    noul: f64,
}

/// `session` / `guide` 仅在 `action=existing` 时参与，故延迟解析
#[derive(Deserialize)]
struct Answers {
    action: Value,
    session: Value,
    guide: Value,
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
    let (session_id, running_intent) = if action == Action::Existing {
        let (session, confidence) = choice_answer("session", answers.session)?;
        require_confidence("session", &session, confidence, min_confidence)?;
        let guide: NoulAnswer = answer("guide", answers.guide)?;
        let intent = if guide.noul >= GUIDE_THRESHOLD {
            RunningIntent::GuideExisting
        } else {
            RunningIntent::None
        };
        (Some(session), intent)
    } else {
        (None, RunningIntent::None)
    };
    Ok(RouteDecision {
        action,
        session_id,
        running_intent,
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

    fn response(action: &str, conf: f64, session: &str, guide: f64) -> Value {
        json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": {
                "action": {"type": "choice", "choice": action, "confidence": conf,
                           "probabilities": {"existing": 1, "new": 0}},
                "session": {"type": "choice", "choice": session, "confidence": 1,
                            "probabilities": {"sess_a": 1, "sess_b": 0}},
                "guide": {"type": "noul", "noul": guide}
            },
            "usage": {"input_tokens": 522, "output_tokens": 70, "cost": 0.000021924},
            "id": "gen-dec-1", "provider": "TypeSafe"
        })
    }

    fn decision(v: Value) -> Result<RouteDecision, DecisionError> {
        parse_response(v, 0.6).unwrap().decision
    }

    #[test]
    fn request_maps_candidates_to_choices() {
        let body = build_request("typesafe/jev-1.13", &input());
        let sessions = body["questions"]["session"]["criteria"]
            .as_object()
            .unwrap();
        assert_eq!(sessions["sess_a"], "整理笔记");
        assert_eq!(sessions["sess_b"], UNTITLED);
        // 候选按 id 排序，用户消息在 state 末尾
        assert_eq!(body["state"]["candidates"][0]["id"], "sess_a");
        assert_eq!(body["state"]["user_text"], "再看看");
    }

    #[test]
    fn answers_map_to_decision() {
        let r = parse_response(response("existing", 0.95, "sess_a", 0.1), 0.6).unwrap();
        assert_eq!(r.usage.unwrap().total_tokens, 592);
        let d = r.decision.unwrap();
        assert_eq!(d.action, Action::Existing);
        assert_eq!(d.session_id.as_deref(), Some("sess_a"));
        assert_eq!(d.running_intent, RunningIntent::None);

        let g = decision(response("existing", 0.9, "sess_b", 0.96)).unwrap();
        assert_eq!(g.running_intent, RunningIntent::GuideExisting);

        // 非 existing 时忽略 session / guide 答案
        let n = decision(response("new", 0.9, "sess_a", 0.9)).unwrap();
        assert_eq!(
            (n.session_id, n.running_intent),
            (None, RunningIntent::None)
        );
    }

    /// OpenCode 直通 System One 响应，无 OpenRouter 的 id / provider / cost 外层字段。
    /// 协议参考：https://docs.typesafe.ai/introduction/quickstart#call-it-the-api
    #[test]
    fn system_one_response_maps_answers_and_usage() {
        let r = parse_response(
            json!({
                "model": "jev-1.13.0",
                "answers": {
                    "action": {"type": "choice", "choice": "existing", "confidence": 1.0,
                        "probabilities": {"existing": 1.0, "new": 0.0}},
                    "session": {"type": "choice", "choice": "sess_b", "confidence": 1.0,
                        "probabilities": {"sess_a": 0.0, "sess_b": 1.0}},
                    "guide": {"type": "noul", "noul": 0.99}
                },
                "usage": {"input_tokens": 428, "output_tokens": 73}
            }),
            0.6,
        )
        .unwrap();
        let d = r.decision.unwrap();
        assert_eq!(d.action, Action::Existing);
        assert_eq!(d.session_id.as_deref(), Some("sess_b"));
        assert_eq!(d.running_intent, RunningIntent::GuideExisting);
        let usage = r.usage.unwrap();
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (428, 73, 501)
        );
    }

    #[test]
    fn low_confidence_is_uncertain_and_missing_confidence_is_invalid() {
        // 答案不合格时 usage 仍带回
        let r = parse_response(response("existing", 0.4, "sess_a", 0.0), 0.6).unwrap();
        assert!(r.usage.is_some());
        let e = r.decision.unwrap_err();
        assert!(
            matches!(
                e,
                DecisionError::LowConfidence {
                    question: "action",
                    ..
                }
            ),
            "{e}"
        );
        let mut v = response("existing", 1.0, "sess_a", 0.0);
        v["answers"]["action"]
            .as_object_mut()
            .unwrap()
            .remove("confidence");
        assert!(
            matches!(decision(v).unwrap_err(), DecisionError::Invalid(e) if e.contains("缺少 confidence"))
        );
    }

    #[test]
    fn unknown_action_and_malformed_rejected() {
        assert!(
            decision(response("casual", 1.0, "sess_a", 0.0))
                .unwrap_err()
                .to_string()
                .contains("未知 action")
        );
        assert!(parse_response(json!({"answers": {}}), 0.6).is_err());
    }

    #[test]
    fn low_session_confidence_is_uncertain_and_invalid_scores_are_rejected() {
        let mut v = response("existing", 0.9, "sess_b", 0.0);
        v["answers"]["session"]["confidence"] = json!(0.2);
        assert!(matches!(
            decision(v).unwrap_err(),
            DecisionError::LowConfidence {
                question: "session",
                ..
            }
        ));

        for confidence in [-0.1, 1.1] {
            assert!(matches!(
                decision(response("existing", confidence, "sess_a", 0.0)).unwrap_err(),
                DecisionError::Invalid(_)
            ));
        }
        assert!(matches!(
            decision(response("unknown", 0.2, "sess_a", 0.0)).unwrap_err(),
            DecisionError::Invalid(_)
        ));
    }
}
