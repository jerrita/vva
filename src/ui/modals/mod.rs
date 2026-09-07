pub mod config;
pub mod message;
pub mod models;
pub mod sessions;
pub mod variants;

use crossterm::event::KeyCode;

pub use config::{ConfigItemKind, ConfigModalState, ConfigSettingItem};
pub use message::{ForkTitleState, MESSAGE_ACTIONS, MessageActionState};
pub use models::{ModelItem, ModelsModalState};
pub use sessions::{ProjectScope, SessionsModalState};
pub use variants::{VariantItem, VariantsModalState, active_variant_index};

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

/// 当前活跃的弹窗：状态随变体一同持有，界面开启与状态存在互为同一事实
#[derive(Debug, Clone, Default)]
pub enum ActiveModal {
    #[default]
    None,
    Sessions(SessionsModalState),
    Models(ModelsModalState),
    Variants(VariantsModalState),
    Config(ConfigModalState),
    MessageAction(MessageActionState),
    ForkTitle(ForkTitleState),
    Help,
}

/// 弹窗中确认选中项后请求执行的动作
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModalAction {
    None,
    SwitchSession(String),
    DeleteSession(String),
    SwitchModel(String),
    SwitchVariant(Option<String>),
    OpenModels,
    OpenVariants,
    ToggleWebSearch,
    ToggleThinkingDisplay,
    RevertTurn { turn_index: usize, prompt: String },
    OpenForkTitle { turn_index: usize, prompt: String },
    ConfirmFork { turn_index: usize, title: String },
}

impl ActiveModal {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// 可过滤列表弹窗的访问视图（Help / None 无列表）
    pub(crate) fn as_filterable_mut(&mut self) -> Option<&mut dyn FilterableModal> {
        match self {
            Self::Sessions(state) => Some(state),
            Self::Models(state) => Some(state),
            Self::Variants(state) => Some(state),
            Self::Config(state) => Some(state),
            Self::MessageAction(state) => Some(state),
            Self::None | Self::Help | Self::ForkTitle(_) => None,
        }
    }

    /// 读取当前选中项并转换为待执行动作
    pub(crate) fn activate(&self) -> ModalAction {
        match self {
            Self::Sessions(state) => state
                .selected_session()
                .map(|s| ModalAction::SwitchSession(s.id.clone()))
                .unwrap_or(ModalAction::None),
            Self::Models(state) => state
                .selected_model()
                .map(|m| ModalAction::SwitchModel(m.full_id.clone()))
                .unwrap_or(ModalAction::None),
            Self::Variants(state) => state
                .selected_variant()
                .map(|v| ModalAction::SwitchVariant(v.id.clone()))
                .unwrap_or(ModalAction::None),
            Self::Config(state) => match state.selected_item().map(|item| item.kind) {
                Some(ConfigItemKind::Model) => ModalAction::OpenModels,
                Some(ConfigItemKind::Variant) => ModalAction::OpenVariants,
                Some(ConfigItemKind::WebSearch) => ModalAction::ToggleWebSearch,
                Some(ConfigItemKind::ThinkingDisplay) => ModalAction::ToggleThinkingDisplay,
                None => ModalAction::None,
            },
            Self::MessageAction(state) => {
                if state.is_fork() {
                    ModalAction::OpenForkTitle {
                        turn_index: state.turn_index,
                        prompt: state.prompt.clone(),
                    }
                } else {
                    ModalAction::RevertTurn {
                        turn_index: state.turn_index,
                        prompt: state.prompt.clone(),
                    }
                }
            }
            Self::ForkTitle(state) => ModalAction::ConfirmFork {
                turn_index: state.turn_index,
                title: state.title.text.clone(),
            },
            Self::None | Self::Help => ModalAction::None,
        }
    }

    pub(crate) fn delete_selected(&self) -> ModalAction {
        match self {
            Self::Sessions(state) => state
                .pending_delete()
                .map(|session_id| ModalAction::DeleteSession(session_id.to_string()))
                .unwrap_or(ModalAction::None),
            Self::None
            | Self::Models(_)
            | Self::Variants(_)
            | Self::Config(_)
            | Self::Help
            | Self::MessageAction(_)
            | Self::ForkTitle(_) => ModalAction::None,
        }
    }

    /// 将导航与过滤输入应用到当前弹窗状态
    pub(crate) fn apply_input(&mut self, input: ModalInput) {
        match self {
            Self::Sessions(state) => input.apply(state),
            Self::Models(state) => input.apply(state),
            Self::Variants(state) => input.apply(state),
            Self::Config(state) => input.apply(state),
            Self::MessageAction(state) => input.apply(state),
            Self::None | Self::Help | Self::ForkTitle(_) => {}
        }
    }
}
