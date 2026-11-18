#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
}

pub fn find_sse_delimiter(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            if i + 1 < buf.len() && buf[i + 1] == b'\n' {
                return Some((i, 2));
            }
        } else if buf[i] == b'\r' {
            if i + 3 < buf.len()
                && buf[i + 1] == b'\n'
                && buf[i + 2] == b'\r'
                && buf[i + 3] == b'\n'
            {
                return Some((i, 4));
            } else if i + 1 < buf.len() && buf[i + 1] == b'\r' {
                return Some((i, 2));
            }
        }
        i += 1;
    }
    None
}

pub fn parse_sse_frame(raw: &str) -> Option<SseFrame> {
    let mut id = None;
    let mut event = None;
    let mut data = String::new();
    let mut has_data = false;

    for line in raw.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() || trimmed.starts_with(':') {
            continue;
        }

        if let Some((field, val)) = trimmed.split_once(':') {
            let val = val.strip_prefix(' ').unwrap_or(val);
            match field.trim() {
                "id" => id = Some(val.to_string()),
                "event" => event = Some(val.to_string()),
                "data" => {
                    if has_data {
                        data.push('\n');
                    }
                    data.push_str(val);
                    has_data = true;
                }
                _ => {}
            }
        }
    }

    if id.is_none() && event.is_none() && !has_data {
        None
    } else {
        Some(SseFrame { id, event, data })
    }
}

pub fn extract_sse_frame(buf: &mut Vec<u8>) -> Option<SseFrame> {
    while let Some((pos, delim_len)) = find_sse_delimiter(buf) {
        let frame_bytes = buf[..pos].to_vec();
        buf.drain(..pos + delim_len);
        let frame_str = String::from_utf8_lossy(&frame_bytes);
        if let Some(frame) = parse_sse_frame(&frame_str) {
            return Some(frame);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_across_chunk_boundaries() {
        let mut buf = Vec::new();
        // Chunk 1: partial frame
        buf.extend_from_slice(b": keep-alive\n\nid: 1\nev");
        let frame1 = extract_sse_frame(&mut buf);
        // First was keep-alive (skipped), second is partial, so frame1 is None
        assert_eq!(frame1, None);

        // Chunk 2: rest of frame 1
        buf.extend_from_slice(b"ent: StepStarted\ndata: {\"step\":\"a\"}\n\n");
        let frame2 = extract_sse_frame(&mut buf);
        assert_eq!(
            frame2,
            Some(SseFrame {
                id: Some("1".to_string()),
                event: Some("StepStarted".to_string()),
                data: "{\"step\":\"a\"}".to_string(),
            })
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn skips_keep_alive_comments() {
        let mut buf = b": keep-alive\n\n: 15s ping\n\n".to_vec();
        assert_eq!(extract_sse_frame(&mut buf), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn handles_crlf_delimiters() {
        let mut buf = b"id: 42\r\nevent: ExecutionCompleted\r\ndata: ok\r\n\r\n".to_vec();
        let frame = extract_sse_frame(&mut buf).unwrap();
        assert_eq!(frame.id.as_deref(), Some("42"));
        assert_eq!(frame.event.as_deref(), Some("ExecutionCompleted"));
        assert_eq!(frame.data, "ok");
    }
}
