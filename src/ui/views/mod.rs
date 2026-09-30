pub mod ask_user_modal;
pub mod chat;
pub mod config_modal;
pub mod header;
pub mod help_modal;
pub mod input_area;
pub mod message_modal;
pub mod modal_shell;
pub mod models_modal;
pub mod sessions_modal;
pub mod sidebar;
pub mod skills_modal;
pub mod variants_modal;

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType};

/// 三大面板共用的圆角边框色
pub const PANEL_BORDER: Color = Color::Rgb(78, 82, 92);

pub fn panel_block() -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(PANEL_BORDER))
}

/// 纯前景色样式
pub const fn fg(color: Color) -> Style {
    Style::new().fg(color)
}

/// 加粗前景色样式
pub const fn bold(color: Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}
