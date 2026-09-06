pub mod chat;
pub mod config_modal;
pub mod header;
pub mod help_modal;
pub mod input_area;
pub mod modal_shell;
pub mod models_modal;
pub mod sessions_modal;
pub mod sidebar;
pub mod variants_modal;

use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType};

/// 三大面板共用的圆角边框色
pub const PANEL_BORDER: Color = Color::Rgb(78, 82, 92);

pub fn panel_block() -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(PANEL_BORDER))
}
