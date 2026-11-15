//! wait / request / expect / wait_until in Monty (SPEC §6.3, §6.5).
//! Every sugar function: matching reply, unsolicited frames still parsed, timeout, junk.

use super::common::*;
use crate::{Rule, SimDevice, Value};
use eyre::Result;

const VERBS: &str = r#"
@dev.verb("req")
def req(cmd, prefix, timeout=1.0):
    return request(cmd, match=lambda f: f.startswith(prefix), timeout=timeout)

@dev.verb("req_bytes")
def req_bytes(cmd, prefix):
    return request(cmd, match=prefix, timeout=1.0)

@dev.verb("exp")
def exp(prefix, timeout=1.0):
    return expect(prefix, timeout=timeout)

@dev.verb("until")
def until(key, timeout=1.0):
    return wait_until(lambda: state.get(key) == True, timeout=timeout)

@dev.verb("wait_for")
def wait_for(data, timeout=1.0):
    ev = wait(lambda e: e["kind"] == "frame" and e["data"] == data, timeout)
    if ev is None:
        return None
    return [ev["data"], ev["t"]]
"#;

const UNTIL_PARSER: &str = r#"
@dev.on_frame
def tx_state(frame):
    if frame == b"[radio] TX started":
        state["tx"] = True
"#;

fn req(cmd: &[u8], prefix: &[u8]) -> Vec<(&'static str, Value)> {
    vec![("cmd", b(cmd)), ("prefix", b(prefix))]
}

// -- request ------------------------------------------------------------------

#[test]
fn request_returns_matching_reply() -> Result<()> {
    let mut s = Session::new(
        VERBS,
        SimDevice::new().on(Rule::prefix(b"radio ch").reply(b"ch=19\n").delay(0.05)),
    )?;
    assert_eq!(s.verb("req", &req(b"radio ch 19\n", b"ch="))?, b(b"ch=19"));
    assert_eq!(s.tx(), frames(&[b"radio ch 19\n"]));
    assert_approx(s.now(), 0.05);
    Ok(())
}

#[test]
fn request_bytes_match_is_a_prefix() -> Result<()> {
    let mut s = Session::new(
        VERBS,
        SimDevice::new().on(Rule::prefix(b"v").reply(b"ver 1.2\n")),
    )?;
    assert_eq!(s.verb("req_bytes", &req(b"v\n", b"ver "))?, b(b"ver 1.2"));
    Ok(())
}

#[test]
fn request_unsolicited_frames_still_reach_parser() -> Result<()> {
    let sim = SimDevice::new().on(Rule::prefix(b"go")
        .reply(b"[log] boot\n")
        .reply(b"T=21\n")
        .reply_at(0.1, b"ok\n"));
    let mut s = Session::new(VERBS, sim)?;
    assert_eq!(s.verb("req", &req(b"go\n", b"ok"))?, b(b"ok"));
    assert_eq!(s.parsed(), frames(&[b"[log] boot", b"T=21", b"ok"])); // the reply too
    Ok(())
}

#[test]
fn request_timeout_raises_and_uses_virtual_time() -> Result<()> {
    let mut s = Session::new(
        VERBS,
        SimDevice::new().on(Rule::prefix(b"go").reply(b"nope\n")),
    )?;
    let mut args = req(b"go\n", b"ok");
    args.push(("timeout", Value::Float(30.0)));
    expect_err(s.verb("req", &args), "TimeoutError")?;
    assert_approx(s.now(), 30.0);
    assert_eq!(s.parsed(), frames(&[b"nope"]));
    Ok(())
}

#[test]
fn request_junk_frames_do_not_match_or_crash() -> Result<()> {
    let body = format!(
        "{VERBS}\n@dev.verb(\"bin\")\ndef bin_req():\n    return request(b\"q\", match=lambda f: f[0] == 0x21 and f[1] == 1, timeout=1)\n"
    );
    let sim = SimDevice::new().on(Rule::prefix(b"q")
        .reply(b"\n")
        .reply(b"\x21\n")
        .reply(b"\xff\xfe junk\n")
        .reply(b"\x21\x01\n"));
    let mut s = Session::new(&body, sim)?;
    assert_eq!(s.verb("bin", &[])?, b(b"\x21\x01"));
    assert_eq!(
        s.parsed(),
        frames(&[b"\x21", b"\xff\xfe junk", b"\x21\x01"])
    );
    Ok(())
}

#[test]
fn request_ignores_replies_on_other_channels() -> Result<()> {
    let body = "@dev.verb(\"aux\")\ndef aux():\n    return request(b\"q\", match=b\"ok\", timeout=1, channel=\"aux\")\n";
    let mut s = Session::new(
        body,
        SimDevice::new()
            .channel("aux")
            .on(Rule::prefix(b"q").reply(b"ok\n")),
    )?;
    s.runner.host_mut().deliver(b"ok-main\n", 0.001, "main");
    assert_eq!(s.verb("aux", &[])?, b(b"ok"));
    assert_eq!(s.host().tx("aux"), frames(&[b"q"]));
    assert!(s.tx().is_empty());
    assert_eq!(s.parsed(), frames(&[b"ok-main", b"ok"]));
    Ok(())
}

// -- expect -------------------------------------------------------------------

#[test]
fn expect_returns_frame() -> Result<()> {
    let mut s = Session::new(VERBS, SimDevice::new().at(0.2, b"READY\n"))?;
    assert_eq!(s.verb("exp", &[("prefix", "READY".into())])?, b(b"READY"));
    Ok(())
}

#[test]
fn expect_unsolicited_frames_reach_parser() -> Result<()> {
    let mut s = Session::new(
        VERBS,
        SimDevice::new()
            .at(0.1, b"x\n")
            .at(0.2, b"y\n")
            .at(0.3, b"READY\n"),
    )?;
    s.verb("exp", &[("prefix", b(b"READY"))])?;
    assert_eq!(s.parsed(), frames(&[b"x", b"y", b"READY"]));
    Ok(())
}

#[test]
fn expect_timeout() -> Result<()> {
    let mut s = Session::new(VERBS, SimDevice::new().at(0.5, b"later\n"))?;
    expect_err(
        s.verb(
            "exp",
            &[("prefix", b(b"READY")), ("timeout", Value::Float(2.0))],
        ),
        "TimeoutError",
    )?;
    assert_approx(s.now(), 2.0);
    assert_eq!(s.parsed(), frames(&[b"later"]));
    Ok(())
}

#[test]
fn expect_junk() -> Result<()> {
    let mut s = Session::new(
        VERBS,
        SimDevice::new()
            .at(0.1, b"\x00\xffREADY\n")
            .at(0.2, b"READY!\n"),
    )?;
    assert_eq!(s.verb("exp", &[("prefix", b(b"READY"))])?, b(b"READY!"));
    Ok(())
}

// -- wait_until ---------------------------------------------------------------

fn until_session(sim: SimDevice) -> Result<Session> {
    Session::new(&format!("{VERBS}{UNTIL_PARSER}"), sim)
}

#[test]
fn wait_until_condition_from_parser_state() -> Result<()> {
    let mut s = until_session(
        SimDevice::new().on(Rule::prefix(b"tx").reply_at(0.3, b"[radio] TX started\n")),
    )?;
    s.eval("write(b'tx on\\n')")?;
    assert_eq!(s.verb("until", &[("key", "tx".into())])?, Value::Bool(true));
    assert_approx(s.now(), 0.31);
    Ok(())
}

#[test]
fn wait_until_already_true_returns_immediately() -> Result<()> {
    let mut s = until_session(SimDevice::new())?;
    s.eval("state['tx'] = True")?;
    assert_eq!(s.verb("until", &[("key", "tx".into())])?, Value::Bool(true));
    assert_eq!(s.now(), 0.0);
    Ok(())
}

#[test]
fn wait_until_unsolicited_frames_reach_parser() -> Result<()> {
    let mut s = until_session(
        SimDevice::new()
            .at(0.1, b"T=1\n")
            .at(0.2, b"[radio] TX started\n"),
    )?;
    s.verb("until", &[("key", "tx".into())])?;
    assert_eq!(s.parsed(), frames(&[b"T=1", b"[radio] TX started"]));
    Ok(())
}

#[test]
fn wait_until_timeout() -> Result<()> {
    let mut s = until_session(SimDevice::new().every(1.0, b"T=1\n"))?;
    expect_err(
        s.verb(
            "until",
            &[("key", "tx".into()), ("timeout", Value::Float(5.0))],
        ),
        "TimeoutError",
    )?;
    assert_approx(s.now(), 5.0);
    assert_eq!(s.parsed().len(), 5);
    Ok(())
}

#[test]
fn wait_until_junk() -> Result<()> {
    let mut s = until_session(
        SimDevice::new()
            .at(0.1, b"\xff\xfe\n")
            .at(0.2, b"[radio] TX started\n"),
    )?;
    assert_eq!(s.verb("until", &[("key", "tx".into())])?, Value::Bool(true));
    Ok(())
}

// -- wait ---------------------------------------------------------------------

#[test]
fn wait_returns_event_and_time() -> Result<()> {
    let mut s = Session::new(VERBS, SimDevice::new().at(0.1, b"a\n").at(0.4, b"b\n"))?;
    assert_eq!(
        s.verb("wait_for", &[("data", b(b"b"))])?,
        Value::List(vec![b(b"b"), Value::Float(0.4)])
    );
    assert_eq!(s.parsed(), frames(&[b"a", b"b"]));
    Ok(())
}

#[test]
fn wait_timeout_returns_none() -> Result<()> {
    let mut s = Session::new(VERBS, SimDevice::new())?;
    assert_eq!(
        s.verb(
            "wait_for",
            &[("data", b(b"never")), ("timeout", Value::Float(3.0))]
        )?,
        Value::None
    );
    assert_eq!(s.now(), 3.0);
    Ok(())
}

#[test]
fn wait_junk() -> Result<()> {
    let mut s = Session::new(
        VERBS,
        SimDevice::new().at(0.1, b"\x00\x01\x02\n").at(0.2, b"b\n"),
    )?;
    assert_eq!(
        s.verb("wait_for", &[("data", b(b"b"))])?,
        Value::List(vec![b(b"b"), Value::Float(0.2)])
    );
    Ok(())
}

// -- §6.5 reply-handling patterns --------------------------------------------

#[test]
fn pattern_reply_with_request_id() -> Result<()> {
    let body = r#"
seq = {"n": 0}

@dev.verb("get")
def get():
    seq["n"] += 1
    my_id = seq["n"]
    reply = request(f"{my_id} get\n".encode(), match=lambda f: f.split(b" ")[0] == str(my_id).encode(),
                    timeout=1)
    return reply.split(b" ")[1]
"#;
    // a stale reply for id 7 arrives first and must be ignored
    let sim = SimDevice::new().on(Rule::prefix(b"1 get")
        .reply(b"7 stale\n")
        .reply_at(0.01, b"1 fresh\n"));
    let mut s = Session::new(body, sim)?;
    assert_eq!(s.verb("get", &[])?, b(b"fresh"));
    assert_eq!(s.parsed(), frames(&[b"7 stale", b"1 fresh"]));
    Ok(())
}

#[test]
fn pattern_echo_then_reply() -> Result<()> {
    let body = r#"
@dev.verb("set")
def set_level(level):
    cmd = f"level {level}".encode()
    request(cmd + b"\n", match=lambda f: f == cmd, timeout=1)   # skip the echo
    return expect(b"OK", timeout=1)
"#;
    let sim = SimDevice::new()
        .echo()
        .on(Rule::prefix(b"level").reply_at(0.05, b"OK\n"));
    let mut s = Session::new(body, sim)?;
    assert_eq!(s.verb("set", &[("level", Value::Int(3))])?, b(b"OK"));
    assert_eq!(s.parsed(), frames(&[b"level 3", b"OK"]));
    Ok(())
}

#[test]
fn pattern_silent_device_declares_confirm() -> Result<()> {
    let body = "@dev.tx_on(confirm=\"rf_power\")\ndef tx_on():\n    write(b\"\\x01\")\n";
    let mut s = Session::new(body, SimDevice::new())?;
    let info = &s.registry.verbs["tx_on"];
    assert_eq!(
        (info.confirm.as_deref(), info.standard),
        (Some("rf_power"), true)
    );
    assert_eq!(s.verb("tx_on", &[])?, Value::None);
    assert_eq!(s.tx(), frames(&[b"\x01"]));
    Ok(())
}
