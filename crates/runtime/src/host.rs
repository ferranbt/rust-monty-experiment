//! The host interface between the device-script sandbox and whatever runs it.
//!
//! A control plane (lab engine, edge agent, local CLI, fake host in tests)
//! implements [`Host`]. The sandbox SDK is built only on these five calls plus
//! the pure data helpers (`pack`, `unpack`, `crc`, `cobs_*`, `slip_*`,
//! `bytes_from_list`), which are *not* part of the trait because they don't
//! depend on the host.
//!
//! [`crate::fake::FakeHost`] is the in-memory implementation used by tests and `dut test`.
//!
//! Rules every implementation must follow (SPEC §4, §6, §9.6):
//! - All incoming bytes go through framing before they become [`Event`]s, and
//!   every byte in both directions is written to the raw log with a host timestamp.
//! - `next_event` must enforce its own timeout: the sandbox's time limit
//!   excludes time spent inside host calls.
//! - The host never calls back into the sandbox. Predicates and handlers stay in
//!   sandbox code; the host only moves data.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;

/// Seconds on the host clock (virtual in tests). Same unit as the sandbox's `now()`.
pub type Seconds = f64;

/// Plain data crossing the sandbox boundary (what Monty can pass to the host).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(BTreeMap<String, Value>),
}

impl Value {
    pub fn dict<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Value)>) -> Self {
        Value::Dict(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Ints convert, as in Python's `float()`.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.into())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}
impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<&[u8]> for Value {
    fn from(b: &[u8]) -> Self {
        Value::Bytes(b.to_vec())
    }
}
impl<const N: usize> From<&[u8; N]> for Value {
    fn from(b: &[u8; N]) -> Self {
        Value::Bytes(b.to_vec())
    }
}
impl From<Vec<u8>> for Value {
    fn from(b: Vec<u8>) -> Self {
        Value::Bytes(b)
    }
}

/// RX framing declared by the device script (`Framing.*` in the SDK).
#[derive(Debug, Clone, PartialEq)]
pub enum Framing {
    Delimiter {
        delimiter: Vec<u8>,
        keep_empty: bool,
        max_len: usize,
    },
    Cobs {
        max_len: usize,
    },
    Slip {
        max_len: usize,
    },
    Hdlc {
        max_len: usize,
    },
    LengthPrefixed {
        header: Vec<u8>,
        length_offset: usize,
        length_size: usize,
        trailer_len: usize,
        big_endian: bool,
        max_len: usize,
    },
    Fixed {
        size: usize,
    },
    Raw,
}

/// A periodic timer declared with `@dev.every(seconds=N)`.
/// The host delivers an [`EventKind::Tick`] with this `id` every `seconds`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimerSpec {
    pub id: u32,
    pub seconds: Seconds,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    /// A complete frame from a byte stream (UART, USB CDC-ACM), after framing.
    Frame(Vec<u8>),
    /// A packet from a packet transport (USB HID / vendor bulk), after reassembly.
    Packet(Vec<u8>),
    /// A timer declared by the script fired.
    Tick { timer: u32 },
}

impl TimerSpec {
    pub fn check(timers: &[TimerSpec]) -> Result<(), HostError> {
        match timers
            .iter()
            .find(|t| !(t.seconds > 0.0 && t.seconds.is_finite()))
        {
            Some(t) => Err(HostError::Unsupported(format!(
                "timer period {}",
                t.seconds
            ))),
            None => Ok(()),
        }
    }
}

/// What `next_event` returns. Sandbox shape:
/// `{"kind": "frame"|"packet"|"tick", "channel", "data", "t"}` (+ `"timer"` for ticks).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    /// Transport channel (`"main"`, an endpoint, ...). `None` for ticks.
    pub channel: Option<String>,
    /// Host timestamp of the event.
    pub t: Seconds,
}

/// What a signal is. The script's signals and the SDK/host diagnostics have their own
/// variant; anything else a script emits arrives as [`SignalKind::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SignalKind {
    /// `heartbeat(ok=...)`: `{"ok": bool}`.
    Heartbeat,
    /// `identity(dict)`: whatever the device exposes (firmware, serial, ...).
    Identity,
    /// A `Counter.inc()` or `Gauge.set()` sample: `{"name", "value": float, "labels": {str: str}}`.
    /// Counters report the increment, not the total. See [`Signal::metric`](crate::Signal::metric).
    Metric,
    /// `log(text, **fields)`: `{"text", ...fields}`.
    Log,
    /// `fail(msg)`: `{"msg"}`; the verb or hook was aborted.
    Fail,
    /// An `on_frame` handler raised: `{"error", "channel", "frame"}` (frame as hex).
    ParseError,
    /// An `every` timer raised: `{"error", "timer"}`.
    TimerError,
    /// The host could not frame received bytes: `{"error", "channel"}`.
    FramingError,
    Other(String),
}

impl SignalKind {
    pub fn as_str(&self) -> &str {
        match self {
            SignalKind::Heartbeat => "heartbeat",
            SignalKind::Identity => "identity",
            SignalKind::Metric => "metric",
            SignalKind::Log => "log",
            SignalKind::Fail => "fail",
            SignalKind::ParseError => "parse_error",
            SignalKind::TimerError => "timer_error",
            SignalKind::FramingError => "framing_error",
            SignalKind::Other(kind) => kind,
        }
    }
}

impl From<&str> for SignalKind {
    fn from(kind: &str) -> Self {
        match kind {
            "heartbeat" => SignalKind::Heartbeat,
            "identity" => SignalKind::Identity,
            "metric" => SignalKind::Metric,
            "log" => SignalKind::Log,
            "fail" => SignalKind::Fail,
            "parse_error" => SignalKind::ParseError,
            "timer_error" => SignalKind::TimerError,
            "framing_error" => SignalKind::FramingError,
            other => SignalKind::Other(other.to_string()),
        }
    }
}

impl fmt::Display for SignalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// The channel doesn't exist on this transport.
    UnknownChannel(String),
    /// The transport failed (link down, pod silent, ...). The run is invalid, not the device.
    Transport(String),
    /// A configuration the host can't honour (unsupported framing, too many timers, ...).
    Unsupported(String),
    /// Size/rate limit exceeded (SPEC §10).
    Limit(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::UnknownChannel(c) => write!(f, "unknown channel {c:?}"),
            HostError::Transport(m) => write!(f, "transport error: {m}"),
            HostError::Unsupported(m) => write!(f, "unsupported: {m}"),
            HostError::Limit(m) => write!(f, "limit exceeded: {m}"),
        }
    }
}

impl std::error::Error for HostError {}

/// What a control plane implements to run device scripts.
///
/// Errors returned here surface in the sandbox as exceptions in the verb/hook
/// being run; the session itself survives.
pub trait Host {
    /// Called once after the script is loaded, with the declarations read from
    /// its registry. Sets up RX framing for every channel and the tick timers.
    fn configure(
        &mut self,
        framing: &Framing,
        timers: &[TimerSpec],
    ) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Send bytes to the device. Raw-logged as `tx` with the host timestamp.
    fn write(
        &mut self,
        channel: &str,
        data: &[u8],
    ) -> impl Future<Output = Result<(), HostError>> + Send;

    /// Wait for the next frame/packet/tick, or until `timeout` seconds pass
    /// (`Ok(None)`). Must not overshoot the timeout: after `Ok(None)`,
    /// `now()` is at least the deadline. Raw-logs every `rx` chunk it consumes.
    fn next_event(
        &mut self,
        timeout: Seconds,
    ) -> impl Future<Output = Result<Option<Event>, HostError>> + Send;

    /// Host clock in seconds. Monotonic. Virtual in tests.
    fn now(&self) -> Seconds;

    /// A signal from the script (see [`SignalKind`]).
    /// The host timestamps it; `payload` is a [`Value::Dict`].
    fn emit(
        &mut self,
        kind: SignalKind,
        payload: Value,
    ) -> impl Future<Output = Result<(), HostError>> + Send;
}
