//! What a host records: the raw log (every byte both ways, host-timestamped)
//! and the signals emitted by the script. Shared by every [`Host`](crate::Host).

use std::collections::BTreeMap;
use std::io::{self, Write};

use crate::host::{Seconds, SignalKind, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Rx,
    Tx,
}

/// One chunk in the raw log, timestamped by the host.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEntry {
    pub t: Seconds,
    pub dir: Dir,
    pub channel: String,
    pub data: Vec<u8>,
}

/// A signal emitted by the script (`heartbeat`, `record`, ...) or the host (`framing_error`).
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub t: Seconds,
    pub kind: SignalKind,
    pub payload: Value,
}

/// One metric sample, read from a [`SignalKind::Metric`] signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Metric<'a> {
    pub name: &'a str,
    /// A gauge's value, or a counter's increment.
    pub value: f64,
    pub labels: BTreeMap<String, String>,
}

impl Signal {
    /// The sample of a metric signal; `None` for every other kind.
    pub fn metric(&self) -> Option<Metric<'_>> {
        if self.kind != SignalKind::Metric {
            return None;
        }
        let labels = match self.payload.get("labels") {
            Some(Value::Dict(d)) => d
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect(),
            _ => BTreeMap::new(),
        };
        Some(Metric {
            name: self.payload.get("name").and_then(Value::as_str)?,
            value: self.payload.get("value").and_then(Value::as_float)?,
            labels,
        })
    }

    /// The text of a `log(...)` signal; `None` for every other kind.
    pub fn log_text(&self) -> Option<&str> {
        if self.kind != SignalKind::Log {
            return None;
        }
        self.payload.get("text").and_then(Value::as_str)
    }
}

/// Where a host sends raw-log entries and signals as they happen.
pub trait Recorder {
    fn raw(&mut self, entry: RawEntry);
    fn signal(&mut self, signal: Signal);
}

/// Keeps everything in memory (tests, short runs).
#[derive(Debug, Default)]
pub struct MemoryRecorder {
    pub raw: Vec<RawEntry>,
    pub signals: Vec<Signal>,
}

impl Recorder for MemoryRecorder {
    fn raw(&mut self, entry: RawEntry) {
        self.raw.push(entry);
    }

    fn signal(&mut self, signal: Signal) {
        self.signals.push(signal);
    }
}

/// Streams JSON lines: signals to one writer, the raw log (optionally) to another.
///
/// Signal: `{"t": 1.25, "kind": "record", "payload": {...}}`
/// Raw:    `{"t": 1.25, "dir": "rx", "ch": "main", "hex": "543d32310a"}`
pub struct JsonLinesRecorder<S: Write, R: Write> {
    signals: S,
    raw: Option<R>,
    /// First write error, if any. Recording keeps going for the other stream.
    pub error: Option<io::Error>,
}

impl<S: Write, R: Write> JsonLinesRecorder<S, R> {
    pub fn new(signals: S, raw: Option<R>) -> Self {
        Self {
            signals,
            raw,
            error: None,
        }
    }

    /// The writers back: (signals, raw log).
    pub fn into_inner(self) -> (S, Option<R>) {
        (self.signals, self.raw)
    }

    fn keep(&mut self, result: io::Result<()>) {
        if let Err(e) = result {
            self.error.get_or_insert(e);
        }
    }
}

impl<S: Write, R: Write> Recorder for JsonLinesRecorder<S, R> {
    fn raw(&mut self, e: RawEntry) {
        if let Some(out) = self.raw.as_mut() {
            let dir = match e.dir {
                Dir::Rx => "rx",
                Dir::Tx => "tx",
            };
            let line = format!(
                "{{\"t\": {}, \"dir\": \"{dir}\", \"ch\": {}, \"hex\": \"{}\"}}",
                json_float(e.t),
                json_string(&e.channel),
                hex(&e.data)
            );
            let result = writeln!(out, "{line}").and_then(|()| out.flush());
            self.keep(result);
        }
    }

    fn signal(&mut self, s: Signal) {
        let line = format!(
            "{{\"t\": {}, \"kind\": {}, \"payload\": {}}}",
            json_float(s.t),
            json_string(s.kind.as_str()),
            to_json(&s.payload)
        );
        let result = writeln!(self.signals, "{line}").and_then(|()| self.signals.flush());
        self.keep(result);
    }
}

/// JSON for a sandbox value. Bytes become hex strings; NaN/inf become null.
pub fn to_json(v: &Value) -> String {
    match v {
        Value::None => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => json_float(*f),
        Value::Str(s) => json_string(s),
        Value::Bytes(b) => format!("\"{}\"", hex(b)),
        Value::List(items) => format!(
            "[{}]",
            items.iter().map(to_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Dict(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{}: {}", json_string(k), to_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn json_float(f: f64) -> String {
    if !f.is_finite() {
        "null".into()
    } else if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use eyre::Result;

    use super::*;

    #[test]
    fn values_to_json() {
        let v = Value::dict([
            (
                "a",
                Value::List(vec![
                    Value::Int(1),
                    Value::Float(2.5),
                    Value::Float(3.0),
                    Value::None,
                ]),
            ),
            ("b", Value::Bytes(vec![0x01, 0xff])),
            ("c", Value::from("q\"\n\u{1}")),
            ("d", Value::Float(f64::NAN)),
            ("e", Value::Bool(true)),
        ]);
        assert_eq!(
            to_json(&v),
            r#"{"a": [1, 2.5, 3.0, null], "b": "01ff", "c": "q\"\n\u0001", "d": null, "e": true}"#
        );
    }

    #[test]
    fn json_lines_recorder() -> Result<()> {
        let mut rec = JsonLinesRecorder::new(Vec::new(), Some(Vec::new()));
        rec.signal(Signal {
            t: 1.5,
            kind: "record".into(),
            payload: Value::dict([("temp_c", 21.5.into())]),
        });
        rec.raw(RawEntry {
            t: 2.0,
            dir: Dir::Tx,
            channel: "main".into(),
            data: b"hi\n".to_vec(),
        });
        assert_eq!(
            String::from_utf8(rec.signals)?,
            "{\"t\": 1.5, \"kind\": \"record\", \"payload\": {\"temp_c\": 21.5}}\n"
        );
        let raw = rec.raw.unwrap_or_default();
        assert_eq!(
            String::from_utf8(raw)?,
            "{\"t\": 2.0, \"dir\": \"tx\", \"ch\": \"main\", \"hex\": \"68690a\"}\n"
        );
        assert!(rec.error.is_none());
        Ok(())
    }
}
