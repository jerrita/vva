//! 手动 /compact 与 Agent Space rollover 共用的上下文压缩请求。

use super::task::TurnOutcome;
use super::*;

/// 手动压缩与 rollover 请求使用的压缩 prompt
const COMPRESS_PROMPT: &str = "\
请对到目前为止的整个对话做一次上下文压缩，产出供后续会话承接的摘要。\
该摘要将是新会话中仅有的历史，不会附带任何原始对话，因此必须自足。\
本次回复不得调用任何工具，只输出摘要正文，按以下分节书写（某节没有内容时写“无”）：

## 目标
## 偏好
## 关键事实
## 约束
## 决策
## 待办
## 未决问题
## 当前进展

“当前进展”写明最近一条用户请求的原意、已完成与正在进行的步骤。

要求：只保留继续工作所需的信息；保留具体的路径、标识符、数值与命令；\
不确定或未经验证的信息标注“未确认”；不要编造，不要复述寒暄。";

impl SessionTask {
    /// 在独立请求副本上生成摘要，成功后一次落盘，失败或取消保留原上下文。
    pub(super) async fn compact(
        &mut self,
        engine: AgentEngine,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<TurnOutcome, String> {
        let mut context = self.view.session.context().into_owned();
        context.append_turn(Turn::user_turn(Message::text(COMPRESS_PROMPT)));
        self.text_parts.clear();
        let started = Instant::now();
        let (tx, mut rx) = mpsc::channel(64);
        let definitions = engine.tools.definitions();
        let request =
            engine
                .provider
                .complete_stream(&context, &definitions, &engine.options, Some(tx));
        let Some(response) = self.drive(request, commands, &mut rx).await else {
            return Ok(TurnOutcome::Aborted);
        };
        let response = response.map_err(|error| format!("压缩请求失败: {error}"))?;
        let mut turn = Turn::assistant_turn(response.messages);
        if turn
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .any(|part| {
                matches!(
                    part,
                    ContentPart::ToolCall(_)
                        | ContentPart::WebSearchCall(_)
                        | ContentPart::ImageGenerationCall(_)
                )
            })
        {
            return Err("压缩请求发起了工具调用，视为失败".into());
        }
        let summary = turn.final_text();
        let summary = summary.trim();
        if summary.is_empty() {
            return Err("压缩请求未返回摘要".into());
        }
        turn.messages = vec![Message::text(format!("上下文已压缩。\n\n{summary}"))];
        turn.compaction = true;
        turn.usage = response.usage;
        turn.meta = response.meta;
        turn.elapsed_ms = Some(started.elapsed().as_millis() as u64);
        turn.worked_ms = turn.elapsed_ms;
        let mut session = (*self.view.session).clone();
        session.append_turn(Turn::user_turn(Message::text("/compact")));
        session.append_turn(turn);
        self.store
            .save_header(&session)
            .await
            .map_err(|error| error.to_string())?;
        self.view.draft.clear();
        self.replace_session(session);
        Ok(TurnOutcome::Completed)
    }
}
