use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use vva::config::{AppConfig, home_dir, load_config};
use vva::harness::{AgentEvent, SessionManager};
use vva::model::{Annotation, ContentPart, ImageGenerationStatus, WebSearchAction, WebSearchCall};
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

    /// 启动 Web 界面 (浏览器访问)
    Web {
        /// 监听地址
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// 监听端口
        #[arg(short, long, default_value_t = 3000)]
        port: u16,

        /// 工作空间目录路径 (默认为当前目录)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// 列出所有会话记录
    List,
    /// 查看指定会话详情、轮次与元数据
    Inspect { session_id: String },
}

/// 打印一次原生搜索调用的动作摘要（Run 流式输出与 Inspect 详情共用）
fn print_search_action(prefix: &str, call: &WebSearchCall) {
    match &call.action {
        Some(WebSearchAction::Search) => println!(
            "{prefix}[web_search] {} ({} sources)",
            call.query,
            call.sources.len()
        ),
        Some(WebSearchAction::OpenPage { url }) => println!("{prefix}[web_open] {url}"),
        Some(WebSearchAction::FindInPage { url, pattern }) => {
            println!("{prefix}[web_find] {url} / {pattern}")
        }
        None => println!("{prefix}[web_search] (action not reported)"),
    }
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

            let manager = Arc::new(SessionManager::new(
                store.clone(),
                app_config,
                workspace,
                vva_dir.join("state.json"),
            ));
            let session = manager
                .open_session(session_id.as_deref(), model, variant, web_search)
                .await?;
            let view = session.snapshot();
            println!("🚀 [vva] Session ID: {}", session.id);
            println!(
                "📂 Workspace: {}",
                view.session.header.workspace_root.display()
            );
            println!("🎯 Session Model: {}", view.session.header.current_model);
            let (tx, mut rx) = mpsc::channel::<AgentEvent>(32);
            session.submit(&prompt, Vec::new(), Some(tx)).await?;

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
                    AgentEvent::Citation { annotation, .. } => match annotation {
                        Annotation::UrlCitation { title, url, .. }
                        | Annotation::WebSearchResultLocation { title, url, .. } => {
                            println!("\n🔗 [Citation] {} ({})", title, url);
                        }
                        Annotation::FileCitation { .. } => {}
                    },
                    AgentEvent::Capabilities(notices) => {
                        for notice in notices {
                            eprintln!("\n⚠️  [Notice] {notice}");
                        }
                    }
                    AgentEvent::TaskCompleted { text: notice, .. } | AgentEvent::Notice(notice) => {
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
                        print_search_action("\n", &call);
                    }
                    AgentEvent::ImageGenerationCall(call) => {
                        in_thought = false;
                        in_text = false;
                        let status = match call.status {
                            ImageGenerationStatus::InProgress => "in_progress",
                            ImageGenerationStatus::Completed => "completed",
                            ImageGenerationStatus::Incomplete => "incomplete",
                        };
                        println!("\n🖼️ [Image Generation] id={} status={status}", call.id);
                    }
                    AgentEvent::ToolFinished {
                        name,
                        result,
                        is_error,
                    } => {
                        let status_sym = if is_error { "❌" } else { "✅" };
                        println!("{} [{}] output:\n{}", status_sym, name, result.trim());
                    }
                    AgentEvent::AskUser(request) => {
                        println!("\n❓ [AskUser] {} question(s)", request.questions.len());
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

        Some(Commands::Web {
            host,
            port,
            workspace,
        }) => {
            let workspace =
                workspace.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            let manager =
                SessionManager::new(store, app_config, workspace, vva_dir.join("state.json"))
                    .with_config_path(cli.config.as_deref());
            vva::web::run_server(Arc::new(manager), host, port).await?;
        }

        Some(Commands::Session { action }) => match action {
            SessionAction::List => {
                let sessions = store.list_sessions().await?;
                println!(
                    "{:<12} | {:<20} | {:<8} | {:<5} | {:<16} | Title / Workspace",
                    "Session ID", "Model", "Variant", "Turns", "Created At"
                );
                println!(
                    "{:-<12}-+-{:-<20}-+-{:-<8}-+-{:-<5}-+-{:-<16}-+-{:-<30}",
                    "", "", "", "", "", ""
                );
                for s in sessions {
                    let title = s.title.as_deref().unwrap_or("Untitled");
                    let variant_str = s.current_variant.as_deref().unwrap_or("-");
                    println!(
                        "{:<12} | {:<20} | {:<8} | {:<5} | {:<16} | {} ({})",
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
                println!("ID: {}", session.header.id);
                println!("Title: {:?}", session.header.title);
                println!("Workspace: {}", session.header.workspace_root.display());
                println!("Selected Model: {}", session.header.current_model);
                println!("Reasoning Variant: {:?}", session.header.current_variant);
                println!("Created At: {}", session.header.created_at);

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
                                        match ann {
                                            Annotation::UrlCitation {
                                                url,
                                                title,
                                                start_index,
                                                end_index,
                                            } => {
                                                println!(
                                                    "      🔗 Citation: {} ({}) [{}-{}]",
                                                    title, url, start_index, end_index
                                                );
                                            }
                                            Annotation::WebSearchResultLocation {
                                                url,
                                                title,
                                                cited_text,
                                            } => {
                                                println!(
                                                    "      🔗 Citation: {} ({}) \"{}\"",
                                                    title, url, cited_text
                                                );
                                            }
                                            Annotation::FileCitation { .. } => {}
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
                                    print_search_action("    ", call);
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
                                ContentPart::File {
                                    source, filename, ..
                                } => {
                                    println!("    📄 [File] {filename}: {:?}", source);
                                }
                                ContentPart::Image { source, .. } => {
                                    println!("    🖼️ [Image]: {:?}", source);
                                }
                                ContentPart::ImageGenerationCall(call) => {
                                    let status = match call.status {
                                        ImageGenerationStatus::InProgress => "in_progress",
                                        ImageGenerationStatus::Completed => "completed",
                                        ImageGenerationStatus::Incomplete => "incomplete",
                                    };
                                    println!(
                                        "    🖼️ [ImageGeneration] id={} status={status}",
                                        call.id
                                    );
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
