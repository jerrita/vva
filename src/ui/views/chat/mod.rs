//! 聊天流渲染：领域会话视图 → 终端行流水线与交互（选区/滚动/链接）。

mod citations;
mod diff;
mod pipeline;
mod selection;
mod state;
#[cfg(test)]
mod tests;
mod thought;
mod tool;
mod tool_block;
mod user_block;
mod view;

pub(crate) use citations::push_citations_block;
pub(crate) use pipeline::{
    append_collapsed_response, append_md, append_notices, append_part, append_parts,
    collect_tool_results, is_empty_session, part_context, polish_md_line, push_empty_hint,
    thought_body_style, thought_kind, thought_prefix_style,
};
pub(crate) use selection::{apply_selection_highlight, extract_selected_text};
pub(crate) use state::{ChatRender, PartContext, RenderState, ThoughtKind};
pub use view::render_chat;

use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::harness::SessionView;
use crate::model::{
    Annotation, ContentPart, Role, ToolCall, ToolResult, WebSearchAction, WebSearchCall,
};
use crate::ui::app::{
    App, CitationsHit, ResponseHit, TextCoord, TextSelection, ThoughtHit, ToolExpand, ToolHit,
    char_in_column_range,
};
use crate::ui::markdown::{LinkRegion, render_markdown, wrap_spans_to_lines};
use crate::ui::text::truncate;
use crate::ui::views::header::session_title_line;
use crate::ui::views::{PANEL_BORDER, panel_block};
