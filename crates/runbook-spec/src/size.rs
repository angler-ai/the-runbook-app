use serde_json::Value;

// Count compact JSON UTF-8 bytes without materializing a serialized copy or
// recursing through a caller-owned tree. Child iterators retain bounded depth;
// every container and scalar spends bytes before descending or scanning.
pub(crate) fn measure(root: &Value, limit: usize, max_depth: usize) -> Result<usize, ()> {
    fn charge(left: &mut usize, bytes: usize) -> bool {
        match left.checked_sub(bytes) {
            Some(next) => {
                *left = next;
                true
            }
            None => false,
        }
    }
    fn string(left: &mut usize, value: &str) -> bool {
        if !charge(left, 2) || !charge(left, value.len()) {
            return false;
        }
        for byte in value.bytes() {
            let extra = match byte {
                b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 1,
                0..=31 => 5,
                _ => 0,
            };
            if !charge(left, extra) {
                return false;
            }
        }
        true
    }
    enum Frame<'a> {
        Value(&'a Value, usize),
        Array(std::slice::Iter<'a, Value>, usize),
        Object(serde_json::map::Iter<'a>, usize),
    }
    let mut left = limit;
    let mut stack = vec![Frame::Value(root, 0)];
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Value(value, depth) => {
                if depth > max_depth {
                    return Err(());
                }
                match value {
                    Value::Null => {
                        if !charge(&mut left, 4) {
                            return Err(());
                        }
                    }
                    Value::Bool(value) => {
                        if !charge(&mut left, if *value { 4 } else { 5 }) {
                            return Err(());
                        }
                    }
                    Value::Number(value) => {
                        if !charge(&mut left, value.as_str().len()) {
                            return Err(());
                        }
                    }
                    Value::String(value) => {
                        if !string(&mut left, value) {
                            return Err(());
                        }
                    }
                    Value::Array(values) => {
                        if !charge(&mut left, 2)
                            || !charge(&mut left, values.len().saturating_sub(1))
                        {
                            return Err(());
                        }
                        stack.push(Frame::Array(values.iter(), depth + 1));
                    }
                    Value::Object(values) => {
                        if !charge(&mut left, 2)
                            || !charge(&mut left, values.len().saturating_sub(1))
                            || !charge(&mut left, values.len())
                        {
                            return Err(());
                        }
                        stack.push(Frame::Object(values.iter(), depth + 1));
                    }
                }
            }
            Frame::Array(mut values, depth) => {
                if let Some(value) = values.next() {
                    stack.push(Frame::Array(values, depth));
                    stack.push(Frame::Value(value, depth));
                }
            }
            Frame::Object(mut values, depth) => {
                if let Some((key, value)) = values.next() {
                    if !string(&mut left, key) {
                        return Err(());
                    }
                    stack.push(Frame::Object(values, depth));
                    stack.push(Frame::Value(value, depth));
                }
            }
        }
    }
    Ok(limit - left)
}

pub(crate) fn quoted_len(value: &str, limit: usize) -> Result<usize, ()> {
    let mut size = value
        .len()
        .checked_add(2)
        .filter(|n| *n <= limit)
        .ok_or(())?;
    for byte in value.bytes() {
        let extra = match byte {
            b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 1,
            0..=31 => 5,
            _ => 0,
        };
        size = size.checked_add(extra).filter(|n| *n <= limit).ok_or(())?;
    }
    Ok(size)
}
