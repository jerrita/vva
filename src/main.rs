use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use vva::config::{AppConfig, FALLBACK_MODEL_SPEC, home_dir, load_config};
use vva::harness::{AgentEvent, build_engine};
use vva::model::{Annotation, ContentPart, Message, Turn};
use vva::storage::SessionStore;
use vva::storage::jsonl::JsonlSessionStore;
use vva::ui::run_tui;

#[derive(Parser)]
#[command(
    name = "vva",
    author,
    version,
    about = "Vendor-Versatile Agent Harness for Coding"
)]
struct Cli {
    /// 配置文件路径 (.json 或 .jsonc)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 执行 Coding Agent 任务交互 (未传入 prompt 则启动终端界面)
    Run {
        /// 目标模型标识 (例如 a/gpt-5.6-terra, a/claude-sonnet-4-6, gemini-2.5-pro)
        #[arg(short, long)]
        model: Option<String>,

        /// 推理强度档位 (例如 minimal, low, medium, high, xhigh, max)
        #[arg(short, long)]
        variant: Option<String>,

        /// 开启网络搜索
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        web_search: Option<bool>,

        /// 恢复指定 Session ID (省略则自动新建)
        #[arg(short, long)]
        session_id: Option<String>,

        /// 工作空间目录路径 (默认为当前目录)
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// 用户 Prompt 指令 (省略则启动终端界面)
        prompt: Option<String>,
    },

    /// 启动终端用户界面 (Ratatui TUI)
    Ui {
        /// 目标模型标识
        #[arg(short, long)]
        model: Option<String>,

        /// 推理强度档位
        #[arg(short, long)]
        variant: Option<String>,

        /// 开启网络搜索
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        web_search: Option<bool>,

        /// 恢复指定 Session ID
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
    /// 列出所有会话记录
    List,
    /// 查看指定会话详情、轮次与元数据
    Inspect { session_id: String },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let home_dir = home_dir().unwrap_or_else(|| PathBuf::from("."));
    let vva_dir = home_dir.join(".vva");
    let sessions_dir = vva_dir.join("sessions");
    let store = Arc::new(JsonlSessionStore::new(&sessions_dir)?);

    // 判断是否启动全屏 TUI 模式，避免 tracing 输出干扰终端屏幕
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

    // 读取配置 (支持 JSON 与 JSONC)
    let app_config: Option<AppConfig> = match load_config(cli.config.as_deref()) {
        Ok(cfg) => Some(cfg),
        Err(err) => {
            warn!(
                "Config not loaded ({}). Falling back to direct env vars.",
                err
            );
            None
        }
    };

    let default_model = app_config
        .as_ref()
        .map(|c| c.settings.default.clone())
        .unwrap_or_else(|| FALLBACK_MODEL_SPEC.to_string());

    match cli.command {
        None => {
            // 启动 TUI
            let workspace = std::env::current_dir().unwrap_or_else(|_| ".".into());
            run_tui(store, app_config, None, workspace, None, None, None).await?;
        }

        Some(Commands::Ui {
            model,
            variant,
            web_search,
            session_id,
            workspace,
        }) => {
            let workspace =
                workspace.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            run_tui(
                store, app_config, session_id, workspace, model, variant, web_search,
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
            let workspace =
                workspace.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

            // 未提供 prompt 时启动 TUI 交互
            let prompt = match prompt {
                Some(p) => p,
                None => {
                    return run_tui(
                        store, app_config, session_id, workspace, model, variant, web_search,
                    )
                    .await;
                }
            };

            // 显式指定的模型严格校验：不可解析时直接报错退出，不静默回退默认模型
            if let (Some(cfg), Some(m)) = (&app_config, &model)
                && let Err(err) = cfg.resolve_model(m, variant.as_deref())
            {
                eprintln!("Error resolving model '{m}': {err}");
                std::process::exit(1);
            }

            // CLI 单次非交互运行流程
            let mut session = if let Some(sid) = session_id {
                let mut s = store
                    .load_session(&sid)
                    .await?
                    .ok_or_else(|| format!("Session not found: {sid}"))?;
                if s.apply_overrides(model, variant, web_search) {
                    store.save_header(&s).await?;
                }
                s
            } else {
                let target_model = model.unwrap_or(default_model);
                let target_search = web_search.unwrap_or(true);
                vva::system::new_session(
                    &workspace,
                    Some(&vva::model::provisional_title(&prompt)),
                    target_model,
                    variant,
                    target_search,
                )
            };

            println!("🚀 [vva] Session ID: {}", session.id);
            println!("📂 Workspace: {}", session.workspace_root.display());
            println!("🎯 Session Model: {}", session.current_model);
            if let Some(v) = &session.current_variant {
                println!("⚡ Reasoning Variant: {}", v);
            }

            // 引擎组装共享单点（配置路由 + 能力协商降级通知）
            let (engine, notices) = match build_engine(
                &app_config,
                &session.current_model,
                session.current_variant.as_deref(),
                session.web_search,
                &workspace,
                store.clone(),
            ) {
                Ok(v) => v,
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            };

            println!(
                "🤖 Active Provider: {} (API Model: {})",
                engine.provider_name(),
                engine.model()
            );
            if session.web_search {
                println!("🌐 Native Web Search: Enabled");
            }
            for notice in notices {
                eprintln!("⚠️  [Notice] {notice}");
            }

            let user_msg = Message::user(&prompt);
            let parent_turn_id = session.last_turn_id();
            let user_turn = Turn::user_turn(user_msg, parent_turn_id);
            store.append_turn(&session.id, &user_turn).await?;
            session.append_turn(user_turn);

            let (tx, mut rx) = mpsc::channel::<AgentEvent>(32);

            let run_handle =
                tokio::spawn(async move { engine.run_turn(&mut session, Some(tx)).await });

            let mut in_thought = false;
            let mut in_text = false;

            while let Some(event) = rx.recv().await {
                match event {
                    AgentEvent::Thought(thought) => {
                        if !in_thought {
                            println!("\n🧠 [Thinking]");
                            in_thought = true;
                        }
                        print!("{}", thought);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                    }
                    AgentEvent::Text(text) => {
                        in_thought = false;
                        if !in_text {
                            println!("\n💬 [Assistant]");
                            in_text = true;
                        }
                        print!("{}", text);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                    }
                    AgentEvent::Citation { title, url } => {
                        println!("\n🔗 [Citation] {} ({})", title, url);
                    }
                    AgentEvent::Notice(notice) => {
                        eprintln!("\n⚠️  [Notice] {notice}");
                    }
                    AgentEvent::ToolExecuting { name, args } => {
                        in_thought = false;
                        in_text = false;
                        println!("\n⚙️ [Tool Calling] {}({})", name, args);
                    }
                    AgentEvent::ToolFinished {
                        name,
                        result,
                        is_error,
                    } => {
                        let status_sym = if is_error { "❌" } else { "✅" };
                        println!("{} [{}] output:\n{}", status_sym, name, result.trim());
                    }
                    AgentEvent::Finished => {
                        in_thought = false;
                        in_text = false;
                        println!("\n✨ Turn completed successfully.");
                    }
                    AgentEvent::Aborted => {
                        in_thought = false;
                        in_text = false;
                        println!("\n🛑 Turn aborted.");
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
                let session = store
                    .load_session(&session_id)
                    .await?
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
                    println!(
                        "\n--- Turn #{} [{:?}] ID: {} ---",
                        i + 1,
                        turn.role,
                        turn.id
                    );
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
                            "🏷️ [Response Meta] ID: {:?}, Prev: {:?}, Status: {:?}, Store: {}",
                            resp.response_id, resp.previous_response_id, resp.status, resp.store
                        );
                    }
                    if let Some(anthropic) = &turn.meta.anthropic {
                        println!(
                            "🏷️ [Anthropic Meta] Cache Write: {:?}, Cache Read: {:?}",
                            anthropic.cache_creation_input_tokens,
                            anthropic.cache_read_input_tokens
                        );
                    }
                    if let Some(gemini) = &turn.meta.gemini {
                        println!(
                            "🏷️ [Gemini Meta] Finish: {:?}, Safety: {}",
                            gemini.finish_reason,
                            gemini.safety_ratings.len()
                        );
                    }

                    for msg in &turn.messages {
                        println!("  Message [{:?}] ID: {}", msg.role, msg.id);
                        for part in &msg.parts {
                            match part {
                                ContentPart::Text { text, annotations } => {
                                    println!("    [Text]: {}", text);
                                    for ann in annotations {
                                        if let Annotation::UrlCitation {
                                            url,
                                            title,
                                            start_index,
                                            end_index,
                                        } = ann
                                        {
                                            println!(
                                                "      🔗 Citation: {} ({}) [{}-{}]",
                                                title, url, start_index, end_index
                                            );
                                        }
                                    }
                                }
                                ContentPart::Reasoning { content, signature } => {
                                    println!(
                                        "    [Reasoning (sig: {:?})]: {}",
                                        signature.as_deref().unwrap_or("none"),
                                        content
                                    );
                                }
                                ContentPart::WebSearchCall(call) => {
                                    println!(
                                        "    🌐 [WebSearchCall]: Query='{}' Sources={}",
                                        call.query,
                                        call.sources.len()
                                    );
                                    for src in &call.sources {
                                        println!("       - {} ({})", src.title, src.url);
                                    }
                                }
                                ContentPart::ToolCall(call) => {
                                    println!(
                                        "    ⚙️ [ToolCall]: {}({})",
                                        call.name, call.arguments
                                    );
                                }
                                ContentPart::ToolResult(res) => {
                                    println!(
                                        "    📦 [ToolResult] Error={}: {}",
                                        res.is_error, res.content
                                    );
                                }
                                ContentPart::Image { source, .. } => {
                                    println!("    🖼️ [Image]: {:?}", source);
                                }
                            }
                        }
                    }
                }
            }
        },
    }

    Ok(())
}
