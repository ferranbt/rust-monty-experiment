//! `dut replay`: run a script's parsers over recorded device output, without hardware.
//!
//! The input is either a raw log from `dut run --raw-log` (received bytes replayed at
//! their recorded times) or a capture of device output: with line framing one line per
//! frame, otherwise the bytes as received. Leading `#` lines are a comment header
//! (as in gpsd's test logs) and are skipped.

use std::path::Path;

use dut_runtime::{
    Event, FakeHost, Framing, Host, HostError, JsonLinesRecorder, MontyRunner, Recorder,
    ScriptRunner, Seconds, SignalKind, TimerSpec, Value,
};

/// A FakeHost whose timers never fire: replay shows only what the recorded output produced.
struct NoTimers(FakeHost);

impl Host for NoTimers {
    async fn configure(
        &mut self,
        framing: &Framing,
        _timers: &[TimerSpec],
    ) -> Result<(), HostError> {
        self.0.configure(framing, &[]).await
    }

    async fn write(&mut self, channel: &str, data: &[u8]) -> Result<(), HostError> {
        self.0.write(channel, data).await
    }

    async fn next_event(&mut self, timeout: Seconds) -> Result<Option<Event>, HostError> {
        self.0.next_event(timeout).await
    }

    fn now(&self) -> Seconds {
        self.0.now()
    }

    async fn emit(&mut self, kind: SignalKind, payload: Value) -> Result<(), HostError> {
        self.0.emit(kind, payload).await
    }
}

/// Received chunks: (time, channel, bytes).
type Chunks = Vec<(Seconds, String, Vec<u8>)>;

/// Seconds between lines of a plain text sample.
const LINE_GAP: Seconds = 0.01;

pub fn replay(script: &Path, input: &Path) -> Result<(), String> {
    let source =
        std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    // Bytes, not text: device output may hold junk that is not valid UTF-8.
    let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;

    let mut runner = MontyRunner::new(NoTimers(FakeHost::new()));
    let registry = runner
        .load(&source)
        .map_err(|e| format!("load: {e}\n{}", e.traceback))?;
    let chunks = match raw_log(&String::from_utf8_lossy(&bytes))? {
        Some(chunks) => chunks,
        None => capture(&bytes, &registry.framing),
    };
    let end = chunks.last().map_or(0.0, |(t, _, _)| *t) + 1.0;
    for (t, channel, data) in &chunks {
        runner.host_mut().0.deliver(data, *t, channel);
    }
    runner
        .pump(end)
        .map_err(|e| format!("{e}\n{}", e.traceback))?;

    let mut out = JsonLinesRecorder::new(std::io::stdout(), None::<std::io::Sink>);
    for signal in &runner.host().0.signals {
        out.signal(signal.clone());
    }
    match out.error {
        Some(e) => Err(format!("writing output: {e}")),
        None => Ok(()),
    }
}

/// A raw log is JSON lines as `JsonLinesRecorder` writes them. `None` if `text` is not one.
fn raw_log(text: &str) -> Result<Option<Chunks>, String> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .peekable();
    if !lines
        .peek()
        .is_some_and(|(_, l)| l.trim_start().starts_with('{'))
    {
        return Ok(None);
    }
    let mut chunks = vec![];
    for (n, line) in lines {
        let bad = || {
            format!(
                "raw log line {}: expected {{\"t\", \"dir\", \"ch\", \"hex\"}}",
                n + 1
            )
        };
        if field(line, "dir").ok_or_else(bad)? != "rx" {
            continue;
        }
        let t = field(line, "t")
            .and_then(|t| t.parse().ok())
            .ok_or_else(bad)?;
        let channel = field(line, "ch").ok_or_else(bad)?.to_string();
        let data = unhex(field(line, "hex").ok_or_else(bad)?).ok_or_else(bad)?;
        chunks.push((t, channel, data));
    }
    Ok(Some(chunks))
}

/// The value of `"key": ...` in one of our own JSON lines (no escapes in these fields).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\": ");
    let rest = &line[line.find(&pattern)? + pattern.len()..];
    match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next(),
        None => rest.split([',', '}']).next().map(str::trim),
    }
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Bytes of a binary capture delivered per chunk, like a serial port returns them.
const CHUNK: usize = 64;

/// A capture of device output. With line framing each line becomes one frame, ended with
/// the script's delimiter; with any other framing the bytes are delivered as they are.
fn capture(bytes: &[u8], framing: &Framing) -> Chunks {
    let mut bytes = bytes;
    while bytes.starts_with(b"#") {
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes = &bytes[end..];
    }
    let Framing::Delimiter { delimiter, .. } = framing else {
        return bytes
            .chunks(CHUNK)
            .enumerate()
            .map(|(i, chunk)| (i as Seconds * LINE_GAP, "main".to_string(), chunk.to_vec()))
            .collect();
    };
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes
        .split(|b| *b == b'\n')
        .enumerate()
        .map(|(i, line)| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let mut data = line.to_vec();
            data.extend_from_slice(delimiter);
            (i as Seconds * LINE_GAP, "main".to_string(), data)
        })
        .collect()
}
