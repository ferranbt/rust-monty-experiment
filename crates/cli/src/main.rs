//! `dut` CLI for device scripts.

mod interactive;
mod replay;
mod serial;
mod typecheck;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use dut_runtime::runner::SUPPORTED_SDK_VERSIONS;
use dut_runtime::{FakeHost, JsonLinesRecorder, MontyRunner, Registry, ScriptError, ScriptRunner};

use crate::serial::{Link, SerialHost, Transport, connect_tcp, open_port};

#[derive(Parser)]
#[command(name = "dut", version, about = "Tools for device scripts")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Where the device is: `--port` (serial) or `--tcp` (emulator console, network bridge).
#[derive(clap::Args)]
struct Connection {
    /// Serial port. Linux: /dev/ttyUSB0, /dev/ttyACM0, /dev/serial0 (Pi).
    /// macOS: /dev/cu.usbserial-*, /dev/cu.usbmodem* (use cu.*, not tty.*).
    #[arg(long, required_unless_present = "tcp", conflicts_with = "tcp")]
    port: Option<String>,
    /// TCP address of the device's serial stream, e.g. localhost:4321 for QEMU
    /// `-serial tcp::4321,server`, Renode or Wokwi.
    #[arg(long)]
    tcp: Option<String>,
    /// Serial baud rate (ignored with --tcp).
    #[arg(long, default_value_t = 115_200)]
    baud: u32,
}

impl Connection {
    fn open(&self) -> Result<Link, String> {
        match (&self.port, &self.tcp) {
            (_, Some(address)) => connect_tcp(address).map(Link::Tcp),
            (Some(port), None) => open_port(port, self.baud).map(Link::Serial),
            (None, None) => unreachable!("clap requires --port or --tcp"),
        }
        .map_err(|e| e.to_string())
    }
}

#[derive(Subcommand)]
enum Command {
    /// Type-check a device script, load it in the sandbox and check what it declares.
    Test {
        /// Path to the device script.
        script: PathBuf,
    },
    /// Run a device script against a device; signals stream to stdout as JSON lines.
    Run {
        /// Path to the device script.
        script: PathBuf,
        #[command(flatten)]
        connection: Connection,
        /// Verb to call after the `prepare` hook (no arguments). Repeat for several, in order.
        #[arg(long = "verb")]
        verbs: Vec<String>,
        /// Seconds to keep listening (parsers and timers run) before the `finish` hook.
        #[arg(long, default_value_t = 10.0)]
        duration: f64,
        /// Also write the raw log (every byte both ways, timestamped) as JSON lines here.
        #[arg(long)]
        raw_log: Option<PathBuf>,
    },
    /// Run a script's parsers over recorded output, no hardware: a raw log from
    /// `dut run --raw-log`, or a capture of device output (text or binary).
    /// Signals go to stdout as JSON lines.
    Replay {
        /// Path to the device script.
        script: PathBuf,
        /// Raw log or text sample.
        input: PathBuf,
    },
    /// Load a script against a device and call verbs by hand. Signals stream to
    /// stdout as JSON lines; prompts and results go to stderr.
    Shell {
        /// Path to the device script.
        script: PathBuf,
        #[command(flatten)]
        connection: Connection,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Test { script } => test(&script),
        Command::Run {
            script,
            connection,
            verbs,
            duration,
            raw_log,
        } => exit(run(
            &script,
            &connection,
            &verbs,
            duration,
            raw_log.as_deref(),
        )),
        Command::Replay { script, input } => exit(replay::replay(&script, &input)),
        Command::Shell { script, connection } => exit(
            std::fs::read_to_string(&script)
                .map_err(|e| format!("{}: {e}", script.display()))
                .and_then(|source| interactive::shell(&source, connection.open()?)),
        ),
    }
}

fn exit(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    script: &Path,
    connection: &Connection,
    verbs: &[String],
    duration: f64,
    raw_log: Option<&Path>,
) -> Result<(), String> {
    let source =
        std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    let raw = raw_log
        .map(|p| std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display())))
        .transpose()?;
    let port = connection.open()?;
    let recorder = JsonLinesRecorder::new(std::io::stdout(), raw);
    let recorder = session(&source, SerialHost::new(port, recorder), verbs, duration)?;
    match recorder.error {
        Some(e) => Err(format!("writing output: {e}")),
        None => Ok(()),
    }
}

/// Load the script, `prepare`, call the verbs, listen for `duration`, `finish`.
fn session<T: Transport + Send, S: Write + Send, R: Write + Send>(
    source: &str,
    host: SerialHost<T, JsonLinesRecorder<S, R>>,
    verbs: &[String],
    duration: f64,
) -> Result<JsonLinesRecorder<S, R>, String> {
    let script_err = |what: &str, e: ScriptError| {
        let mut msg = format!("{what}: {e}");
        if !e.traceback.is_empty() {
            msg = format!("{msg}\n{}", e.traceback);
        }
        msg
    };
    let mut runner = MontyRunner::new(host);
    runner.load(source).map_err(|e| script_err("load", e))?;
    runner
        .call_hook("prepare", &[])
        .map_err(|e| script_err("prepare", e))?;
    for verb in verbs {
        runner
            .call_verb(verb, &[])
            .map_err(|e| script_err(verb, e))?;
    }
    runner.pump(duration).map_err(|e| script_err("listen", e))?;
    runner
        .call_hook("finish", &[])
        .map_err(|e| script_err("finish", e))?;
    Ok(runner.into_host().into_parts().1)
}

fn test(path: &Path) -> ExitCode {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("device.py");
    match typecheck::check(&source, name) {
        Ok(None) => {}
        Ok(Some(diagnostics)) => {
            print!("{diagnostics}");
            return ExitCode::FAILURE;
        }
        Err(e) => println!("warning: type check unavailable: {e}"),
    }
    let registry = match MontyRunner::new(FakeHost::new()).load(&source) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            if !e.traceback.is_empty() {
                eprintln!("{}", e.traceback);
            }
            return ExitCode::FAILURE;
        }
    };
    let (errors, warnings) = check(&registry);
    for e in &errors {
        println!("error: {e}");
    }
    for w in &warnings {
        println!("warning: {w}");
    }
    if !errors.is_empty() {
        return ExitCode::FAILURE;
    }
    let list = |v: Vec<&str>| {
        if v.is_empty() {
            "-".to_string()
        } else {
            v.join(", ")
        }
    };
    println!(
        "ok: device {:?}, sdk {}",
        registry.name,
        registry.sdk_declared.as_deref().unwrap_or("-")
    );
    println!(
        "  verbs:   {}",
        list(registry.verbs.keys().map(String::as_str).collect())
    );
    println!(
        "  hooks:   {}",
        list(registry.hooks.iter().map(String::as_str).collect())
    );
    println!(
        "  parsers: {} on_frame, readers: {}, timers: {}",
        registry.handlers,
        list(registry.readers.iter().map(String::as_str).collect()),
        registry.timers.len()
    );
    ExitCode::SUCCESS
}

fn check(r: &Registry) -> (Vec<String>, Vec<String>) {
    let mut errors = vec![];
    let mut warnings = vec![];
    match &r.sdk_declared {
        None => warnings.push(format!(
            "no SDK version declared; add Device(..., sdk=\"{}\")",
            r.sdk
        )),
        Some(v) if !SUPPORTED_SDK_VERSIONS.contains(&v.as_str()) => errors.push(format!(
            "SDK version {v:?} not supported (supported: {})",
            SUPPORTED_SDK_VERSIONS.join(", ")
        )),
        Some(_) => {}
    }
    if r.verbs.is_empty() {
        warnings.push("no verbs defined".into());
    }
    if r.handlers == 0 && r.readers.is_empty() {
        warnings
            .push("no @dev.on_frame handler or @dev.reader: device output is never parsed".into());
    }
    if r.heartbeat_within.is_none() {
        warnings.push("heartbeat_within not set: liveness cannot be monitored".into());
    }
    (errors, warnings)
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read, Write};
    use std::thread;
    use std::time::Duration;

    use eyre::{Result, eyre};
    use serialport::{SerialPort, TTYPort};

    use super::*;

    /// `dut run` logic over a real pseudo-terminal pair (macOS and Linux), with the
    /// SPEC §6.4 sensor script and a fake device on the other end.
    #[test]
    fn run_session_over_a_pty() -> Result<()> {
        let (host_end, mut device) = TTYPort::pair()?;
        let device = thread::spawn(move || -> std::io::Result<()> {
            device.set_timeout(Duration::from_millis(50))?;
            device.write_all(b"FW 1.4.2\n")?;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 64];
            let end = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < end {
                match device.read(&mut chunk) {
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    // the host closed its end: session over
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                    Err(e) => return Err(e),
                }
                while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=i).collect();
                    if line.starts_with(b"radio tx on") {
                        device.write_all(b"T=21.5\n[radio] TX started\n")?;
                    }
                }
            }
            Ok(())
        });

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/sensor_v2.py");
        let source = std::fs::read_to_string(path)?;
        let port: Box<dyn SerialPort> = Box::new(host_end);
        let host = SerialHost::new(port, JsonLinesRecorder::new(Vec::new(), Some(Vec::new())));
        let rec = session(&source, host, &["tx_on".to_string()], 0.3).map_err(|e| eyre!(e))?;
        device
            .join()
            .map_err(|_| eyre!("device thread panicked"))??;

        let (signals, raw) = rec.into_inner();
        let signals = String::from_utf8(signals)?;
        assert!(
            signals.contains(r#""kind": "identity", "payload": {"fw": "1.4.2"}"#),
            "{signals}"
        );
        assert!(
            signals.contains(
                r#""kind": "metric", "payload": {"labels": {}, "name": "temp_c", "value": 21.5}"#
            ),
            "{signals}"
        );
        let raw = String::from_utf8(raw.unwrap_or_default())?;
        assert!(
            raw.contains(&format!(
                r#""dir": "tx", "ch": "main", "hex": "{}""#,
                dut_runtime::record::hex(b"radio tx on\n")
            )),
            "{raw}"
        );
        Ok(())
    }
}
