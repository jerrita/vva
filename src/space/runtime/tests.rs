use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::mode_model::MODE_MODEL_FILE;
use super::*;
use crate::config::model::ModelLimitConfig;
use crate::config::{ModelConfig, ProviderConfig, ProviderType};
use crate::space::channel::{self, FileSource, InboundFile};
use crate::space::command::Command;
use crate::space::curator::CuratorResult;
use crate::space::router::{ModelReply, RouteInput, RouteModel};
use crate::space::scheduler::Task;
use crate::storage::jsonl::JsonlSessionStore;

/// 内存假 channel：记录出站文本，id 依次为 out1、out2…
#[derive(Default)]
struct FakeChannel {
    sent: StdMutex<Vec<OutboundText>>,
    fail_binding: StdMutex<Option<String>>,
}

#[async_trait::async_trait]
impl Channel for FakeChannel {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn send(&self, msg: Outbound) -> channel::Result<Vec<String>> {
        let Outbound::Text(text) = msg else {
            panic!("本期只发文本");
        };
        if self.fail_binding.lock().unwrap().as_deref() == Some(&text.binding) {
            return Err(channel::ChannelError::Protocol("模拟发送失败".into()));
        }
        let mut sent = self.sent.lock().unwrap();
        sent.push(text);
        Ok(vec![format!("out{}", sent.len())])
    }
    async fn run(self: Arc<Self>, _inbound: mpsc::Sender<Inbound>) -> channel::Result<()> {
        Ok(())
    }
}

impl FakeChannel {
    /// 等待累计出站消息达到 `n` 条
    async fn wait(&self, n: usize) -> Vec<OutboundText> {
        for _ in 0..200 {
            {
                let sent = self.sent.lock().unwrap();
                if sent.len() >= n {
                    return sent.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("等待 {n} 条出站消息超时: {:?}", self.sent.lock().unwrap());
    }
}

/// 任何调用都视为失败：L0 路径不应触达小模型
struct NoModel;

#[async_trait::async_trait]
impl RouteModel for NoModel {
    async fn decide(&self, _: &RouteInput) -> Result<ModelReply, String> {
        Err("L0 路径不应调用小模型".into())
    }
}

#[derive(Debug)]
struct BytesSource(&'static [u8]);

#[async_trait::async_trait]
impl FileSource for BytesSource {
    async fn download(&self, dest: &Path) -> channel::Result<()> {
        tokio::fs::write(dest, self.0).await?;
        Ok(())
    }
}

/// 按 Anthropic Messages 流式规范应答固定文本的本地端点
async fn mock_anthropic() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = conn.read(&mut chunk).await.unwrap_or(0);
                    buf.extend_from_slice(&chunk[..n]);
                    if n == 0 || request_complete(&buf) {
                        break;
                    }
                }
                let events = [
                    r#"{"type":"message_start","message":{"usage":{"input_tokens":10}}}"#,
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"pong"}}"#,
                    r#"{"type":"content_block_stop","index":0}"#,
                    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
                    r#"{"type":"message_stop"}"#,
                ];
                let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                let _ = conn.write_all(format!("{head}{body}").as_bytes()).await;
            });
        }
    });
    url
}

fn request_complete(buf: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buf);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    body.len() >= length
}

struct Fixture {
    runtime: Arc<Runtime>,
    schedule: Arc<ScheduleStore>,
    channel: Arc<FakeChannel>,
    manager: Arc<SessionManager>,
    dir: PathBuf,
}

async fn fixture() -> Fixture {
    fixture_with_config(AgentSpaceConfig::default()).await
}

async fn fixture_with_config(config: AgentSpaceConfig) -> Fixture {
    let dir = std::env::temp_dir().join(format!("vva-runtime-{}", uuid::Uuid::new_v4()));
    let mut app = crate::config::AppConfig::default();
    app.settings.default = "p/m".into();
    app.providers.insert(
        "p".into(),
        ProviderConfig {
            api_key: "k".into(),
            base_url: mock_anthropic().await,
            provider_type: Some(ProviderType::Anthropic),
            models: ["m", "other"]
                .into_iter()
                .map(|id| {
                    (
                        id.to_string(),
                        ModelConfig {
                            name: "M".into(),
                            limit: Some(ModelLimitConfig {
                                context: 1_000_000,
                                output: 1000,
                            }),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        },
    );
    let manager = Arc::new(SessionManager::new(
        Arc::new(JsonlSessionStore::new(dir.join("store")).unwrap()),
        app,
        &dir,
        dir.join("state.json"),
    ));
    let channel = Arc::new(FakeChannel::default());
    let router = Router::new(Box::new(NoModel), dir.join("router.jsonl"));
    let schedule = ScheduleStore::open(&dir).unwrap();
    let runtime = Runtime::new(
        manager.clone(),
        config,
        channel.clone(),
        router,
        schedule.clone(),
    )
    .unwrap();
    Fixture {
        runtime,
        schedule,
        channel,
        manager,
        dir,
    }
}

const BINDING: &str = "fake:c2c:u1";

fn inbound(id: &str, text: &str) -> Inbound {
    Inbound {
        binding: BINDING.into(),
        user_id: "u1".into(),
        message_id: id.into(),
        text: text.into(),
        files: Vec::new(),
        reply_to: None,
    }
}

impl Fixture {
    async fn topic(&self, binding: &str) -> Arc<ManagedSession> {
        let handle = self
            .runtime
            .create(binding, SessionKind::Topic, "test")
            .await
            .unwrap();
        self.manager
            .store()
            .save_header(&handle.snapshot().session)
            .await
            .unwrap();
        handle
    }

    async fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self
            .manager
            .store()
            .list_sessions()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        ids.sort();
        ids
    }

    async fn focus(&self) -> Option<String> {
        self.runtime
            .state
            .lock()
            .await
            .bindings
            .focus(BINDING)
            .map(str::to_string)
    }
}

#[tokio::test]
async fn first_message_creates_session_replies_and_reply_anchor_skips_router() {
    let f = fixture().await;
    f.runtime.handle(inbound("m1", "hello")).await;
    let sent = f.channel.wait(1).await;
    assert_eq!(sent[0].text, "pong");
    assert_eq!(sent[0].reply_to.as_deref(), Some("m1"));

    let ids = f.session_ids().await;
    assert_eq!(ids.len(), 1);
    assert_eq!(f.focus().await.as_deref(), Some(ids[0].as_str()));
    let header = f
        .manager
        .live(&ids[0])
        .await
        .unwrap()
        .snapshot()
        .session
        .header
        .clone();
    assert_eq!(header.origin.unwrap().chat_id, "c2c:u1");
    assert_eq!(header.routed_by.as_deref(), Some("first"));

    // 回复 bot 出站消息：L0 直达同一 session（NoModel 保证未调用小模型）
    let mut follow = inbound("m2", "继续");
    follow.reply_to = Some(channel::ReplyRef {
        message_id: "out1".into(),
        text: "pong".into(),
    });
    f.runtime.handle(follow).await;
    let sent = f.channel.wait(2).await;
    assert_eq!(sent[1].text, "pong");
    assert_eq!(f.session_ids().await, ids);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn files_without_text_are_buffered_then_merged_into_next_message() {
    let f = fixture().await;
    let mut only_file = inbound("m1", "");
    only_file.files = vec![InboundFile::new(
        "a.pdf".into(),
        "application/pdf".into(),
        BytesSource(b"pdf"),
    )];
    f.runtime.handle(only_file).await;
    let sent = f.channel.wait(1).await;
    assert!(sent[0].text.contains("已收到"), "{}", sent[0].text);
    assert!(f.session_ids().await.is_empty(), "无文字文件不触发路由");

    f.runtime.handle(inbound("m2", "总结一下")).await;
    f.channel.wait(2).await;
    let id = f.session_ids().await.remove(0);
    let session = f
        .manager
        .live(&id)
        .await
        .unwrap()
        .snapshot()
        .session
        .clone();
    let user = session
        .turns
        .iter()
        .find(|t| t.text_content().contains("总结一下"))
        .unwrap();
    let path = f.dir.join("files/fake_c2c_u1/a.pdf");
    assert!(user.text_content().contains(&path.display().to_string()));
    assert_eq!(std::fs::read(&path).unwrap(), b"pdf");
    assert_eq!(f.runtime.state.lock().await.pending.count(BINDING), 0);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn switch_resolves_prefix_and_rejects_ambiguity() {
    let f = fixture().await;
    f.runtime.handle(inbound("m1", "/new 第一个")).await;
    f.channel.wait(1).await;
    f.runtime.handle(inbound("m2", "/new 第二个")).await;
    f.channel.wait(2).await;
    let ids = f.session_ids().await;
    assert_eq!(ids.len(), 2);
    let focus = f.focus().await.unwrap();
    let other = ids.iter().find(|id| **id != focus).unwrap().clone();
    f.runtime
        .handle(inbound("m3", &format!("/switch {other}")))
        .await;
    let sent = f.channel.wait(3).await;
    assert!(
        sent[2].text.contains(&crate::space::markdown::text(&other)),
        "{}",
        sent[2].text
    );
    assert_eq!(f.focus().await, Some(other));

    f.runtime.handle(inbound("m4", "/switch sess_")).await;
    let sent = f.channel.wait(4).await;
    assert!(sent[3].text.contains("匹配多个"), "{}", sent[3].text);

    f.runtime.handle(inbound("m5", "/sessions")).await;
    let sent = f.channel.wait(5).await;
    assert!(
        ids.iter()
            .all(|id| { sent[4].text.contains(&crate::space::markdown::text(id)) })
    );
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn rename_unknown_and_clear_commands() {
    let f = fixture().await;
    f.runtime.handle(inbound("m1", "/new 第一个")).await;
    f.channel.wait(1).await;
    let id = f.focus().await.unwrap();

    f.runtime.handle(inbound("m2", "/rename 新标题")).await;
    let sent = f.channel.wait(2).await;
    assert!(sent[1].text.contains("新标题"), "{}", sent[1].text);
    let handle = f.manager.live_or_load(&id).await.unwrap();
    assert_eq!(
        handle.snapshot().session.header.title.as_deref(),
        Some("新标题")
    );

    // 未知斜杠命令显式拒绝，不交给模型
    f.runtime.handle(inbound("m3", "/stat2")).await;
    let sent = f.channel.wait(3).await;
    assert!(sent[2].text.contains("未知命令 /stat2"), "{}", sent[2].text);

    f.runtime.handle(inbound("m4", "/clear")).await;
    let sent = f.channel.wait(4).await;
    assert!(sent[3].text.contains("已删除"), "{}", sent[3].text);
    assert!(f.session_ids().await.is_empty());
    assert_eq!(f.focus().await, None);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn model_switch_targets_focus_session_or_mode() {
    let f = fixture().await;
    let file = f.dir.join(MODE_MODEL_FILE);
    // 无 focus：改 agent 模式并落盘
    f.runtime.handle(inbound("m1", "/model p/m high")).await;
    let sent = f.channel.wait(1).await;
    assert!(sent[0].text.contains("Agent 模式"), "{}", sent[0].text);
    assert!(std::fs::read_to_string(&file).unwrap().contains("high"));
    // 未声明 limit.context / 不存在的模型显式拒绝
    f.runtime.handle(inbound("m2", "/model nope/x")).await;
    f.channel.wait(2).await;
    assert!(std::fs::read_to_string(&file).unwrap().contains("p/m"));

    // 有 focus：只改会话，不动 agent 模式
    f.runtime.handle(inbound("m3", "/new 你好")).await;
    f.channel.wait(3).await;
    std::fs::remove_file(&file).unwrap();
    f.runtime.handle(inbound("m3b", "/model list")).await;
    let sent = f.channel.wait(4).await;
    assert!(
        sent[3].text.lines().any(|line| line.contains("p/m")
            && line.contains("默认会话")
            && line.contains("Agent 模式")),
        "{}",
        sent[3].text
    );
    f.runtime.handle(inbound("m4", "/model p/m")).await;
    let sent = f.channel.wait(5).await;
    assert!(!sent[4].text.contains("Agent 模式"), "{}", sent[4].text);
    assert!(!file.exists());
    // --global：两者都改
    f.runtime.handle(inbound("m5", "/model --global p/m")).await;
    let sent = f.channel.wait(6).await;
    assert!(sent[5].text.contains("Agent 模式"), "{}", sent[5].text);
    assert!(file.exists());
    // 仅 --variant：沿用当前模型只改档位
    f.runtime
        .handle(inbound("m7", "/model --variant high"))
        .await;
    let sent = f.channel.wait(7).await;
    assert!(sent[6].text.contains("p/m (high)"), "{}", sent[6].text);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn curator_model_selection_across_mode_changes() {
    for configured in [false, true] {
        let config = serde_json::from_value(if configured {
            serde_json::json!({"curator_model": "m"})
        } else {
            serde_json::json!({})
        })
        .unwrap();
        let f = fixture_with_config(config).await;
        let source = f.topic(BINDING).await;
        let mut session = source.snapshot().session.as_ref().clone();
        session.header.curation_requested = true;
        f.manager.store().save_header(&session).await.unwrap();
        let mut seen = HashSet::new();
        for mode in ["p/m", "p/other"] {
            let next = super::mode_model::check_mode_model(&f.manager, mode, None).unwrap();
            f.runtime.save_mode_model(next).unwrap();
            f.manager
                .store()
                .append_turn(
                    &source.id,
                    &crate::model::Turn::user_turn(crate::model::Message::text("示例偏好")),
                )
                .await
                .unwrap();
            let result = f.runtime.curator.curate(&source.id).await;
            assert_ne!(result.status, CuratorStatus::Failed, "{}", result.summary);
            let sessions = f.manager.store().list_sessions().await.unwrap();
            let created: Vec<_> = sessions
                .iter()
                .filter(|s| s.kind == SessionKind::Curator && !seen.contains(&s.id))
                .collect();
            assert_eq!(created.len(), 1);
            assert_eq!(
                created[0].current_model,
                if configured { "p/m" } else { mode }
            );
            seen.insert(created[0].id.clone());
        }
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}

#[tokio::test]
async fn invalid_curator_model_rejects_runtime_startup() {
    let f = fixture().await;
    for model in ["", "missing/model", "p/missing"] {
        let config = AgentSpaceConfig {
            curator_model: Some(model.into()),
            ..Default::default()
        };
        let result = Runtime::new(
            f.manager.clone(),
            config,
            f.channel.clone(),
            Router::new(Box::new(NoModel), f.dir.join("router.jsonl")),
            f.schedule.clone(),
        );
        let error = result.err().expect("无效 curator 模型须在启动时拒绝");
        assert!(error.contains("settings.agent.curator_model"), "{error}");
    }
    let _ = std::fs::remove_dir_all(&f.dir);
}

fn task(json: &str) -> Task {
    serde_json::from_str(json).unwrap()
}

#[tokio::test]
async fn scheduled_task_runs_once_in_original_scope_and_notifies_only_homes() {
    let f = fixture().await;
    {
        let mut state = f.runtime.state.lock().await;
        state.bindings.set_home("fake:c2c:home", true).unwrap();
        state.bindings.set_home("fake:group:home", true).unwrap();
    }
    f.runtime
        .fire(task(
            r#"{"id":"multi","every":"1h","prompt":"汇报","bindings":["fake:c2c:u1","fake:group:g2"]}"#,
        ))
        .await;
    let sent = f.channel.wait(2).await;
    let mut bindings: Vec<_> = sent.iter().map(|m| m.binding.as_str()).collect();
    bindings.sort();
    assert_eq!(bindings, ["fake:c2c:home", "fake:group:home"]);
    assert!(sent.iter().all(|m| m.text == "pong"));
    // 只执行一次：只有一个 oneshot 会话，来源是第一个 binding
    let ids = f.session_ids().await;
    assert_eq!(ids.len(), 1);
    let session = f
        .manager
        .live(&ids[0])
        .await
        .unwrap()
        .snapshot()
        .session
        .clone();
    assert_eq!(session.header.kind, SessionKind::Oneshot);
    assert_eq!(session.header.origin.as_ref().unwrap().chat_id, "c2c:u1");
    assert!(
        session
            .turns
            .iter()
            .any(|t| t.text_content().contains("type=\"scheduled\""))
    );
    assert_eq!(f.focus().await, None, "oneshot 不抢占 focus");
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn scheduled_failure_notifies_and_disables_on_permanent_error() {
    let f = fixture().await;
    f.runtime
        .state
        .lock()
        .await
        .bindings
        .set_home("fake:c2c:home", true)
        .unwrap();
    let t = task(
        r#"{"id":"gone","every":"1h","prompt":"p","session":"sess_none0","bindings":["fake:c2c:u1"]}"#,
    );
    f.schedule.add(t.clone()).unwrap();
    f.runtime.fire(t).await;
    let sent = f.channel.wait(1).await;
    assert_eq!(sent[0].binding, "fake:c2c:home");
    assert!(sent[0].text.contains("已停用"), "{}", sent[0].text);
    assert!(sent[0].text.contains("sess_none0"), "{}", sent[0].text);
    assert!(!f.schedule.list()[0].1.enabled);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn schedule_command_manages_tasks() {
    let f = fixture().await;
    f.runtime.handle(inbound("m1", "/schedule")).await;
    let sent = f.channel.wait(1).await;
    assert!(sent[0].text.contains("暂无"), "{}", sent[0].text);

    f.schedule
        .add(task(
            r#"{"id":"water1","every":"1h","prompt":"喝水","bindings":["fake:c2c:u1"]}"#,
        ))
        .unwrap();
    f.runtime.handle(inbound("m2", "/schedule list")).await;
    f.runtime.handle(inbound("m3", "/schedule pause wat")).await;
    f.runtime
        .handle(inbound("m4", "/schedule run water1"))
        .await;
    f.runtime
        .handle(inbound("m5", "/schedule resume water1"))
        .await;
    let sent = f.channel.wait(5).await;
    assert!(
        sent[1].text.contains("water1") && sent[1].text.contains("every 1h"),
        "{}",
        sent[1].text
    );
    assert!(sent[2].text.contains("已暂停"), "{}", sent[2].text);
    assert!(sent[3].text.contains("已停用"), "{}", sent[3].text);
    assert!(sent[4].text.contains("已恢复"), "{}", sent[4].text);

    f.runtime
        .handle(inbound("m6", "/schedule delete water1"))
        .await;
    let sent = f.channel.wait(6).await;
    assert!(sent[5].text.contains("已删除"), "{}", sent[5].text);
    assert!(f.schedule.list().is_empty());
    f.runtime.handle(inbound("m7", "/schedule nope")).await;
    let sent = f.channel.wait(7).await;
    assert!(sent[6].text.contains("用法"), "{}", sent[6].text);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn memory_update_broadcast_notes_eligible_sessions_without_a_turn() {
    let f = fixture().await;
    let mut ids = BTreeMap::new();
    for (name, kind, superseded) in [
        ("topic", SessionKind::Topic, false),
        ("oneshot", SessionKind::Oneshot, false),
        ("curator", SessionKind::Curator, false),
        ("superseded", SessionKind::Topic, true),
        ("source", SessionKind::Topic, false),
    ] {
        let mut session = Session::new(f.dir.clone(), None);
        session.header.kind = kind;
        session.header.origin = Some(Origin {
            channel: "fake".into(),
            chat_id: "c2c:u1".into(),
        });
        if superseded {
            session.header.superseded_by = Some("sess_next".into());
        }
        f.manager.store().save_header(&session).await.unwrap();
        ids.insert(name, session.header.id.clone());
    }
    // 来源会话已加载（运行时句柄）与冷会话两条路径都要覆盖：预加载 topic
    f.manager.live_or_load(&ids["topic"]).await.unwrap();

    f.runtime
        .push_curator(CuratorReport {
            session_id: ids["source"].clone(),
            origin: None,
            result: CuratorResult {
                status: CuratorStatus::Updated,
                summary: "新增偏好主题".into(),
            },
        })
        .await;

    for (name, id) in &ids {
        let stored = f.manager.store().load_session(id).await.unwrap().unwrap();
        if matches!(*name, "topic" | "oneshot") {
            assert_eq!(stored.turns.len(), 1, "{name}");
            let text = stored.turns[0].text_content();
            assert!(text.starts_with("<system-reminder type=\"memory-updated\">"));
            assert!(text.contains("新增偏好主题"));
            assert!(text.contains("MEMORY.md"));
        } else {
            assert!(stored.turns.is_empty(), "{name}");
        }
    }
    // 广播不触发模型调用：已加载会话也没有助手轮次
    let live = f.manager.live(&ids["topic"]).await.unwrap().snapshot();
    assert!(
        live.session
            .turns
            .iter()
            .all(|t| t.role != crate::model::Role::Assistant)
    );
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn group_scope_filters_lists_switches_and_clears_legacy_focus() {
    let f = fixture().await;
    let personal = f.topic(BINDING).await;
    let second = f.topic("fake:c2c:u2").await;
    let group = f.topic("fake:group:room").await;
    let other_group = f.topic("fake:group:other").await;
    let personal_candidates = f.runtime.candidates(BINDING).await.unwrap();
    assert_eq!(personal_candidates.len(), 2);
    assert!(personal_candidates.iter().any(|c| c.id == personal.id));
    assert!(personal_candidates.iter().any(|c| c.id == second.id));
    let group_candidates = f.runtime.candidates("fake:group:room").await.unwrap();
    assert_eq!(group_candidates.len(), 1);
    assert_eq!(group_candidates[0].id, group.id);

    let mut msg = inbound("scope", "");
    msg.binding = "fake:group:room".into();
    f.runtime.command(&msg, Command::Sessions).await.unwrap();
    let sent = f.channel.wait(1).await;
    let list = &sent[0].text;
    assert!(list.contains(&crate::space::markdown::text(&group.id)));
    for id in [&personal.id, &second.id, &other_group.id] {
        assert!(!list.contains(&crate::space::markdown::text(id)));
        assert!(
            f.runtime
                .command(&msg, Command::Switch(id.clone()))
                .await
                .is_err()
        );
        assert!(
            f.runtime
                .command(&msg, Command::Clear(id.clone()))
                .await
                .is_err()
        );
    }
    {
        let mut state = f.runtime.state.lock().await;
        state
            .bindings
            .set_focus(&msg.binding, Some(personal.id.clone()))
            .unwrap();
        state
            .bindings
            .set_focus(BINDING, Some(personal.id.clone()))
            .unwrap();
    }
    f.runtime.command(&msg, Command::Status).await.unwrap();
    assert_eq!(
        f.runtime.state.lock().await.bindings.focus(&msg.binding),
        None
    );
    assert_eq!(f.focus().await.as_deref(), Some(personal.id.as_str()));
    assert!(f.runtime.command(&msg, Command::Stop).await.is_err());
    assert!(
        f.runtime
            .command(&msg, Command::Clear(String::new()))
            .await
            .is_err()
    );
    for cmd in [
        Command::Model(String::new()),
        Command::Schedule(String::new()),
        Command::Skills,
        Command::Delete(String::new()),
    ] {
        assert!(
            f.runtime
                .command(&msg, cmd)
                .await
                .unwrap_err()
                .contains("私聊")
        );
    }
    f.runtime.command(&msg, Command::Help).await.unwrap();
    let sent = f.channel.wait(3).await;
    assert!(sent[2].text.contains("/sethome"));
    assert!(!sent[2].text.contains("/schedule"));
    assert!(
        f.manager
            .store()
            .load_session(&personal.id)
            .await
            .unwrap()
            .is_some()
    );
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn first_group_message_ignores_personal_candidates_and_rejects_foreign_reply() {
    let f = fixture().await;
    let personal = f.topic(BINDING).await;
    f.topic("fake:group:other").await;
    {
        let mut state = f.runtime.state.lock().await;
        state
            .bindings
            .set_focus(BINDING, Some(personal.id.clone()))
            .unwrap();
        state.index.record("fake:outside", &personal.id).unwrap();
    }
    let mut msg = inbound("group-first", "ping");
    msg.binding = "fake:group:room".into();
    f.runtime.handle(msg.clone()).await;
    let sent = f.channel.wait(1).await;
    assert_eq!(sent[0].text, "pong");
    assert_eq!(sent[0].binding, msg.binding);
    let candidates = f.runtime.candidates(&msg.binding).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].focus);
    assert_ne!(candidates[0].id, personal.id);
    assert_eq!(f.focus().await.as_deref(), Some(personal.id.as_str()));
    msg.message_id = "group-reply".into();
    msg.reply_to = Some(channel::ReplyRef {
        message_id: "outside".into(),
        text: "sample".into(),
    });
    f.runtime.handle(msg).await;
    let sent = f.channel.wait(2).await;
    assert!(sent[1].text.contains("引用会话不属于当前聊天"));
    assert_eq!(f.session_ids().await.len(), 3);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn home_commands_persist_and_notifications_follow_latest_set() {
    let f = fixture().await;
    let personal = inbound("home", "/sethome");
    let mut group = inbound("group-home", "/sethome");
    group.binding = "fake:group:home".into();
    f.runtime.handle(personal.clone()).await;
    f.runtime.handle(personal.clone()).await;
    f.runtime.handle(group.clone()).await;
    assert_eq!(
        BindingStore::load(&f.dir).unwrap().homes(),
        [BINDING, group.binding.as_str()]
    );
    assert!(
        f.runtime
            .command(&personal, Command::UnsetHome("extra".into()))
            .await
            .is_err()
    );
    f.channel.sent.lock().unwrap().clear();
    *f.channel.fail_binding.lock().unwrap() = Some(BINDING.into());
    // 一个 home 发送失败仍继续投递其他 home；来源与 focus 不决定目标。
    f.runtime
        .push_curator(CuratorReport {
            session_id: "sess_sample".into(),
            origin: Some(Origin {
                channel: "fake".into(),
                chat_id: "group:source".into(),
            }),
            result: CuratorResult {
                status: CuratorStatus::Failed,
                summary: "sample failure".into(),
            },
        })
        .await;
    let sent = f.channel.wait(1).await;
    assert_eq!(sent[0].binding, group.binding);
    assert!(sent[0].reply_to.is_none());
    *f.channel.fail_binding.lock().unwrap() = None;
    f.runtime.handle(inbound("unset-home", "/unsethome")).await;
    f.channel.sent.lock().unwrap().clear();
    f.runtime
        .send_report(DeliveryTarget::Homes, "sample notice", None)
        .await;
    let sent = f.channel.wait(1).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].binding, group.binding);
    group.text = "/unsethome".into();
    f.runtime.handle(group).await;
    f.channel.sent.lock().unwrap().clear();
    f.runtime
        .send_report(DeliveryTarget::Homes, "sample notice", None)
        .await;
    assert!(f.channel.sent.lock().unwrap().is_empty());
    assert!(BindingStore::load(&f.dir).unwrap().homes().is_empty());
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[tokio::test]
async fn scheduled_execution_without_home_still_completes_and_missed_notices_use_home() {
    let f = fixture().await;
    let t =
        task(r#"{"id":"sample","every":"1h","prompt":"ping","bindings":["fake:group:source"]}"#);
    f.schedule.add(t.clone()).unwrap();
    assert!(f.runtime.fire(t.clone()).await);
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.schedule.get("sample").unwrap().1.last_run.is_none() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(f.channel.sent.lock().unwrap().is_empty());
    assert_eq!(f.session_ids().await.len(), 1);
    f.runtime
        .state
        .lock()
        .await
        .bindings
        .set_home(BINDING, true)
        .unwrap();
    f.runtime
        .report_missed(vec![scheduler::Missed {
            task: t,
            last: scheduler::now_minute(),
        }])
        .await;
    let sent = f.channel.wait(1).await;
    assert_eq!(sent[0].binding, BINDING);
    assert!(sent[0].text.contains("sample"));
    let _ = std::fs::remove_dir_all(&f.dir);
}
