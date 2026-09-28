//! Web 传输投影：首帧快照，后续相对该连接已发送版本的增量。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::harness::{Notice, SessionView};
use crate::model::{
    ContentPart, Message, Role, Session, SessionHeader, TokenUsage, Turn, UserQuestionRequest,
};

/// 浏览器只接收展示字段；供应商回传元数据保留在领域存储中。
#[derive(Serialize)]
struct TurnView<'a> {
    id: &'a str,
    role: &'a Role,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: &'a Option<TokenUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worked_ms: Option<u64>,
}

impl<'a> From<&'a Turn> for TurnView<'a> {
    fn from(turn: &'a Turn) -> Self {
        Self {
            id: &turn.id,
            role: &turn.role,
            messages: &turn.messages,
            usage: &turn.usage,
            elapsed_ms: turn.elapsed_ms,
            worked_ms: turn.worked_ms,
        }
    }
}

#[derive(Serialize, PartialEq)]
struct SessionInfo {
    #[serde(flatten)]
    header: SessionHeader,
    updated_at: DateTime<Utc>,
}

impl From<&Session> for SessionInfo {
    fn from(session: &Session) -> Self {
        Self {
            header: SessionHeader::from(session),
            updated_at: session.updated_at,
        }
    }
}

#[derive(Serialize)]
struct SessionData<'a> {
    #[serde(flatten)]
    info: SessionInfo,
    turns: Vec<TurnView<'a>>,
}

#[derive(Serialize)]
pub(super) struct SessionSnapshot<'a> {
    revision: u64,
    state: String,
    persisted: bool,
    notices: &'a [Notice],
    external_web: &'a [String],
    session: SessionData<'a>,
    draft: Vec<&'a ContentPart>,
    pending_user_question: &'a Option<UserQuestionRequest>,
}

impl<'a> From<&'a SessionView> for SessionSnapshot<'a> {
    fn from(view: &'a SessionView) -> Self {
        Self {
            revision: view.revision,
            state: view.state.status(),
            persisted: view.persisted,
            notices: &view.notices,
            external_web: &view.external_web,
            session: SessionData {
                info: SessionInfo::from(&*view.session),
                turns: view.session.turns.iter().map(TurnView::from).collect(),
            },
            draft: view.draft.iter().map(|part| &**part).collect(),
            pending_user_question: &view.pending_user_question,
        }
    }
}

/// 保留前 from 轮，再用 items 替换尾部；覆盖追加、回退及末轮更新。
#[derive(Serialize)]
struct TurnsPatch<'a> {
    from: usize,
    items: Vec<TurnView<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PartPatch<'a> {
    Set { index: usize, part: &'a ContentPart },
    Text { index: usize, delta: &'a str },
    Reasoning { index: usize, delta: &'a str },
}

#[derive(Serialize)]
struct DraftPatch<'a> {
    len: usize,
    updates: Vec<PartPatch<'a>>,
}

#[derive(Serialize)]
pub(super) struct SessionDelta<'a> {
    base_revision: u64,
    revision: u64,
    state: String,
    persisted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    notices: Option<&'a [Notice]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_web: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turns: Option<TurnsPatch<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    draft: Option<DraftPatch<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_user_question: Option<&'a Option<UserQuestionRequest>>,
}

impl<'a> SessionDelta<'a> {
    pub(super) fn between(previous: &'a SessionView, current: &'a SessionView) -> Self {
        let mut session = None;
        let mut turns = None;
        // 流式文本期间历史 Arc 不变，跳过历史比较和序列化。
        if !Arc::ptr_eq(&previous.session, &current.session) {
            let info = SessionInfo::from(&*current.session);
            if info != SessionInfo::from(&*previous.session) {
                session = Some(info);
            }
            let from = previous
                .session
                .turns
                .iter()
                .zip(&current.session.turns)
                .take_while(|(a, b)| a == b)
                .count();
            if from != previous.session.turns.len() || from != current.session.turns.len() {
                turns = Some(TurnsPatch {
                    from,
                    items: current.session.turns[from..]
                        .iter()
                        .map(TurnView::from)
                        .collect(),
                });
            }
        }
        let mut updates = Vec::new();
        for (index, part) in current.draft.iter().enumerate() {
            let old = previous.draft.get(index);
            // 部件身份相同即内容未变（发布侧写时复制保证），O(1) 跳过
            if old.is_some_and(|old| Arc::ptr_eq(old, part)) {
                continue;
            }
            let update = match (old.map(Arc::as_ref), part.as_ref()) {
                (
                    Some(ContentPart::Text {
                        text: before,
                        annotations: a,
                    }),
                    ContentPart::Text {
                        text: after,
                        annotations: b,
                    },
                ) if a == b && after.starts_with(before) => PartPatch::Text {
                    index,
                    delta: &after[before.len()..],
                },
                (
                    Some(ContentPart::Reasoning {
                        content: before,
                        signature: a,
                    }),
                    ContentPart::Reasoning {
                        content: after,
                        signature: b,
                    },
                ) if a == b && after.starts_with(before) => PartPatch::Reasoning {
                    index,
                    delta: &after[before.len()..],
                },
                _ => PartPatch::Set { index, part },
            };
            updates.push(update);
        }
        let draft = (previous.draft.len() != current.draft.len() || !updates.is_empty()).then_some(
            DraftPatch {
                len: current.draft.len(),
                updates,
            },
        );
        Self {
            base_revision: previous.revision,
            revision: current.revision,
            state: current.state.status(),
            persisted: current.persisted,
            notices: (previous.notices != current.notices).then_some(&current.notices),
            external_web: (previous.external_web != current.external_web)
                .then_some(&current.external_web),
            session,
            turns,
            draft,
            pending_user_question: (previous.pending_user_question
                != current.pending_user_question)
                .then_some(&current.pending_user_question),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::RunState;
    use crate::model::Annotation;
    use serde_json::{Value, json};

    fn view() -> SessionView {
        SessionView {
            revision: 1,
            session: Arc::new(Session::new("/synthetic", None)),
            draft: vec![Arc::new(ContentPart::text("起点🙂"))],
            notices: Vec::new(),
            external_web: Vec::new(),
            state: RunState::Responding,
            persisted: true,
            pending_user_question: None,
        }
    }

    fn delta(before: &SessionView, after: &SessionView) -> Value {
        serde_json::to_value(SessionDelta::between(before, after)).unwrap()
    }

    #[test]
    fn text_delta_size_is_independent_of_history_and_accumulated_text() {
        let mut sizes = Vec::new();
        for length in [0, 1_000_000] {
            let mut before = view();
            Arc::make_mut(&mut before.session)
                .append_turn(Turn::user_turn(Message::text("h".repeat(length))));
            before.draft = vec![Arc::new(ContentPart::text("文".repeat(length)))];
            let mut after = before.clone();
            after.revision = 2;
            if let ContentPart::Text { text, .. } = Arc::make_mut(&mut after.draft[0]) {
                text.push_str("新增🙂");
            }
            let encoded = delta(&before, &after);
            assert_eq!(
                encoded["draft"],
                json!({"len": 1, "updates": [
                    {"type": "text", "index": 0, "delta": "新增🙂"}
                ]})
            );
            assert!(encoded.get("session").is_none());
            assert!(encoded.get("turns").is_none());
            sizes.push(encoded.to_string().len());
        }
        assert_eq!(sizes[0], sizes[1]);
        assert!(sizes[0] < 256);
    }

    #[test]
    fn changes_cover_skipped_revisions_commit_revert_and_metadata_clear() {
        let mut before = view();
        Arc::make_mut(&mut before.session).current_variant = Some("high".into());
        Arc::make_mut(&mut before.session).append_turn(Turn::user_turn(Message::text("sample")));
        let mut after = before.clone();
        after.revision = 12;
        after.draft.clear();
        after.state = RunState::Idle;
        let session = Arc::make_mut(&mut after.session);
        session.current_variant = None;
        session.append_turn(Turn::assistant_turn(vec![Message::text("result")]));
        session.append_turn(Turn::new(Role::Tool, vec![Message::text("output")]));
        let update = delta(&before, &after);
        assert_eq!(update["base_revision"], 1);
        assert_eq!(update["revision"], 12);
        assert_eq!(update["turns"]["from"], 1);
        assert_eq!(update["turns"]["items"].as_array().unwrap().len(), 2);
        assert!(update["turns"]["items"][0].get("meta").is_none());
        assert!(update["session"].get("current_variant").is_none());
        assert_eq!(update["draft"], json!({"len": 0, "updates": []}));
        assert_eq!(update["state"], "");
        let mut reverted = after.clone();
        reverted.revision += 1;
        Arc::make_mut(&mut reverted.session).truncate_turns(0);
        assert_eq!(
            delta(&after, &reverted)["turns"],
            json!({"from": 0, "items": []})
        );
    }

    #[test]
    fn draft_handles_reasoning_citations_relocation_and_abort() {
        let mut before = view();
        before
            .draft
            .insert(0, Arc::new(ContentPart::reasoning("思考", None)));
        let mut after = before.clone();
        after.revision += 1;
        after.draft[0] = Arc::new(ContentPart::reasoning("思考继续", None));
        if let ContentPart::Text { annotations, .. } = Arc::make_mut(&mut after.draft[1]) {
            annotations.push(Annotation::UrlCitation {
                url: "https://example.com".into(),
                title: "example".into(),
                start_index: 0,
                end_index: 2,
            });
        }
        let update = delta(&before, &after);
        assert_eq!(
            update["draft"]["updates"][0],
            json!({"type": "reasoning", "index": 0, "delta": "继续"})
        );
        assert_eq!(update["draft"]["updates"][1]["type"], "set");
        assert_eq!(
            update["draft"]["updates"][1]["part"]["annotations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut relocated = after.clone();
        relocated.revision += 1;
        relocated.draft.swap(0, 1);
        let update = delta(&after, &relocated);
        assert!(
            update["draft"]["updates"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] == "set")
        );
        let mut aborted = relocated.clone();
        aborted.revision += 1;
        aborted.draft.clear();
        aborted.notices.push(Notice::new(
            "aborted".into(),
            crate::harness::NoticeKind::Session,
        ));
        aborted.state = RunState::Idle;
        let update = delta(&relocated, &aborted);
        assert_eq!(update["draft"]["len"], 0);
        assert_eq!(update["notices"][0]["text"], "aborted");
        assert_eq!(update["notices"][0]["id"], aborted.notices[0].id);
    }

    #[test]
    fn snapshot_projects_turns_and_updates_existing_turn() {
        let mut before = view();
        Arc::make_mut(&mut before.session)
            .append_turn(Turn::assistant_turn(vec![Message::text("result")]));
        let snapshot = serde_json::to_value(SessionSnapshot::from(&before)).unwrap();
        assert!(snapshot["session"]["turns"][0].get("meta").is_none());
        let mut after = before.clone();
        after.revision += 1;
        Arc::make_mut(&mut after.session).turns[0].worked_ms = Some(500);
        let update = delta(&before, &after);
        assert_eq!(update["turns"]["from"], 0);
        assert_eq!(update["turns"]["items"][0]["worked_ms"], 500);
    }

    #[test]
    fn synthetic_stream_bandwidth() {
        let mut current = view();
        Arc::make_mut(&mut current.session)
            .append_turn(Turn::user_turn(Message::text("h".repeat(34_000))));
        let mut full_bytes = 0;
        let mut delta_bytes = 0;
        for _ in 0..30 {
            let before = current.clone();
            current.revision += 1;
            if let ContentPart::Text { text, .. } = Arc::make_mut(&mut current.draft[0]) {
                text.push_str("abcdefghij");
            }
            full_bytes += serde_json::to_vec(&SessionSnapshot::from(&current))
                .unwrap()
                .len();
            delta_bytes += serde_json::to_vec(&SessionDelta::between(&before, &current))
                .unwrap()
                .len();
        }
        eprintln!("30 synthetic frames: full={full_bytes} bytes, delta={delta_bytes} bytes");
        assert!(delta_bytes * 100 < full_bytes);
    }
}
