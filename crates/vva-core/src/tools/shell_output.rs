//! Shell 输出采集：按调用隔离、有界尾部、合并唤醒；读取端不等待 UI。
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

use crate::model::ShellOutput;

pub const OUTPUT_CAP: usize = 256 * 1024;
pub const OUTPUT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Default)]
pub struct ShellOutputs {
    entries: Mutex<BTreeMap<String, Arc<Mutex<OutputBuffer>>>>,
    dirty: AtomicBool,
    notify: Notify,
}

impl ShellOutputs {
    pub fn capture(self: &Arc<Self>, call_id: &str) -> OutputCapture {
        let buffer = Arc::new(Mutex::new(OutputBuffer::default()));
        self.entries
            .lock()
            .expect("shell outputs lock")
            .insert(call_id.into(), buffer.clone());
        self.wake();
        OutputCapture {
            handle: OutputHandle {
                buffer,
                outputs: self.clone(),
            },
        }
    }

    fn wake(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            self.notify.notify_one();
        }
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    /// 每个脏调用只复制一次尾部；终态 buffer 从采集表释放。
    pub fn drain(&self) -> Vec<(String, Arc<ShellOutput>)> {
        // 先清标记，随后到达的写入会重新唤醒；已在锁内读取的写入由本批覆盖。
        self.dirty.store(false, Ordering::Release);
        let mut updates = Vec::new();
        self.entries
            .lock()
            .expect("shell outputs lock")
            .retain(|id, buffer| {
                let mut buffer = buffer.lock().expect("shell output lock");
                if buffer.dirty {
                    buffer.dirty = false;
                    updates.push((id.clone(), Arc::new(buffer.snapshot())));
                }
                !buffer.finished
            });
        updates
    }
}

#[derive(Clone)]
pub struct OutputHandle {
    buffer: Arc<Mutex<OutputBuffer>>,
    outputs: Arc<ShellOutputs>,
}

impl OutputHandle {
    fn push(&self, text: &str, bytes: usize) {
        let mut buffer = self.buffer.lock().expect("shell output lock");
        if buffer.finished {
            return;
        }
        buffer.total_bytes += bytes as u64;
        if !buffer.carriage_return && !text.contains('\r') {
            // 普通日志直接批量拷贝；先淘汰旧字节，环形缓冲无需扩容到两倍上限。
            let mut skip = text.len().saturating_sub(OUTPUT_CAP);
            while !text.is_char_boundary(skip) {
                skip += 1;
            }
            let text = &text[skip..];
            let mut excess = (buffer.data.len() + text.len()).saturating_sub(OUTPUT_CAP);
            while buffer.data.get(excess).is_some_and(|b| b & 0xc0 == 0x80) {
                excess += 1;
            }
            buffer.data.drain(..excess);
            buffer.start += (excess + skip) as u64;
            buffer.data.extend(text.as_bytes());
        } else {
            for ch in text.chars() {
                match ch {
                    '\r' => {
                        // CR 进度行在下一段文本到达时替换；CRLF 保留原行。
                        buffer.carriage_return = true;
                    }
                    '\n' => {
                        buffer.carriage_return = false;
                        buffer.data.push_back(b'\n');
                    }
                    _ => {
                        if buffer.carriage_return {
                            while buffer.data.back().is_some_and(|b| *b != b'\n') {
                                buffer.data.pop_back();
                            }
                            buffer.carriage_return = false;
                        }
                        let mut encoded = [0; 4];
                        buffer.data.extend(ch.encode_utf8(&mut encoded).as_bytes());
                    }
                }
            }
        }
        if buffer.data.len() > OUTPUT_CAP {
            let mut excess = buffer.data.len() - OUTPUT_CAP;
            while buffer.data.get(excess).is_some_and(|b| b & 0xc0 == 0x80) {
                excess += 1;
            }
            buffer.data.drain(..excess);
            buffer.start += excess as u64;
        }
        buffer.changed();
        drop(buffer);
        self.outputs.wake();
    }

    pub fn snapshot(&self) -> ShellOutput {
        self.buffer.lock().expect("shell output lock").snapshot()
    }

    pub fn finish(&self, error: Option<String>) {
        let mut buffer = self.buffer.lock().expect("shell output lock");
        if buffer.finished {
            return;
        }
        buffer.finished = true;
        buffer.error = error;
        buffer.changed();
        drop(buffer);
        self.outputs.wake();
    }
}

pub struct OutputCapture {
    pub handle: OutputHandle,
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        self.handle.finish(Some("Command stopped".into()));
    }
}

#[derive(Default)]
struct OutputBuffer {
    data: VecDeque<u8>,
    start: u64,
    revision: u64,
    total_bytes: u64,
    finished: bool,
    error: Option<String>,
    dirty: bool,
    carriage_return: bool,
}

impl OutputBuffer {
    fn changed(&mut self) {
        self.revision += 1;
        self.dirty = true;
    }

    fn snapshot(&self) -> ShellOutput {
        let text =
            String::from_utf8(self.data.iter().copied().collect()).expect("normalized UTF-8");
        ShellOutput {
            revision: self.revision,
            start: self.start,
            text,
            total_bytes: self.total_bytes,
            finished: self.finished,
            error: self.error.clone(),
        }
    }
}

/// 每条管道独立维护 UTF-8 和 ANSI 状态，避免 stdout/stderr 的半个字符互相污染。
#[derive(Default)]
pub struct OutputDecoder {
    pending: Vec<u8>,
    escape: AnsiState,
}

#[derive(Default)]
enum AnsiState {
    #[default]
    None,
    Start,
    Csi,
    Osc,
    OscEscape,
}

impl OutputDecoder {
    pub fn push(&mut self, handle: &OutputHandle, bytes: &[u8], eof: bool) {
        self.pending.extend_from_slice(bytes);
        let mut text = String::with_capacity(self.pending.len());
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(valid) => {
                    text.push_str(valid);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&self.pending[consumed..valid_end])
                            .expect("valid prefix"),
                    );
                    consumed = valid_end;
                    if let Some(len) = error.error_len() {
                        text.push('\u{fffd}');
                        consumed += len;
                    } else if eof {
                        text.push('\u{fffd}');
                        consumed = self.pending.len();
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        let mut clean = String::with_capacity(text.len());
        for ch in text.chars() {
            match self.escape {
                AnsiState::None => match ch {
                    '\x1b' => self.escape = AnsiState::Start,
                    '\n' | '\r' | '\t' => clean.push(ch),
                    ch if !ch.is_control() => clean.push(ch),
                    _ => {}
                },
                AnsiState::Start => {
                    self.escape = match ch {
                        '[' => AnsiState::Csi,
                        ']' => AnsiState::Osc,
                        _ => AnsiState::None,
                    }
                }
                AnsiState::Csi => {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        self.escape = AnsiState::None;
                    }
                }
                AnsiState::Osc => match ch {
                    '\x07' => self.escape = AnsiState::None,
                    '\x1b' => self.escape = AnsiState::OscEscape,
                    _ => {}
                },
                AnsiState::OscEscape => {
                    self.escape = if ch == '\\' {
                        AnsiState::None
                    } else {
                        AnsiState::Osc
                    }
                }
            }
        }
        handle.push(&clean, bytes.len());
    }
}

pub async fn read_output(
    mut pipe: impl AsyncRead + Unpin,
    handle: Option<&OutputHandle>,
    mut file: Option<&mut tokio::fs::File>,
    mut consume: impl FnMut(&[u8]) + Send,
) -> Result<(), String> {
    let mut decoder = OutputDecoder::default();
    let mut buf = [0; 8192];
    loop {
        let n = pipe
            .read(&mut buf)
            .await
            .map_err(|e| format!("Failed to read command output: {e}"))?;
        if let Some(handle) = handle {
            decoder.push(handle, &buf[..n], n == 0);
        }
        if n == 0 {
            return Ok(());
        }
        if let Some(file) = file.as_mut() {
            file.write_all(&buf[..n])
                .await
                .map_err(|e| format!("Failed to save command output: {e}"))?;
        }
        consume(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_split_unicode_ansi_osc_and_progress_lines() {
        let outputs = Arc::new(ShellOutputs::default());
        let capture = outputs.capture("synthetic");
        let mut decoder = OutputDecoder::default();
        let bytes =
            "\x1b[31m中文🙂\x1b[0m\r\nprogress 1\rprogress 2\x1b]0;title\x1b\\\n".as_bytes();
        for byte in bytes {
            decoder.push(&capture.handle, &[*byte], false);
        }
        decoder.push(&capture.handle, &[], true);
        assert_eq!(capture.handle.snapshot().text, "中文🙂\nprogress 2\n");
        assert_eq!(capture.handle.snapshot().total_bytes, bytes.len() as u64);
    }

    #[test]
    fn tail_is_utf8_safe_bounded_and_terminal_capture_is_released() {
        let outputs = Arc::new(ShellOutputs::default());
        let capture = outputs.capture("synthetic");
        let mut decoder = OutputDecoder::default();
        let data = "界🙂".repeat(OUTPUT_CAP / 2);
        for chunk in data.as_bytes().chunks(8192) {
            decoder.push(&capture.handle, chunk, false);
        }
        decoder.push(&capture.handle, &[], true);
        let snapshot = capture.handle.snapshot();
        assert!(snapshot.text.len() <= OUTPUT_CAP);
        assert!(snapshot.start > 0);
        assert!(data.ends_with(&snapshot.text));
        assert!(!snapshot.text.contains('\u{fffd}'));
        assert_eq!(outputs.drain().len(), 1);
        assert!(outputs.drain().is_empty());
        capture.handle.finish(None);
        assert!(outputs.drain()[0].1.finished);
        assert!(outputs.entries.lock().unwrap().is_empty());
        assert!(outputs.drain().is_empty());
    }

    #[tokio::test]
    async fn reads_without_newlines_and_cancellation_preserves_output() {
        let outputs = Arc::new(ShellOutputs::default());
        let capture = outputs.capture("synthetic");
        let (pipe, mut writer) = tokio::io::duplex(64);
        let handle = capture.handle.clone();
        let reader =
            tokio::spawn(async move { read_output(pipe, Some(&handle), None, |_| {}).await });
        writer.write_all(b"partial").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                outputs.notified().await;
                if outputs
                    .drain()
                    .iter()
                    .any(|(_, output)| output.text == "partial")
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(!reader.is_finished());
        drop(capture);
        let terminal = outputs.drain().pop().unwrap().1;
        assert!(terminal.finished);
        assert_eq!(terminal.text, "partial");
        assert!(terminal.error.is_some());
        reader.abort();
        let _ = reader.await;
    }
}
