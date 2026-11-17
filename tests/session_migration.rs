use serde_json::json;
use std::{fs, io::Write};
use vva::app::{
    migration::migrate,
    records::{Application, SessionKind},
};
use vva_core::{model::*, storage::SessionStore as _};

#[tokio::test]
async fn v2_migration_preserves_native_history_and_keeps_source_unchanged_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("v2");
    let destination = root.path().join("v3");
    let project = source.join("legacy-project");
    fs::create_dir_all(project.join("scratch")).unwrap();
    fs::create_dir_all(project.join("tool_outputs")).unwrap();
    let scratch = project.join("scratch/attachment.pdf");
    let output = project.join("tool_outputs/full.txt");
    fs::write(&scratch, b"%PDF-1.4 retained").unwrap();
    fs::write(&output, b"retained output").unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut response = Turn::assistant_turn(vec![Message::text(format!(
        "{} {}",
        scratch.display(),
        output.display()
    ))]);
    let mut meta = ResponseTurnMeta::new("resp_chain");
    meta.previous_response_id = Some("resp_parent".into());
    meta.reasoning_effort = Some("high".into());
    meta.scope = Some("responses".into());
    meta.output_items = vec![ResponseOutputItem::Reasoning {
        id: "rs_1".into(),
        summary: Vec::new(),
        content: Vec::new(),
        encrypted_content: Some("encrypted-reasoning".into()),
        status: Some("completed".into()),
    }];
    response.meta.response = Some(meta);
    let mut anthropic = Turn::assistant_turn(vec![Message::text("signed thinking")]);
    anthropic.meta.anthropic = Some(AnthropicTurnMeta {
        cache_creation_input_tokens: Some(123),
        cache_read_input_tokens: Some(456),
        content_blocks: vec![AnthropicContentBlock::Thinking {
            thinking: "reasoning".into(),
            signature: Some("signed-thought".into()),
        }],
        ..Default::default()
    });
    let mut gemini = Turn::assistant_turn(vec![Message::text("native part")]);
    gemini.meta.gemini = Some(GeminiTurnMeta {
        content_parts: vec![GeminiContentPart {
            text: Some("reasoning".into()),
            thought: Some(true),
            thought_signature: Some("gemini-signature".into()),
            function_call: None,
        }],
        ..Default::default()
    });
    let turns = vec![response, anthropic, gemini];
    let topic = json!({"type":"header","version":2,"id":"sess_topic","title":"Topic","workspace_root":workspace,"created_at":"2026-11-10T00:00:00Z","current_model":"p/m","current_variant":"high","editor":"hashline","web_search":false,"origin":{"channel":"qq","chat_id":"c2c:user"},"routed_by":"router","ttl_secs":43200,"last_activity":"2026-11-10T01:00:00Z","curation_requested":true,"superseded_by":"sess_coding"});
    let mut curator = topic.clone();
    curator["id"] = "sess_curator".into();
    curator["kind"] = "curator".into();
    curator["origin"] = serde_json::Value::Null;
    let coding = json!({"type":"header","version":2,"id":"sess_coding","workspace_root":workspace,"created_at":"2026-11-10T00:00:00Z","current_model":"p/m","editor":"replace","web_search":true,"parent":"sess_topic","continued_from":"sess_topic"});
    let mut original = Vec::new();
    for header in [topic, curator, coding] {
        let path = project.join(format!("{}.jsonl", header["id"].as_str().unwrap()));
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "{header}").unwrap();
        for turn in &turns {
            writeln!(
                file,
                "{}",
                serde_json::to_string(&JsonlRecord::Turn(Box::new(turn.clone()))).unwrap()
            )
            .unwrap();
        }
        original.push((path.clone(), fs::read(path).unwrap()));
    }
    let old = vva::app::store::AppSessionStore::new(&source).unwrap();
    assert!(
        old.list_sessions()
            .await
            .unwrap_err()
            .to_string()
            .contains("vva session migrate")
    );
    let manifest = migrate(&source, &destination, true).await.unwrap();
    assert_eq!(manifest.sessions.len(), 3);
    let core = vva_core::storage::jsonl::JsonlSessionStore::new(&destination).unwrap();
    let app = vva::app::store::AppSessionStore::new(&destination).unwrap();
    for entry in &manifest.sessions {
        let loaded = core.load_session(&entry.session_id).await.unwrap().unwrap();
        assert_eq!(loaded.turns, turns);
        assert_eq!(loaded.header.id, entry.session_id);
        assert_eq!(entry.resources.scratch, project.join("scratch"));
        assert_eq!(entry.resources.tool_outputs, project.join("tool_outputs"));
    }
    let record = app.slot("sess_topic").unwrap().borrow().clone();
    assert_eq!(record.coding.editor, EditorKind::Hashline);
    assert!(!record.coding.web_search);
    let Application::Space(space) = record.application else {
        panic!("expected Space association")
    };
    assert_eq!(space.origin.unwrap().channel, "qq");
    assert_eq!(space.ttl_secs, Some(43200));
    assert!(space.curation_requested);
    assert!(space.last_activity.is_some());
    assert!(
        matches!(&app.slot("sess_curator").unwrap().borrow().application,Application::Space(space) if space.kind==SessionKind::Curator)
    );
    assert!(matches!(
        app.slot("sess_coding").unwrap().borrow().application,
        Application::Coding
    ));
    let successor = core.load_session("sess_coding").await.unwrap().unwrap();
    assert_eq!(successor.header.parent.as_deref(), Some("sess_topic"));
    assert_eq!(
        successor.header.continued_from.as_deref(),
        Some("sess_topic")
    );
    let record_path = app.records.join("sess_coding.json");
    let record_bytes = fs::read(&record_path).unwrap();
    fs::remove_file(&record_path).unwrap();
    assert!(app.load_session("sess_coding").await.is_err());
    fs::write(&record_path, record_bytes).unwrap();
    for (index, broken) in [
        "{invalid JSON".to_string(),
        json!({"type":"header","version":99}).to_string(),
    ]
    .into_iter()
    .enumerate()
    {
        let invalid = project.join("sess_bad.jsonl");
        fs::write(&invalid, &broken).unwrap();
        let target = root.path().join(format!("failed-{index}"));
        assert!(migrate(&source, &target, true).await.is_err());
        assert!(!target.exists());
        assert_eq!(fs::read_to_string(&invalid).unwrap(), broken);
        for (path, bytes) in &original {
            assert_eq!(&fs::read(path).unwrap(), bytes);
        }
        fs::remove_file(invalid).unwrap();
    }
    for (path, bytes) in original {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::read(&scratch).unwrap(), b"%PDF-1.4 retained");
    assert_eq!(fs::read(&output).unwrap(), b"retained output");
    assert!(destination.join("migration-manifest.json").is_file());
}
