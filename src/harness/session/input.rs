use super::*;

impl SessionTask {
    pub(super) async fn queue_input(
        &mut self,
        prompt: String,
        attachments: Vec<ContentPart>,
    ) -> Result<(), String> {
        if !attachments.is_empty() {
            let engine = self.build_engine().await?;
            task::validate_attachments(&engine, &attachments)?;
        }
        Arc::make_mut(&mut self.view.pending_inputs).push(PendingInput {
            id: uuid::Uuid::new_v4().to_string(),
            prompt,
            attachments,
            mode: InputMode::After,
        });
        self.publish();
        Ok(())
    }

    pub(super) fn set_input_mode(&mut self, id: &str, mode: InputMode) -> Result<(), String> {
        let input = Arc::make_mut(&mut self.view.pending_inputs)
            .iter_mut()
            .find(|input| input.id == id)
            .ok_or("消息已发送或已取回")?;
        input.mode = mode;
        self.publish();
        Ok(())
    }

    pub(super) fn take_input(&mut self, id: &str) -> Result<PendingInput, String> {
        let index = self
            .view
            .pending_inputs
            .iter()
            .position(|input| input.id == id)
            .ok_or("消息已发送或已取回")?;
        let input = Arc::make_mut(&mut self.view.pending_inputs).remove(index);
        self.publish();
        Ok(input)
    }

    async fn start_pending_input(
        &mut self,
        index: usize,
        steering: bool,
    ) -> Result<AgentEngine, String> {
        let input = Arc::make_mut(&mut self.view.pending_inputs).remove(index);
        let result = self
            .append_input(&input.prompt, input.attachments.clone(), steering)
            .await;
        if result.is_err() {
            Arc::make_mut(&mut self.view.pending_inputs).insert(index, input);
            self.publish();
        }
        result
    }

    pub(super) async fn drain_steering(&mut self) -> Result<bool, String> {
        let mut consumed = false;
        while let Some(index) = self
            .view
            .pending_inputs
            .iter()
            .position(|input| input.mode == InputMode::Steer)
        {
            self.start_pending_input(index, true).await?;
            consumed = true;
        }
        Ok(consumed)
    }

    /// 正常收尾后逐条执行待发消息；停止和错误暂停消费，内容仍可取回。
    pub(super) async fn run_pending_inputs(
        &mut self,
        commands: &mut mpsc::Receiver<Command>,
    ) -> bool {
        while self.auto_resume && !self.view.pending_inputs.is_empty() {
            let engine = match self.start_pending_input(0, false).await {
                Ok(engine) => engine,
                Err(error) => {
                    self.auto_resume = false;
                    self.event(AgentEvent::Error(error)).await;
                    return false;
                }
            };
            let result = self.run_turns(engine, commands).await;
            if self.finish_turn(result).await {
                return true;
            }
        }
        false
    }
}
