use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use vva::config::{AppConfig, home_dir, load_config};
use vva::harness::{AgentEvent, SessionManager};
use vva::model::{Annotation, ContentPart, WebSearchAction};
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

    let app_config = match load_config(cli.config.as_deref()) {
        Ok(config) => config,
        Err(vva::config::ConfigError::NotFound(_))
            if cli.config.is_none() && std::env::var_os("VVA_CONFIG").is_none() =>
        {
            AppConfig::from_env()
        }
        Err(error) => return Err(error.into()),
    };

    match cli.command {
        // 无子命令等价于不带 prompt 的 run
        None => {
            let workspace = std::env::current_dir().unwrap_or_else(|_| ".".into());
            run_tui(store, app_config, None, workspace, None, None, None).await?;
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

            let manager = SessionManager::new(store.clone(), app_config, workspace);
            let session = manager
                .open_session(session_id.as_deref(), model, variant, web_search)
                .await?;
            let view = session.snapshot();
            println!("🚀 [vva] Session ID: {}", session.id);
            println!("📂 Workspace: {}", view.session.workspace_root.display());
            println!("🎯 Session Model: {}", view.session.current_model);
            let (tx, mut rx) = mpsc::channel::<AgentEvent>(32);
            session.submit(&prompt, Some(tx)).await?;

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
                    AgentEvent::Text { delta: text, .. } => {
                        in_thought = false;
                        if !in_text {
                            println!("\n💬 [Assistant]");
                            in_text = true;
                        }
                        print!("{}", text);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                    }
                    AgentEvent::Citation { annotation, .. } => {
                        if let Annotation::UrlCitation { title, url, .. } = annotation {
                            println!("\n🔗 [Citation] {} ({})", title, url);
                        }
                    }
                    AgentEvent::Notice(notice) => {
                        eprintln!("\n⚠️  [Notice] {notice}");
                    }
                    AgentEvent::ToolExecuting { name, args } => {
                        in_thought = false;
                        in_text = false;
                        println!("\n⚙️ [Tool Calling] {}({})", name, args);
                    }
                    AgentEvent::WebSearchCall(call) => {
                        in_thought = false;
                        in_text = false;
                        match &call.action {
                            WebSearchAction::Search => println!(
                                "\n[web_search] {} ({} sources)",
                                call.query,
                                call.sources.len()
                            ),
                            WebSearchAction::OpenPage { url } => println!("\n[web_open] {url}"),
                            WebSearchAction::FindInPage { url, pattern } => {
                                println!("\n[web_find] {url} / {pattern}")
                            }
                        }
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

                println!("\n=== Turns ({}) ===", session.turns.len());
                for (i, turn) in session.turns.iter().enumerate() {
                    println!(
                        "\n--- Turn #{} [{:?}] ID: {} ---",
                        i + 1,
                        turn.role,
                        turn.id
                    );
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
                        println!("  Message:");
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
                                    match &call.action {
                                        WebSearchAction::Search => println!(
                                            "    [web_search] {} ({} sources)",
                                            call.query,
                                            call.sources.len()
                                        ),
                                        WebSearchAction::OpenPage { url } => {
                                            println!("    [web_open] {url}")
                                        }
                                        WebSearchAction::FindInPage { url, pattern } => {
                                            println!("    [web_find] {url} / {pattern}")
                                        }
                                    }
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
