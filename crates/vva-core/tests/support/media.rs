pub(crate) fn media_tool_session() -> crate::model::Session {
    use crate::model::*;
    let mut result = ToolResult::success("read_media", "Read sample.png");
    result.attachments.push(MediaContent {
        kind: MediaKind::Image,
        mime_type: "image/png".into(),
        filename: "sample.png".into(),
        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=".into(),
    });
    let mut session = Session::new(".", None);
    session.append_turn(Turn::assistant_turn(vec![Message::new(vec![
        ContentPart::ToolCall(ToolCall::new(
            "read_media",
            "Read",
            r#"{"path":"sample.png"}"#,
        )),
    ])]));
    session.append_turn(Turn::new(Role::Tool, vec![Message::tool_result(result)]));
    session
}
