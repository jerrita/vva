//! Session 生命周期：空闲超过 TTL → 按标记沉淀 → 删除。
//!
//! 周期扫描 agent 工作区中由 agent space 管理的会话（带 IM 来源或非 Topic 类别）。
//! 沉淀失败的会话保留并告警，按指数退避重试（同一原因只推送一次）；运行中的会话不清理。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::curator::{Curator, CuratorReport, CuratorStatus};
use crate::config::AgentSpaceConfig;
use crate::harness::SessionManager;
use crate::model::{SessionKind, SessionSummary};

/// 默认扫描周期
pub const SWEEP_PERIOD: Duration = Duration::from_secs(60);

/// 沉淀失败后的首次重试间隔，之后逐次翻倍直至上限
const RETRY_BASE_SECS: i64 = 300;
const RETRY_MAX_SECS: i64 = 6 * 3600;

/// 某会话的连续沉淀失败状态
struct Failure {
    count: u32,
    retry_at: DateTime<Utc>,
    summary: String,
}

/// 每次整理完成（含失败）的回调，上层据 `origin` 推送摘要到 IM
pub type CuratorNotify = Arc<dyn Fn(CuratorReport) + Send + Sync>;

pub struct Lifecycle {
    manager: Arc<SessionManager>,
    curator: Arc<Curator>,
    config: AgentSpaceConfig,
    notify: CuratorNotify,
    failures: Mutex<HashMap<String, Failure>>,
}

impl Lifecycle {
    pub fn new(
        manager: Arc<SessionManager>,
        curator: Arc<Curator>,
        config: AgentSpaceConfig,
        notify: CuratorNotify,
    ) -> Self {
        Self {
            manager,
            curator,
            config,
            notify,
            failures: Mutex::new(HashMap::new()),
        }
    }

    fn backing_off(&self, id: &str, now: DateTime<Utc>) -> bool {
        let failures = self.failures.lock().unwrap();
        failures.get(id).is_some_and(|f| now < f.retry_at)
    }

    /// 记录一次失败并返回是否需要推送（首次失败或原因变化）
    fn record_failure(&self, id: &str, summary: &str, now: DateTime<Utc>) -> bool {
        let mut failures = self.failures.lock().unwrap();
        let entry = failures.entry(id.to_string()).or_insert(Failure {
            count: 0,
            retry_at: now,
            summary: String::new(),
        });
        let announce = entry.count == 0 || entry.summary != summary;
        entry.count += 1;
        let delay = (RETRY_BASE_SECS << (entry.count - 1).min(16)).min(RETRY_MAX_SECS);
        entry.retry_at = now + chrono::Duration::seconds(delay);
        entry.summary = summary.to_string();
        announce
    }

    /// 启动周期清理任务；调用方持有句柄，退出时 abort
    pub fn spawn(self: Arc<Self>, period: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(error) = self.sweep().await {
                    tracing::warn!("Session cleanup sweep failed: {error}");
                }
            }
        })
    }

    async fn sweep(&self) -> Result<Vec<String>, String> {
        self.sweep_at(Utc::now()).await
    }

    /// 以 `now` 为基准扫描一次，返回本次删除的会话 id
    async fn sweep_at(&self, now: DateTime<Utc>) -> Result<Vec<String>, String> {
        let summaries = self
            .manager
            .store()
            .list_sessions()
            .await
            .map_err(|e| e.to_string())?;
        let mut deleted = Vec::new();
        let (mut candidates, mut due) = (0, 0);
        for summary in summaries {
            if !summary.agent_managed() || summary.workspace_root != self.manager.workspace() {
                continue;
            }
            candidates += 1;
            let ttl = summary.ttl_secs.unwrap_or(self.config.ttl_secs);
            let idle = (now - summary.last_activity.unwrap_or(summary.created_at))
                .num_seconds()
                .max(0) as u64;
            if idle < ttl {
                continue;
            }
            due += 1;
            let live = self.manager.live(&summary.id).await;
            if live
                .as_ref()
                .is_some_and(|handle| handle.is_running() || handle.has_background_work())
            {
                continue;
            }
            if summary.curation_requested && self.backing_off(&summary.id, now) {
                continue;
            }
            let seen_turns = live.as_ref().map(|handle| handle.turn_seq());
            // curator 会话自身是沉淀的执行载体，不再沉淀
            if summary.kind != SessionKind::Curator && !self.curate(&summary, now).await {
                continue;
            }
            // 沉淀期间会话又有活动：保留，下个周期增量沉淀
            if let Some(handle) = &live
                && (handle.is_running()
                    || handle.has_background_work()
                    || Some(handle.turn_seq()) != seen_turns)
            {
                continue;
            }
            match self.delete(&summary.id).await {
                Ok(()) => {
                    tracing::info!(
                        "会话已删除: id={} 原因=空闲 {idle}s 超过 TTL {ttl}s",
                        summary.id
                    );
                    deleted.push(summary.id);
                }
                Err(error) => tracing::warn!("Failed to delete {}: {error}", summary.id),
            }
        }
        tracing::debug!(
            "生命周期扫描: 候选 {candidates} 个, 到期 {due} 个, 删除 {} 个",
            deleted.len()
        );
        Ok(deleted)
    }

    /// 沉淀并按需推送结果；返回是否成功（失败则保留会话、进入退避）
    async fn curate(&self, summary: &SessionSummary, now: DateTime<Utc>) -> bool {
        let result = self.curator.curate(&summary.id).await;
        let failed = result.status == CuratorStatus::Failed;
        let announce = if failed {
            tracing::warn!("Curator failed for {}: {}", summary.id, result.summary);
            self.record_failure(&summary.id, &result.summary, now)
        } else {
            self.failures.lock().unwrap().remove(&summary.id);
            true
        };
        if announce {
            (self.notify)(CuratorReport {
                session_id: summary.id.clone(),
                origin: summary.origin.clone(),
                result,
            });
        }
        !failed
    }

    async fn delete(&self, id: &str) -> Result<(), String> {
        let handle = self.manager.live_or_load(id).await?;
        self.manager.delete_session(&handle).await?;
        self.failures.lock().unwrap().remove(id);
        self.curator.forget(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ModelConfig, ProviderConfig};
    use crate::model::{Message, Origin, Session, Turn};
    use crate::space::curator::CuratorRunner;
    use crate::storage::jsonl::JsonlSessionStore;
    use std::path::PathBuf;

    /// 固定结果的 curator 后端，记录被整理的次数
    struct Stub {
        result: Result<String, String>,
        runs: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl CuratorRunner for Stub {
        async fn run(&self, _prompt: &str, _label: &str) -> Result<String, String> {
            self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.result.clone()
        }
    }

    struct Fixture {
        dir: PathBuf,
        manager: Arc<SessionManager>,
        stub: Arc<Stub>,
        lifecycle: Lifecycle,
        reports: Arc<Mutex<Vec<CuratorReport>>>,
    }

    impl Fixture {
        fn runs(&self) -> usize {
            self.stub.runs.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// 落盘一个带对话的 agent space 会话，空闲起点为 `idle_secs` 之前
        async fn seed(&self, kind: SessionKind, idle_secs: i64) -> Session {
            let mut session = Session::new(&self.dir, None);
            session.header.kind = kind;
            session.header.curation_requested = kind.is_topic();
            session.header.origin = Some(Origin {
                channel: "qq".into(),
                chat_id: "c1".into(),
            });
            session.header.last_activity = Some(Utc::now() - chrono::Duration::seconds(idle_secs));
            let store = self.manager.store();
            store.save_header(&session).await.unwrap();
            for turn in [
                Turn::user_turn(Message::text("记住我喜欢茶")),
                Turn::assistant_turn(vec![Message::text("好的")]),
            ] {
                store.append_turn(&session.header.id, &turn).await.unwrap();
            }
            session
        }

        async fn exists(&self, id: &str) -> bool {
            self.manager
                .store()
                .load_session(id)
                .await
                .unwrap()
                .is_some()
        }
    }

    fn fixture(result: Result<String, String>, app: AppConfig) -> Fixture {
        let dir = std::env::temp_dir().join(format!("vva-lifecycle-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(JsonlSessionStore::new(&dir).unwrap());
        let manager = Arc::new(SessionManager::new(
            store.clone(),
            app,
            &dir,
            dir.join("state.json"),
        ));
        let stub = Arc::new(Stub {
            result,
            runs: Default::default(),
        });
        let curator = Arc::new(Curator::new(
            stub.clone(),
            store,
            dir.join("agent"),
            dir.join("skills"),
        ));
        let reports = Arc::new(Mutex::new(Vec::new()));
        let sink = reports.clone();
        let config = AgentSpaceConfig {
            ttl_secs: 3600,
            ..Default::default()
        };
        let lifecycle = Lifecycle::new(
            manager.clone(),
            curator,
            config,
            Arc::new(move |report| sink.lock().unwrap().push(report)),
        );
        Fixture {
            dir,
            manager,
            stub,
            lifecycle,
            reports,
        }
    }

    #[tokio::test]
    async fn default_ttl_keeps_sessions_until_24_hours_of_inactivity() {
        let mut f = fixture(Ok("无需更新".into()), AppConfig::default());
        f.lifecycle.config = AgentSpaceConfig::default();
        let mut session = f.seed(SessionKind::Topic, 0).await;
        session.header.curation_requested = false;
        f.manager.store().save_header(&session).await.unwrap();
        let last_activity = session.header.last_activity.unwrap();

        for seconds in [18_000, 86_399] {
            let now = last_activity + chrono::Duration::seconds(seconds);
            assert!(f.lifecycle.sweep_at(now).await.unwrap().is_empty());
            assert!(f.exists(&session.header.id).await);
        }
        let now = last_activity + chrono::Duration::hours(24);
        assert_eq!(
            f.lifecycle.sweep_at(now).await.unwrap(),
            vec![session.header.id.clone()]
        );
        assert!(!f.exists(&session.header.id).await);
        assert_eq!(f.runs(), 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn persisted_session_ttl_overrides_global_config_at_expiry_boundary() {
        let f = fixture(Ok("无需更新".into()), AppConfig::default());
        for hours in [3, 12] {
            let mut session = f.seed(SessionKind::Topic, 0).await;
            session.header.curation_requested = false;
            session.header.ttl_secs = Some(hours * 3600);
            f.manager.store().save_header(&session).await.unwrap();
            let last_activity = session.header.last_activity.unwrap();
            let ttl = chrono::Duration::hours(hours as i64);
            assert!(
                f.lifecycle
                    .sweep_at(last_activity + ttl - chrono::Duration::seconds(1))
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(f.exists(&session.header.id).await);
            assert_eq!(
                f.lifecycle.sweep_at(last_activity + ttl).await.unwrap(),
                vec![session.header.id.clone()]
            );
            assert!(!f.exists(&session.header.id).await);
        }
        assert_eq!(f.runs(), 0);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn expired_sessions_are_curated_then_deleted() {
        let f = fixture(Ok("无需更新".into()), AppConfig::default());
        let topic = f.seed(SessionKind::Topic, 7200).await;
        let seeded = f.seed(SessionKind::Topic, 7200).await;
        let mut superseded = f
            .manager
            .store()
            .load_session(&seeded.header.id)
            .await
            .unwrap()
            .unwrap();
        superseded.header.superseded_by = Some("sess_other".into());
        f.manager.store().save_header(&superseded).await.unwrap();
        let fresh = f.seed(SessionKind::Topic, 10).await;

        let mut deleted = f.lifecycle.sweep().await.unwrap();
        deleted.sort();
        let mut expected = vec![topic.header.id.clone(), superseded.header.id.clone()];
        expected.sort();
        assert_eq!(deleted, expected);
        assert!(f.exists(&fresh.header.id).await, "未到期的会话保留");
        assert_eq!(f.runs(), 2);
        let reports = f.reports.lock().unwrap();
        assert_eq!(reports.len(), 2);
        assert!(
            reports
                .iter()
                .all(|r| r.result.status == CuratorStatus::Unchanged
                    && r.origin.as_ref().is_some_and(|o| o.channel == "qq"))
        );
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn unmarked_and_revoked_sessions_are_deleted_without_curation_or_backoff() {
        let f = fixture(Err("unavailable".into()), AppConfig::default());
        let mut session = f.seed(SessionKind::Topic, 7200).await;
        assert!(f.lifecycle.sweep().await.unwrap().is_empty());
        assert_eq!(f.runs(), 1);

        session.header.curation_requested = false;
        f.manager.store().save_header(&session).await.unwrap();
        let mut unmarked = f.seed(SessionKind::Topic, 7200).await;
        unmarked.header.curation_requested = false;
        f.manager.store().save_header(&unmarked).await.unwrap();
        let deleted = f.lifecycle.sweep().await.unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(deleted.contains(&session.header.id));
        assert!(deleted.contains(&unmarked.header.id));
        assert_eq!(f.runs(), 1);
        assert!(f.lifecycle.failures.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn failed_curation_keeps_session_and_retries_next_sweep() {
        let f = fixture(Err("boom".into()), AppConfig::default());
        let session = f.seed(SessionKind::Topic, 7200).await;

        assert!(f.lifecycle.sweep().await.unwrap().is_empty());
        assert!(f.exists(&session.header.id).await);
        assert_eq!(
            f.reports.lock().unwrap()[0].result.status,
            CuratorStatus::Failed
        );

        // 退避期内不重试、不重复推送
        assert!(f.lifecycle.sweep().await.unwrap().is_empty());
        assert_eq!(f.runs(), 1);
        assert_eq!(f.reports.lock().unwrap().len(), 1);

        // 退避结束后重试：游标未推进，仍会再次整理；同一原因不再推送
        let later = Utc::now() + chrono::Duration::seconds(RETRY_BASE_SECS + 1);
        assert!(f.lifecycle.sweep_at(later).await.unwrap().is_empty());
        assert_eq!(f.runs(), 2);
        assert_eq!(f.reports.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[tokio::test]
    async fn curator_sessions_and_unmanaged_sessions_follow_their_own_rules() {
        let f = fixture(Ok("x".into()), AppConfig::default());
        let curator = f.seed(SessionKind::Curator, 7200).await;
        // TUI / Web 创建的普通会话（无 origin、Topic）永不被清理
        let mut plain = Session::new(&f.dir, None);
        plain.header.created_at = Utc::now() - chrono::Duration::days(30);
        f.manager.store().save_header(&plain).await.unwrap();

        assert_eq!(
            f.lifecycle.sweep().await.unwrap(),
            std::slice::from_ref(&curator.header.id)
        );
        assert_eq!(f.runs(), 0, "curator 会话直接删除，不再沉淀");
        assert!(f.exists(&plain.header.id).await);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// 接受连接但永不应答的端点：会话请求一直挂起，保持 Running
    async fn hanging_config() -> (AppConfig, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let held = tokio::spawn(async move {
            let mut conns = Vec::new();
            while let Ok((conn, _)) = listener.accept().await {
                conns.push(conn);
            }
        });
        let mut app = AppConfig::default();
        app.settings.default = "p/m".into();
        app.providers.insert(
            "p".into(),
            ProviderConfig {
                api_key: "k".into(),
                base_url: url,
                models: [(
                    "m".to_string(),
                    ModelConfig {
                        name: "M".into(),
                        ..Default::default()
                    },
                )]
                .into(),
                ..Default::default()
            },
        );
        (app, held)
    }

    #[tokio::test]
    async fn running_session_is_not_cleaned_and_turn_end_refreshes_activity() {
        let (app, server) = hanging_config().await;
        let f = fixture(Ok("x".into()), app);
        let seeded = f.seed(SessionKind::Topic, 7200).await;
        let mut session = f
            .manager
            .store()
            .load_session(&seeded.header.id)
            .await
            .unwrap()
            .unwrap();
        session.header.current_model = "p/m".into();
        f.manager.store().save_header(&session).await.unwrap();
        let handle = f.manager.live_or_load(&session.header.id).await.unwrap();
        handle.submit("继续", Vec::new(), None).await.unwrap();
        assert!(handle.is_running() || handle.has_background_work());

        assert!(f.lifecycle.sweep().await.unwrap().is_empty());
        assert_eq!(f.runs(), 0, "运行中的会话不沉淀");
        assert!(f.exists(&session.header.id).await);

        // 中止后轮次结束，last_activity 刷新为当前时刻，TTL 重新计时
        handle.abort_current().await.unwrap();
        let mut turns = handle.subscribe();
        while handle.is_running() {
            let _ = tokio::time::timeout(Duration::from_secs(5), turns.changed())
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let stored = f
            .manager
            .store()
            .load_session(&session.header.id)
            .await
            .unwrap()
            .unwrap();
        let age = Utc::now() - stored.header.last_activity.unwrap();
        assert!(age < chrono::Duration::seconds(30), "{age}");
        assert!(f.lifecycle.sweep().await.unwrap().is_empty());
        server.abort();
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}
