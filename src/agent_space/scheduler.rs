//! Scheduler：定时任务的定义、状态与计时。
//!
//! 任务定义在 `<agent_dir>/schedule.json`（工具与命令写入，人手修改亦按 mtime 热重载），
//! 运行状态（启停、上次执行、连续失败、interval 的下次时刻）在 `schedule_state.json`。
//! 触发类型三选一：`cron`（5 字段，分 时 日 月 周，本地时区）、`at`（一次性时刻）、
//! `every`（`30m` / `2h` / `1d`，从上次**完成**起算，天然不重叠）。
//! 到点只产出 [`Event::Fire`]，投递由 daemon 执行；停机期间错过的任务启动时以
//! [`Event::Missed`] 上报，不补跑。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::parser::strip_jsonc_comments;

pub const SCHEDULE_FILE: &str = "schedule.json";
const STATE_FILE: &str = "schedule_state.json";
/// 错过任务只回溯这么久，避免长期停机后逐分钟扫描过久
const MAX_LOOKBACK: ChronoDuration = ChronoDuration::days(7);
/// 列表中计算 cron 下次触发时向后查找的上限
const MAX_LOOKAHEAD: ChronoDuration = ChronoDuration::days(400);
const TICK: Duration = Duration::from_secs(20);

// ================== cron ==================

/// 5 字段 cron，每字段是取值位集
#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    day_any: bool,
    weekday_any: bool,
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return Err(format!("cron `{expr}` 须为 5 个字段（分 时 日 月 周）"));
        };
        let mut weekdays = parse_field(weekday, 0, 7)?;
        // 7 与 0 都表示周日
        weekdays = (weekdays | (weekdays >> 7 & 1)) & 0x7f;
        Ok(Self {
            minutes: parse_field(minute, 0, 59)?,
            hours: parse_field(hour, 0, 23)?,
            days: parse_field(day, 1, 31)?,
            months: parse_field(month, 1, 12)?,
            weekdays,
            day_any: day == "*",
            weekday_any: weekday == "*",
        })
    }

    /// 分钟精度匹配；日与周都受限时按标准 cron 取并集
    pub fn matches(&self, t: &NaiveDateTime) -> bool {
        let bit = |mask: u64, v: u32| mask >> v & 1 == 1;
        let day = bit(self.days, t.day());
        let weekday = bit(self.weekdays, t.weekday().num_days_from_sunday());
        let day_ok = if self.day_any || self.weekday_any {
            day && weekday
        } else {
            day || weekday
        };
        bit(self.minutes, t.minute())
            && bit(self.hours, t.hour())
            && bit(self.months, t.month())
            && day_ok
    }
}

fn parse_field(field: &str, min: u32, max: u32) -> Result<u64, String> {
    let num = |s: &str| -> Result<u32, String> {
        let n: u32 = s.parse().map_err(|_| format!("非法 cron 取值 `{s}`"))?;
        if (min..=max).contains(&n) {
            Ok(n)
        } else {
            Err(format!("cron 取值 {n} 超出范围 {min}-{max}"))
        }
    };
    let mut mask = 0u64;
    for item in field.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => (r, Some(num_step(s)?)),
            None => (item, None),
        };
        let (lo, hi) = match range.split_once('-') {
            _ if range == "*" => (min, max),
            Some((a, b)) => (num(a)?, num(b)?),
            // `n/step` 表示从 n 起到上限
            None if step.is_some() => (num(range)?, max),
            None => (num(range)?, num(range)?),
        };
        if lo > hi {
            return Err(format!("cron 范围 `{item}` 起点大于终点"));
        }
        mask |= (lo..=hi)
            .step_by(step.unwrap_or(1) as usize)
            .fold(0, |m, v| m | 1 << v);
    }
    Ok(mask)
}

fn num_step(s: &str) -> Result<u32, String> {
    s.parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("非法 cron 步长 `{s}`"))
}

// ================== 任务定义 ==================

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Cron { expr: String, cron: Cron },
    Once(NaiveDateTime),
    Interval { minutes: u32 },
}

const AT_FORMATS: [&str; 2] = ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"];

fn parse_every(s: &str) -> Result<u32, String> {
    let bad = || format!("every `{s}` 非法，示例: 30m / 2h / 1d");
    let unit = s.chars().last().ok_or_else(bad)?;
    let n: u32 = s[..s.len() - 1].parse().map_err(|_| bad())?;
    let minutes = match unit {
        'm' => n,
        'h' => n.checked_mul(60).ok_or_else(bad)?,
        'd' => n.checked_mul(1440).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    if minutes == 0 {
        return Err(bad());
    }
    Ok(minutes)
}

fn format_every(minutes: u32) -> String {
    match minutes {
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

impl Trigger {
    pub fn describe(&self) -> String {
        match self {
            Self::Cron { expr, .. } => format!("cron {expr}"),
            Self::Once(at) => format!("once {}", at.format("%Y-%m-%d %H:%M")),
            Self::Interval { minutes } => format!("every {}", format_every(*minutes)),
        }
    }

    fn interval(&self) -> Option<ChronoDuration> {
        match self {
            Self::Interval { minutes } => Some(ChronoDuration::minutes(*minutes as i64)),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    every: Option<String>,
    prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bindings: Vec<String>,
}

/// 一项任务。`session` 指定时在该会话内执行、回复发往其来源聊天；否则在 `bindings[0]`
/// 新建 oneshot 会话执行。回复发往 `bindings` 全部聊天（去重，含 session 来源）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawTask", into = "RawTask")]
pub struct Task {
    pub id: String,
    pub trigger: Trigger,
    pub prompt: String,
    pub session: Option<String>,
    pub bindings: Vec<String>,
}

impl TryFrom<RawTask> for Task {
    type Error = String;

    fn try_from(raw: RawTask) -> Result<Self, String> {
        let id = raw.id.trim().to_string();
        if id.is_empty() || id.chars().any(char::is_whitespace) {
            return Err(format!("任务 id `{}` 须非空且不含空白", raw.id));
        }
        let err = |e: String| format!("任务 {id}: {e}");
        let trigger = match (&raw.cron, &raw.at, &raw.every) {
            (Some(expr), None, None) => Trigger::Cron {
                cron: Cron::parse(expr).map_err(err)?,
                expr: expr.clone(),
            },
            (None, Some(at), None) => Trigger::Once(
                AT_FORMATS
                    .iter()
                    .find_map(|f| NaiveDateTime::parse_from_str(at, f).ok())
                    .ok_or_else(|| err(format!("at `{at}` 非法，示例: 2026-10-11T09:00")))?,
            ),
            (None, None, Some(every)) => Trigger::Interval {
                minutes: parse_every(every).map_err(err)?,
            },
            _ => return Err(err("须且只能指定 cron / at / every 之一".into())),
        };
        if raw.prompt.trim().is_empty() {
            return Err(err("prompt 不能为空".into()));
        }
        if raw.session.is_none() && raw.bindings.is_empty() {
            return Err(err("须指定 session 或 bindings".into()));
        }
        Ok(Self {
            id,
            trigger,
            prompt: raw.prompt,
            session: raw.session,
            bindings: raw.bindings,
        })
    }
}

impl From<Task> for RawTask {
    fn from(task: Task) -> Self {
        let (mut cron, mut at, mut every) = (None, None, None);
        match task.trigger {
            Trigger::Cron { expr, .. } => cron = Some(expr),
            Trigger::Once(t) => at = Some(t.format("%Y-%m-%dT%H:%M").to_string()),
            Trigger::Interval { minutes } => every = Some(format_every(minutes)),
        }
        Self {
            id: task.id,
            cron,
            at,
            every,
            prompt: task.prompt,
            session: task.session,
            bindings: task.bindings,
        }
    }
}

impl Task {
    /// 从工具 / 命令参数构造（与文件格式同一校验）
    pub fn build(
        id: String,
        trigger: (&str, &str),
        prompt: String,
        session: Option<String>,
        bindings: Vec<String>,
    ) -> Result<Self, String> {
        let (kind, value) = trigger;
        let value = Some(value.trim().to_string());
        let (mut cron, mut at, mut every) = (None, None, None);
        match kind {
            "cron" => cron = value,
            "at" => at = value,
            "every" => every = value,
            other => return Err(format!("未知触发类型 `{other}`，应为 cron / at / every")),
        }
        Task::try_from(RawTask {
            id,
            cron,
            at,
            every,
            prompt,
            session,
            bindings,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleFile {
    tasks: Vec<Task>,
}

fn parse_schedule(raw: &str) -> Result<Vec<Task>, String> {
    let file: ScheduleFile =
        serde_json::from_str(&strip_jsonc_comments(raw)).map_err(|e| e.to_string())?;
    for (i, task) in file.tasks.iter().enumerate() {
        if file.tasks[..i].iter().any(|t| t.id == task.id) {
            return Err(format!("任务 id 重复: {}", task.id));
        }
    }
    Ok(file.tasks)
}

// ================== 运行状态 ==================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskState {
    pub enabled: bool,
    pub last_run: Option<NaiveDateTime>,
    pub last_error: Option<String>,
    /// 连续失败次数，成功或 resume 后清零
    pub failures: u32,
    /// interval 的下次触发时刻
    pub next: Option<NaiveDateTime>,
}

impl Default for TaskState {
    fn default() -> Self {
        Self {
            enabled: true,
            last_run: None,
            last_error: None,
            failures: 0,
            next: None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    last_tick: Option<NaiveDateTime>,
    #[serde(default)]
    tasks: BTreeMap<String, TaskState>,
}

/// 执行失败的性质：永久性错误（目标不存在等）立即停用，暂时性错误累计到上限才停用
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    Permanent(String),
    Transient(String),
}

/// 停机期间错过的任务：`last` 为最近一次应触发的时刻
#[derive(Debug, Clone, PartialEq)]
pub struct Missed {
    pub task: Task,
    pub last: NaiveDateTime,
}

/// 一次执行结束后的状态变化，供上层决定如何通知
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub failures: u32,
    pub disabled: bool,
}

// ================== 任务表 ==================

struct Inner {
    tasks: Vec<Task>,
    mtime: Option<SystemTime>,
    state: StateFile,
}

/// 任务定义与状态的唯一持有者；Runtime、Schedule 工具与计时循环共享
pub struct ScheduleStore {
    schedule_path: PathBuf,
    state_path: PathBuf,
    inner: Mutex<Inner>,
}

fn floor_minute(t: NaiveDateTime) -> NaiveDateTime {
    t.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .expect("0 秒 0 纳秒总是合法")
}

fn now_minute() -> NaiveDateTime {
    floor_minute(Local::now().naive_local())
}

fn write_atomic(path: &PathBuf, body: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

fn file_mtime(path: &PathBuf) -> Result<Option<SystemTime>, String> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.modified().ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

impl ScheduleStore {
    /// 状态文件存在但无法解析时显式报错；任务文件无效沿用空任务表并告警
    pub fn open(dir: &std::path::Path) -> Result<Arc<Self>, String> {
        let state_path = dir.join(STATE_FILE);
        let state = match std::fs::read_to_string(&state_path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| format!("解析 {} 失败: {e}", state_path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StateFile::default(),
            Err(e) => return Err(format!("读取 {} 失败: {e}", state_path.display())),
        };
        let store = Arc::new(Self {
            schedule_path: dir.join(SCHEDULE_FILE),
            state_path,
            inner: Mutex::new(Inner {
                tasks: Vec::new(),
                mtime: None,
                state,
            }),
        });
        match store.refresh() {
            Some(Ok(n)) => tracing::info!("定时任务已加载: {n} 项"),
            Some(Err(e)) => tracing::warn!("定时任务文件无效: {e}"),
            None => {}
        }
        Ok(store)
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("schedule store")
    }

    fn save_state(&self, inner: &Inner) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&inner.state).map_err(|e| e.to_string())?;
        write_atomic(&self.state_path, &json)
    }

    fn save_tasks(&self, inner: &mut Inner) -> Result<(), String> {
        #[derive(Serialize)]
        struct Out<'a> {
            tasks: &'a [Task],
        }
        let json = serde_json::to_string_pretty(&Out {
            tasks: &inner.tasks,
        })
        .map_err(|e| e.to_string())?;
        write_atomic(&self.schedule_path, &json)?;
        // 自己的写入不触发热重载
        inner.mtime = file_mtime(&self.schedule_path)?;
        Ok(())
    }

    /// 新任务 / 重载后补齐 interval 的下次时刻，并清理已删除任务的状态
    fn reconcile_state(&self, inner: &mut Inner, now: NaiveDateTime) {
        let ids: Vec<String> = inner.tasks.iter().map(|t| t.id.clone()).collect();
        inner.state.tasks.retain(|id, _| ids.contains(id));
        for task in &inner.tasks {
            let state = inner.state.tasks.entry(task.id.clone()).or_default();
            if let Some(every) = task.trigger.interval()
                && state.next.is_none()
                && state.enabled
            {
                state.next = Some(now + every);
            }
        }
        if let Err(e) = self.save_state(inner) {
            tracing::warn!("{e}");
        }
    }

    /// mtime 变化时重新解析。发生重载返回 `Some`；解析失败保留旧任务表，
    /// 同一 mtime 不重复报错
    pub fn refresh(&self) -> Option<Result<usize, String>> {
        let mut inner = self.inner();
        let mtime = match file_mtime(&self.schedule_path) {
            Ok(m) => m,
            Err(e) => return Some(Err(e)),
        };
        if mtime == inner.mtime {
            return None;
        }
        inner.mtime = mtime;
        let loaded = if mtime.is_none() {
            Ok(Vec::new())
        } else {
            std::fs::read_to_string(&self.schedule_path)
                .map_err(|e| format!("读取 {} 失败: {e}", self.schedule_path.display()))
                .and_then(|raw| parse_schedule(&raw))
                .map_err(|e| format!("{}: {e}", self.schedule_path.display()))
        };
        Some(loaded.map(|tasks| {
            inner.tasks = tasks;
            self.reconcile_state(&mut inner, now_minute());
            inner.tasks.len()
        }))
    }

    pub fn list(&self) -> Vec<(Task, TaskState)> {
        let inner = self.inner();
        inner
            .tasks
            .iter()
            .map(|t| {
                (
                    t.clone(),
                    inner.state.tasks.get(&t.id).cloned().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// 精确匹配优先，否则唯一前缀
    fn resolve(inner: &Inner, token: &str) -> Result<usize, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("任务 id 不能为空".into());
        }
        if let Some(i) = inner.tasks.iter().position(|t| t.id == token) {
            return Ok(i);
        }
        let hits: Vec<usize> = (0..inner.tasks.len())
            .filter(|&i| inner.tasks[i].id.starts_with(token))
            .collect();
        match hits[..] {
            [i] => Ok(i),
            [] => Err(format!("任务不存在: {token}")),
            _ => Err(format!(
                "任务 id 前缀 `{token}` 匹配多个: {}",
                hits.iter()
                    .map(|&i| inner.tasks[i].id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    pub fn add(&self, task: Task) -> Result<(), String> {
        let mut inner = self.inner();
        if inner.tasks.iter().any(|t| t.id == task.id) {
            return Err(format!("任务 id 已存在: {}", task.id));
        }
        if let Trigger::Once(at) = task.trigger
            && at <= now_minute()
        {
            return Err(format!(
                "一次性任务时间已过: {}",
                at.format("%Y-%m-%d %H:%M")
            ));
        }
        inner.tasks.push(task);
        if let Err(e) = self.save_tasks(&mut inner) {
            inner.tasks.pop();
            return Err(e);
        }
        self.reconcile_state(&mut inner, now_minute());
        Ok(())
    }

    pub fn remove(&self, token: &str) -> Result<Task, String> {
        let mut inner = self.inner();
        let i = Self::resolve(&inner, token)?;
        let task = inner.tasks.remove(i);
        if let Err(e) = self.save_tasks(&mut inner) {
            inner.tasks.insert(i, task);
            return Err(e);
        }
        self.reconcile_state(&mut inner, now_minute());
        Ok(task)
    }

    /// 暂停 / 恢复。恢复会清零失败计数；已执行或已过期的一次性任务不可恢复
    pub fn set_enabled(&self, token: &str, enabled: bool) -> Result<Task, String> {
        let mut inner = self.inner();
        let i = Self::resolve(&inner, token)?;
        let task = inner.tasks[i].clone();
        let now = now_minute();
        let state = inner.state.tasks.entry(task.id.clone()).or_default();
        if enabled {
            if let Trigger::Once(at) = task.trigger
                && (state.last_run.is_some() || at <= now)
            {
                return Err("一次性任务已执行或已过期，请新建任务".into());
            }
            state.failures = 0;
            state.last_error = None;
            state.next = task.trigger.interval().map(|every| now + every);
        }
        state.enabled = enabled;
        self.save_state(&inner)?;
        Ok(task)
    }

    pub fn get(&self, token: &str) -> Result<Task, String> {
        let inner = self.inner();
        Self::resolve(&inner, token).map(|i| inner.tasks[i].clone())
    }

    /// 在该分钟到点的已启用任务
    pub fn due(&self, minute: NaiveDateTime) -> Vec<Task> {
        let inner = self.inner();
        inner
            .tasks
            .iter()
            .filter(|t| {
                let state = inner.state.tasks.get(&t.id).cloned().unwrap_or_default();
                state.enabled && Self::is_due(t, &state, minute)
            })
            .cloned()
            .collect()
    }

    fn is_due(task: &Task, state: &TaskState, minute: NaiveDateTime) -> bool {
        match &task.trigger {
            Trigger::Cron { cron, .. } => cron.matches(&minute),
            Trigger::Once(at) => state.last_run.is_none() && *at == minute,
            Trigger::Interval { .. } => state.next.is_some_and(|n| n <= minute),
        }
    }

    /// 任务下次触发时刻（仅用于展示）
    pub fn next_fire(task: &Task, state: &TaskState, from: NaiveDateTime) -> Option<NaiveDateTime> {
        if !state.enabled {
            return None;
        }
        match &task.trigger {
            Trigger::Cron { cron, .. } => {
                let mut t = floor_minute(from) + ChronoDuration::minutes(1);
                let end = from + MAX_LOOKAHEAD;
                while t <= end {
                    if cron.matches(&t) {
                        return Some(t);
                    }
                    t += ChronoDuration::minutes(1);
                }
                None
            }
            Trigger::Once(at) => (state.last_run.is_none() && *at > from).then_some(*at),
            Trigger::Interval { .. } => state.next,
        }
    }

    /// 一次执行结束：更新状态并返回结果供上层通知。任务已被删除则返回 `None`
    pub fn settle(
        &self,
        id: &str,
        outcome: Result<(), Failure>,
        max_failures: u32,
    ) -> Option<Settled> {
        let mut inner = self.inner();
        let task = inner.tasks.iter().find(|t| t.id == id)?.clone();
        let now = now_minute();
        let state = inner.state.tasks.entry(id.to_string()).or_default();
        state.last_run = Some(now);
        state.next = task.trigger.interval().map(|every| now + every);
        let mut disabled = matches!(task.trigger, Trigger::Once(_));
        match outcome {
            Ok(()) => {
                state.failures = 0;
                state.last_error = None;
            }
            Err(failure) => {
                state.failures += 1;
                let (Failure::Permanent(msg) | Failure::Transient(msg)) = &failure;
                state.last_error = Some(msg.clone());
                disabled |= matches!(failure, Failure::Permanent(_))
                    || state.failures >= max_failures.max(1);
            }
        }
        if disabled {
            state.enabled = false;
        }
        let settled = Settled {
            failures: state.failures,
            disabled,
        };
        if let Err(e) = self.save_state(&inner) {
            tracing::warn!("{e}");
        }
        Some(settled)
    }

    /// 启动时对账 `(last_tick, now]`：返回错过的任务（不补跑）。
    /// cron 回溯上限 7 天；once 已过未执行；interval 的 `next` 已过期，并把它重排到 `now` 之后
    pub fn reconcile_startup(&self, from: NaiveDateTime, to: NaiveDateTime) -> Vec<Missed> {
        let mut inner = self.inner();
        let from = from.max(to - MAX_LOOKBACK);
        let mut found: Vec<Missed> = Vec::new();
        let tasks = inner.tasks.clone();
        for task in &tasks {
            let state = inner
                .state
                .tasks
                .entry(task.id.clone())
                .or_default()
                .clone();
            if !state.enabled {
                continue;
            }
            let last = match &task.trigger {
                Trigger::Cron { cron, .. } => {
                    let mut t = floor_minute(from) + ChronoDuration::minutes(1);
                    let mut last = None;
                    while t <= to {
                        if cron.matches(&t) {
                            last = Some(t);
                        }
                        t += ChronoDuration::minutes(1);
                    }
                    last
                }
                Trigger::Once(at) => {
                    (state.last_run.is_none() && *at > from && *at <= to).then_some(*at)
                }
                Trigger::Interval { .. } => state.next.filter(|n| *n <= to),
            };
            if let Some(last) = last {
                found.push(Missed {
                    task: task.clone(),
                    last,
                });
            }
            if let Some(every) = task.trigger.interval()
                && last.is_some()
            {
                inner.state.tasks.get_mut(&task.id).expect("state").next = Some(to + every);
            }
        }
        if let Err(e) = self.save_state(&inner) {
            tracing::warn!("{e}");
        }
        found
    }

    pub fn last_tick(&self) -> Option<NaiveDateTime> {
        self.inner().state.last_tick
    }

    pub fn save_tick(&self, minute: NaiveDateTime) {
        let mut inner = self.inner();
        inner.state.last_tick = Some(minute);
        if let Err(e) = self.save_state(&inner) {
            tracing::warn!("{e}");
        }
    }
}

// ================== 展示 ==================

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";

/// 当前本地时间（分钟精度），工具返回给模型用于换算 `at`
pub fn local_now() -> NaiveDateTime {
    now_minute()
}

pub fn format_time(t: NaiveDateTime) -> String {
    t.format(TIME_FORMAT).to_string()
}

/// 任务一览：`id [状态] 触发 -> 目标 | 下次 | 上次 | 错误`，第二行为 prompt 预览
pub fn describe_task(task: &Task, state: &TaskState) -> String {
    let mut targets: Vec<String> = task
        .session
        .iter()
        .map(|s| format!("session {s}"))
        .collect();
    targets.extend(task.bindings.iter().cloned());
    let status = match (state.enabled, &task.trigger) {
        (true, _) => "启用",
        (false, Trigger::Once(_)) if state.last_run.is_some() => "已完成",
        (false, _) => "停用",
    };
    let next = ScheduleStore::next_fire(task, state, now_minute()).map_or("-".into(), format_time);
    let mut line = format!(
        "{} [{status}] {} -> {} | 下次 {next}",
        task.id,
        task.trigger.describe(),
        targets.join(", ")
    );
    if let Some(last) = state.last_run {
        line.push_str(&format!(" | 上次 {}", format_time(last)));
    }
    if let Some(error) = &state.last_error {
        line.push_str(&format!(" | 错误(连续 {} 次): {error}", state.failures));
    }
    let prompt: String = task.prompt.trim().chars().take(60).collect();
    format!("{line}\n  {prompt}")
}

// ================== 运行循环 ==================

/// Scheduler 产出给 daemon 的事件
#[derive(Debug)]
pub enum Event {
    Fire(Task),
    Missed(Vec<Missed>),
}

/// 启动计时循环；调用方持有句柄，退出时 abort
pub fn spawn(
    store: Arc<ScheduleStore>,
    events: mpsc::Sender<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let now = now_minute();
        if let Some(last) = store.last_tick() {
            let missed = store.reconcile_startup(last, now);
            if !missed.is_empty() {
                for m in &missed {
                    tracing::warn!(
                        "停机期间错过定时任务 {}（最近 {}），不补跑",
                        m.task.id,
                        m.last
                    );
                }
                let _ = events.send(Event::Missed(missed)).await;
            }
        }
        let mut last_minute = now;
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            let minute = now_minute();
            if minute == last_minute {
                continue;
            }
            last_minute = minute;
            match store.refresh() {
                Some(Ok(n)) => tracing::info!("定时任务已加载: {n} 项"),
                Some(Err(e)) => tracing::warn!("定时任务文件无效，沿用旧任务表: {e}"),
                None => {}
            }
            for task in store.due(minute) {
                if events.send(Event::Fire(task)).await.is_err() {
                    return;
                }
            }
            store.save_tick(minute);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn cron(expr: &str) -> Cron {
        Cron::parse(expr).unwrap()
    }

    #[test]
    fn cron_fields_match() {
        // 2026-03-02 是周一
        let t = at(2026, 3, 2, 9, 30);
        assert!(cron("* * * * *").matches(&t));
        assert!(cron("30 9 * * *").matches(&t));
        assert!(!cron("31 9 * * *").matches(&t));
        assert!(cron("*/15 8-10 * * *").matches(&t));
        assert!(!cron("*/20 * * * *").matches(&t));
        assert!(cron("0,30 9 2 3 1").matches(&t));
        assert!(cron("5/25 * * * *").matches(&at(2026, 3, 2, 9, 30)));
        assert!(!cron("0 9 * * 1-5").matches(&t));
    }

    #[test]
    fn cron_day_and_weekday_are_or_when_both_restricted() {
        let monday_2nd = at(2026, 3, 2, 0, 0);
        assert!(cron("0 0 15 * 1").matches(&monday_2nd), "周一命中");
        assert!(cron("0 0 2 * 5").matches(&monday_2nd), "2 号命中");
        assert!(!cron("0 0 15 * 5").matches(&monday_2nd));
        // 周日可写 0 或 7
        let sunday = at(2026, 3, 1, 0, 0);
        assert!(cron("0 0 * * 0").matches(&sunday) && cron("0 0 * * 7").matches(&sunday));
    }

    #[test]
    fn cron_rejects_invalid() {
        for bad in [
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "5-1 * * * *",
            "*/0 * * * *",
            "a * * * *",
            "0 0 0 * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }

    fn store(dir: &std::path::Path, body: Option<&str>) -> Arc<ScheduleStore> {
        if let Some(body) = body {
            std::fs::write(dir.join(SCHEDULE_FILE), body).unwrap();
        }
        ScheduleStore::open(dir).unwrap()
    }

    const TWO: &str = r#"{
        // 注释
        "tasks": [
            {"id": "a", "cron": "0 9 * * *", "prompt": "早报", "session": "sess_1"},
            {"id": "b", "every": "30m", "prompt": "喝水", "bindings": ["qq:c2c:u", "qq:group:g"]}
        ]
    }"#;

    fn bump_mtime(path: &std::path::Path) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
    }

    #[test]
    fn loads_three_trigger_kinds_and_multiple_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(
            dir.path(),
            Some(
                r#"{"tasks":[
                {"id":"c","cron":"0 9 * * *","prompt":"p","bindings":["qq:c2c:u"]},
                {"id":"o","at":"2099-01-02T03:04","prompt":"p","bindings":["qq:c2c:u","qq:group:g"]},
                {"id":"i","every":"2h","prompt":"p","session":"sess_1"}]}"#,
            ),
        );
        let tasks = s.list();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[1].0.bindings.len(), 2);
        assert_eq!(tasks[1].0.trigger.describe(), "once 2099-01-02 03:04");
        assert_eq!(tasks[2].0.trigger, Trigger::Interval { minutes: 120 });
        assert!(tasks[2].1.next.is_some(), "interval 补齐下次时刻");
    }

    #[test]
    fn validation_errors() {
        for bad in [
            // 触发类型缺失 / 多个
            r#"{"tasks":[{"id":"a","prompt":"p","session":"s"}]}"#,
            r#"{"tasks":[{"id":"a","cron":"* * * * *","every":"1m","prompt":"p","session":"s"}]}"#,
            // 目标缺失
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":"p"}]}"#,
            // 非法取值
            r#"{"tasks":[{"id":"a","every":"0m","prompt":"p","session":"s"}]}"#,
            r#"{"tasks":[{"id":"a","every":"5s","prompt":"p","session":"s"}]}"#,
            r#"{"tasks":[{"id":"a","at":"明天","prompt":"p","session":"s"}]}"#,
            r#"{"tasks":[{"id":"a","cron":"bad","prompt":"p","session":"s"}]}"#,
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":" ","session":"s"}]}"#,
            // id 重复 / 未知字段
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":"p","session":"s"},{"id":"a","cron":"* * * * *","prompt":"p","session":"s"}]}"#,
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":"p","session":"s","oops":1}]}"#,
        ] {
            assert!(parse_schedule(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hot_reload_keeps_old_tasks_on_error_and_ignores_own_writes() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), Some(TWO));
        assert_eq!(s.list().len(), 2);
        assert_eq!(s.refresh(), None, "mtime 未变不重载");

        // 自己的写入不触发重载
        s.add(
            Task::build(
                "c".into(),
                ("every", "1h"),
                "p".into(),
                None,
                vec!["qq:c2c:u".into()],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(s.refresh(), None);
        assert_eq!(s.list().len(), 3);

        // 写坏：沿用旧表，同一 mtime 只报一次
        let path = dir.path().join(SCHEDULE_FILE);
        std::fs::write(
            &path,
            r#"{"tasks":[{"id":"x","cron":"bad","prompt":"p","session":"s"}]}"#,
        )
        .unwrap();
        bump_mtime(&path);
        assert!(matches!(s.refresh(), Some(Err(_))));
        assert_eq!(s.list().len(), 3);
        assert_eq!(s.refresh(), None);

        // 删除文件视为清空，并清理状态
        std::fs::remove_file(&path).unwrap();
        assert_eq!(s.refresh(), Some(Ok(0)));
    }

    #[test]
    fn add_remove_resume_and_prefix_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), None);
        let task = |id: &str| {
            Task::build(
                id.into(),
                ("every", "1h"),
                "p".into(),
                None,
                vec!["qq:c2c:u".into()],
            )
            .unwrap()
        };
        s.add(task("water1")).unwrap();
        s.add(task("water2")).unwrap();
        assert!(s.add(task("water1")).is_err(), "id 重复");
        assert!(s.get("water").unwrap_err().contains("匹配多个"));
        assert_eq!(s.get("water1").unwrap().id, "water1");

        s.set_enabled("water1", false).unwrap();
        assert!(!s.list()[0].1.enabled);
        assert!(
            s.due(now_minute() + ChronoDuration::days(1))
                .iter()
                .all(|t| t.id != "water1")
        );
        s.set_enabled("water1", true).unwrap();
        assert!(s.list()[0].1.enabled);

        s.remove("water2").unwrap();
        assert_eq!(s.list().len(), 1);
        assert!(s.remove("nope").is_err());

        // 已过时刻的一次性任务拒绝创建
        let past = Task::build(
            "p".into(),
            ("at", "2000-01-01T00:00"),
            "p".into(),
            None,
            vec!["qq:c2c:u".into()],
        )
        .unwrap();
        assert!(s.add(past).unwrap_err().contains("已过"));
    }

    #[test]
    fn settle_success_failure_and_auto_disable() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), Some(TWO));
        // 暂时性失败：累计到上限才停用
        let a = s
            .settle("a", Err(Failure::Transient("网络".into())), 3)
            .unwrap();
        assert_eq!((a.failures, a.disabled), (1, false));
        assert_eq!(s.list()[0].1.last_error.as_deref(), Some("网络"));
        s.settle("a", Err(Failure::Transient("网络".into())), 3);
        let a = s
            .settle("a", Err(Failure::Transient("网络".into())), 3)
            .unwrap();
        assert!(a.disabled && !s.list()[0].1.enabled);
        // resume 清零
        s.set_enabled("a", true).unwrap();
        assert_eq!(s.list()[0].1.failures, 0);
        // 成功清零失败；interval 重排下次
        s.settle("a", Err(Failure::Transient("x".into())), 3);
        s.settle("a", Ok(()), 3);
        assert_eq!(s.list()[0].1.failures, 0);
        assert!(s.list()[0].1.last_error.is_none());
        // 永久性错误立即停用
        let b = s
            .settle("b", Err(Failure::Permanent("会话不存在".into())), 3)
            .unwrap();
        assert!(b.disabled);
        // 已删除的任务忽略
        assert!(s.settle("zzz", Ok(()), 3).is_none());
    }

    #[test]
    fn once_fires_exactly_once_then_disables() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), None);
        let at = now_minute() + ChronoDuration::minutes(5);
        let task = Task::build(
            "o".into(),
            ("at", &at.format("%Y-%m-%dT%H:%M").to_string()),
            "p".into(),
            None,
            vec!["qq:c2c:u".into()],
        )
        .unwrap();
        s.add(task).unwrap();
        assert!(s.due(at - ChronoDuration::minutes(1)).is_empty());
        assert_eq!(s.due(at).len(), 1);
        s.settle("o", Ok(()), 3);
        assert!(s.due(at).is_empty());
        assert!(!s.list()[0].1.enabled);
        assert!(s.set_enabled("o", true).is_err());
    }

    #[test]
    fn interval_is_anchored_on_completion_and_state_persists() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), Some(TWO));
        let first = s.list()[1].1.next.unwrap();
        assert!(s.due(first).iter().any(|t| t.id == "b"));
        s.settle("b", Ok(()), 3);
        let next = s.list()[1].1.next.unwrap();
        assert!(next >= now_minute() + ChronoDuration::minutes(30));
        assert!(
            s.due(next - ChronoDuration::minutes(1))
                .iter()
                .all(|t| t.id != "b")
        );
        // 重启后状态仍在
        let again = ScheduleStore::open(dir.path()).unwrap();
        assert_eq!(again.list()[1].1.next, Some(next));
    }

    #[test]
    fn startup_reports_missed_without_replaying() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(
            dir.path(),
            Some(
                r#"{"tasks":[
                {"id":"a","cron":"0 9 * * *","prompt":"p","bindings":["qq:c2c:u"]},
                {"id":"o","at":"2026-03-02T09:30","prompt":"p","bindings":["qq:c2c:u"]},
                {"id":"i","every":"30m","prompt":"p","bindings":["qq:c2c:u"]}]}"#,
            ),
        );
        // 停机 8:50 → 10:10：cron 错过 9:00；once 错过 9:30；interval 的 next 已重排
        let to = at(2026, 3, 2, 10, 10);
        {
            // interval 的 next 视为停机前排定的 9:20
            let mut inner = s.inner();
            inner.state.tasks.get_mut("i").unwrap().next = Some(at(2026, 3, 2, 9, 20));
        }
        let missed = s.reconcile_startup(at(2026, 3, 2, 8, 50), to);
        let find = |id: &str| missed.iter().find(|m| m.task.id == id).map(|m| m.last);
        assert_eq!(find("a"), Some(at(2026, 3, 2, 9, 0)));
        assert_eq!(find("o"), Some(at(2026, 3, 2, 9, 30)));
        assert_eq!(find("i"), Some(at(2026, 3, 2, 9, 20)));
        assert_eq!(
            s.list()[2].1.next,
            Some(at(2026, 3, 2, 10, 40)),
            "interval 重排而非补跑"
        );
        // 区间为 (from, to]：from 恰是触发时刻不算错过
        assert!(
            s.reconcile_startup(at(2026, 3, 2, 9, 0), at(2026, 3, 2, 9, 20))
                .iter()
                .all(|m| m.task.id != "a")
        );
    }

    #[test]
    fn next_fire_for_listing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), Some(TWO));
        let (task, state) = s.list().remove(0);
        let next = ScheduleStore::next_fire(&task, &state, at(2026, 3, 2, 8, 0)).unwrap();
        assert_eq!(next, at(2026, 3, 2, 9, 0));
        let paused = TaskState {
            enabled: false,
            ..state
        };
        assert!(ScheduleStore::next_fire(&task, &paused, at(2026, 3, 2, 8, 0)).is_none());
    }
}
