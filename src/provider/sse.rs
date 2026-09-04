//! SSE (Server-Sent Events) 流解析工具

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
    current_event: Option<String>,
    current_data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解码传入的 chunk 字节流，产生解析完成的 SSE 事件列表
    pub fn decode(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let chunk_str = String::from_utf8_lossy(chunk);
        self.buffer.push_str(&chunk_str);

        let mut events = Vec::new();

        while let Some(pos) = self.buffer.find('\n') {
            let mut line = self.buffer[..pos].to_string();
            self.buffer.drain(..=pos);

            if line.ends_with('\r') {
                line.pop();
            }

            if line.is_empty() {
                // 空行触发单次事件派发
                if !self.current_data.is_empty() {
                    let data = self.current_data.join("\n");
                    events.push(SseEvent {
                        event: self.current_event.take(),
                        data,
                    });
                    self.current_data.clear();
                } else if self.current_event.is_some() {
                    self.current_event = None;
                }
            } else if let Some(stripped) = line.strip_prefix("data:") {
                let val = stripped.strip_prefix(' ').unwrap_or(stripped);
                self.current_data.push(val.to_string());
            } else if let Some(stripped) = line.strip_prefix("event:") {
                let val = stripped.strip_prefix(' ').unwrap_or(stripped);
                self.current_event = Some(val.to_string());
            }
        }

        events
    }

    /// 在流结束时如果存在残余数据，产生最终事件
    pub fn flush(&mut self) -> Option<SseEvent> {
        if !self.current_data.is_empty() {
            let data = self.current_data.join("\n");
            self.current_data.clear();
            Some(SseEvent {
                event: self.current_event.take(),
                data,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_decoder_basic() {
        let mut decoder = SseDecoder::new();
        let payload =
            b"event: message\ndata: {\"text\": \"hello\"}\n\nevent: done\ndata: [DONE]\n\n";
        let events = decoder.decode(payload);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data, "{\"text\": \"hello\"}");
        assert_eq!(events[1].event.as_deref(), Some("done"));
        assert_eq!(events[1].data, "[DONE]");
    }

    #[test]
    fn test_sse_decoder_chunked_input() {
        let mut decoder = SseDecoder::new();
        let p1 = b"data: part1";
        let ev1 = decoder.decode(p1);
        assert!(ev1.is_empty());

        let p2 = b" and part2\n\n";
        let ev2 = decoder.decode(p2);
        assert_eq!(ev2.len(), 1);
        assert_eq!(ev2[0].data, "part1 and part2");
    }

    #[test]
    fn test_sse_decoder_crlf() {
        let mut decoder = SseDecoder::new();
        let payload = b"event: test\r\ndata: hello world\r\n\r\n";
        let events = decoder.decode(payload);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("test"));
        assert_eq!(events[0].data, "hello world");
    }
}
