use super::App;

impl App {
    /// 更新当前对话流的可视区与最大可滚动行数，自适应维持滚动位置
    pub fn update_viewport(&mut self, max_scroll: usize, viewport_height: usize) {
        self.max_scroll = max_scroll;
        self.viewport_height = viewport_height;
        if self.auto_scroll {
            self.scroll_offset = max_scroll;
        } else {
            self.scroll_offset = self.scroll_offset.min(max_scroll);
        }
    }

    /// 向上滚动（查看更早历史消息）
    pub fn scroll_up(&mut self, delta: usize) {
        if self.max_scroll == 0 {
            return;
        }
        if self.auto_scroll {
            self.auto_scroll = false;
            self.scroll_offset = self.max_scroll.saturating_sub(delta);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(delta);
        }
    }

    /// 向下滚动（接近最新消息），若到达底部则恢复自动吸底模式
    pub fn scroll_down(&mut self, delta: usize) {
        if self.max_scroll == 0 {
            return;
        }
        if !self.auto_scroll {
            let next_offset = self.scroll_offset.saturating_add(delta);
            if next_offset >= self.max_scroll {
                self.scroll_offset = self.max_scroll;
                self.auto_scroll = true;
            } else {
                self.scroll_offset = next_offset;
            }
        }
    }

    /// 一键滚动到会话顶部
    pub fn scroll_to_top(&mut self) {
        if self.max_scroll > 0 {
            self.auto_scroll = false;
            self.scroll_offset = 0;
        }
    }

    /// 一键滚动到底部并开启自动吸底
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll;
        self.auto_scroll = true;
    }

    /// 根据比例 (0.0 ..= 1.0) 精确设置滚动条与视口偏移量（用于滚动条点击或平滑拖拽）
    pub fn scroll_to_ratio(&mut self, ratio: f64) {
        if self.max_scroll == 0 {
            return;
        }
        let clamped = ratio.clamp(0.0, 1.0);
        let target = (clamped * self.max_scroll as f64).round() as usize;
        if target >= self.max_scroll {
            self.scroll_offset = self.max_scroll;
            self.auto_scroll = true;
        } else {
            self.scroll_offset = target;
            self.auto_scroll = false;
        }
    }
}
