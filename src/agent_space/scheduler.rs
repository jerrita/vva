//! Scheduler：cron 定时向 session 投递 reminder。
//!
//! 任务定义在 `<agent_dir>/schedule.jsonc`（agent 自己用 Edit/Write 改这个文件即可增删任务），
//! 按 mtime 热重载。cron 为 5 字段标准写法（分 时 日 月 周，本地时区），
//! 仅支持 `*`、数字、列表、范围、步长。到点只产出 [`Event::Fire`]，投递由 daemon 执行；
//! 停机期间错过的任务启动时以 [`Event::Missed`] 上报，不补跑。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::parser::strip_jsonc_comments;

pub const SCHEDULE_FILE: &str = "schedule.jsonc";
const STATE_FILE: &str = "schedule_state.json";
/// 错过任务只回溯这么久，避免长期停机后逐分钟扫描过久
const MAX_LOOKBACK: ChronoDuration = ChronoDuration::days(7);
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

/// 到点后回复送往何处
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// 投递到已有 session，回复发往其 origin binding
    Session(String),
    /// 新建 oneshot session 执行，回复发往该 binding
    Binding(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    id: String,
    cron: String,
    prompt: String,
    session: Option<String>,
    binding: Option<String>,
}

/// `schedule.jsonc` 中的一项任务；`session` 与 `binding` 二选一
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "RawTask")]
pub struct Task {
    pub id: String,
    pub expr: String,
    pub cron: Cron,
    pub prompt: String,
    pub target: Target,
}

impl TryFrom<RawTask> for Task {
    type Error = String;

    fn try_from(raw: RawTask) -> Result<Self, String> {
        let target = match (raw.session, raw.binding) {
            (Some(session), None) => Target::Session(session),
            (None, Some(binding)) => Target::Binding(binding),
            _ => {
                return Err(format!(
                    "任务 {} 须且只能指定 session 或 binding 之一",
                    raw.id
                ));
            }
        };
        let cron = Cron::parse(&raw.cron).map_err(|e| format!("任务 {}: {e}", raw.id))?;
        Ok(Self {
            id: raw.id,
            expr: raw.cron,
            cron,
            prompt: raw.prompt,
            target,
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

/// 停机期间错过的任务：`last` 为最近一次应触发的时刻
#[derive(Debug, Clone, PartialEq)]
pub struct Missed {
    pub task: Task,
    pub last: NaiveDateTime,
}

/// 任务表：按 mtime 热重载
#[derive(Debug)]
pub struct Schedule {
    path: PathBuf,
    mtime: Option<SystemTime>,
    tasks: Vec<Task>,
}

impl Schedule {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            mtime: None,
            tasks: Vec::new(),
        }
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// mtime 变化时重新解析。发生重载返回 `Some`；解析失败保留旧任务表，
    /// 同一 mtime 不重复报错
    pub fn refresh(&mut self) -> Option<Result<usize, String>> {
        let mtime = match std::fs::metadata(&self.path) {
            Ok(meta) => meta.modified().ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Some(Err(format!("读取 {} 失败: {e}", self.path.display()))),
        };
        if mtime == self.mtime {
            return None;
        }
        self.mtime = mtime;
        let Some(_) = mtime else {
            self.tasks.clear();
            return Some(Ok(0));
        };
        let loaded = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("读取 {} 失败: {e}", self.path.display()))
            .and_then(|raw| parse_schedule(&raw))
            .map_err(|e| format!("{}: {e}", self.path.display()));
        Some(loaded.map(|tasks| {
            self.tasks = tasks;
            self.tasks.len()
        }))
    }

    /// 在该分钟到点的任务
    pub fn due(&self, minute: &NaiveDateTime) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.cron.matches(minute))
            .collect()
    }

    /// `(from, to]` 内本应触发却未触发的任务（回溯上限 7 天）
    pub fn missed(&self, from: NaiveDateTime, to: NaiveDateTime) -> Vec<Missed> {
        let from = from.max(to - MAX_LOOKBACK);
        let mut found: Vec<Missed> = Vec::new();
        let mut t = floor_minute(from) + ChronoDuration::minutes(1);
        while t <= to {
            for task in self.due(&t) {
                match found.iter_mut().find(|m| m.task.id == task.id) {
                    Some(m) => m.last = t,
                    None => found.push(Missed {
                        task: task.clone(),
                        last: t,
                    }),
                }
            }
            t += ChronoDuration::minutes(1);
        }
        found
    }
}

fn floor_minute(t: NaiveDateTime) -> NaiveDateTime {
    t.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .expect("0 秒 0 纳秒总是合法")
}

// ================== 运行循环 ==================

/// Scheduler 产出给 daemon 的事件
#[derive(Debug)]
pub enum Event {
    Fire(Task),
    Missed(Vec<Missed>),
}

#[derive(Serialize, Deserialize)]
struct TickState {
    last_tick: NaiveDateTime,
}

fn load_tick(path: &Path) -> Result<Option<NaiveDateTime>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<TickState>(&raw)
            .map(|s| Some(s.last_tick))
            .map_err(|e| format!("解析 {} 失败: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

fn save_tick(path: &Path, last_tick: NaiveDateTime) -> Result<(), String> {
    let json = serde_json::to_string(&TickState { last_tick }).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

fn refresh_logged(schedule: &mut Schedule) {
    match schedule.refresh() {
        Some(Ok(n)) => tracing::info!("定时任务已加载: {n} 项"),
        Some(Err(e)) => tracing::warn!("定时任务文件无效，沿用旧任务表: {e}"),
        None => {}
    }
}

/// 启动调度循环；调用方持有句柄，退出时 abort
pub fn spawn(agent_dir: &Path, events: mpsc::Sender<Event>) -> tokio::task::JoinHandle<()> {
    let mut schedule = Schedule::new(agent_dir.join(SCHEDULE_FILE));
    let state_path = agent_dir.join(STATE_FILE);
    tokio::spawn(async move {
        let now = Local::now().naive_local();
        refresh_logged(&mut schedule);
        match load_tick(&state_path) {
            Ok(Some(last)) => {
                let missed = schedule.missed(last, now);
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
            Ok(None) => {}
            Err(e) => tracing::warn!("{e}"),
        }
        let mut last_minute = floor_minute(now);
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            let minute = floor_minute(Local::now().naive_local());
            if minute == last_minute {
                continue;
            }
            last_minute = minute;
            refresh_logged(&mut schedule);
            for task in schedule.due(&minute) {
                if events.send(Event::Fire(task.clone())).await.is_err() {
                    return;
                }
            }
            if let Err(e) = save_tick(&state_path, minute) {
                tracing::warn!("{e}");
            }
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

    fn write(dir: &Path, body: &str) -> Schedule {
        std::fs::write(dir.join(SCHEDULE_FILE), body).unwrap();
        Schedule::new(dir.join(SCHEDULE_FILE))
    }

    const TWO: &str = r#"{
        // 注释
        "tasks": [
            {"id": "a", "cron": "0 9 * * *", "prompt": "早报", "session": "sess_1"},
            {"id": "b", "cron": "*/30 * * * *", "prompt": "喝水", "binding": "qq:c2c:u"}
        ]
    }"#;

    #[test]
    fn schedule_loads_jsonc_and_hot_reloads_on_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = write(dir.path(), TWO);
        assert_eq!(s.refresh(), Some(Ok(2)));
        assert_eq!(s.refresh(), None, "mtime 未变不重载");
        assert_eq!(s.tasks()[0].target, Target::Session("sess_1".into()));
        assert_eq!(s.tasks()[1].target, Target::Binding("qq:c2c:u".into()));

        let path = dir.path().join(SCHEDULE_FILE);
        std::fs::write(&path, r#"{"tasks": []}"#).unwrap();
        let bumped = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(bumped)
            .unwrap();
        assert_eq!(s.refresh(), Some(Ok(0)));
        assert!(s.tasks().is_empty());

        std::fs::remove_file(&path).unwrap();
        assert_eq!(s.refresh(), Some(Ok(0)), "文件删除视为清空");
    }

    #[test]
    fn invalid_reload_keeps_old_tasks_and_reports_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = write(dir.path(), TWO);
        s.refresh();
        let path = dir.path().join(SCHEDULE_FILE);
        std::fs::write(
            &path,
            r#"{"tasks": [{"id": "x", "cron": "bad", "prompt": "p", "session": "s"}]}"#,
        )
        .unwrap();
        let bumped = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(bumped)
            .unwrap();
        assert!(matches!(s.refresh(), Some(Err(_))));
        assert_eq!(s.tasks().len(), 2);
        assert_eq!(s.refresh(), None);
    }

    #[test]
    fn schedule_validation_errors() {
        for bad in [
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":"p"}]}"#,
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":"p","session":"s","binding":"b"}]}"#,
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":"p","session":"s"},{"id":"a","cron":"* * * * *","prompt":"p","session":"s"}]}"#,
            r#"{"tasks":[{"id":"a","cron":"* * * * *","prompt":"p","session":"s","oops":1}]}"#,
        ] {
            assert!(parse_schedule(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn missed_tasks_are_detected_with_injected_times() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = write(dir.path(), TWO);
        s.refresh();
        // 停机 8:50 → 10:10：错过 9:00（a）与 9:00..10:00 的半点（b，最近 10:00）
        let missed = s.missed(at(2026, 3, 2, 8, 50), at(2026, 3, 2, 10, 10));
        let a = missed.iter().find(|m| m.task.id == "a").unwrap();
        let b = missed.iter().find(|m| m.task.id == "b").unwrap();
        assert_eq!(a.last, at(2026, 3, 2, 9, 0));
        assert_eq!(b.last, at(2026, 3, 2, 10, 0));
        // 区间为 (from, to]：from 恰好是触发时刻不算错过
        assert!(
            s.missed(at(2026, 3, 2, 9, 0), at(2026, 3, 2, 9, 20))
                .is_empty()
        );
        // 回溯有上限
        let long = s.missed(at(2020, 1, 1, 0, 0), at(2026, 3, 2, 10, 10));
        assert_eq!(
            long.iter().find(|m| m.task.id == "a").unwrap().last,
            at(2026, 3, 2, 9, 0)
        );
    }
}
