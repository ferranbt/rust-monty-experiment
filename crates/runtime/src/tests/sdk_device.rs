//! Device registry, decorators, signals, timers, readers and runtime guards.

use std::time::Duration;

use super::common::*;
use crate::{FakeHost, Framing, MontyRunner, Rule, ScriptRunner, SimDevice, TimerSpec, Value};
use eyre::{OptionExt, Result, bail};
use monty_types::ResourceLimits;

#[test]
fn registry_is_plain_data() -> Result<()> {
    let body = r#"
@dev.status
def status():
    pass

@dev.verb("blink")
def blink(times):
    pass

@dev.hook("prepare")
def prepare():
    pass

@dev.hook("enter")
def enter(mode):
    pass

@dev.every(seconds=2)
def poll():
    pass

@dev.reader
def one():
    return None

@dev.reader("other")
def two():
    return None
"#;
    let r = Session::new(body, SimDevice::new())?.registry;
    assert_eq!(
        (r.name.as_str(), r.sdk.as_str(), r.sdk_declared.as_deref()),
        ("t", "0.1", Some("0.1"))
    );
    assert_eq!(
        r.framing,
        Framing::Delimiter {
            delimiter: b"\n".to_vec(),
            keep_empty: false,
            max_len: 4096
        }
    );
    let verbs: Vec<_> = r
        .verbs
        .iter()
        .map(|(n, i)| (n.as_str(), i.confirm.clone(), i.standard))
        .collect();
    assert_eq!(verbs, [("blink", None, false), ("status", None, true)]);
    assert_eq!(r.hooks, ["enter", "prepare"]);
    assert_eq!(
        r.timers,
        [TimerSpec {
            id: 0,
            seconds: 2.0
        }]
    );
    assert_eq!(r.readers, ["default", "other"]);
    assert_eq!(r.handlers, 1);
    Ok(())
}

#[test]
fn framing_declarations_reach_the_host() -> Result<()> {
    let src = format!(
        "{IMPORTS}dev = Device(\"t\", framing=Framing.length_prefixed(header=b\"\\xaa\", length_offset=1, trailer_len=2, endian=\"big\"))\n"
    );
    let s = Session::load(&src, SimDevice::new())?;
    assert_eq!(
        s.registry.framing,
        Framing::LengthPrefixed {
            header: vec![0xAA],
            length_offset: 1,
            length_size: 1,
            trailer_len: 2,
            big_endian: true,
            max_len: 4096
        }
    );
    Ok(())
}

#[test]
fn verbs_and_hooks_receive_arguments() -> Result<()> {
    let body = r#"
@dev.set_channel
def set_channel(channel):
    write(f"ch {channel}".encode())
    return channel * 2

@dev.hook("enter")
def enter(mode):
    write(f"mode {mode}".encode())
"#;
    let mut s = Session::new(body, SimDevice::new())?;
    assert_eq!(
        s.verb("set_channel", &[("channel", Value::Int(19))])?,
        Value::Int(38)
    );
    s.hook("enter", &[("mode", "tx".into())])?;
    assert_eq!(s.hook("recover", &[])?, Value::None); // undefined hooks are no-ops
    assert_eq!(s.tx(), frames(&[b"ch 19", b"mode tx"]));
    Ok(())
}

#[test]
fn unknown_verb_errors() -> Result<()> {
    let mut s = Session::new("", SimDevice::new())?;
    let e = expect_err(s.verb("tx_on", &[]), "KeyError")?;
    assert!(e.message.contains("not defined"), "{e}");
    Ok(())
}

#[test]
fn load_errors() -> Result<()> {
    for (body, exc, msg) in [
        (
            "@dev.hook(\"bogus\")\ndef f():\n    pass\n",
            "ValueError",
            "unknown hook",
        ),
        (
            "@dev.status\ndef a():\n    pass\n@dev.status\ndef b():\n    pass\n",
            "ValueError",
            "defined twice",
        ),
        ("Device(\"again\")\n", "RuntimeError", "only one Device"),
        (
            "Framing.length_prefixed(length_size=3)\n",
            "ValueError",
            "length_size",
        ),
        ("def f(:\n  pass\n", "SyntaxError", ""),
    ] {
        let Err(e) = Session::load(&script(body, false), SimDevice::new()) else {
            bail!("{body} loaded")
        };
        assert_eq!(e.exc_type, exc, "{body}: {e}");
        assert!(e.message.contains(msg), "{body}: {e}");
    }
    Ok(())
}

#[test]
fn signals_are_timestamped_by_the_host() -> Result<()> {
    let body = r#"
temp = Gauge("temp_c", "temperature", unit="°C")
doors = Counter("doors", "door state changes", labels=["state"])

@dev.verb("sig")
def sig():
    heartbeat()
    heartbeat(ok=False)
    identity({"fw": "1.2", "sha": "ab12"})
    temp.set(21.5)
    doors.inc(state="open")
"#;
    let mut s = Session::bare(body, SimDevice::new())?;
    s.runner.host_mut().set_time(7.0);
    s.verb("sig", &[])?;
    let got: Vec<_> = s
        .host()
        .signals
        .iter()
        .map(|x| (x.t, x.kind.as_str(), x.payload.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (7.0, "heartbeat", Value::dict([("ok", true.into())])),
            (7.0, "heartbeat", Value::dict([("ok", false.into())])),
            (
                7.0,
                "identity",
                Value::dict([("fw", "1.2".into()), ("sha", "ab12".into())])
            ),
            (
                7.0,
                "metric",
                Value::dict([
                    ("name", "temp_c".into()),
                    ("value", 21.5.into()),
                    ("labels", Value::dict::<&str>([]))
                ])
            ),
            (
                7.0,
                "metric",
                Value::dict([
                    ("name", "doors".into()),
                    ("value", 1.0.into()),
                    ("labels", Value::dict([("state", "open".into())]))
                ])
            ),
        ]
    );
    Ok(())
}

#[test]
fn fail_reports_and_aborts_verb() -> Result<()> {
    let body = "@dev.status\ndef status():\n    fail(\"sensor missing\")\n    write(b\"never\")\n";
    let mut s = Session::new(body, SimDevice::new())?;
    let e = expect_err(s.verb("status", &[]), "RuntimeError")?;
    assert!(e.message.contains("sensor missing"));
    assert_eq!(
        s.of_kind("fail"),
        [Value::dict([("msg", "sensor missing".into())])]
    );
    assert!(s.tx().is_empty());
    Ok(())
}

#[test]
fn parser_exception_is_reported_and_next_frames_still_parsed() -> Result<()> {
    let body = "temp = Gauge(\"temp_c\", \"temperature\")\n@dev.on_frame\ndef strict(frame):\n    temp.set(float(frame.decode()))\n";
    let sim = SimDevice::new()
        .at(0.1, b"21.5\n")
        .at(0.2, b"garbage\n")
        .at(0.3, b"22\n");
    let mut s = Session::bare(body, sim)?;
    s.pump(1.0)?;
    assert_eq!(s.values("temp_c"), [21.5, 22.0]);
    let errs = s.of_kind("parse_error");
    assert_eq!(errs.len(), 1);
    assert_eq!(
        errs[0].get("frame").and_then(Value::as_str),
        Some(hex(b"garbage").as_str())
    );
    assert!(
        errs[0]
            .get("error")
            .and_then(Value::as_str)
            .ok_or_eyre("no error text")?
            .contains("ValueError")
    );
    Ok(())
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn fail_in_parser_is_reported_once() -> Result<()> {
    let body = "@dev.on_frame\ndef p(frame):\n    if frame == b\"PANIC\":\n        fail(\"device panic\")\n";
    let mut s = Session::bare(body, SimDevice::new().at(0.1, b"PANIC\n"))?;
    s.pump(1.0)?;
    assert_eq!(
        s.of_kind("fail"),
        [Value::dict([("msg", "device panic".into())])]
    );
    assert!(s.of_kind("parse_error").is_empty());
    Ok(())
}

#[test]
fn parser_cannot_wait() -> Result<()> {
    let body = "@dev.on_frame\ndef p(frame):\n    request(b\"x\", match=b\"y\", timeout=1)\n";
    let mut s = Session::bare(body, SimDevice::new().at(0.1, b"a\n"))?;
    s.pump(1.0)?;
    let errs = s.of_kind("parse_error");
    assert_eq!(errs.len(), 1);
    let msg = errs[0]
        .get("error")
        .and_then(Value::as_str)
        .ok_or_eyre("no error text")?;
    assert!(
        msg.contains("cannot be used inside an on_frame handler"),
        "{msg}"
    );
    assert!(s.tx().is_empty());
    Ok(())
}

#[test]
fn nested_request_is_rejected() -> Result<()> {
    let body = "@dev.verb(\"nested\")\ndef nested():\n    return request(b\"a\", match=lambda f: request(b\"b\", match=b\"x\", timeout=1) is not None, timeout=1)\n";
    let mut s = Session::new(body, SimDevice::new().on(Rule::prefix(b"a").reply(b"r\n")))?;
    // the inner request raises -> treated as "no match" -> the outer one times out
    expect_err(s.verb("nested", &[]), "TimeoutError")?;
    assert_eq!(s.tx(), frames(&[b"a"]));
    Ok(())
}

#[test]
fn every_runs_when_idle() -> Result<()> {
    let mut s = Session::new(
        "@dev.every(seconds=5)\ndef poll():\n    write(b\"status\\n\")\n",
        SimDevice::new(),
    )?;
    s.pump(16.0)?;
    assert_eq!(s.tx(), frames(&[b"status\n", b"status\n", b"status\n"]));
    Ok(())
}

#[test]
fn every_tick_during_request_is_deferred_not_lost() -> Result<()> {
    let body = r#"
@dev.every(seconds=1)
def poll():
    write(b"status\n")

@dev.verb("slow")
def slow():
    return request(b"slow\n", match=b"done", timeout=10)
"#;
    let mut s = Session::new(
        body,
        SimDevice::new().on(Rule::prefix(b"slow").reply_at(3.5, b"done\n")),
    )?;
    assert_eq!(s.verb("slow", &[])?, b(b"done"));
    assert_eq!(s.tx(), frames(&[b"slow\n"])); // ticks at 1, 2, 3 deferred and coalesced
    s.pump(0.1)?;
    assert_eq!(s.tx(), frames(&[b"slow\n", b"status\n"]));
    Ok(())
}

#[test]
fn timer_may_request_when_idle() -> Result<()> {
    let body = "ok = Gauge(\"status_ok\", \"1 when the status is ok\")\n@dev.every(seconds=2)\ndef poll():\n    r = request(b\"status\\n\", match=b\"st=\", timeout=1)\n    ok.set(r[3:] == b\"ok\")\n";
    let mut s = Session::bare(
        body,
        SimDevice::new().on(Rule::prefix(b"status").reply(b"st=ok\n")),
    )?;
    s.pump(5.0)?;
    assert_eq!(s.values("status_ok"), [1.0, 1.0]);
    Ok(())
}

#[test]
fn timer_errors_are_reported() -> Result<()> {
    let body = "@dev.every(seconds=1)\ndef poll():\n    request(b\"status\\n\", match=b\"st=\", timeout=0.5)\n";
    let mut s = Session::new(body, SimDevice::new())?;
    s.pump(1.5)?;
    let errs = s.of_kind("timer_error");
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].get("timer"), Some(&Value::Int(0)));
    assert!(
        errs[0]
            .get("error")
            .and_then(Value::as_str)
            .ok_or_eyre("no error text")?
            .contains("TimeoutError")
    );
    Ok(())
}

#[test]
fn reader_returns_one_value_per_call() -> Result<()> {
    let body = "temp = Gauge(\"temp_c\", \"temperature\")\n@dev.reader\ndef temps():\n    f = expect(b\"T=\", timeout=5)\n    v = float(f[2:].decode())\n    temp.set(v)\n    return v\n";
    let mut s = Session::bare(body, SimDevice::new().every(1.0, b"T=20\n"))?;
    assert_eq!(s.read()?, Value::Float(20.0));
    assert_eq!(s.read()?, Value::Float(20.0));
    assert_eq!(s.values("temp_c"), [20.0, 20.0]);
    assert_approx(s.now(), 2.0);
    Ok(())
}

#[test]
fn write_channel_and_data_helpers_cross_the_boundary() -> Result<()> {
    let body = r#"
@dev.verb("bin")
def bin_cmd():
    body = bytes_from_list([1, 2]) + pack("<H", [0x1234])
    write(slip_encode(body), channel="aux")
    return [unpack("<BBH", body), crc("crc16-ccitt", b"123456789"), cobs_decode(cobs_encode(b"\x00a"))]
"#;
    let mut s = Session::new(body, SimDevice::new())?;
    assert_eq!(
        s.verb("bin", &[])?,
        Value::List(vec![
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(0x1234)]),
            Value::Int(0x29B1),
            b(b"\x00a"),
        ])
    );
    assert_eq!(s.host().tx("aux"), frames(&[b"\xc0\x01\x02\x34\x12\xc0"]));
    Ok(())
}

#[test]
fn helper_errors_become_python_exceptions() -> Result<()> {
    let mut s = Session::new("", SimDevice::new())?;
    expect_err(s.eval("unpack('<H', b'\\x01')"), "ValueError")?;
    expect_err(s.eval("write(42)"), "TypeError")?;
    assert_eq!(
        s.eval(
            "r = None\ntry:\n    crc('nope', b'')\nexcept ValueError as e:\n    r = 'caught'\nr"
        )?,
        Value::from("caught")
    );
    Ok(())
}

#[test]
fn state_persists_between_calls() -> Result<()> {
    let body = "@dev.verb(\"inc\")\ndef inc():\n    state[\"n\"] = state.get(\"n\", 0) + 1\n    return state[\"n\"]\n";
    let mut s = Session::new(body, SimDevice::new())?;
    let got = (0..3)
        .map(|_| s.verb("inc", &[]))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(got, [Value::Int(1), Value::Int(2), Value::Int(3)]);
    Ok(())
}

#[test]
fn session_survives_errors() -> Result<()> {
    let body = "@dev.verb(\"boom\")\ndef boom():\n    return 1 / 0\n\n@dev.status\ndef status():\n    return \"ok\"\n";
    let mut s = Session::new(body, SimDevice::new())?;
    expect_err(s.verb("boom", &[]), "ZeroDivisionError")?;
    assert_eq!(s.verb("status", &[])?, Value::from("ok"));
    Ok(())
}

#[test]
fn runaway_code_is_stopped() -> Result<()> {
    let limits = ResourceLimits {
        max_feed_duration: Some(Duration::from_millis(200)),
        ..Default::default()
    };
    let mut runner = MontyRunner::with_limits(FakeHost::new(), limits);
    runner.load(&script(
        "@dev.verb(\"spin\")\ndef spin():\n    while True:\n        pass\n",
        false,
    ))?;
    let e = runner
        .call_verb("spin", &[])
        .err()
        .ok_or_eyre("runaway loop was not stopped")?;
    assert!(e.message.contains("time limit"), "{e}");
    Ok(())
}

#[test]
fn sandbox_blocks_io() -> Result<()> {
    let mut s = Session::new("", SimDevice::new())?;
    for code in [
        "open('/etc/passwd').read()",
        "import socket",
        "import subprocess",
    ] {
        assert!(s.eval(code).is_err(), "{code} allowed");
    }
    Ok(())
}
