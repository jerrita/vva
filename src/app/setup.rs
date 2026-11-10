const SKILL_TOOL_DESCRIPTION: &str = "Load a skill's full instructions by name. Call it before starting a task that matches a skill listed in the conversation's Skills reminder.

Managing skills (installing, creating, removing): a skill is a directory `<name>/SKILL.md` whose frontmatter has `name` (same as the directory, lowercase letters/digits/hyphens) and `description`. Skills are discovered from these directories, lowest to highest priority; a later directory overrides a same-named skill from an earlier one:
1. `~/.agents/skills` (global)
2. `~/.vva/skills` (global)
3. `<workspace>/.agents/skills` (project)
4. `<workspace>/.vva/skills` (project)
Install and create skills in the project directory `<workspace>/.vva/skills` (the workspace is the session's working directory; for the resident agent it is `~/.vva/space`); use the global directories when the user explicitly asks for a global install. Run `npx skills add` from the workspace root without `-g`/`--global`: the default project install lands in `<workspace>/.agents/skills`, which is discovered. After installing, confirm the skill directory sits under one of the paths above; new sessions pick up new skills.";

use std::path::PathBuf;
use std::sync::Arc;
use vva_core::harness::ExecutionPlan;
use vva_core::tools::names;

use crate::config::{AppConfig, ResolvedModel};
use tokio::sync::mpsc;
use vva_core::model::{Message, Role, Session, Turn};
use vva_core::provider::CompletionOptions;
use vva_core::provider::Provider;
use vva_core::tools::ToolRegistry;
use vva_core::tools::WorkspaceToolDeps;

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
pub type SessionToolFactory = Arc<
    dyn Fn(&crate::app::model::SessionHeader) -> Vec<Arc<dyn vva_core::tools::Tool>> + Send + Sync,
>;

/// 会话前导：按会话 header 提供首次落盘时追加的 `<system-reminder>` 文本（如记忆索引快照），
/// 空列表表示无需注入
pub type SessionPreamble =
    Arc<dyn Fn(&crate::app::model::SessionHeader) -> Vec<String> + Send + Sync>;

/// Provider 的连接状态随会话保留，工具注册表仍在每次装配时刷新。
#[derive(Default)]
pub(crate) struct SessionProvider {
    binding: std::sync::Mutex<Option<ProviderBinding>>,
}

struct ProviderBinding {
    identity: [u8; 32],
    provider: Arc<dyn Provider>,
}

impl SessionProvider {
    fn resolve(&self, model: &ResolvedModel<'_>) -> Arc<dyn Provider> {
        let identity = model.provider_identity();
        let mut binding = self.binding.lock().expect("provider binding poisoned");
        if let Some(current) = binding.as_ref()
            && current.identity == identity
        {
            return current.provider.clone();
        }
        let provider = model.create_provider();
        *binding = Some(ProviderBinding {
            identity,
            provider: provider.clone(),
        });
        provider
    }
}

/// 引擎装配依赖：会话级共享句柄与每次构建时重新发现的清单
pub(crate) struct EngineDeps {
    pub provider: Arc<SessionProvider>,
    pub tool_outputs: PathBuf,
    pub hashline: Arc<vva_core::tools::hashline::HashlineSession>,
    pub ask_user_requests: mpsc::Sender<vva_core::tools::ask_user::AskUserRequest>,
    pub background: Arc<vva_core::tools::background::BackgroundTasks>,
    pub shell_outputs: Arc<vva_core::tools::shell_output::ShellOutputs>,
    pub skills: Vec<vva_core::skills::Skill>,
    pub agents: Vec<vva_core::agents::AgentDef>,
    pub subagents: Vec<vva_core::agents::AgentDef>,
    pub broker: Arc<dyn vva_core::tools::agent::SessionBroker>,
    /// 工厂产出的会话专属工具，最后注册
    pub extra_tools: Vec<Arc<dyn vva_core::tools::Tool>>,
    pub interactive: bool,
    pub coding: crate::app::records::CodingSessionConfig,
}

impl VvaSessionSetup {
    fn assemble(
        config: &AppConfig,
        session: &Session,
        deps: EngineDeps,
    ) -> Result<ExecutionPlan, String> {
        let resolved = config
            .resolve_model(
                &session.header.current_model,
                session.header.current_variant.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        let inputs = resolved.input_capabilities();
        let coding = deps.coding.clone();
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
        let provider = deps.provider.resolve(&resolved);
        let mut tools = Self::base_tools(session, &inputs, deps)?;
        // subagent 会话按定义白名单裁剪
        let allowed = |name: &str| {
            agent_def
                .as_ref()
                .is_none_or(|def| vva_core::tools::tool_allowed(&def.tools, name))
        };
        let search_allowed = allowed("WebSearch");
        let fetch_allowed = allowed("WebFetch");
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
        let web_declared = coding.web_search && resolved.model_config.declare_web_search();
        let standalone_search = web_declared
            && search_allowed
            && resolved.effective_type() == vva_core::provider::ProviderType::Anthropic
            && resolved.model_config.standalone_search();
        let search_provider = standalone_search.then(|| resolved.create_anthropic_provider());
        let options = CompletionOptions {
            model: resolved.model_id,
            max_tokens,
            reasoning_effort: resolved.selected_variant,
            enable_web_search: web_declared
                && search_allowed
                && (!bundled_fetch || fetch_allowed)
                && !standalone_search,
            enable_web_fetch: web_declared
                && resolved.model_config.declare_web_fetch()
                && fetch_allowed,
            web_tool_version: resolved.model_config.web_tool_version().map(str::to_string),
            service_tier: resolved.model_config.service_tier().map(str::to_string),
            affinity: Some(affinity.clone()),
            ..Default::default()
        };
        if let Some(provider) = search_provider {
            tools.register(Arc::new(
                vva_core::tools::web::standalone::StandaloneSearchTool::new(
                    provider,
                    options.clone(),
                ),
            ));
        }
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
        if coding.web_search && (search_allowed || fetch_allowed) {
            let external = super::web::resolve(config)?;
            let native_search = standalone_search
                || (options.enable_web_search && capabilities.supports_native_search);
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
                tools.register(Arc::new(vva_core::tools::web::tool::WebSearchTool::new(
                    search.clone(),
                )));
                external_web.push(search.name().to_string());
                external_search = true;
            }
            if fetch_allowed
                && !native_fetch
                && let Some(fetch) = &external.fetch
            {
                tools.register(Arc::new(vva_core::tools::web::tool::WebFetchTool::new(
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
        Ok(ExecutionPlan {
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
        inputs: &vva_core::provider::InputCapabilities,
        deps: EngineDeps,
    ) -> Result<ToolRegistry, String> {
        let mut tools = ToolRegistry::with_workspace_tools(
            &session.header.workspace_root,
            Some(deps.tool_outputs),
            deps.coding.editor,
            WorkspaceToolDeps {
                hashline: deps.hashline,
                inputs: inputs.clone(),
                background: Some(deps.background.clone()),
                shell_outputs: Some(deps.shell_outputs),
                env: crate::config::env::workspace_env(&session.header.workspace_root)?
                    .into_iter()
                    .collect(),
            },
        );
        let broker = deps.broker;
        let task_stop = vva_core::tools::background::TaskStopTool::new(deps.background.clone())
            .with_broker(broker.clone());
        let mut extra: Vec<Arc<dyn vva_core::tools::Tool>> = vec![
            Arc::new(vva_core::tools::background::TaskOutputTool::new(
                deps.background,
            )),
            Arc::new(task_stop),
            Arc::new(vva_core::tools::skill::SkillTool::new(
                Arc::new(deps.skills),
                SKILL_TOOL_DESCRIPTION.into(),
            )),
        ];
        // 交互能力由宿主装配明确指定。
        if deps.interactive {
            extra.push(Arc::new(vva_core::tools::ask_user::AskUserTool::new(
                deps.ask_user_requests,
            )));
        }
        extra.extend(deps.extra_tools);
        for tool in extra {
            tools.register(tool);
        }
        let subagent_tools: [Arc<dyn vva_core::tools::Tool>; 2] = [
            Arc::new(vva_core::tools::agent::AgentTool::new(
                broker.clone(),
                Arc::new(deps.subagents),
                vva_core::tools::agent::SubagentContext::from_session(session),
            )),
            Arc::new(vva_core::tools::session_ipc::SendMessageTool::new(broker)),
        ];
        for tool in subagent_tools {
            tools.register(tool);
        }
        Ok(tools)
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

pub(crate) async fn generate_title(
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
        disable_thinking: resolved.model_config.reasoning,
        web_tool_version: None,
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

/// Read / hashline_read 命中工作空间内子目录 AGENTS.md 时返回待注入的 (路径, 正文)；
/// 会话已载入同路径或本轮已排队的文件跳过。
fn collect_on_demand_agents(
    session: &Session,
    call: &vva_core::model::ToolCall,
    result: &vva_core::model::ToolResult,
    pending: &[(PathBuf, String)],
) -> Option<(PathBuf, String)> {
    if !matches!(call.name.as_str(), names::READ | names::HASHLINE_READ) || result.is_error {
        return None;
    }
    let args = serde_json::from_str::<serde_json::Value>(&call.arguments).ok()?;
    let path_str = args.get("path").and_then(|p| p.as_str())?;
    let file_path =
        vva_core::tools::filesystem::resolve_path(&session.header.workspace_root, path_str);
    let agents_path =
        crate::system::find_directory_agents_md(&session.header.workspace_root, &file_path)?;
    let ws_canonical = session
        .header
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| session.header.workspace_root.clone());
    let agents_canonical = agents_path
        .canonicalize()
        .unwrap_or_else(|_| agents_path.clone());
    let rel_path = agents_canonical
        .strip_prefix(&ws_canonical)
        .unwrap_or(&agents_path)
        .to_string_lossy()
        .replace('\\', "/");

    let already_in_session = crate::system::session_contains_agents_md(session, &rel_path);
    let already_pending = pending.iter().any(|(p, _)| p == &agents_path);
    if already_in_session || already_pending {
        return None;
    }
    let content = std::fs::read_to_string(&agents_path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    Some((agents_path, content))
}

impl VvaSessionSetup {
    async fn initial_turns(
        &self,
        source: &Session,
        skills: &[vva_core::skills::Skill],
    ) -> Result<Vec<Turn>, String> {
        let mut session = source.clone();
        let workspace = session.header.workspace_root.clone();
        let scratch = self
            .store
            .slot(&session.header.id)
            .map_err(|e| e.to_string())?
            .borrow()
            .resources
            .scratch
            .clone();
        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        let (system, global_agents, project_agents) = tokio::task::spawn_blocking(move || {
            let system = crate::system::build_system_prompt(&workspace, &scratch);
            let global = crate::system::load_global_agents_md();
            let project = crate::system::load_project_agents_md(&workspace);
            (system, global, project)
        })
        .await
        .map_err(|e| e.to_string())?;

        // 子代理会话：agent 定义的角色指令追加到基础 System Prompt
        let agent_def = session
            .header
            .agent
            .as_deref()
            .map(|name| {
                self.agents(&session.header.workspace_root)
                    .into_iter()
                    .find(|def| def.name == name)
                    .ok_or_else(|| format!("Agent definition '{name}' is unavailable"))
            })
            .transpose()?;
        let system = match agent_def
            .as_ref()
            .and_then(|def| def.system_prompt.as_deref())
        {
            Some(extra) => format!("{system}\n\n{extra}"),
            None => system,
        };
        session.append_turn(Turn::new(Role::System, vec![Message::text(system)]));

        let mut loaded_canonical = Vec::new();
        if let Some((path, content)) = global_agents {
            if let Ok(c) = path.canonicalize() {
                loaded_canonical.push(c);
            }
            let reminder = crate::system::format_global_agents_reminder(&path, &content);
            session.append_turn(Turn::new(Role::User, vec![Message::text(reminder)]));
        }
        if let Some((path, content)) = project_agents {
            let is_dup = path
                .canonicalize()
                .ok()
                .is_some_and(|c| loaded_canonical.contains(&c));
            if !is_dup {
                let reminder = crate::system::format_project_agents_reminder(&path, &content);
                session.append_turn(Turn::new(Role::User, vec![Message::text(reminder)]));
            }
        }
        // 子代理工具白名单未含 Skill 时不注入技能清单（指令与能力保持一致）
        let skill_allowed = agent_def
            .as_ref()
            .is_none_or(|def| def.tools.iter().any(|tool| tool == "Skill"));
        if !skills.is_empty() && skill_allowed {
            let reminder = crate::system::format_skills_reminder(skills);
            session.append_turn(Turn::new(Role::User, vec![Message::text(reminder)]));
        }
        let header = crate::app::model::SessionHeader::from_core(
            session.header.clone(),
            &self
                .store
                .slot(&session.header.id)
                .map_err(|e| e.to_string())?
                .borrow(),
        );
        // 会话前导（如记忆索引快照）：动态内容走 reminder 追加，不进 system prompt
        for text in self
            .preamble
            .as_ref()
            .map(|preamble| preamble(&header))
            .unwrap_or_default()
        {
            session.append_turn(Turn::new(Role::User, vec![Message::text(text)]));
        }
        Ok(session.turns[source.turns.len()..].to_vec())
    }
}

pub struct VvaSessionSetup {
    config: Arc<AppConfig>,
    store: Arc<crate::app::store::AppSessionStore>,
    state_path: PathBuf,
    provider: Arc<SessionProvider>,
    broker: Arc<dyn vva_core::tools::agent::SessionBroker>,
    tool_factory: Option<SessionToolFactory>,
    preamble: Option<SessionPreamble>,
    interactive: bool,
    internal_agents: Vec<vva_core::agents::AgentDef>,
}

impl VvaSessionSetup {
    pub fn new(
        config: Arc<AppConfig>,
        store: Arc<crate::app::store::AppSessionStore>,
        state_path: PathBuf,
        broker: Arc<dyn vva_core::tools::agent::SessionBroker>,
        tool_factory: Option<SessionToolFactory>,
        preamble: Option<SessionPreamble>,
    ) -> Self {
        Self {
            config,
            store,
            state_path,
            provider: Arc::default(),
            broker,
            tool_factory,
            preamble,
            interactive: true,
            internal_agents: Vec::new(),
        }
    }

    pub fn with_interaction(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    pub fn with_internal_agents(mut self, agents: Vec<vva_core::agents::AgentDef>) -> Self {
        self.internal_agents = agents;
        self
    }

    fn agents(&self, workspace: &std::path::Path) -> Vec<vva_core::agents::AgentDef> {
        let mut agents = crate::agents::discover_agents(workspace);
        for internal in &self.internal_agents {
            if !agents.iter().any(|def| def.name == internal.name) {
                agents.push(internal.clone());
            }
        }
        agents
    }
}

#[async_trait::async_trait]
impl vva_core::harness::SessionSetup for VvaSessionSetup {
    async fn plan(
        &self,
        session: &Session,
        services: &vva_core::harness::SessionServices,
    ) -> Result<ExecutionPlan, String> {
        let record = self
            .store
            .slot(&session.header.id)
            .map_err(|e| e.to_string())?
            .borrow()
            .clone();
        std::fs::create_dir_all(&record.resources.tool_outputs).map_err(|e| e.to_string())?;
        let header = crate::app::model::SessionHeader::from_core(session.header.clone(), &record);
        let mut extra_tools = self
            .tool_factory
            .as_ref()
            .map(|factory| factory(&header))
            .unwrap_or_default();
        if header.supports_curation() {
            extra_tools.push(Arc::new(
                crate::space::curation_tool::SetSessionCuration::new(
                    self.store.clone(),
                    session.header.id.clone(),
                ),
            ));
        }
        let agents = self.agents(&session.header.workspace_root);
        let subagents = agents
            .iter()
            .filter(|def| {
                !self
                    .internal_agents
                    .iter()
                    .any(|internal| internal.name == def.name)
            })
            .cloned()
            .collect();
        Self::assemble(
            &self.config,
            session,
            EngineDeps {
                provider: self.provider.clone(),
                tool_outputs: record.resources.tool_outputs,
                hashline: services.hashline.clone(),
                ask_user_requests: services.ask_user_requests.clone(),
                background: services.background.clone(),
                shell_outputs: services.shell_outputs.clone(),
                skills: crate::skills::discover_skills(&session.header.workspace_root),
                agents,
                subagents,
                broker: self.broker.clone(),
                extra_tools,
                interactive: self.interactive,
                coding: record.coding,
            },
        )
    }

    async fn prepare_input(
        &self,
        session: &Session,
        mut input: Message,
        new_session: bool,
    ) -> Result<vva_core::harness::PreparedInput, String> {
        let skills = crate::skills::discover_skills(&session.header.workspace_root);
        let prompt = input.text_content();
        let inputs = self
            .config
            .resolve_model(
                &session.header.current_model,
                session.header.current_variant.as_deref(),
            )
            .map_err(|e| e.to_string())?
            .input_capabilities();
        let mut notices = Vec::new();
        for token in super::mention::extract_mention_paths(&prompt) {
            match super::mention::load_mention(&session.header.workspace_root, &token).await {
                Ok(part) => {
                    input.parts.push(if inputs.validate_part(&part).is_err() {
                        super::mention::capability_note(&token)
                    } else {
                        part
                    });
                }
                Err(reason) => notices.push(format!("@{token}: {reason}")),
            }
        }
        let initial = if new_session && session.turns.is_empty() {
            self.initial_turns(session, &skills).await?
        } else {
            Vec::new()
        };
        let mut prepared = vva_core::harness::PreparedInput::unchanged(input);
        prepared.notices = notices;
        if session.header.agent.is_none() {
            let changed = crate::system::skills_change_reminder(
                session
                    .turns
                    .iter()
                    .chain(initial.iter())
                    .filter(|turn| turn.role == Role::User)
                    .flat_map(|turn| &turn.messages)
                    .flat_map(|message| &message.parts)
                    .filter_map(|part| match part {
                        vva_core::model::ContentPart::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    }),
                &skills,
            );
            if let Some(reminder) = changed {
                prepared.before.push(Message::text(reminder));
            }
        }
        prepared.initial = initial;
        if let Some(skill) = crate::skills::match_invocation(&prompt, &skills) {
            prepared
                .after
                .push(Message::text(crate::system::format_skill_load_reminder(
                    skill,
                )));
        }
        Ok(prepared)
    }

    async fn after_tools(
        &self,
        session: &Session,
        batch: &[vva_core::harness::ToolExecution],
    ) -> Result<Vec<Message>, String> {
        let mut pending = Vec::new();
        for execution in batch {
            if let Some(agents) =
                collect_on_demand_agents(session, &execution.call, &execution.result, &pending)
            {
                pending.push(agents);
            }
        }
        Ok(pending
            .into_iter()
            .map(|(path, content)| {
                Message::text(crate::system::format_directory_agents_reminder(
                    &session.header.workspace_root,
                    &path,
                    &content,
                ))
            })
            .collect())
    }

    fn configure(
        &self,
        session: &mut Session,
        model: Option<String>,
        variant: Option<Option<String>>,
    ) -> Result<(), String> {
        let target = model.as_deref().unwrap_or(&session.header.current_model);
        let selected = variant.unwrap_or_else(|| session.header.current_variant.clone());
        let resolved = self
            .config
            .resolve_model(target, selected.as_deref())
            .map_err(|e| e.to_string())?;
        resolved.input_capabilities().validate_session(session)?;
        session.set_model(resolved.full_spec, selected);
        Ok(())
    }

    fn selection_saved(&self, session: &Session) -> Result<(), String> {
        if matches!(
            self.store
                .slot(&session.header.id)
                .map_err(|e| e.to_string())?
                .borrow()
                .application,
            crate::app::records::Application::Coding
        ) {
            crate::config::AppState::persist_selection(
                &self.state_path,
                &session.header.current_model,
                session.header.current_variant.as_deref(),
            );
        }
        Ok(())
    }

    async fn title(&self, session: &Session, prompt: &str) -> Result<Option<String>, String> {
        generate_title(&self.config, &session.header.current_model, prompt)
            .await
            .map(Some)
    }

    async fn abort_children(&self, session_id: &str) {
        self.broker.abort_children(session_id).await;
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
