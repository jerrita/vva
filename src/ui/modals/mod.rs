pub mod config;
pub mod models;
pub mod sessions;
pub mod variants;

use crossterm::event::KeyCode;

pub use config::{ConfigItemKind, ConfigModalState, ConfigSettingItem};
pub use models::{ModelItem, ModelsModalState};
pub use sessions::{ProjectScope, SessionsModalState};
pub use variants::{VariantItem, VariantsModalState};

pub(crate) fn fuzzy_matches(query: &str, candidate: &str) -> bool {
    let candidate = candidate.to_lowercase();
    let mut candidate = candidate.chars();
    query
        .to_lowercase()
        .chars()
        .all(|query_char| candidate.by_ref().any(|c| c == query_char))
}

/// 可过滤列表模态弹窗的公共交互契约。
/// 实现者提供过滤后条目数量、选中索引与重过滤钩子，
/// 即可获得输入补全、退格与环形上下导航的默认行为。
pub trait FilterableModal {
    /// 过滤后的条目数量
    fn filtered_len(&self) -> usize;

    fn selected_index(&self) -> usize;

    fn selected_index_mut(&mut self) -> &mut usize;

    fn query_mut(&mut self) -> &mut String;

    /// 查询词变化后的重过滤钩子（各模态自行决定选中索引的重置/恢复策略）
    fn refilter(&mut self);

    fn push_query(&mut self, c: char) {
        self.query_mut().push(c);
        self.refilter();
    }

    fn pop_query(&mut self) {
        self.query_mut().pop();
        self.refilter();
    }

    fn next(&mut self) {
        let len = self.filtered_len();
        if len > 0 {
            let idx = self.selected_index_mut();
            *idx = (*idx + 1) % len;
        }
    }

    fn previous(&mut self) {
        let len = self.filtered_len();
        if len > 0 {
            let idx = self.selected_index_mut();
            *idx = if *idx == 0 { len - 1 } else { *idx - 1 };
        }
    }
}

/// 模态弹窗通用按键的归一化语义
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModalInput {
    Close,
    Activate,
    PushQuery(char),
    PopQuery,
    NavigateUp,
    NavigateDown,
    None,
}

/// 将模态内按键归一化为通用语义；j/k 恒为导航键，`reserved` 中的字符不进入过滤词
pub(crate) fn classify_modal_key(code: KeyCode, reserved: &[char]) -> ModalInput {
    match code {
        KeyCode::Esc => ModalInput::Close,
        KeyCode::Backspace => ModalInput::PopQuery,
        KeyCode::Enter => ModalInput::Activate,
        KeyCode::Up => ModalInput::NavigateUp,
        KeyCode::Down => ModalInput::NavigateDown,
        KeyCode::Char('j') => ModalInput::NavigateDown,
        KeyCode::Char('k') => ModalInput::NavigateUp,
        KeyCode::Char(c) if reserved.contains(&c) => ModalInput::None,
        KeyCode::Char(c) => ModalInput::PushQuery(c),
        _ => ModalInput::None,
    }
}

impl ModalInput {
    /// 将导航与过滤词输入应用到模态状态机（Close / Activate 由调用方处理）
    pub(crate) fn apply<M: FilterableModal + ?Sized>(self, modal: &mut M) {
        match self {
            ModalInput::PushQuery(c) => modal.push_query(c),
            ModalInput::PopQuery => modal.pop_query(),
            ModalInput::NavigateUp => modal.previous(),
            ModalInput::NavigateDown => modal.next(),
            _ => {}
        }
    }
}

/// 当前活跃的弹窗视图类型
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ActiveModal {
    #[default]
    None,
    Sessions,
    Models,
    Variants,
    Config,
    Help,
}
