//! Router：L0 确定性锚点 / L1 小模型 / L2 显式报错。
//! 依据 focus 元数据、用户文字、reply 片段与附件名判断是否新建，不读历史；
//! 低置信度决策续接当前 focus；调用失败与非法决策显式报错。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::binding::{AttachedFile, MessageIndex};
use crate::config::AppConfig;
use crate::model::{Message, Role, Session, TokenUsage, Turn};
use crate::provider::{CompletionOptions, ResponseSchema};

/// 日志中输入文字的摘要上限（字符数）
const LOG_TEXT_LIMIT: usize = 200;
/// 路由决策只含短字段，输出上限足够
const ROUTE_MAX_TOKENS: u32 = 300;

// ================== 决策模型 ==================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Existing,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTtl {
    #[serde(rename = "3h")]
    Short,
    #[serde(rename = "12h")]
    Normal,
}

impl SessionTtl {
    pub fn seconds(self) -> u64 {
        match self {
            Self::Short => 3 * 3600,
            Self::Normal => 12 * 3600,
        }
    }
}

/// L1 结构化决策：小模型原生结构化输出，或 Decisions 答案回填
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteDecision {
    pub action: Action,
    pub focus: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub ttl: Option<SessionTtl>,
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
                    "action": {"type": "string", "enum": ["existing", "new"]},
                    "focus": {"type": "boolean"},
                    "ttl": {"anyOf": [{"type": "string", "enum": ["3h", "12h"]}, {"type": "null"}]},
                    "reason": {"type": "string"}
                },
                "required": ["action", "focus", "ttl", "reason"],
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
    New {
        quoted: Option<String>,
        focus: bool,
        ttl: SessionTtl,
    },
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
    #[error("自动路由失败: {0}。请引用回复目标会话的消息，或用 /new <消息> 新建")]
    Model(String),
    #[error(
        "自动路由无法续接默认会话: {0}。请用 /switch <id> 设置默认会话后重发，或用 /new <消息> 新建"
    )]
    Uncertain(String),
    #[error("自动路由决策自相矛盾: {0}。请引用回复目标会话的消息，或用 /new <消息> 新建")]
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
    /// router 候选（已排除 curator / superseded），无需预排序
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

/// 区分判断不明确与非法输出，供调度层应用默认续接规则。
#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    #[error("{0}")]
    Invalid(String),
    #[error("Decisions 置信度不足: {question}={choice} ({confidence:.2} < {minimum:.2})")]
    LowConfidence {
        question: &'static str,
        choice: String,
        confidence: f64,
        minimum: f64,
    },
}

impl From<String> for DecisionError {
    fn from(error: String) -> Self {
        Self::Invalid(error)
    }
}

/// 模型已应答（已计费）的结果：usage 随决策与错误一起交回调度层。
#[derive(Debug)]
pub struct ModelReply {
    pub decision: Result<RouteDecision, DecisionError>,
    pub usage: Option<TokenUsage>,
}

/// L1 模型：自行把 `input` 组装为后端请求，`Err` 表示调用未得到应答。
/// 只负责产出形状合法的决策，语义校验由 `Router` 统一完成
#[async_trait]
pub trait RouteModel: Send + Sync {
    async fn decide(&self, input: &RouteInput) -> Result<ModelReply, String>;
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
    async fn decide(&self, input: &RouteInput) -> Result<ModelReply, String> {
        // 系统提示固定在前，候选表在前、用户消息置尾的 JSON 作为 user 轮
        let prompt = serde_json::to_string(&Prompt::new(input)).expect("Prompt 仅含可序列化字段");
        tracing::debug!(
            "路由 L1 调用小模型: 候选 {} 个, 提示词 {} 字符",
            input.candidates.len(),
            prompt.chars().count()
        );
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
        session.append_turn(Turn::new(Role::System, vec![Message::text(SYSTEM_PROMPT)]));
        session.append_turn(Turn::user_turn(Message::text(prompt)));
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
        let text: String = response
            .messages
            .iter()
            .map(Message::text_content)
            .collect();
        Ok(ModelReply {
            decision: serde_json::from_str(&text)
                .map_err(|e| DecisionError::Invalid(format!("小模型返回了非法 JSON: {e}: {text}"))),
            usage: response.usage,
        })
    }
}

// ================== Router ==================

const SYSTEM_PROMPT: &str = "\
你是个人 agent 的消息路由器，只判断是否新建会话，不回答用户的问题。

输入 JSON 中 focus 为当前默认会话（可为 null），包含 id、title、running；files 为附件名与类型，reply_to 为引用片段，user_text 为用户消息。你看不到历史正文。

规则：
- 默认 action=existing：承接、追问、改写、继续前文、索取先前结果或文件，以及无法确定是否新话题。始终续接当前 focus，运行状态由程序处理。
- 用户明确开启与 focus 无关的独立话题或任务时 action=new。
- 新建时独立判断 focus：预期持续讨论的新话题为 true；oneshot 一次性任务为 false，保留原 focus。
- 新建时选择 ttl：一次性、短期价值的任务用 3h；正常会话用 12h。TTL 与是否切换 focus 分开判断。
- action=existing 时 focus=false、ttl=null；action=new 时 ttl 必须为 3h 或 12h。
- reason 用一句话说明判断依据。";

/// 路由输入载荷：只向模型提供当前 focus，用户消息置尾以利前缀缓存。
#[derive(Serialize)]
pub(super) struct Prompt<'a> {
    focus: Option<&'a Candidate>,
    files: &'a [AttachedFile],
    reply_to: Option<&'a ReplyRef>,
    user_text: &'a str,
}

impl<'a> Prompt<'a> {
    pub fn new(input: &'a RouteInput) -> Self {
        Self {
            focus: input.candidates.iter().find(|c| c.focus),
            files: &input.files,
            reply_to: input.reply_to.as_ref(),
            user_text: &input.text,
        }
    }
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
                        focus: true,
                        ttl: SessionTtl::Normal,
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
                    focus: true,
                    ttl: SessionTtl::Normal,
                },
                "first",
            ));
        }

        // L1
        let (validated, usage) = match self.model.decide(input).await {
            Ok(ModelReply { decision, usage }) => (
                decision
                    .or_else(|error| match error {
                        DecisionError::Invalid(error) => Err(RouteError::Model(error)),
                        DecisionError::LowConfidence { .. } => Ok(RouteDecision {
                            action: Action::Existing,
                            focus: false,
                            ttl: None,
                            reason: format!("续接默认会话: {error}"),
                        }),
                    })
                    .and_then(|d| validate(&d, input).map(|route| (d, route))),
                usage,
            ),
            Err(e) => (Err(RouteError::Model(e)), None),
        };
        match &validated {
            Ok((d, _)) => tracing::info!(
                "路由决策: action={:?} focus={} ttl={:?} reason={} 小模型 usage={}",
                d.action,
                d.focus,
                d.ttl,
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

/// L2：校验决策并转为 `Route`，任何不一致都显式报错
fn validate(d: &RouteDecision, input: &RouteInput) -> Result<Route, RouteError> {
    match d.action {
        Action::New => Ok(Route::New {
            quoted: None,
            focus: d.focus,
            ttl: d.ttl.ok_or(RouteError::Inconsistent("new 必须指定 ttl"))?,
        }),
        Action::Existing => {
            if d.focus || d.ttl.is_some() {
                return Err(RouteError::Inconsistent(
                    "existing 的 focus 须为 false、ttl 须为 null",
                ));
            }
            let target = input
                .candidates
                .iter()
                .find(|c| c.focus)
                .ok_or_else(|| RouteError::Uncertain("当前没有 focus".into()))?;
            if target.running {
                Ok(Route::Guide(target.id.clone()))
            } else {
                Ok(Route::Existing(target.id.clone()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::binding::FileKind;
    use std::sync::{Arc, Mutex};

    struct FakeModel {
        reply: Mutex<Option<Result<ModelReply, String>>>,
        /// 每次调用收到的提示词载荷
        prompts: Mutex<Vec<String>>,
    }

    impl FakeModel {
        fn deciding(decision: RouteDecision) -> Self {
            Self::with(Ok(ModelReply {
                decision: Ok(decision),
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
    impl RouteModel for Arc<FakeModel> {
        async fn decide(&self, input: &RouteInput) -> Result<ModelReply, String> {
            self.prompts
                .lock()
                .unwrap()
                .push(serde_json::to_string(&Prompt::new(input)).unwrap());
            self.reply
                .lock()
                .unwrap()
                .take()
                .expect("模型被意外多次调用")
        }
    }

    fn decision(action: Action, focus: bool, ttl: Option<SessionTtl>) -> RouteDecision {
        RouteDecision {
            action,
            focus,
            ttl,
            reason: "测试".into(),
        }
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
        model: Arc<FakeModel>,
        router: Router,
        index: MessageIndex,
    }

    fn env(model: FakeModel) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(model);
        let router = Router::new(Box::new(model.clone()), dir.path().join("router.jsonl"));
        let index = MessageIndex::load(dir.path()).unwrap();
        Env {
            dir,
            model,
            router,
            index,
        }
    }

    /// sess_a 为 focus，sess_r 正在运行
    fn candidates() -> Vec<Candidate> {
        vec![
            cand("sess_r", true, false),
            cand("sess_b", false, false),
            cand("sess_a", false, true),
        ]
    }

    fn no_model() -> FakeModel {
        FakeModel::with(Err("不应调用".into()))
    }

    fn uncertain(question: &'static str, choice: &str) -> FakeModel {
        FakeModel::with(Ok(ModelReply {
            decision: Err(DecisionError::LowConfidence {
                question,
                choice: choice.into(),
                confidence: 0.25,
                minimum: 0.75,
            }),
            usage: Some(TokenUsage {
                prompt_tokens: 80,
                ..TokenUsage::default()
            }),
        }))
    }

    #[tokio::test]
    async fn uncertain_decision_continues_focus_and_records_reason() {
        for (question, choice) in [
            ("action", "existing"),
            ("action", "new"),
            ("focus", "switch"),
            ("ttl", "3h"),
        ] {
            let e = env(uncertain(question, choice));
            let mut i = input(candidates());
            i.files.push(AttachedFile {
                path: "/tmp/diagram.png".into(),
                name: "diagram.png".into(),
                kind: FileKind::Image,
            });
            let r = e.router.route(&i, &e.index).await.unwrap();
            assert_eq!(r.route, Route::Existing("sess_a".into()));
            let log = std::fs::read_to_string(e.dir.path().join("router.jsonl")).unwrap();
            let entry: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
            assert_eq!(entry["decision"]["action"], "existing");
            assert!(
                entry["decision"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("置信度不足")
            );
            assert_eq!(entry["usage"]["prompt_tokens"], 80);
            assert!(entry["error"].is_null());
        }
    }

    #[tokio::test]
    async fn uncertain_message_is_delivered_to_running_focus() {
        let e = env(uncertain("action", "new"));
        let i = input(vec![
            cand("sess_a", true, true),
            cand("sess_b", false, false),
        ]);
        assert_eq!(
            e.router.route(&i, &e.index).await.unwrap().route,
            Route::Guide("sess_a".into())
        );
    }

    #[tokio::test]
    async fn uncertainty_without_focus_and_invalid_answers_remain_errors() {
        let e = env(uncertain("action", "existing"));
        let i = input(vec![cand("sess_b", false, false)]);
        assert!(matches!(
            e.router.route(&i, &e.index).await.unwrap_err(),
            RouteError::Uncertain(_)
        ));

        let e = env(FakeModel::with(Ok(ModelReply {
            decision: Err(DecisionError::Invalid("缺少必填字段".into())),
            usage: None,
        })));
        assert_eq!(
            e.router
                .route(&input(candidates()), &e.index)
                .await
                .unwrap_err(),
            RouteError::Model("缺少必填字段".into())
        );
    }

    #[tokio::test]
    async fn reply_hit_goes_direct_without_model() {
        let mut e = env(no_model());
        e.index.record("m1", "sess_b").unwrap();
        let mut i = input(candidates());
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
        let mut e = env(no_model());
        e.index.record("m1", "sess_gone").unwrap();
        let mut i = input(candidates());
        i.reply_to = Some(ReplyRef {
            message_id: "m1".into(),
            text: "旧消息".into(),
        });
        let r = e.router.route(&i, &e.index).await.unwrap();
        assert_eq!(
            r.route,
            Route::New {
                quoted: Some("旧消息".into()),
                focus: true,
                ttl: SessionTtl::Normal
            }
        );
        assert!(r.warning.unwrap().contains("sess_gone"));
    }

    #[tokio::test]
    async fn zero_sessions_creates_new() {
        let e = env(no_model());
        let r = e.router.route(&input(vec![]), &e.index).await.unwrap();
        assert_eq!(
            r.route,
            Route::New {
                quoted: None,
                focus: true,
                ttl: SessionTtl::Normal
            }
        );
        assert_eq!(r.routed_by, "first");
    }

    #[tokio::test]
    async fn unindexed_reply_falls_to_l1_with_reply_text() {
        let e = env(FakeModel::deciding(decision(Action::Existing, false, None)));
        let mut i = input(candidates());
        i.reply_to = Some(ReplyRef {
            message_id: "unknown".into(),
            text: "引用原文".into(),
        });
        let r = e.router.route(&i, &e.index).await.unwrap();
        assert_eq!(r.route, Route::Existing("sess_a".into()));
        assert!(e.model.prompts.lock().unwrap()[0].contains("引用原文"));
    }

    #[tokio::test]
    async fn new_decisions_preserve_independent_focus_and_ttl() {
        for focus in [false, true] {
            for ttl in [SessionTtl::Short, SessionTtl::Normal] {
                let e = env(FakeModel::deciding(decision(Action::New, focus, Some(ttl))));
                let r = e
                    .router
                    .route(&input(candidates()), &e.index)
                    .await
                    .unwrap();
                assert_eq!(
                    r.route,
                    Route::New {
                        quoted: None,
                        focus,
                        ttl
                    }
                );
                assert_eq!(r.routed_by, "router");
            }
        }
    }

    #[tokio::test]
    async fn existing_always_continues_focus_with_running_state_handled_by_router() {
        for running in [false, true] {
            let e = env(FakeModel::deciding(decision(Action::Existing, false, None)));
            let i = input(vec![
                cand("sess_other", !running, false),
                cand("sess_focus", running, true),
            ]);
            let r = e.router.route(&i, &e.index).await.unwrap();
            let want = if running {
                Route::Guide("sess_focus".into())
            } else {
                Route::Existing("sess_focus".into())
            };
            assert_eq!(r.route, want);
        }
    }

    #[tokio::test]
    async fn invalid_decisions_and_missing_focus_are_explicit_errors() {
        let cases = [
            (decision(Action::New, false, None), input(candidates())),
            (decision(Action::Existing, true, None), input(candidates())),
            (
                decision(Action::Existing, false, Some(SessionTtl::Short)),
                input(candidates()),
            ),
        ];
        for (d, i) in cases {
            let e = env(FakeModel::deciding(d));
            assert!(matches!(
                e.router.route(&i, &e.index).await,
                Err(RouteError::Inconsistent(_))
            ));
        }
        let e = env(FakeModel::deciding(decision(Action::Existing, false, None)));
        assert!(matches!(
            e.router
                .route(&input(vec![cand("sess_other", false, false)]), &e.index)
                .await,
            Err(RouteError::Uncertain(_))
        ));
    }

    #[tokio::test]
    async fn prompt_only_exposes_focus_and_decision_is_logged() {
        let e = env(FakeModel::deciding(decision(Action::Existing, false, None)));
        e.router
            .route(&input(candidates()), &e.index)
            .await
            .unwrap();
        let user = e.model.prompts.lock().unwrap()[0].clone();
        assert!(!user.contains("sess_b"));
        assert!(!user.contains("sess_r"));
        assert!(user.find("sess_a").unwrap() < user.find("user_text").unwrap());

        let log = std::fs::read_to_string(e.dir.path().join("router.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(line["binding"], "qq:c2c:u1");
        assert_eq!(line["decision"]["action"], "existing");
        assert_eq!(line["usage"]["prompt_tokens"], 120);
        assert!(line["error"].is_null());
    }

    #[tokio::test]
    async fn failures_are_logged_too() {
        let e = env(FakeModel::with(Err("模型不可用".into())));
        e.router
            .route(&input(candidates()), &e.index)
            .await
            .unwrap_err();
        let log = std::fs::read_to_string(e.dir.path().join("router.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert!(line["decision"].is_null());
        assert!(line["error"].as_str().unwrap().contains("模型不可用"));
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
    fn decision_requires_all_fields_and_rejects_invalid_ttl() {
        let valid = serde_json::to_value(decision(Action::Existing, false, None)).unwrap();
        assert!(serde_json::from_value::<RouteDecision>(valid.clone()).is_ok());
        for field in ["action", "focus", "ttl", "reason"] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<RouteDecision>(missing).is_err());
        }
        let mut invalid = valid;
        invalid["ttl"] = json!("0h");
        assert!(serde_json::from_value::<RouteDecision>(invalid).is_err());
    }

    #[test]
    fn schema_matches_decision_fields() {
        let s = RouteDecision::response_schema();
        let props = s.schema["properties"].as_object().unwrap();
        let ser =
            serde_json::to_value(decision(Action::New, true, Some(SessionTtl::Normal))).unwrap();
        let mut fields: Vec<_> = ser.as_object().unwrap().keys().collect();
        let mut schema_fields: Vec<_> = props.keys().collect();
        fields.sort();
        schema_fields.sort();
        assert_eq!(fields, schema_fields);
    }
}
