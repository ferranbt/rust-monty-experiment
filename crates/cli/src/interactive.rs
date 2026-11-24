//! `dut shell`: load a script against a device and call verbs by hand.
//!
//! Signals stream to stdout as JSON lines, also between commands: while waiting for
//! input the script keeps listening (parsers and timers run). Prompts and results go
//! to stderr, so stdout stays pure JSON lines.

use std::io::BufRead;
use std::sync::mpsc;
use std::time::Duration;

use dut_runtime::record::to_json;
use dut_runtime::{JsonLinesRecorder, MontyRunner, ScriptError, ScriptRunner, Value};

use crate::serial::{Link, SerialHost};

/// How long the script listens between checks for a typed command.
const IDLE_LISTEN: f64 = 0.2;

const HELP: &str = "commands:
  <verb> [name=value ...]   call a verb, e.g. `set_channel channel=19`
  <hook> [name=value ...]   call a defined hook, e.g. `recover`
  listen <seconds>          only listen
  help                      this text
  quit                      run the `finish` hook and exit (also Ctrl-D)";

pub fn shell(script: &str, port: Link) -> Result<(), String> {
    let recorder = JsonLinesRecorder::new(std::io::stdout(), None::<std::io::Sink>);
    let mut runner = MontyRunner::new(SerialHost::new(port, recorder));
    let registry = runner.load(script).map_err(|e| describe("load", &e))?;
    if let Err(e) = runner.call_hook("prepare", &[]) {
        eprintln!("{}", describe("prepare", &e));
    }

    let names: Vec<&str> = registry.verbs.keys().map(String::as_str).collect();
    eprintln!(
        "device {:?}; verbs: {}\n{HELP}",
        registry.name,
        names.join(", ")
    );

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        match rx.try_recv() {
            Ok(line) => {
                let mut words = line.split_whitespace();
                let Some(name) = words.next() else { continue };
                let args: Vec<(String, Value)> = words.map(arg).collect();
                let args: Vec<(&str, Value)> =
                    args.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                let result = match name {
                    "quit" | "exit" => break,
                    "help" => {
                        eprintln!("{HELP}");
                        continue;
                    }
                    "listen" => {
                        let seconds = line.split_whitespace().nth(1).and_then(|s| s.parse().ok());
                        match seconds {
                            Some(seconds) => runner.pump(seconds).map(|()| Value::None),
                            None => {
                                eprintln!("usage: listen <seconds>");
                                continue;
                            }
                        }
                    }
                    verb if registry.verbs.contains_key(verb) => runner.call_verb(verb, &args),
                    hook if registry.hooks.iter().any(|h| h == hook) => {
                        runner.call_hook(hook, &args)
                    }
                    other => {
                        eprintln!("unknown command {other:?} (type `help`)");
                        continue;
                    }
                };
                match result {
                    Ok(value) => eprintln!("= {}", to_json(&value)),
                    Err(e) => eprintln!("{}", describe(name, &e)),
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                if let Err(e) = runner.pump(IDLE_LISTEN) {
                    eprintln!("{}", describe("listen", &e));
                    std::thread::sleep(Duration::from_secs_f64(IDLE_LISTEN));
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }
    if let Err(e) = runner.call_hook("finish", &[]) {
        eprintln!("{}", describe("finish", &e));
    }
    Ok(())
}

/// `name=value`: numbers, `true`/`false` and `none` are typed; anything else is a str.
fn arg(word: &str) -> (String, Value) {
    let (name, value) = word.split_once('=').unwrap_or((word, "true"));
    let value = if let Ok(i) = value.parse::<i64>() {
        Value::Int(i)
    } else if let Ok(f) = value.parse::<f64>() {
        Value::Float(f)
    } else {
        match value {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "none" => Value::None,
            text => Value::Str(text.trim_matches('"').to_string()),
        }
    };
    (name.to_string(), value)
}

fn describe(what: &str, e: &ScriptError) -> String {
    if e.traceback.is_empty() {
        format!("{what}: {e}")
    } else {
        format!("{what}: {e}\n{}", e.traceback)
    }
}
