use std::path::PathBuf;
use std::sync::Arc;
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use vva::config::{load_config, AppConfig, ResolvedModel};
use vva::harness::{AgentConfig, AgentEngine, AgentEvent};
use vva::model::{Annotation, ContentPart, Message, Session, Turn};
use vva::provider::anthropic::AnthropicProvider;
use vva::provider::gemini::GeminiProvider;
use vva::provider::response::ResponseProvider;
use vva::provider::{CompletionOptions, Provider};
use vva::storage::jsonl::JsonlSessionStore;
use vva::storage::SessionStore;
use vva::tools::bash::BashTool;
use vva::tools::file::{FileEditTool, FileViewTool};
use vva::tools::ToolRegistry;
use vva::ui::run_tui;

#[derive(Parser)]
#[command(name = "vva", author, version, about = "Vendor-Versatile Agent Harness for Coding")]
struct Cli {
    /// 显式指定配置文件路径 (.json 或 .jsonc)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 执行 Coding Agent 任务交互 (若未传入 prompt 则启动交互式 TUI)
    Run {
        /// 目标模型规格标识 (例如 a/gpt-5.6-terra, a/claude-sonnet-4-6, gemini-2.5-pro)
        #[arg(short, long)]
        model: Option<String>,

        /// 推理强度档位 (例如 minimal, low, medium, high, xhigh, max)
        #[arg(short, long)]
        variant: Option<String>,

        /// 开启原生网络搜索 / 实时 Grounding
        #[arg(long, default_value_t = false)]
        web_search: bool,

        /// 继存现有 Session ID (若不指定则自动新建)
        #[arg(short, long)]
        session_id: Option<String>,

        /// 工作空间目录路径 (默认为当前目录)
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// 用户输入 Prompt 指令 (省略则启动交互式终端界面)
        prompt: Option<String>,
    },

    /// 启动交互式终端用户界面 (Ratatui TUI)
    Ui {
        /// 目标模型规格标识
        #[arg(short, long)]
        model: Option<String>,

        /// 推理强度档位
        #[arg(short, long)]
        variant: Option<String>,

        /// 开启原生网络搜索 / 实时 Grounding
        #[arg(long, default_value_t = false)]
        web_search: bool,

        /// 继存现有 Session ID
        #[arg(short, long)]
        session_id: Option<String>,

        /// 工作空间目录路径 (默认为当前目录)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// 管理和查看历史 Session
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// 列出所有已持久化的会话记录
    List,
    /// 检查指定会话详情、轮次与供应商元数据
    Inspect {
        session_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let vva_dir = PathBuf::from(&home_dir).join(".vva");
    let sessions_dir = vva_dir.join("sessions");
    let store = Arc::new(JsonlSessionStore::new(&sessions_dir)?);

    // 判断是否启动全屏 TUI 模式，避免 tracing 输出污染终端屏幕
    let is_tui = match &cli.command {
        None => true,
        Some(Commands::Ui { .. }) => true,
        Some(Commands::Run { prompt, .. }) => prompt.is_none(),
        _ => false,
    };

    if is_tui {
        // TUI 模式下日志写入文件
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(vva_dir.join("vva.log"));
        if let Ok(file) = log_file {
            let _ = tracing_subscriber::fmt()
                .with_writer(Arc::new(file))
                .with_env_filter(EnvFilter::from_default_env())
                .try_init();
        }
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .try_init();
    }

    // 尝试读取配置 (支持 JSON 与 JSONC)
    let app_config: Option<AppConfig> = match load_config(cli.config.as_deref()) {
        Ok(cfg) => Some(cfg),
        Err(err) => {
            warn!("Config not loaded ({}). Falling back to direct env vars.", err);
            None
        }
    };

    let default_model = app_config
        .as_ref()
        .map(|c| c.settings.default.clone())
        .unwrap_or_else(|| "response/gpt-4.5-preview".to_string());

    match cli.command {
        None => {
            // 默认无参数启动 TUI
            let workspace = std::env::current_dir().unwrap_or_else(|_| ".".into());
            run_tui(
                store,
                app_config,
                None,
                workspace,
                default_model,
                None,
                false,
            )
            .await?;
        }

        Some(Commands::Ui {
            model,
            variant,
            web_search,
            session_id,
            workspace,
        }) => {
            let workspace = workspace.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            let target_model = model.unwrap_or(default_model);
            run_tui(
                store,
                app_config,
                session_id,
                workspace,
                target_model,
                variant,
                web_search,
            )
            .await?;
        }

        Some(Commands::Run {
            model,
            variant,
            web_search,
            session_id,
            workspace,
            prompt,
        }) => {
            let workspace = workspace.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            let target_model = model.unwrap_or(default_model);

            // 如果没有提供 prompt，直接进入 TUI 交互
            let prompt = match prompt {
                Some(p) => p,
                None => {
                    return run_tui(
                        store,
                        app_config,
                        session_id,
                        workspace,
                        target_model,
                        variant,
                        web_search,
                    )
                    .await;
                }
            };

            // CLI 单次非交互运行流程
            let mut session = if let Some(sid) = session_id {
                let mut s = store
                    .load_session(&sid)
                    .await?
                    .ok_or_else(|| format!("Session not found: {sid}"))?;

                let new_model = target_model;
                let new_variant = variant.or_else(|| s.current_variant.clone());
                s.set_model(new_model, new_variant);
                s
            } else {
                let s = Session::with_model(
                    &workspace,
                    Some(prompt.chars().take(40).collect()),
                    target_model,
                    variant,
                );
                store.save_header(&s).await?;
                s
            };

            println!("🚀 [vva] Session ID: {}", session.id);
            println!("📂 Workspace: {}", session.workspace_root.display());
            println!("🎯 Session Model: {}", session.current_model);
            if let Some(v) = &session.current_variant {
                println!("⚡ Reasoning Variant: {}", v);
            }

            // 解析 Provider 实例
            let (provider_inst, effective_model_name, reasoning_effort) = if let Some(cfg) = &app_config {
                match cfg.resolve_model(&session.current_model, session.current_variant.as_deref()) {
                    Ok(resolved) => {
                        let ResolvedModel {
                            model_id,
                            provider,
                            selected_variant,
                            ..
                        } = resolved;
                        (provider, model_id, selected_variant)
                    }
                    Err(err) => {
                        eprintln!("Error resolving model '{}': {}", session.current_model, err);
                        std::process::exit(1);
                    }
                }
            } else {
                let (provider_inst, model_id): (Arc<dyn Provider>, String) = match session.current_model.to_lowercase().as_str() {
                    m if m.contains("anthropic") || m.contains("claude") => {
                        let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                        (Arc::new(AnthropicProvider::new(key)), session.current_model.clone())
                    }
                    m if m.contains("gemini") => {
                        let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
                        (Arc::new(GeminiProvider::new(key)), session.current_model.clone())
                    }
                    _ => {
                        let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                        (Arc::new(ResponseProvider::new(key)), session.current_model.clone())
                    }
                };
                let effort = session.current_variant.clone();
                (provider_inst, model_id, effort)
            };

            println!("🤖 Active Provider: {} (API Model: {})", provider_inst.name(), effective_model_name);
            if web_search {
                println!("🌐 Native Web Search: Enabled");
            }

            // 初始化 Coding 工具
            let mut tools = ToolRegistry::new();
            tools.register(Arc::new(BashTool::new(&workspace)));
            tools.register(Arc::new(FileViewTool::new(&workspace)));
            tools.register(Arc::new(FileEditTool::new(&workspace)));

            let user_msg = Message::user(&prompt);
            let parent_turn_id = session.last_turn_id();
            let user_turn = Turn::user_turn(user_msg, parent_turn_id);
            store.append_turn(&session.id, &user_turn).await?;
            session.append_turn(user_turn);

            let agent_config = AgentConfig {
                max_iterations: 15,
                completion_options: CompletionOptions {
                    model: effective_model_name,
                    enable_web_search: web_search,
                    reasoning_effort,
                    ..Default::default()
                },
            };

            let engine = AgentEngine::new(provider_inst, store.clone(), tools, agent_config);
            let (tx, mut rx) = mpsc::channel::<AgentEvent>(32);

            let run_handle = tokio::spawn(async move {
                engine.run_turn(&mut session, Some(tx)).await
            });

            while let Some(event) = rx.recv().await {
                match event {
                    AgentEvent::Thought(thought) => {
                        println!("\n🧠 [Thinking]\n{}", thought.trim());
                    }
                    AgentEvent::Text(text) => {
                        println!("\n💬 [Assistant]\n{}", text);
                    }
                    AgentEvent::ToolExecuting { name, args } => {
                        println!("\n⚙️ [Tool Calling] {}({})", name, args);
                    }
                    AgentEvent::ToolFinished { name, result, is_error } => {
                        let status_sym = if is_error { "❌" } else { "✅" };
                        println!("{} [{}] output:\n{}", status_sym, name, result.trim());
                    }
                    AgentEvent::Finished => {
                        println!("\n✨ Turn completed successfully.");
                    }
                    AgentEvent::Error(err) => {
                        eprintln!("\n❌ [Error] {}", err);
                    }
                }
            }

            run_handle.await??;
        }

        Some(Commands::Session { action }) => match action {
            SessionAction::List => {
                let sessions = store.list_sessions().await?;
                println!(
                    "{:<36} | {:<20} | {:<8} | {:<5} | {:<16} | Title / Workspace",
                    "Session ID", "Model", "Variant", "Turns", "Created At"
                );
                println!(
                    "{:-<36}-+-{:-<20}-+-{:-<8}-+-{:-<5}-+-{:-<16}-+-{:-<30}",
                    "", "", "", "", "", ""
                );
                for s in sessions {
                    let title = s.title.as_deref().unwrap_or("Untitled");
                    let variant_str = s.current_variant.as_deref().unwrap_or("-");
                    println!(
                        "{:<36} | {:<20} | {:<8} | {:<5} | {:<16} | {} ({})",
                        s.id,
                        s.current_model,
                        variant_str,
                        s.turn_count,
                        s.created_at.format("%Y-%m-%d %H:%M"),
                        title,
                        s.workspace_root.display()
                    );
                }
            }
            SessionAction::Inspect { session_id } => {
                let session = store.load_session(&session_id).await?
                    .ok_or_else(|| format!("Session not found: {session_id}"))?;

                println!("=== Session Header ===");
                println!("ID: {}", session.id);
                println!("Title: {:?}", session.title);
                println!("Workspace: {}", session.workspace_root.display());
                println!("Selected Model: {}", session.current_model);
                println!("Reasoning Variant: {:?}", session.current_variant);
                println!("Created At: {}", session.created_at);
                println!("Global Meta: {:#?}", session.meta);

                println!("\n=== Turns ({}) ===", session.turns.len());
                for (i, turn) in session.turns.iter().enumerate() {
                    println!("\n--- Turn #{} [{:?}] ID: {} ---", i + 1, turn.role, turn.id);
                    if let Some(parent) = &turn.parent_turn_id {
                        println!("Parent Turn: {}", parent);
                    }
                    if let Some(usage) = &turn.usage {
                        println!(
                            "Tokens: Prompt: {}, Completion: {}, Total: {}",
                            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                        );
                    }
                    if let Some(resp) = &turn.meta.response {
                        println!(
                            "🏷️ [Response Meta] ID: {}, Prev: {:?}, Status: {:?}",
                            resp.response_id, resp.previous_response_id, resp.status
                        );
                    }
                    if let Some(anthropic) = &turn.meta.anthropic {
                        println!(
                            "🏷️ [Anthropic Meta] Cache Write: {:?}, Cache Read: {:?}",
                            anthropic.cache_creation_input_tokens, anthropic.cache_read_input_tokens
                        );
                    }
                    if let Some(gemini) = &turn.meta.gemini {
                        println!(
                            "🏷️ [Gemini Meta] Finish: {:?}, Safety: {}",
                            gemini.finish_reason, gemini.safety_ratings.len()
                        );
                    }

                    for msg in &turn.messages {
                        println!("  Message [{:?}] ID: {}", msg.role, msg.id);
                        for part in &msg.parts {
                            match part {
                                ContentPart::Text { text, annotations } => {
                                    println!("    [Text]: {}", text);
                                    for ann in annotations {
                                        if let Annotation::UrlCitation { url, title, start_index, end_index } = ann {
                                            println!("      🔗 Citation: {} ({}) [{}-{}]", title, url, start_index, end_index);
                                        }
                                    }
                                }
                                ContentPart::Reasoning { content, signature } => {
                                    println!("    [Reasoning (sig: {:?})]: {}", signature.as_deref().unwrap_or("none"), content);
                                }
                                ContentPart::WebSearchCall(call) => {
                                    println!("    🌐 [WebSearchCall]: Query='{}' Sources={}", call.query, call.sources.len());
                                    for src in &call.sources {
                                        println!("       - {} ({})", src.title, src.url);
                                    }
                                }
                                ContentPart::ToolCall(call) => {
                                    println!("    ⚙️ [ToolCall]: {}({})", call.name, call.arguments);
                                }
                                ContentPart::ToolResult(res) => {
                                    println!("    📦 [ToolResult] Error={}: {}", res.is_error, res.content);
                                }
                                ContentPart::Image { source, .. } => {
                                    println!("    🖼️ [Image]: {:?}", source);
                                }
                                ContentPart::Document { filename, .. } => {
                                    println!("    📄 [Document]: {:?}", filename);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
