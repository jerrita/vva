use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

use vva_core::harness::Notice;
use vva_core::harness::NoticeKind;

#[derive(Debug, Clone, Copy)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug)]
pub struct Toast {
    pub text: String,
    pub level: ToastLevel,
    notice_id: Option<String>,
}

#[derive(Debug)]
pub struct Toasts {
    queue: VecDeque<Toast>,
    shown_at: Instant,
}

impl Default for Toasts {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            shown_at: Instant::now(),
        }
    }
}

impl Toasts {
    pub fn current(&self) -> Option<&Toast> {
        self.queue.front()
    }

    pub fn pending(&self) -> usize {
        self.queue.len().saturating_sub(1)
    }

    pub fn show(&mut self, text: String, level: ToastLevel) {
        if self.queue.is_empty() {
            self.shown_at = Instant::now();
        }
        self.queue.push_back(Toast {
            text,
            level,
            notice_id: None,
        });
    }

    pub fn error(&mut self, text: String) {
        self.show(text, ToastLevel::Error);
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }

    /// 只投递新增或变化的横幅；自动消失和 Esc 不改会话快照。
    pub fn sync_notices(&mut self, previous: &[Notice], current: &[Notice]) {
        for notice in previous.iter().filter(|notice| notice.is_banner()) {
            if !current.contains(notice)
                && let Some(index) = self
                    .queue
                    .iter()
                    .position(|toast| toast.notice_id.as_ref() == Some(&notice.id))
            {
                self.queue.remove(index);
                if index == 0 {
                    self.shown_at = Instant::now();
                }
            }
        }
        for notice in current
            .iter()
            .filter(|notice| notice.is_banner() && !previous.contains(notice))
        {
            let level = if matches!(notice.kind, NoticeKind::Capability) {
                ToastLevel::Warning
            } else {
                ToastLevel::Info
            };
            if self.queue.is_empty() {
                self.shown_at = Instant::now();
            }
            self.queue.push_back(Toast {
                text: notice.text.clone(),
                level,
                notice_id: Some(notice.id.clone()),
            });
        }
    }

    /// 每条通知从成为可见项起计时，空闲时也自动消失。
    pub fn expire(&mut self) -> bool {
        if !self.queue.is_empty() && self.shown_at.elapsed() >= Duration::from_secs(5) {
            self.queue.pop_front();
            self.shown_at = Instant::now();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn queued_toast_gets_its_own_five_seconds() {
        let mut toasts = Toasts::default();
        toasts.error("First error".into());
        tokio::time::advance(Duration::from_secs(4)).await;
        toasts.error("Queued error".into());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(toasts.expire());
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(!toasts.expire());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(toasts.expire());
    }
}
