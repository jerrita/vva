use std::path::PathBuf;
use std::sync::Arc;

use crate::config::AppConfig;
use crate::model::{Message, Role, Session, Turn};
use crate::provider::{CompletionOptions, Provider};
use crate::tools::{ToolRegistry, WorkspaceToolDeps};
use tokio::sync::mpsc;

/// 能力协商：补全选项与供应商原生能力不匹配时生成显式降级通知（不静默失效）。
/// `external_search` 为真表示外置 web search 工具已接管，不再报"选项被忽略"。
pub fn capability_notices(
    provider: &dyn Provider,
    options: &CompletionOptions,
    external_search: bool,
) -> Vec<String> {
    let caps = provider.capabilities();
    let mut notices = Vec::new();
    if options.enable_web_search && !caps.supports_native_search && !external_search {
        notices.push(format!(
            "Web search is enabled but provider '{}' has no native search support; the option is ignored.",
            provider.name()
        ));
    }
    if let Some(effort) = &options.reasoning_effort
        && !effort.is_empty()
        && !caps.supports_reasoning_effort
    {
        notices.push(format!(
            "Reasoning effort '{effort}' is set but provider '{}' does not support the effort parameter; it is ignored.",
            provider.name()
        ));
    }
    notices
}

/// 会话工具工厂：按会话 header 决定额外挂载的运行期工具（如渠道绑定的 SendFile）
pub type SessionToolFactory =
    Arc<dyn Fn(&crate::model::SessionHeader) -> Vec<Arc<dyn crate::tools::Tool>> + Send + Sync>;

pub(super) struct AgentEngine {
    pub provider: Arc<dyn Provider>,
    pub tools: ToolRegistry,
    pub options: CompletionOptions,
    /// 能力协商通知：原生能力降级提示，任务启动时发布
    pub notices: Vec<String>,
    /// 外置 web 工具接管的后端名：原生能力缺位时由客户端工具补全，
    /// 经 SessionView 发布，由状态栏标注而非聊天通知
    pub external_web: Vec<String>,
    /// 当前模型的输入能力，供提交前校验用户附件
    pub(super) inputs: crate::config::resolver::InputCapabilities,
}

/// 引擎装配依赖：会话级共享句柄与每次构建时重新发现的清单
pub(super) struct EngineDeps {
    pub tool_outputs: PathBuf,
    pub hashline: Arc<crate::tools::hashline::HashlineSession>,
    pub ask_user_requests: mpsc::Sender<crate::tools::ask_user::AskUserRequest>,
    pub background: Arc<crate::tools::background::BackgroundTasks>,
    pub skills: Vec<crate::skills::Skill>,
    pub agents: Vec<crate::agents::AgentDef>,
    pub broker: Arc<dyn crate::tools::agent::SessionBroker>,
    /// 工厂产出的会话专属工具，最后注册
    pub extra_tools: Vec<Arc<dyn crate::tools::Tool>>,
}

impl AgentEngine {
    pub fn new(config: &AppConfig, session: &Session, deps: EngineDeps) -> Result<Self, String> {
        let resolved = config
            .resolve_model(
                &session.header.current_model,
                session.header.current_variant.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        let inputs = resolved.input_capabilities();
        inputs.validate_session(session)?;
        let agent_def = session
            .header
            .agent
            .as_deref()
            .map(|name| {
                deps.agents
                    .iter()
                    .find(|def| def.name == name)
                    .cloned()
                    .ok_or_else(|| format!("Agent definition '{name}' is unavailable"))
            })
            .transpose()?;
        let mut tools = Self::base_tools(session, &inputs, deps);
        // subagent 会话按定义白名单裁剪
        let allowed = |name: &str| {
            agent_def
                .as_ref()
                .is_none_or(|def| crate::tools::tool_allowed(&def.tools, name))
        };
        let search_allowed = allowed("WebSearch");
        let fetch_allowed = allowed("WebFetch");
        let provider = resolved.create_provider();
        let capabilities = provider.capabilities();
        // Responses 的 web_search 同时提供 open_page/find_in_page，须授权完整工具能力。
        let bundled_fetch = capabilities.server_tool_names.contains(&"web_search")
            && !capabilities.server_tool_names.contains(&"web_fetch");
        let affinity = resolved.affinity();
        let max_tokens = resolved
            .model_config
            .limit
            .as_ref()
            .map(|limit| {
                u32::try_from(limit.output)
                    .map_err(|_| format!("Model output limit exceeds u32: {}", limit.output))
            })
            .transpose()?;
        let web_declared = session.header.web_search && resolved.model_config.declare_web_search();
        let options = CompletionOptions {
            model: resolved.model_id,
            max_tokens,
            reasoning_effort: resolved.selected_variant,
            enable_web_search: web_declared && search_allowed && (!bundled_fetch || fetch_allowed),
            enable_web_fetch: web_declared
                && resolved.model_config.declare_web_fetch()
                && fetch_allowed,
            service_tier: resolved.model_config.service_tier().map(str::to_string),
            affinity: Some(affinity.clone()),
            ..Default::default()
        };
        // 开启原生联网搜索时预留服务端工具名：端点未代执行而由模型误发起
        // 客户端调用时，注册表给出显式诊断而非误导性的 Tool not found
        tools.reserve_server_tools(
            capabilities
                .server_tool_names
                .iter()
                .copied()
                .filter(|name| match *name {
                    "web_search" => options.enable_web_search,
                    "web_fetch" => options.enable_web_fetch,
                    _ => allowed(name),
                }),
        );
        // 原生 web 能力缺位时以外置 provider 的客户端工具补全（配置错误在此显式报错）
        let mut external_web = Vec::new();
        let mut external_search = false;
        if session.header.web_search && (search_allowed || fetch_allowed) {
            let external = crate::tools::web::ExternalWeb::resolve(config.settings.web.as_ref())?;
            let native_search = options.enable_web_search && capabilities.supports_native_search;
            // 独立 web_fetch server tool（Anthropic）按声明位；
            // 无独立工具时，web_search 含 open_page（Response）随原生搜索在位
            let native_fetch = if capabilities.server_tool_names.contains(&"web_fetch") {
                options.enable_web_fetch
            } else {
                native_search && bundled_fetch
            };
            if search_allowed
                && !native_search
                && let Some(search) = &external.search
            {
                tools.register(Arc::new(crate::tools::web::tool::WebSearchTool::new(
                    search.clone(),
                )));
                external_web.push(search.name().to_string());
                external_search = true;
            }
            if fetch_allowed
                && !native_fetch
                && let Some(fetch) = &external.fetch
            {
                tools.register(Arc::new(crate::tools::web::tool::WebFetchTool::new(
                    fetch.clone(),
                )));
                external_web.push(fetch.name().to_string());
            }
            external_web.dedup();
        }
        if let Some(def) = &agent_def {
            tools.retain(&def.tools);
        }
        let mut notices: Vec<String> = affinity_notice(session, &affinity).into_iter().collect();
        notices.extend(capability_notices(
            provider.as_ref(),
            &options,
            external_search,
        ));
        Ok(Self {
            provider,
            tools,
            options,
            notices,
            external_web,
            inputs,
        })
    }

    /// 工作空间工具与会话级工具 (交互 / 后台 / 技能 / 子代理编排)
    fn base_tools(
        session: &Session,
        inputs: &crate::config::resolver::InputCapabilities,
        deps: EngineDeps,
    ) -> ToolRegistry {
        let mut tools = ToolRegistry::with_workspace_tools(
            &session.header.workspace_root,
            Some(deps.tool_outputs),
            session.header.editor,
            WorkspaceToolDeps {
                hashline: deps.hashline,
                inputs: inputs.clone(),
                background: Some(deps.background.clone()),
            },
        );
        let broker = deps.broker;
        let team = session.header.team;
        let mut task_stop = crate::tools::background::TaskStopTool::new(deps.background.clone());
        if team {
            task_stop = task_stop.with_broker(broker.clone());
        }
        let mut extra: Vec<Arc<dyn crate::tools::Tool>> = vec![
            Arc::new(crate::tools::background::TaskOutputTool::new(
                deps.background,
            )),
            Arc::new(task_stop),
            Arc::new(crate::tools::skill::SkillTool::new(Arc::new(deps.skills))),
        ];
        // agent space 会话无人值守，不挂载 AskUser（工具定义即提示词，一并缺席）
        if !session.header.agent_managed() {
            extra.push(Arc::new(crate::tools::ask_user::AskUserTool::new(
                deps.ask_user_requests,
            )));
        }
        extra.extend(deps.extra_tools);
        for tool in extra {
            tools.register(tool);
        }
        // team 关闭时不挂载子代理编排工具（工具定义即提示词，一并缺席）
        if team {
            let team_tools: [Arc<dyn crate::tools::Tool>; 3] = [
                Arc::new(crate::tools::agent::AgentTool::new(
                    broker.clone(),
                    // curator 由 agent space 系统创建，不可由模型派生
                    Arc::new(
                        deps.agents
                            .into_iter()
                            .filter(|def| def.name != crate::agent_space::curator::CURATOR_AGENT)
                            .collect(),
                    ),
                    crate::tools::agent::SubagentContext::from_session(session),
                )),
                Arc::new(crate::tools::session_ipc::SendMessageTool::new(
                    broker.clone(),
                )),
                Arc::new(crate::tools::session_ipc::WaitSessionTool::new(broker)),
            ];
            for tool in team_tools {
                tools.register(tool);
            }
        }
        tools
    }
}

/// 亲和组切换：前一段原生状态（链锚点 / 签名块）无法回传，历史降维重建，显式告知
fn affinity_notice(session: &Session, affinity: &str) -> Option<String> {
    let previous = session.turns.iter().rev().find_map(|turn| {
        if turn.role != Role::Assistant {
            return None;
        }
        let response = turn
            .meta
            .response
            .as_ref()
            .and_then(|meta| meta.scope.as_deref());
        response.or_else(|| {
            turn.meta
                .anthropic
                .as_ref()
                .and_then(|meta| meta.scope.as_deref())
        })
    })?;
    (previous != affinity).then(|| {
        format!(
            "Native context from '{previous}' cannot be replayed under affinity '{affinity}'; history has been rebuilt in plain form."
        )
    })
}

pub(super) async fn generate_title(
    config: &AppConfig,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    let resolved = config
        .resolve_model(
            config.settings.small_model.as_deref().unwrap_or(model),
            None,
        )
        .map_err(|e| e.to_string())?;
    let mut session = Session::new(".", None);
    session.append_turn(Turn::user_turn(Message::text(format!(
        "Generate a brief title (3-6 words, plain text, no quotes) summarizing this prompt. Put spaces between Chinese and English text. Return only the title.\n\n{prompt}"
    ))));
    let options = CompletionOptions {
        model: resolved.model_id.clone(),
        temperature: Some(0.3),
        max_tokens: Some(30),
        enable_web_search: false,
        enable_web_fetch: false,
        reasoning_effort: None,
        service_tier: resolved.model_config.service_tier().map(str::to_string),
        affinity: None,
        response_schema: None,
    };
    let response = resolved
        .create_provider()
        .complete(&session, &[], &options)
        .await
        .map_err(|e| e.to_string())?;
    Ok(response
        .messages
        .iter()
        .map(Message::text_content)
        .collect::<String>()
        .trim()
        .trim_matches(['"', '\'', '`', '#', '*'])
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderError, ProviderResponse};

    /// 无原生搜索能力的供应商桩：覆盖能力协商的降级通知分支
    struct NoSearchProvider;

    #[async_trait::async_trait]
    impl Provider for NoSearchProvider {
        fn name(&self) -> &str {
            "no-search"
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            crate::provider::ProviderCapabilities {
                supports_native_search: false,
                supports_reasoning_effort: false,
                server_tool_names: Vec::new(),
            }
        }

        async fn complete(
            &self,
            _session: &Session,
            _tools: &[crate::model::ToolDefinition],
            _options: &CompletionOptions,
        ) -> Result<ProviderResponse, ProviderError> {
            unimplemented!()
        }

        async fn complete_stream(
            &self,
            _session: &Session,
            _tools: &[crate::model::ToolDefinition],
            _options: &CompletionOptions,
            _event_sender: Option<tokio::sync::mpsc::Sender<crate::event::AgentEvent>>,
        ) -> Result<ProviderResponse, ProviderError> {
            unimplemented!()
        }
    }

    #[test]
    fn test_capability_notices_flag_unsupported_options() {
        // 全能力供应商（Responses）无通知
        let options = CompletionOptions {
            model: "claude-x".into(),
            enable_web_search: true,
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let response = crate::provider::response::ResponseProvider::new("k");
        assert!(capability_notices(&response, &options, false).is_empty());

        // 无原生搜索能力的供应商生成搜索降级通知
        let search_only = CompletionOptions {
            model: "claude-x".into(),
            enable_web_search: true,
            ..Default::default()
        };
        let notices = capability_notices(&NoSearchProvider, &search_only, false);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("native search"));
        assert!(notices[0].contains("no-search"));

        // 外置搜索已接管时不再报"选项被忽略"
        assert!(capability_notices(&NoSearchProvider, &search_only, true).is_empty());
    }

    #[test]
    fn test_capability_notices_flag_reasoning_effort_only() {
        // Anthropic 已支持原生搜索，仅推理档位生成降级通知
        let provider = crate::provider::anthropic::AnthropicProvider::new("k");
        let options = CompletionOptions {
            model: "claude-x".into(),
            enable_web_search: true,
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let notices = capability_notices(&provider, &options, false);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("Reasoning effort 'high'"));
    }

    /// AgentEngine::new 的测试构造器：不发起网络请求
    fn build_engine(config: &AppConfig, session: &Session) -> Result<AgentEngine, String> {
        build_engine_with_agents(config, session, crate::agents::builtin_agents())
    }

    fn build_engine_with_agents(
        config: &AppConfig,
        session: &Session,
        agents: Vec<crate::agents::AgentDef>,
    ) -> Result<AgentEngine, String> {
        build_engine_full(config, session, agents, Vec::new())
    }

    fn build_engine_full(
        config: &AppConfig,
        session: &Session,
        agents: Vec<crate::agents::AgentDef>,
        extra_tools: Vec<Arc<dyn crate::tools::Tool>>,
    ) -> Result<AgentEngine, String> {
        let (tx, _rx) = mpsc::channel(1);
        AgentEngine::new(
            config,
            session,
            EngineDeps {
                tool_outputs: std::env::temp_dir()
                    .join(format!("vva-agent-test-{}", uuid::Uuid::new_v4())),
                hashline: crate::tools::hashline::HashlineSession::new(),
                ask_user_requests: tx,
                background: Arc::new(crate::tools::background::BackgroundTasks::new(Arc::new(
                    |_| {},
                ))),
                skills: Vec::new(),
                agents,
                broker: Arc::new(crate::harness::testing::StubBroker(None)),
                extra_tools,
            },
        )
    }

    #[tokio::test]
    async fn ask_user_is_absent_for_agent_managed_sessions() {
        let config = web_config(None);
        let plain = build_engine(&config, &session_for("p/nos", false)).unwrap();
        assert!(plain.tools.get("AskUser").is_some());

        let mut managed = session_for("p/nos", false);
        managed.header.origin = Some(crate::model::Origin {
            channel: "qq".into(),
            chat_id: "c2c:1".into(),
        });
        let engine = build_engine(&config, &managed).unwrap();
        assert!(engine.tools.get("AskUser").is_none());

        let mut curator = session_for("p/nos", false);
        curator.header.kind = crate::model::SessionKind::Curator;
        assert!(
            build_engine(&config, &curator)
                .unwrap()
                .tools
                .get("AskUser")
                .is_none()
        );
    }

    #[tokio::test]
    async fn factory_tools_are_registered_after_static_ones() {
        struct Probe;
        #[async_trait::async_trait]
        impl crate::tools::Tool for Probe {
            fn name(&self) -> &str {
                "Probe"
            }
            fn description(&self) -> &str {
                "probe"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _: &str) -> Result<String, String> {
                Ok(String::new())
            }
        }
        let config = web_config(None);
        let session = session_for("p/nos", false);
        let engine = build_engine_full(
            &config,
            &session,
            crate::agents::builtin_agents(),
            vec![Arc::new(Probe)],
        )
        .unwrap();
        assert!(engine.tools.get("Probe").is_some());
        assert!(
            build_engine(&config, &session)
                .unwrap()
                .tools
                .get("Probe")
                .is_none()
        );
    }

    #[tokio::test]
    async fn engine_registers_subagent_tools_for_top_level_sessions() {
        let config = web_config(None);
        let engine = build_engine(&config, &session_for("p/nos", false)).unwrap();

        assert!(engine.tools.get("Agent").is_some());
        assert!(engine.tools.get("SendMessage").is_some());
        assert!(engine.tools.get("WaitSession").is_some());
        // agent 参数 enum 来自发现结果（含内置定义）
        let agent = engine.tools.get("Agent").unwrap();
        let parameters = agent.parameters();
        let names: Vec<_> = parameters["properties"]["agent"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert_eq!(names, ["explore", "general"]);
        let task_stop = engine.tools.get("TaskStop").unwrap();
        assert!(task_stop.description().contains("sess_"));
    }

    #[tokio::test]
    async fn engine_omits_subagent_tools_when_team_disabled() {
        let config = web_config(None);
        let mut session = session_for("p/nos", false);
        session.header.team = false;
        let engine = build_engine(&config, &session).unwrap();

        for name in ["Agent", "SendMessage", "WaitSession"] {
            assert!(engine.tools.get(name).is_none(), "{name} should be absent");
        }
        // TaskStop 保留后台 Shell 能力，但不再宣称可中止 sess_ 会话
        let task_stop = engine.tools.get("TaskStop").unwrap();
        assert!(!task_stop.description().contains("sess_"));
        assert!(!task_stop.parameters().to_string().contains("sess_"));
        assert!(
            task_stop
                .execute(r#"{"task_id":"sess_abc"}"#)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn subagent_session_registry_is_pruned_to_def_allowlist() {
        let config = web_config(None);
        let mut session = session_for("p/nos", false);
        session.header.agent = Some("explore".into());
        session.header.parent = Some("sess_p0".into());
        session.header.depth = 1;
        let engine = build_engine(&config, &session).unwrap();

        assert!(engine.tools.get("Read").is_some());
        assert!(engine.tools.get("Glob").is_some());
        assert!(engine.tools.get("Grep").is_some());
        for pruned in [
            "Shell",
            "Write",
            "Edit",
            "Agent",
            "SendMessage",
            "WaitSession",
        ] {
            assert!(
                engine.tools.get(pruned).is_none(),
                "{pruned} should be pruned"
            );
        }
    }

    #[tokio::test]
    async fn subagent_missing_definition_is_rejected() {
        let config = web_config(None);
        let mut session = session_for("p/nos", true);
        session.header.agent = Some("missing-agent".into());
        let Err(error) = build_engine(&config, &session) else {
            panic!("missing definition was accepted");
        };
        assert!(error.contains("Agent definition 'missing-agent' is unavailable"));
    }

    #[tokio::test]
    async fn subagent_web_permissions_cover_native_and_external_tools() {
        for provider_type in [
            crate::config::ProviderType::Response,
            crate::config::ProviderType::Anthropic,
            crate::config::ProviderType::Gemini,
        ] {
            for native in [true, false] {
                for (search, fetch) in [(false, false), (true, false), (false, true), (true, true)]
                {
                    let mut config = web_config(Some(crate::config::model::ModelAnnotation {
                        web_search: Some(native),
                        ..Default::default()
                    }));
                    config.providers.get_mut("p").unwrap().provider_type = Some(provider_type);
                    let mut session = session_for("p/nos", true);
                    session.header.agent = Some("restricted".into());
                    let mut allowed = vec!["Read".to_string()];
                    if search {
                        allowed.push("WebSearch".into());
                    }
                    if fetch {
                        allowed.push("WebFetch".into());
                    }
                    let defs = vec![crate::agents::AgentDef {
                        name: "restricted".into(),
                        description: "Restricted agent".into(),
                        model: None,
                        tools: allowed,
                        system_prompt: None,
                    }];
                    let engine = build_engine_with_agents(&config, &session, defs).unwrap();
                    if !search {
                        assert!(!engine.options.enable_web_search);
                        assert!(engine.tools.get("web_search").is_none());
                        assert!(!engine.tools.is_server_tool("web_search"));
                    }
                    if !fetch {
                        assert!(!engine.options.enable_web_fetch);
                        assert!(engine.tools.get("web_fetch").is_none());
                        assert!(!engine.tools.is_server_tool("web_fetch"));
                    }
                    if !native {
                        assert_eq!(engine.tools.get("web_search").is_some(), search);
                        assert_eq!(engine.tools.get("web_fetch").is_some(), fetch);
                    }
                    if native && provider_type == crate::config::ProviderType::Anthropic {
                        assert_eq!(engine.options.enable_web_search, search);
                        assert_eq!(engine.options.enable_web_fetch, fetch);
                    }
                }
            }
        }
    }

    fn web_config(annotation: Option<crate::config::model::ModelAnnotation>) -> AppConfig {
        let mut config = AppConfig::default();
        config.settings.web = Some(
            serde_json::from_str(
                r#"{
                "provider": {
                    "brave": { "api_key": "k-brave" },
                    "tavily": { "api_key": "k-tavily" }
                },
                "search": "tavily",
                "fetch": "local"
            }"#,
            )
            .unwrap(),
        );
        let mut models = std::collections::HashMap::new();
        models.insert(
            "nos".into(),
            crate::config::ModelConfig {
                name: "No Search".into(),
                annotation,
                ..Default::default()
            },
        );
        config.providers.insert(
            "p".into(),
            crate::config::ProviderConfig {
                api_key: "k".into(),
                models,
                ..Default::default()
            },
        );
        config
    }

    fn session_for(model: &str, web_search: bool) -> Session {
        let mut session = Session::new(".", None);
        session.set_model(model.to_string(), None);
        session.set_toggles(crate::model::SessionToggles {
            web_search,
            ..Default::default()
        });
        session
    }

    #[tokio::test]
    async fn engine_registers_external_web_tools_when_native_missing() {
        // annotation.web_search: false 关闭原生搜索；fetch 随 search 一并缺位
        let annotation = crate::config::model::ModelAnnotation {
            web_search: Some(false),
            ..Default::default()
        };
        let config = web_config(Some(annotation));
        let engine = build_engine(&config, &session_for("p/nos", true)).unwrap();

        assert!(!engine.options.enable_web_search);
        assert!(engine.tools.get("web_search").is_some());
        assert!(engine.tools.get("web_fetch").is_some());
        // 客户端工具接管时不再预留原生 server tool 名
        assert!(!engine.tools.is_server_tool("web_search"));
        assert_eq!(engine.external_web, ["tavily", "local"]);
        assert!(
            !engine
                .notices
                .iter()
                .any(|notice| notice.contains("the option is ignored"))
        );
    }

    #[tokio::test]
    async fn engine_skips_external_tools_without_native_gap_or_toggle() {
        // 原生 web_search 在位（无注记）时搜索与 fetch 均不外置：
        // Response 的 web_search 含 open_page / find_in_page
        let config = web_config(None);
        let engine = build_engine(&config, &session_for("p/nos", true)).unwrap();
        assert!(engine.options.enable_web_search);
        assert!(engine.tools.get("web_search").is_none());
        assert!(engine.tools.get("web_fetch").is_none());
        assert!(engine.tools.is_server_tool("web_search"));
        assert!(engine.notices.is_empty());
        assert!(engine.external_web.is_empty());

        // 会话 web 开关关闭时一律不注册
        let engine = build_engine(&config, &session_for("p/nos", false)).unwrap();
        assert!(!engine.options.enable_web_search);
        assert!(engine.tools.get("web_search").is_none());
        assert!(engine.tools.get("web_fetch").is_none());
        assert!(!engine.tools.is_server_tool("web_search"));
        assert!(engine.notices.is_empty());
    }

    #[tokio::test]
    async fn engine_fails_on_web_provider_capability_mismatch() {
        let mut config = web_config(Some(crate::config::model::ModelAnnotation {
            web_search: Some(false),
            ..Default::default()
        }));
        config.settings.web.as_mut().unwrap().search = Some("local".into());
        let Err(error) = build_engine(&config, &session_for("p/nos", true)) else {
            panic!("expected engine build to fail on capability mismatch");
        };

        assert!(error.contains("only supports web fetch"), "{error}");
    }

    #[tokio::test]
    async fn engine_notices_affinity_downgrade_on_model_switch() {
        let config = web_config(None);
        let mut session = session_for("p/nos", true);
        // 上一轮由不同亲和组产出且携带原生状态
        let mut turn =
            Turn::assistant_turn(vec![Message::new(vec![crate::model::ContentPart::text(
                "earlier answer",
            )])]);
        turn.meta.response = Some(crate::model::ResponseTurnMeta::new("resp_prev"));
        turn.meta.response.as_mut().unwrap().scope = Some("homo/gpt".into());
        session.append_turn(turn);

        let engine = build_engine(&config, &session).unwrap();
        assert!(
            engine
                .notices
                .iter()
                .any(|notice| notice.contains("Native context from 'homo/gpt'")),
            "{:?}",
            engine.notices
        );

        // 同亲和组续跑不提示
        let mut session = session_for("p/nos", true);
        let mut turn = Turn::assistant_turn(vec![Message::text("earlier")]);
        turn.meta.response = Some(crate::model::ResponseTurnMeta::new("resp_prev"));
        turn.meta.response.as_mut().unwrap().scope = Some("p".into());
        session.append_turn(turn);
        let engine = build_engine(&config, &session).unwrap();
        assert!(
            !engine
                .notices
                .iter()
                .any(|notice| notice.contains("cannot be replayed"))
        );
    }
}
