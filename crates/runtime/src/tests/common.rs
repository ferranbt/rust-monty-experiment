//! Shared helpers: device scripts run in Monty on a FakeHost with a simulated device.

use crate::{
    FakeHost, MontyRunner, Registry, ScriptError, ScriptRunner, Seconds, SimDevice, Value,
};
use eyre::{Result, WrapErr, bail};

pub const IMPORTS: &str = "from dut import (Device, Framing, state, write, now, emit, next_event, wait, \
    request, expect, wait_until, heartbeat, identity, Counter, Gauge, fail, pack, unpack, crc, \
    cobs_encode, cobs_decode, slip_encode, slip_decode, bytes_from_list)\n";

/// A device script with standard imports and a Device. With `parser`, every frame
/// is emitted as a `parsed` signal `{"frame": <bytes>}` so tests can see what reached the parser.
pub fn script(body: &str, parser: bool) -> String {
    let mut src = format!("{IMPORTS}dev = Device(\"t\", sdk=\"0.1\")\n");
    if parser {
        src.push_str(
            "@dev.on_frame\ndef _parse(frame):\n    emit(\"parsed\", {\"frame\": frame})\n",
        );
    }
    src + body
}

pub struct Session {
    pub runner: MontyRunner<FakeHost>,
    pub registry: Registry,
}

impl Session {
    pub fn load(source: &str, sim: SimDevice) -> Result<Self, ScriptError> {
        let mut runner = MontyRunner::new(FakeHost::with_device(sim));
        let registry = runner.load(source)?;
        Ok(Self { runner, registry })
    }

    /// Script body + recording parser.
    pub fn new(body: &str, sim: SimDevice) -> Result<Self> {
        Self::load(&script(body, true), sim).wrap_err("loading the test script")
    }

    /// Script body without the recording parser.
    pub fn bare(body: &str, sim: SimDevice) -> Result<Self> {
        Self::load(&script(body, false), sim).wrap_err("loading the test script")
    }

    pub fn example(name: &str, sim: SimDevice) -> Result<Self> {
        let path = format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(&path).wrap_err_with(|| format!("reading {path}"))?;
        Self::load(&source, sim).wrap_err_with(|| format!("loading {path}"))
    }

    pub fn verb(&mut self, name: &str, args: &[(&str, Value)]) -> Result<Value, ScriptError> {
        self.runner.call_verb(name, args)
    }

    pub fn hook(&mut self, name: &str, args: &[(&str, Value)]) -> Result<Value, ScriptError> {
        self.runner.call_hook(name, args)
    }

    pub fn read(&mut self) -> Result<Value, ScriptError> {
        self.runner.read("default")
    }

    pub fn pump(&mut self, seconds: Seconds) -> Result<()> {
        Ok(self.runner.pump(seconds)?)
    }

    pub fn eval(&mut self, code: &str) -> Result<Value, ScriptError> {
        self.runner.eval(code, &[])
    }

    pub fn host(&self) -> &FakeHost {
        self.runner.host()
    }

    pub fn now(&self) -> Seconds {
        crate::Host::now(self.runner.host())
    }

    pub fn of_kind(&self, kind: &str) -> Vec<Value> {
        self.host().of_kind(kind).into_iter().cloned().collect()
    }

    pub fn tx(&self) -> Vec<Vec<u8>> {
        self.host().tx("main")
    }

    /// Samples of one metric, in order: (value, labels).
    pub fn metric(&self, name: &str) -> Vec<(f64, Vec<(String, String)>)> {
        self.host()
            .signals
            .iter()
            .filter_map(|s| s.metric())
            .filter(|m| m.name == name)
            .map(|m| (m.value, m.labels.into_iter().collect()))
            .collect()
    }

    /// Values of one metric, in order.
    pub fn values(&self, name: &str) -> Vec<f64> {
        self.metric(name).into_iter().map(|(v, _)| v).collect()
    }

    /// Frames that reached the recording parser, in order.
    pub fn parsed(&self) -> Vec<Vec<u8>> {
        self.host()
            .of_kind("parsed")
            .into_iter()
            .filter_map(|p| p.get("frame").and_then(Value::as_bytes).map(<[u8]>::to_vec))
            .collect()
    }
}

pub fn b(data: &[u8]) -> Value {
    Value::Bytes(data.to_vec())
}

pub fn frames(list: &[&[u8]]) -> Vec<Vec<u8>> {
    list.iter().map(|f| f.to_vec()).collect()
}

pub fn assert_approx(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

/// The script error, which must be of `exc_type` (e.g. `"TimeoutError"`).
pub fn expect_err(result: Result<Value, ScriptError>, exc_type: &str) -> Result<ScriptError> {
    match result {
        Err(e) if e.exc_type == exc_type => Ok(e),
        other => bail!("expected {exc_type}, got {other:?}"),
    }
}
