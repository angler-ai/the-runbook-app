//! Private bounded tree encoding; public snapshot admission is a later boundary.
#![allow(dead_code)]
use serde::{
    Deserialize, Serialize, Serializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
    ser::{SerializeSeq, SerializeStruct},
};
use serde_json::Value;
use std::{cell::Cell, fmt, io::Write};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub bytes: usize,
    pub values: usize,
    pub depth: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Limit,
    Invalid,
}

/// Owns decoded trees, including intermediate subtrees, without recursive drop.
pub(crate) struct Tree(Value);
impl Tree {
    /// Caller must first enforce a depth suitable for recursive typed conversion.
    pub(crate) fn into_value(mut self) -> Value {
        self.take()
    }
    pub(crate) fn get(&self) -> &Value {
        &self.0
    }
    fn take(&mut self) -> Value {
        std::mem::replace(&mut self.0, Value::Null)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let mut pending = vec![self.take()];
        while let Some(value) = pending.pop() {
            match value {
                Value::Array(items) => pending.extend(items),
                Value::Object(items) => pending.extend(items.into_values()),
                _ => (),
            }
        }
    }
}

#[derive(Serialize)]
enum RefToken<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    String(&'a str),
    Array(usize),
    Object(usize),
    Key(&'a str),
}
#[derive(Deserialize)]
enum Token {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(u64),
    Object(u64),
    Key(String),
}

enum Children<'a> {
    Array(std::slice::Iter<'a, Value>),
    Object(serde_json::map::Iter<'a>),
}
struct Walk<'a> {
    next: Option<&'a Value>,
    stack: Vec<Children<'a>>,
}
impl<'a> Walk<'a> {
    fn new(value: &'a Value) -> Self {
        Self {
            next: Some(value),
            stack: Vec::new(),
        }
    }
}
impl<'a> Iterator for Walk<'a> {
    type Item = (RefToken<'a>, usize);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(value) = self.next.take() {
                let depth = self.stack.len();
                let token = match value {
                    Value::Null => RefToken::Null,
                    Value::Bool(b) => RefToken::Bool(*b),
                    Value::Number(n) => RefToken::Number(n.as_str()),
                    Value::String(s) => RefToken::String(s),
                    Value::Array(a) => {
                        self.stack.push(Children::Array(a.iter()));
                        RefToken::Array(a.len())
                    }
                    Value::Object(o) => {
                        self.stack.push(Children::Object(o.iter()));
                        RefToken::Object(o.len())
                    }
                };
                return Some((token, depth));
            }
            match self.stack.last_mut()? {
                Children::Array(iter) => {
                    if let Some(value) = iter.next() {
                        self.next = Some(value);
                    } else {
                        self.stack.pop();
                    }
                }
                Children::Object(iter) => {
                    if let Some((key, value)) = iter.next() {
                        self.next = Some(value);
                        return Some((RefToken::Key(key), self.stack.len()));
                    } else {
                        self.stack.pop();
                    }
                }
            }
        }
    }
}
struct Tokens<'a>(&'a Value);
impl Serialize for Tokens<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        for (token, _) in Walk::new(self.0) {
            seq.serialize_element(&token)?;
        }
        seq.end()
    }
}
struct Envelope<'a>(&'a Value);
impl Serialize for Envelope<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("Envelope", 2)?;
        object.serialize_field("version", &1u8)?;
        object.serialize_field("tokens", &Tokens(self.0))?;
        object.end()
    }
}
struct Counter {
    bytes: usize,
    limit: usize,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| std::io::Error::other("wire byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn encode(value: &Value, limits: Limits) -> Result<Vec<u8>, Error> {
    let mut values = 0usize;
    for (token, depth) in Walk::new(value) {
        if matches!(token, RefToken::Key(_)) {
            continue;
        }
        values = values.checked_add(1).ok_or(Error::Limit)?;
        if values > limits.values || depth > limits.depth {
            return Err(Error::Limit);
        }
    }
    let envelope = Envelope(value);
    let mut counter = Counter {
        bytes: 0,
        limit: limits.bytes,
    };
    serde_json::to_writer(&mut counter, &envelope).map_err(|_| Error::Limit)?;
    let mut bytes = Vec::with_capacity(counter.bytes);
    serde_json::to_writer(&mut bytes, &envelope).map_err(|_| Error::Invalid)?;
    Ok(bytes)
}

struct Seed<'a> {
    cap: usize,
    limited: &'a Cell<bool>,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Vec<Token>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Vec<Token>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("wire envelope")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut version = None;
        let mut tokens = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" if version.is_none() => {
                    let value = map.next_value::<u8>()?;
                    if value != 1 {
                        return Err(de::Error::custom("unsupported wire version"));
                    }
                    version = Some(value);
                }
                "tokens" if tokens.is_none() => {
                    tokens = Some(map.next_value_seed(TokenSeed {
                        cap: self.cap,
                        limited: self.limited,
                    })?);
                }
                _ => return Err(de::Error::custom("unknown or duplicate wire field")),
            }
        }
        if version.is_none() {
            return Err(de::Error::missing_field("version"));
        }
        tokens.ok_or_else(|| de::Error::missing_field("tokens"))
    }
}
struct TokenSeed<'a> {
    cap: usize,
    limited: &'a Cell<bool>,
}
impl<'de> DeserializeSeed<'de> for TokenSeed<'_> {
    type Value = Vec<Token>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for TokenSeed<'_> {
    type Value = Vec<Token>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded wire tokens")
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        let mut tokens = Vec::new();
        loop {
            if tokens.len() == self.cap {
                if seq.next_element::<de::IgnoredAny>()?.is_some() {
                    self.limited.set(true);
                    return Err(de::Error::custom("wire token limit"));
                }
                break;
            }
            match seq.next_element()? {
                Some(token) => tokens.push(token),
                None => break,
            }
        }
        Ok(tokens)
    }
}

struct Frame {
    tree: Tree,
    remaining: usize,
    key: Option<String>,
}
pub(crate) fn decode(bytes: &[u8], limits: Limits) -> Result<Tree, Error> {
    if bytes.len() > limits.bytes {
        return Err(Error::Limit);
    }
    let limited = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    // Each value contributes one token; object members add at most one key each.
    let tokens = Seed {
        cap: limits.values.saturating_mul(2),
        limited: &limited,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| {
        if limited.get() {
            Error::Limit
        } else {
            Error::Invalid
        }
    })?;
    deserializer.end().map_err(|_| Error::Invalid)?;
    materialize(tokens, limits)
}
fn materialize(tokens: Vec<Token>, limits: Limits) -> Result<Tree, Error> {
    let mut tokens = tokens.into_iter();
    let mut stack: Vec<Frame> = Vec::new();
    let mut root = None;
    let mut values = 0usize;
    while let Some(token) = tokens.next() {
        if root.is_some() {
            return Err(Error::Invalid);
        }
        if let Token::Key(key) = token {
            let frame = stack.last_mut().ok_or(Error::Invalid)?;
            let Value::Object(map) = &frame.tree.0 else {
                return Err(Error::Invalid);
            };
            if frame.key.is_some() || map.contains_key(&key) {
                return Err(Error::Invalid);
            }
            frame.key = Some(key);
            continue;
        }
        if let Some(frame) = stack.last()
            && frame.tree.0.is_object()
            && frame.key.is_none()
        {
            return Err(Error::Invalid);
        }
        values = values.checked_add(1).ok_or(Error::Limit)?;
        if values > limits.values || stack.len() > limits.depth {
            return Err(Error::Limit);
        }
        let (value, children) = match token {
            Token::Null => (Value::Null, 0),
            Token::Bool(b) => (Value::Bool(b), 0),
            Token::String(s) => (Value::String(s), 0),
            Token::Number(n) => (Value::Number(n.parse().map_err(|_| Error::Invalid)?), 0),
            Token::Array(n) | Token::Object(n) => {
                let object = matches!(token, Token::Object(_));
                let n = usize::try_from(n).map_err(|_| Error::Invalid)?;
                let required = n
                    .checked_mul(if object { 2 } else { 1 })
                    .ok_or(Error::Invalid)?;
                if required > tokens.len() {
                    return Err(Error::Invalid);
                }
                if n > limits.values - values {
                    return Err(Error::Limit);
                }
                (
                    if object {
                        Value::Object(Default::default())
                    } else {
                        Value::Array(Vec::new())
                    },
                    n,
                )
            }
            Token::Key(_) => unreachable!(),
        };
        let mut tree = Tree(value);
        if children != 0 {
            stack.push(Frame {
                tree,
                remaining: children,
                key: None,
            });
            continue;
        }
        loop {
            let Some(frame) = stack.last_mut() else {
                root = Some(tree);
                break;
            };
            match &mut frame.tree.0 {
                Value::Array(array) => array.push(tree.take()),
                Value::Object(map) => {
                    map.insert(frame.key.take().ok_or(Error::Invalid)?, tree.take());
                }
                _ => unreachable!(),
            }
            frame.remaining -= 1;
            if frame.remaining != 0 {
                break;
            }
            tree = stack.pop().expect("frame exists").tree;
        }
    }
    if !stack.is_empty() {
        return Err(Error::Invalid);
    }
    root.ok_or(Error::Invalid)
}
#[cfg(test)]
#[path = "wire/tests.rs"]
mod tests;
