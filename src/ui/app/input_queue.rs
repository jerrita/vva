use super::App;
use crate::harness::InputMode;

impl App {
    pub fn selected_input_index(&self) -> Option<usize> {
        self.selected_input
            .as_ref()
            .and_then(|id| {
                self.view
                    .pending_inputs
                    .iter()
                    .position(|input| &input.id == id)
            })
            .or_else(|| self.view.pending_inputs.len().checked_sub(1))
    }

    pub fn select_pending_input(&mut self, direction: isize) {
        if let Some(index) = self.selected_input_index() {
            let index = index
                .saturating_add_signed(direction)
                .min(self.view.pending_inputs.len() - 1);
            self.selected_input = Some(self.view.pending_inputs[index].id.clone());
        }
    }

    pub async fn advance_pending_input(&mut self) {
        let Some(mut index) = self.selected_input_index() else {
            return;
        };
        if self.view.pending_inputs[index].mode == InputMode::Steer {
            let Some(previous) = self.view.pending_inputs[..index]
                .iter()
                .rposition(|input| input.mode == InputMode::After)
            else {
                return;
            };
            index = previous;
        }
        let id = self.view.pending_inputs[index].id.clone();
        self.selected_input = Some(id.clone());
        if let Err(error) = self
            .active_session
            .set_input_mode(id, InputMode::Steer)
            .await
        {
            self.feedback.push(error);
        }
        self.refresh_view();
    }

    pub async fn retreat_pending_input(&mut self) {
        let Some(index) = self.selected_input_index() else {
            return;
        };
        let input = &self.view.pending_inputs[index];
        if input.mode == InputMode::Steer {
            if let Err(error) = self
                .active_session
                .set_input_mode(input.id.clone(), InputMode::After)
                .await
            {
                self.feedback.push(error);
            }
        } else {
            let previous = index
                .checked_sub(1)
                .map(|index| self.view.pending_inputs[index].id.clone());
            match self.active_session.take_input(input.id.clone()).await {
                Ok(mut input) => {
                    let text = [input.prompt.as_str(), self.input.as_str()]
                        .into_iter()
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    input.attachments.append(&mut self.input_attachments);
                    self.input_attachments = input.attachments;
                    self.input.set(&text);
                    self.history_index = None;
                    self.selected_input = previous;
                }
                Err(error) => self.feedback.push(error),
            }
        }
        self.refresh_view();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContentPart, MediaSource};
    use crate::ui::test_support::{create_test_app, test_config};

    #[tokio::test]
    async fn pending_selection_promotes_backwards_and_escape_restores_in_submission_order() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut config = test_config();
            let provider = config.providers.get_mut("openai").unwrap();
            provider.api_key = "fixture".into();
            provider.base_url = format!("http://{}", listener.local_addr().unwrap());
            let app = axum::Router::new().fallback(|| async {
                std::future::pending::<()>().await;
                ""
            });
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let mut ta = create_test_app("openai/test_model", None, Some(config), false).await;
            ta.active_session
                .submit("initial request", vec![], None)
                .await
                .unwrap();
            for prompt in ["first item", "second item", "third item"] {
                ta.run(prompt).await;
            }
            assert_eq!(ta.view.pending_inputs.len(), 3);
            let (_keys, mut events) = tokio::sync::mpsc::unbounded_channel();
            for expected in [2, 1, 0] {
                ta.last_key_input_time = Some(std::time::Instant::now());
                crate::ui::events::handle_crossterm_event(
                    &mut ta.app,
                    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Enter,
                        crossterm::event::KeyModifiers::NONE,
                    )),
                    &mut events,
                )
                .await;
                assert_eq!(ta.selected_input_index(), Some(expected));
                assert_eq!(ta.view.pending_inputs[expected].mode, InputMode::Steer);
            }
            ta.advance_pending_input().await;
            assert_eq!(ta.selected_input_index(), Some(0));
            ta.select_pending_input(1);
            ta.retreat_pending_input().await;
            assert_eq!(ta.view.pending_inputs[1].mode, InputMode::After);
            ta.select_pending_input(1);
            ta.retreat_pending_input().await;
            ta.set_input("current draft");
            let attachment = ContentPart::Image {
                source: MediaSource::Base64 {
                    data: "c3ludGhldGlj".into(),
                    mime_type: "image/png".into(),
                },
                mime_type: None,
            };
            ta.input_attachments.push(attachment.clone());
            ta.retreat_pending_input().await;
            assert_eq!(ta.input.as_str(), "third item\n\ncurrent draft");
            ta.retreat_pending_input().await;
            assert_eq!(
                ta.input.as_str(),
                "second item\n\nthird item\n\ncurrent draft"
            );
            ta.retreat_pending_input().await;
            assert_eq!(ta.view.pending_inputs[0].mode, InputMode::After);
            ta.retreat_pending_input().await;
            assert_eq!(
                ta.input.as_str(),
                "first item\n\nsecond item\n\nthird item\n\ncurrent draft"
            );
            assert_eq!(ta.input_attachments, [attachment]);
            assert!(ta.view.pending_inputs.is_empty());
            assert!(ta.active_session.is_running());
            crate::ui::events::handle_crossterm_event(
                &mut ta.app,
                crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE,
                )),
                &mut events,
            )
            .await;
            assert!(!ta.input.is_empty());
            assert!(ta.active_session.is_running());
            ta.active_session.abort_current().await.unwrap();
            server.abort();
        })
        .await
        .expect("TUI pending queue timed out");
    }
}
