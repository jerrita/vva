//! Router：L0 确定性锚点 / L1 小模型 / L2 显式报错。
//! 只依据元数据（候选表、用户文字、reply 片段、附件名）选择 session，不读历史；
//! 任何非法决策都显式报错，不降级到 focus。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::binding::{AttachedFile, MessageIndex};
use crate::config::AppConfig;
use crate::model::{Message, Role, Session, SessionHeader, TokenUsage, Turn};
use crate::provider::{CompletionOptions, ResponseSchema};

/// 日志中输入文字的摘要上限（字符数）
const LOG_TEXT_LIMIT: usize = 200;
/// 路由决策只含四个短字段，输出上限足够
const ROUTE_MAX_TOKENS: u32 = 300;

// ================== 决策模型 ==================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Oneshot,
    Existing,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunningIntent {
    None,
    GuideExisting,
}

/// 小模型的结构化输出
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteDecision {
    pub action: Action,
    pub session_id: Option<String>,
    pub running_intent: RunningIntent,
    pub reason: String,
}

impl RouteDecision {
    /// 供应商原生结构化输出所用的 JSON Schema（全部字段必填、禁止多余字段）
    pub fn response_schema() -> ResponseSchema {
        ResponseSchema {
            name: "route_decision".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["oneshot", "existing", "new"]},
                    "session_id": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                    "running_intent": {"type": "string", "enum": ["none", "guide_existing"]},
                    "reason": {"type": "string"}
                },
                "required": ["action", "session_id", "running_intent", "reason"],
                "additionalProperties": false
            }),
        }
    }
}

/// 路由结果：交由 daemon 执行
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// 续写已有 session（若其正在运行，仅 L0 reply 直达会出现，由 daemon 以 Deliver 入队）
    Existing(String),
    /// 新建 session；`quoted` 为需作为首条上下文带入的被引用消息原文
    New { quoted: Option<String> },
    /// 一次性问答
    Oneshot,
    /// 向运行中的 session 插话（`Deliver`）
    Guide(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Routed {
    pub route: Route,
    /// 写入 session header 的 `routed_by`：`reply` / `first` / `router`
    pub routed_by: &'static str,
    /// 需要告知用户的提示（如 reply 目标已被清理）
    pub warning: Option<String>,
}

impl Routed {
    fn new(route: Route, routed_by: &'static str) -> Self {
        Self {
            route,
            routed_by,
            warning: None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RouteError {
    #[error("自动路由模型调用失败: {0}。请用 /switch <id> 指定会话，或 /new 新建")]
    Model(String),
    #[error("自动路由返回了非法 JSON: {0}。请用 /switch <id> 指定会话，或 /new 新建")]
    InvalidJson(String),
    #[error("自动路由选择了不存在的会话 {0}。请用 /sessions 查看，/switch 指定或 /new 新建")]
    UnknownSession(String),
    #[error("自动路由选择 existing 却未给出 session_id。请用 /switch 指定或 /new 新建")]
    MissingSessionId,
    #[error("带附件的消息不能作为一次性问答处理。请用 /switch 指定会话，或 /new 新建")]
    OneshotWithMedia,
    #[error("自动路由选择了正在运行的会话 {0}，但并非插话。请等待其完成，或用 /new 新建")]
    RunningNotGuide(String),
    #[error("自动路由决策自相矛盾: {0}。请用 /switch 指定会话，或 /new 新建")]
    Inconsistent(&'static str),
    #[error("没有匹配 `{0}` 的会话")]
    PrefixNotFound(String),
    #[error("前缀 `{prefix}` 匹配多个会话: {}，请输入更长的前缀", .candidates.join(", "))]
    PrefixAmbiguous {
        prefix: String,
        candidates: Vec<String>,
    },
}

// ================== 输入 ==================

/// 候选表行：仅四个字段（title 为小模型生成的会话标题，首轮完成前是占位标题）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub running: bool,
    pub focus: bool,
}

impl Candidate {
    pub fn from_header(header: &SessionHeader, running: bool, focus: Option<&str>) -> Self {
        Self {
            id: header.id.clone(),
            title: header.title.clone().unwrap_or_default(),
            running,
            focus: focus == Some(header.id.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplyRef {
    pub message_id: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct RouteInput {
    pub binding: String,
    pub text: String,
    pub reply_to: Option<ReplyRef>,
    /// 缓冲取出的附件与本条消息自带附件
    pub files: Vec<AttachedFile>,
    /// router 候选（已排除 oneshot / curator / superseded），无需预排序
    pub candidates: Vec<Candidate>,
}

/// 按 id 解析 `/switch` 前缀；`sess_` 可省略，完整 id 精确匹配优先
pub fn resolve_prefix(candidates: &[Candidate], prefix: &str) -> Result<String, RouteError> {
    let prefix = prefix.trim();
    if let Some(c) = candidates.iter().find(|c| c.id == prefix) {
        return Ok(c.id.clone());
    }
    let mut hits: Vec<String> = candidates
        .iter()
        .filter(|c| {
            !prefix.is_empty()
                && (c.id.starts_with(prefix)
                    || c.id
                        .strip_prefix("sess_")
                        .is_some_and(|s| s.starts_with(prefix)))
        })
        .map(|c| c.id.clone())
        .collect();
    hits.sort();
    match hits.len() {
        0 => Err(RouteError::PrefixNotFound(prefix.to_string())),
        1 => Ok(hits.remove(0)),
        _ => Err(RouteError::PrefixAmbiguous {
            prefix: prefix.to_string(),
            candidates: hits,
        }),
    }
}

// ================== 小模型 ==================

pub struct ModelReply {
    pub text: String,
    pub usage: Option<TokenUsage>,
}

/// L1 小模型：`system` 为固定前缀，`user` 为候选表在前、用户消息置尾的 JSON
#[async_trait]
pub trait RouteModel: Send + Sync {
    async fn decide(&self, system: &str, user: &str) -> Result<ModelReply, String>;
}

/// 基于 `settings.small_model` 与原生结构化输出的生产实现
pub struct SmallRouteModel {
    config: AppConfig,
}

impl SmallRouteModel {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl RouteModel for SmallRouteModel {
    async fn decide(&self, system: &str, user: &str) -> Result<ModelReply, String> {
        let spec = self
            .config
            .settings
            .small_model
            .as_deref()
            .ok_or("未配置 settings.small_model")?;
        let resolved = self
            .config
            .resolve_model(spec, None)
            .map_err(|e| e.to_string())?;
        let mut session = Session::new(".", None);
        session.append_turn(Turn::new(Role::System, vec![Message::text(system)]));
        session.append_turn(Turn::user_turn(Message::text(user)));
        let options = CompletionOptions {
            model: resolved.model_id.clone(),
            temperature: Some(0.0),
            max_tokens: Some(ROUTE_MAX_TOKENS),
            enable_web_search: false,
            enable_web_fetch: false,
            service_tier: resolved.model_config.service_tier().map(str::to_string),
            response_schema: Some(RouteDecision::response_schema()),
            ..CompletionOptions::default()
        };
        let response = resolved
            .create_provider()
            .complete(&session, &[], &options)
            .await
            .map_err(|e| e.to_string())?;
        // Anthropic 以 stop_reason 表达拒答与截断，此时输出不可信
        if let Some(reason) = response
            .meta
            .anthropic
            .as_ref()
            .and_then(|m| m.stop_reason.as_deref())
            .filter(|r| matches!(*r, "refusal" | "max_tokens"))
        {
            return Err(format!("小模型未正常结束 (stop_reason={reason})"));
        }
        Ok(ModelReply {
            text: response
                .messages
                .iter()
                .map(Message::text_content)
                .collect(),
            usage: response.usage,
        })
    }
}

// ================== Router ==================

const SYSTEM_PROMPT: &str = "\
你是个人 agent 的消息路由器。你只判断本条用户消息应交给哪个会话，不回答用户的问题。

输入是一个 JSON：candidates 为候选会话表（id、title 会话标题、running 是否正在生成、focus 是否为该聊天的默认会话），\
files 为随消息到达的附件（仅文件名与类型），reply_to 为用户引用的消息片段，user_text 为用户消息。你看不到任何会话的历史正文。

按以下优先级判断：
1. oneshot：寒暄、确认在线、简单感谢，或不依赖任何历史、答完即无长期价值的独立小问题。带附件（files 非空）的消息不得选 oneshot。
2. guide_existing：消息是对某个 running=true 会话的补充、修正、打断或改变方向。此时 action=existing，session_id 为该会话，running_intent=guide_existing。
3. existing：承接、追问、改写、继续前文、索取先前的结果或文件，或无法判断是否新话题。通常选 focus 会话；title 明显指向其他会话时选那个会话。
4. new：用户明确开启与所有候选会话主题完全无关、需要长期上下文的独立任务。

约束：
- 除 guide_existing 外，不要选择 running=true 的会话；独立任务应选 new。
- session_id 仅在 action=existing 时填写候选表中的 id，其余情况为 null。
- running_intent 仅在 guide_existing 时取该值，其余为 none。
- 对是否新话题有疑问时，选 existing，不要轻易 new。
- reason 用一句话说明理由。";

/// 提示词载荷：字段顺序即序列化顺序，候选表在前、用户消息置尾以利前缀缓存
#[derive(Serialize)]
struct Prompt<'a> {
    candidates: Vec<&'a Candidate>,
    files: &'a [AttachedFile],
    reply_to: Option<&'a ReplyRef>,
    user_text: &'a str,
}

#[derive(Serialize)]
struct LogEntry<'a> {
    ts: chrono::DateTime<Utc>,
    binding: &'a str,
    text: String,
    files: usize,
    decision: Option<&'a RouteDecision>,
    error: Option<String>,
    usage: Option<&'a TokenUsage>,
}

pub struct Router {
    model: Box<dyn RouteModel>,
    log_path: PathBuf,
}

impl Router {
    /// `log_path` 为 `router.jsonl` 路径
    pub fn new(model: Box<dyn RouteModel>, log_path: PathBuf) -> Self {
        Self { model, log_path }
    }

    pub async fn route(
        &self,
        input: &RouteInput,
        index: &MessageIndex,
    ) -> Result<Routed, RouteError> {
        let find = |id: &str| input.candidates.iter().find(|c| c.id == id);

        // L0：reply 锚点
        if let Some(reply) = &input.reply_to
            && let Some(session_id) = index.get(&reply.message_id)
        {
            tracing::debug!("路由 L0 命中 reply 锚点: session={session_id}");
            return Ok(match find(session_id) {
                Some(c) => Routed::new(Route::Existing(c.id.clone()), "reply"),
                None => Routed {
                    route: Route::New {
                        quoted: Some(reply.text.clone()),
                    },
                    routed_by: "reply",
                    warning: Some(format!(
                        "引用消息所属会话 {session_id} 已被清理，原上下文不可恢复；已新建会话并带入引用内容"
                    )),
                },
            });
        }
        // L0：零会话直接创建
        if input.candidates.is_empty() {
            tracing::debug!("路由 L0: 无候选会话，直接新建");
            return Ok(Routed::new(
                Route::New {
                    quoted: input.reply_to.as_ref().map(|r| r.text.clone()),
                },
                "first",
            ));
        }

        // L1
        let prompt = build_prompt(input);
        tracing::debug!(
            "路由 L1 调用小模型: 候选 {} 个, 提示词 {} 字符",
            input.candidates.len(),
            prompt.chars().count()
        );
        let reply = self.model.decide(SYSTEM_PROMPT, &prompt).await;
        let (decision, usage) = match reply {
            Ok(r) => (
                serde_json::from_str::<RouteDecision>(&r.text)
                    .map_err(|e| RouteError::InvalidJson(format!("{e}: {}", r.text))),
                r.usage,
            ),
            Err(e) => (Err(RouteError::Model(e)), None),
        };
        let validated = decision.and_then(|d| {
            let routed = validate(&d, input)?;
            Ok((d, routed))
        });
        match &validated {
            Ok((d, _)) => tracing::info!(
                "路由决策: action={:?} session_id={} reason={} 小模型 usage={}",
                d.action,
                d.session_id.as_deref().unwrap_or("-"),
                d.reason,
                usage.as_ref().map_or("无".into(), |u| format!(
                    "{}/{}",
                    u.prompt_tokens, u.completion_tokens
                ))
            ),
            Err(e) => tracing::warn!("路由失败 binding={}: {e}", input.binding),
        }
        self.log(input, &validated, usage.as_ref());
        validated.map(|(_, route)| Routed::new(route, "router"))
    }

    /// 决策追加一行 router.jsonl；落盘失败只告警，不影响本次路由
    fn log(
        &self,
        input: &RouteInput,
        result: &Result<(RouteDecision, Route), RouteError>,
        usage: Option<&TokenUsage>,
    ) {
        let entry = LogEntry {
            ts: Utc::now(),
            binding: &input.binding,
            text: input.text.chars().take(LOG_TEXT_LIMIT).collect(),
            files: input.files.len(),
            decision: result.as_ref().ok().map(|(d, _)| d),
            error: result.as_ref().err().map(ToString::to_string),
            usage,
        };
        let written = serde_json::to_string(&entry)
            .map_err(|e| e.to_string())
            .and_then(|line| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.log_path)
                    .and_then(|mut f| writeln!(f, "{line}"))
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = written {
            tracing::warn!("写入 {} 失败: {e}", self.log_path.display());
        }
    }
}

fn build_prompt(input: &RouteInput) -> String {
    let mut candidates: Vec<&Candidate> = input.candidates.iter().collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    let prompt = Prompt {
        candidates,
        files: &input.files,
        reply_to: input.reply_to.as_ref(),
        user_text: &input.text,
    };
    serde_json::to_string(&prompt).expect("Prompt 仅含可序列化字段")
}

/// L2：校验决策并转为 `Route`，任何不一致都显式报错
fn validate(d: &RouteDecision, input: &RouteInput) -> Result<Route, RouteError> {
    let guide = d.running_intent == RunningIntent::GuideExisting;
    match d.action {
        Action::Oneshot => {
            if !input.files.is_empty() {
                return Err(RouteError::OneshotWithMedia);
            }
            if guide {
                return Err(RouteError::Inconsistent("oneshot 不能携带 guide_existing"));
            }
            Ok(Route::Oneshot)
        }
        Action::New => {
            if guide {
                return Err(RouteError::Inconsistent("new 不能携带 guide_existing"));
            }
            Ok(Route::New { quoted: None })
        }
        Action::Existing => {
            let id = d.session_id.as_deref().filter(|s| !s.is_empty());
            let id = id.ok_or(RouteError::MissingSessionId)?;
            let target = input
                .candidates
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| RouteError::UnknownSession(id.to_string()))?;
            match (target.running, guide) {
                (true, true) => Ok(Route::Guide(target.id.clone())),
                (true, false) => Err(RouteError::RunningNotGuide(target.id.clone())),
                // 目标当前空闲时插话无意义，按普通续写处理
                (false, _) => Ok(Route::Existing(target.id.clone())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_space::binding::FileKind;
    use std::sync::Mutex;

    struct FakeModel {
        reply: Mutex<Option<Result<ModelReply, String>>>,
        prompts: Mutex<Vec<(String, String)>>,
    }

    impl FakeModel {
        fn replying(text: &str) -> Self {
            Self::with(Ok(ModelReply {
                text: text.into(),
                usage: Some(TokenUsage {
                    prompt_tokens: 120,
                    completion_tokens: 20,
                    total_tokens: 140,
                    ..TokenUsage::default()
                }),
            }))
        }
        fn with(r: Result<ModelReply, String>) -> Self {
            Self {
                reply: Mutex::new(Some(r)),
                prompts: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl RouteModel for std::sync::Arc<FakeModel> {
        async fn decide(&self, system: &str, user: &str) -> Result<ModelReply, String> {
            self.prompts
                .lock()
                .unwrap()
                .push((system.into(), user.into()));
            self.reply
                .lock()
                .unwrap()
                .take()
                .expect("模型被意外多次调用")
        }
    }

    fn decision_json(action: &str, sid: Option<&str>, intent: &str) -> String {
        json!({"action": action, "session_id": sid, "running_intent": intent, "reason": "测试"})
            .to_string()
    }

    fn cand(id: &str, running: bool, focus: bool) -> Candidate {
        Candidate {
            id: id.into(),
            title: format!("标题 {id}"),
            running,
            focus,
        }
    }

    fn input(candidates: Vec<Candidate>) -> RouteInput {
        RouteInput {
            binding: "qq:c2c:u1".into(),
            text: "继续".into(),
            reply_to: None,
            files: vec![],
            candidates,
        }
    }

    struct Env {
        dir: tempfile::TempDir,
        model: std::sync::Arc<FakeModel>,
        router: Router,
        index: MessageIndex,
    }

    fn env(model: FakeModel) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let model = std::sync::Arc::new(model);
        let router = Router::new(Box::new(model.clone()), dir.path().join("router.jsonl"));
        let index = MessageIndex::load(dir.path()).unwrap();
        Env {
            dir,
            model,
            router,
            index,
        }
    }

    fn two() -> Vec<Candidate> {
        vec![cand("sess_b", false, false), cand("sess_a", false, true)]
    }

    #[tokio::test]
    async fn reply_hit_goes_direct_without_model() {
        let mut e = env(FakeModel::with(Err("不应调用".into())));
        e.index.record("m1", "sess_b").unwrap();
        let mut i = input(two());
        i.reply_to = Some(ReplyRef {
            message_id: "m1".into(),
            text: "旧消息".into(),
        });
        let r = e.router.route(&i, &e.index).await.unwrap();
        assert_eq!(r.route, Route::Existing("sess_b".into()));
        assert_eq!(r.routed_by, "reply");
        assert!(e.model.prompts.lock().unwrap().is_empty());
        assert!(!e.dir.path().join("router.jsonl").exists());
    }

    #[tokio::test]
    async fn reply_to_cleaned_session_creates_new_with_quote() {
        let mut e = env(FakeModel::with(Err("不应调用".into())));
        e.index.record("m1", "sess_gone").unwrap();
        let mut i = input(two());
        i.reply_to = Some(ReplyRef {
            message_id: "m1".into(),
            text: "旧消息".into(),
        });
        let r = e.router.route(&i, &e.index).await.unwrap();
        assert_eq!(
            r.route,
            Route::New {
                quoted: Some("旧消息".into())
            }
        );
        assert!(r.warning.unwrap().contains("sess_gone"));
    }

    #[tokio::test]
    async fn zero_sessions_creates_new() {
        let e = env(FakeModel::with(Err("不应调用".into())));
        let r = e.router.route(&input(vec![]), &e.index).await.unwrap();
        assert_eq!(r.route, Route::New { quoted: None });
        assert_eq!(r.routed_by, "first");
    }

    #[tokio::test]
    async fn unindexed_reply_falls_to_l1_with_reply_text() {
        let e = env(FakeModel::replying(&decision_json(
            "existing",
            Some("sess_a"),
            "none",
        )));
        let mut i = input(two());
        i.reply_to = Some(ReplyRef {
            message_id: "unknown".into(),
            text: "引用原文".into(),
        });
        let r = e.router.route(&i, &e.index).await.unwrap();
        assert_eq!(r.route, Route::Existing("sess_a".into()));
        assert!(e.model.prompts.lock().unwrap()[0].1.contains("引用原文"));
    }

    #[tokio::test]
    async fn l1_decisions_map_to_routes() {
        let cases = [
            (decision_json("oneshot", None, "none"), Route::Oneshot),
            (
                decision_json("new", None, "none"),
                Route::New { quoted: None },
            ),
            (
                decision_json("existing", Some("sess_a"), "none"),
                Route::Existing("sess_a".into()),
            ),
            // 目标空闲时 guide 降级为普通续写
            (
                decision_json("existing", Some("sess_a"), "guide_existing"),
                Route::Existing("sess_a".into()),
            ),
        ];
        for (json, want) in cases {
            let e = env(FakeModel::replying(&json));
            let r = e.router.route(&input(two()), &e.index).await.unwrap();
            assert_eq!(r.route, want);
            assert_eq!(r.routed_by, "router");
        }
    }

    #[tokio::test]
    async fn guide_running_session() {
        let e = env(FakeModel::replying(&decision_json(
            "existing",
            Some("sess_r"),
            "guide_existing",
        )));
        let i = input(vec![
            cand("sess_r", true, true),
            cand("sess_a", false, false),
        ]);
        let r = e.router.route(&i, &e.index).await.unwrap();
        assert_eq!(r.route, Route::Guide("sess_r".into()));
    }

    async fn err_of(reply: Result<ModelReply, String>, i: RouteInput) -> RouteError {
        let e = env(FakeModel::with(reply));
        e.router.route(&i, &e.index).await.unwrap_err()
    }

    fn ok(text: &str) -> Result<ModelReply, String> {
        Ok(ModelReply {
            text: text.into(),
            usage: None,
        })
    }

    #[tokio::test]
    async fn invalid_decisions_are_explicit_errors() {
        assert!(matches!(
            err_of(ok("not json"), input(two())).await,
            RouteError::InvalidJson(_)
        ));
        // 未知字段 / 未知枚举值
        assert!(matches!(
            err_of(
                ok(r#"{"action":"casual","session_id":null,"running_intent":"none","reason":"x"}"#),
                input(two())
            )
            .await,
            RouteError::InvalidJson(_)
        ));
        assert!(matches!(
            err_of(ok(r#"{"action":"new","session_id":null,"running_intent":"none","reason":"x","extra":1}"#), input(two())).await,
            RouteError::InvalidJson(_)
        ));
        assert_eq!(
            err_of(
                ok(&decision_json("existing", Some("sess_zzz"), "none")),
                input(two())
            )
            .await,
            RouteError::UnknownSession("sess_zzz".into())
        );
        assert_eq!(
            err_of(ok(&decision_json("existing", None, "none")), input(two())).await,
            RouteError::MissingSessionId
        );
        assert_eq!(
            err_of(
                ok(&decision_json("existing", Some(""), "none")),
                input(two())
            )
            .await,
            RouteError::MissingSessionId
        );
        assert_eq!(
            err_of(
                ok(&decision_json("new", None, "guide_existing")),
                input(two())
            )
            .await,
            RouteError::Inconsistent("new 不能携带 guide_existing")
        );
        assert_eq!(
            err_of(
                ok(&decision_json("oneshot", None, "guide_existing")),
                input(two())
            )
            .await,
            RouteError::Inconsistent("oneshot 不能携带 guide_existing")
        );
        assert!(matches!(
            err_of(Err("stop_reason=refusal".into()), input(two())).await,
            RouteError::Model(_)
        ));
    }

    #[tokio::test]
    async fn oneshot_with_media_rejected() {
        let mut i = input(two());
        i.files.push(AttachedFile {
            path: "/tmp/a.png".into(),
            name: "a.png".into(),
            kind: FileKind::Image,
        });
        assert_eq!(
            err_of(ok(&decision_json("oneshot", None, "none")), i).await,
            RouteError::OneshotWithMedia
        );
    }

    #[tokio::test]
    async fn running_without_guide_rejected_not_focus_fallback() {
        let i = input(vec![
            cand("sess_r", true, true),
            cand("sess_a", false, false),
        ]);
        assert_eq!(
            err_of(ok(&decision_json("existing", Some("sess_r"), "none")), i).await,
            RouteError::RunningNotGuide("sess_r".into())
        );
    }

    #[tokio::test]
    async fn prompt_prefix_is_stable_and_logged() {
        let e = env(FakeModel::replying(&decision_json(
            "existing",
            Some("sess_a"),
            "none",
        )));
        e.router.route(&input(two()), &e.index).await.unwrap();
        let (system, user) = e.model.prompts.lock().unwrap()[0].clone();
        assert_eq!(system, SYSTEM_PROMPT);
        // 候选表按 id 排序，且位于用户消息之前；候选行仅四个字段
        let a = user.find("sess_a").unwrap();
        let b = user.find("sess_b").unwrap();
        assert!(a < b && b < user.find("user_text").unwrap());
        let v: serde_json::Value = serde_json::from_str(&user).unwrap();
        let mut keys: Vec<_> = v["candidates"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(keys, ["focus", "id", "running", "title"]);

        let log = std::fs::read_to_string(e.dir.path().join("router.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(line["binding"], "qq:c2c:u1");
        assert_eq!(line["decision"]["session_id"], "sess_a");
        assert_eq!(line["usage"]["prompt_tokens"], 120);
        assert!(line["error"].is_null());
    }

    #[tokio::test]
    async fn failures_are_logged_too() {
        let e = env(FakeModel::replying("garbage"));
        e.router.route(&input(two()), &e.index).await.unwrap_err();
        let log = std::fs::read_to_string(e.dir.path().join("router.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert!(line["decision"].is_null());
        assert!(line["error"].as_str().unwrap().contains("非法 JSON"));
    }

    #[test]
    fn prefix_resolution() {
        let c = vec![
            cand("sess_ab123", false, false),
            cand("sess_ab456", false, false),
            cand("sess_cd789", false, false),
        ];
        assert_eq!(resolve_prefix(&c, "sess_cd").unwrap(), "sess_cd789");
        assert_eq!(resolve_prefix(&c, "cd7").unwrap(), "sess_cd789");
        assert_eq!(resolve_prefix(&c, "sess_ab123").unwrap(), "sess_ab123");
        match resolve_prefix(&c, "ab").unwrap_err() {
            RouteError::PrefixAmbiguous { candidates, .. } => {
                assert_eq!(candidates, ["sess_ab123", "sess_ab456"]);
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(
            resolve_prefix(&c, "zz"),
            Err(RouteError::PrefixNotFound(_))
        ));
        assert!(matches!(
            resolve_prefix(&c, ""),
            Err(RouteError::PrefixNotFound(_))
        ));
    }

    #[test]
    fn candidate_title_comes_from_header() {
        let s = Session::new(".", Some("占位标题".into()));
        let c = Candidate::from_header(&s.header, true, Some(&s.header.id.clone()));
        assert_eq!(c.title, "占位标题");
        assert!(c.running && c.focus);
    }

    #[test]
    fn schema_matches_decision_fields() {
        let s = RouteDecision::response_schema();
        assert_eq!(s.schema["additionalProperties"], false);
        let d: RouteDecision =
            serde_json::from_str(&decision_json("existing", Some("sess_a"), "none")).unwrap();
        let props = s.schema["properties"].as_object().unwrap();
        let ser = serde_json::to_value(&d).unwrap();
        assert_eq!(ser.as_object().unwrap().len(), props.len());
    }
}
