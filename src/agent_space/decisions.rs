//! OpenRouter Decisions API（alpha）路由后端：把候选表映射为 choice / noul 题目，
//! 答案按 `RouteDecision` 形状回填，其后的校验与日志与小模型后端共用。

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};

use super::binding::AttachedFile;
use super::router::{
    Action, ModelReply, ReplyRef, RouteDecision, RouteInput, RouteModel, RunningIntent,
};
use crate::config::DecisionsRouterConfig;
use crate::model::TokenUsage;
use crate::provider::common::header_value;
use crate::provider::http::post_json;

/// 空标题候选在选项说明中的占位
const UNTITLED: &str = "(无标题)";

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
    async fn decide(&self, _: &str, _: &str, input: &RouteInput) -> Result<ModelReply, String> {
        let body = build_request(&self.config.model, input);
        let mut headers = HeaderMap::new();
        let bearer =
            header_value(&format!("Bearer {}", self.api_key)).map_err(|e| e.to_string())?;
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let response = post_json(&self.config.endpoint, headers, &body)
            .await
            .map_err(|e| e.to_string())?;
        parse_response(response, self.config.min_confidence)
    }
}

#[derive(serde::Serialize)]
struct State<'a> {
    candidates: Vec<&'a super::router::Candidate>,
    files: &'a [AttachedFile],
    reply_to: Option<&'a ReplyRef>,
    user_text: &'a str,
}

/// 请求体：候选表按 id 排序，用户消息在 state 末尾
pub fn build_request(model: &str, input: &RouteInput) -> Value {
    let mut candidates: Vec<_> = input.candidates.iter().collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    let sessions: serde_json::Map<String, Value> = candidates
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
    let state = State {
        candidates,
        files: &input.files,
        reply_to: input.reply_to.as_ref(),
        user_text: &input.text,
    };
    json!({
        "model": model,
        "state": state,
        "questions": {
            "action": {
                "type": "choice",
                "instructions": "判断 user_text 应如何路由。不要回答用户的问题。有疑问时选 existing。",
                "criteria": {
                    "oneshot": "寒暄、确认在线、简单感谢，或不依赖任何历史、答完即无长期价值的独立小问题",
                    "existing": "承接、追问、改写、继续前文、索取先前的结果或文件，或无法判断是否新话题",
                    "new": "明确开启与所有候选会话主题完全无关、需要长期上下文的独立任务"
                }
            },
            "session": {
                "type": "choice",
                "instructions": "若承接已有会话，user_text 属于哪个会话（值为会话标题）。通常选 focus=true 的会话；title 明显指向其他会话时选那个。",
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
#[serde(deny_unknown_fields)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    #[allow(dead_code)]
    cost: Option<f64>,
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

fn choice_of(name: &str, v: Value) -> Result<ChoiceAnswer, String> {
    serde_json::from_value(v).map_err(|e| format!("Decisions 题目 {name} 答案非法: {e}"))
}

fn confident(name: &str, a: &ChoiceAnswer, min: f64) -> Result<(), String> {
    let c = a
        .confidence
        .ok_or_else(|| format!("Decisions 题目 {name} 缺少 confidence"))?;
    if c < min {
        return Err(format!(
            "Decisions 置信度不足: {name}={} ({c:.2} < {min:.2})",
            a.choice
        ));
    }
    Ok(())
}

/// 答案 → `RouteDecision` JSON；`session` / `guide` 仅在 `action=existing` 时参与
pub fn parse_response(response: Value, min_confidence: f64) -> Result<ModelReply, String> {
    let r: DecisionsResponse =
        serde_json::from_value(response).map_err(|e| format!("Decisions 响应非法: {e}"))?;
    let action = choice_of("action", r.answers.action)?;
    confident("action", &action, min_confidence)?;
    let action_kind = match action.choice.as_str() {
        "oneshot" => Action::Oneshot,
        "existing" => Action::Existing,
        "new" => Action::New,
        other => return Err(format!("Decisions 返回未知 action: {other}")),
    };
    let (session_id, intent) = if action_kind == Action::Existing {
        let session = choice_of("session", r.answers.session)?;
        confident("session", &session, min_confidence)?;
        let guide: NoulAnswer = serde_json::from_value(r.answers.guide)
            .map_err(|e| format!("Decisions 题目 guide 答案非法: {e}"))?;
        let intent = if guide.noul >= 0.5 {
            RunningIntent::GuideExisting
        } else {
            RunningIntent::None
        };
        (Some(session.choice), intent)
    } else {
        (None, RunningIntent::None)
    };
    let decision = RouteDecision {
        action: action_kind,
        session_id,
        running_intent: intent,
        reason: format!(
            "decisions: action={} ({:.2})",
            action.choice,
            action.confidence.unwrap_or_default()
        ),
    };
    let u = r.usage;
    Ok(ModelReply {
        text: serde_json::to_string(&decision).expect("RouteDecision 可序列化"),
        usage: Some(TokenUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
            ..TokenUsage::default()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_space::router::Candidate;

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
                    title: "修 Rust".into(),
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
                           "probabilities": {"oneshot": 0, "existing": 1, "new": 0}},
                "session": {"type": "choice", "choice": session, "confidence": 1,
                            "probabilities": {"sess_a": 1, "sess_b": 0}},
                "guide": {"type": "noul", "noul": guide}
            },
            "usage": {"input_tokens": 522, "output_tokens": 70, "cost": 0.000021924},
            "id": "gen-dec-1", "provider": "TypeSafe"
        })
    }

    #[test]
    fn request_maps_candidates_to_choices() {
        let body = build_request("typesafe/jev-1.13", &input());
        assert_eq!(body["model"], "typesafe/jev-1.13");
        let sessions = body["questions"]["session"]["criteria"]
            .as_object()
            .unwrap();
        assert_eq!(sessions["sess_a"], "修 Rust");
        assert_eq!(sessions["sess_b"], UNTITLED);
        // 候选按 id 排序，用户消息在 state 末尾
        assert_eq!(body["state"]["candidates"][0]["id"], "sess_a");
        assert_eq!(body["state"]["user_text"], "再看看");
        assert_eq!(body["questions"]["guide"]["type"], "noul");
    }

    #[test]
    fn existing_maps_to_decision() {
        let r = parse_response(response("existing", 0.95, "sess_a", 0.1), 0.6).unwrap();
        let d: RouteDecision = serde_json::from_str(&r.text).unwrap();
        assert_eq!(d.action, Action::Existing);
        assert_eq!(d.session_id.as_deref(), Some("sess_a"));
        assert_eq!(d.running_intent, RunningIntent::None);
        assert_eq!(r.usage.unwrap().total_tokens, 592);
    }

    #[test]
    fn guide_and_non_existing_drop_session() {
        let g = parse_response(response("existing", 0.9, "sess_b", 0.96), 0.6).unwrap();
        let d: RouteDecision = serde_json::from_str(&g.text).unwrap();
        assert_eq!(d.running_intent, RunningIntent::GuideExisting);

        let n = parse_response(response("new", 0.9, "sess_a", 0.9), 0.6).unwrap();
        let d: RouteDecision = serde_json::from_str(&n.text).unwrap();
        assert_eq!(
            (d.session_id, d.running_intent),
            (None, RunningIntent::None)
        );
    }

    #[test]
    fn low_or_missing_confidence_is_explicit_error() {
        let e = parse_response(response("existing", 0.4, "sess_a", 0.0), 0.6).unwrap_err();
        assert!(e.contains("置信度不足"), "{e}");
        let mut v = response("existing", 1.0, "sess_a", 0.0);
        v["answers"]["action"]
            .as_object_mut()
            .unwrap()
            .remove("confidence");
        assert!(
            parse_response(v, 0.6)
                .unwrap_err()
                .contains("缺少 confidence")
        );
    }

    #[test]
    fn unknown_action_and_malformed_rejected() {
        assert!(
            parse_response(response("casual", 1.0, "sess_a", 0.0), 0.6)
                .unwrap_err()
                .contains("未知 action")
        );
        assert!(parse_response(json!({"answers": {}}), 0.6).is_err());
    }
}
