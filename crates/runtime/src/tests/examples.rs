//! The example device scripts, end to end in Monty.

use super::common::*;
use crate::{Rule, SimDevice, Value};
use eyre::Result;

fn sensor_sim() -> SimDevice {
    SimDevice::new()
        .chunked(3)
        .at(0.0, b"FW 1.4.2\n")
        .every(2.0, b"T=21.5\n")
        .on(Rule::prefix(b"radio ch ").delay(0.02).reply_with(|cmd| {
            [
                b"ch=",
                cmd.trim_ascii()
                    .rsplit(|b| *b == b' ')
                    .next()
                    .unwrap_or_default(),
                b"\n",
            ]
            .concat()
        }))
        .on(Rule::prefix(b"radio tx on").reply_at(0.2, b"[radio] TX started\n"))
        .on(Rule::prefix(b"reboot").reply_at(1.0, b"FW 1.4.2\n"))
        .on(Rule::prefix(b"status").reply(b"ERR overheat\n").once())
}

#[test]
fn sensor_v2() -> Result<()> {
    let mut s = Session::example("sensor_v2.py", sensor_sim())?;
    assert_eq!(s.registry.heartbeat_within, Some(10.0));
    assert_eq!(s.registry.channels.get("mid"), Some(&Value::Float(2440e6)));

    s.verb("set_channel", &[("channel", Value::Int(19))])?;
    s.verb("tx_on", &[])?;
    s.pump(6.0)?; // poll at t=5 -> ERR reply
    s.hook("recover", &[])?;
    s.pump(2.0)?;

    let fw = Value::dict([("fw", "1.4.2".into())]);
    assert_eq!(s.of_kind("identity"), [fw.clone(), fw]); // the reboot is visible
    assert!(s.values("temp_c").contains(&21.5));
    assert!(
        s.of_kind("heartbeat")
            .contains(&Value::dict([("ok", false.into())]))
    );
    assert_eq!(
        s.metric("errors"),
        [(1.0, vec![("code".to_string(), "overheat".to_string())])]
    );
    assert_eq!(
        s.tx(),
        frames(&[b"radio ch 19\n", b"radio tx on\n", b"status\n", b"reboot\n"])
    );
    assert!(s.of_kind("parse_error").is_empty());
    Ok(())
}

#[test]
fn sensor_v2_tx_on_times_out_if_radio_never_starts() -> Result<()> {
    let mut s = Session::example("sensor_v2.py", SimDevice::new())?;
    expect_err(s.verb("tx_on", &[]), "TimeoutError")?;
    Ok(())
}

/// A Pi's serial console: echoes each `echo "key=$(command)"` line, then prints `key=<output>`.
fn pi_sim(throttled: &'static str) -> SimDevice {
    SimDevice::new()
        .at(
            7.0,
            b"[   12.345] hwmon hwmon1: Under-voltage detected! (0x00050005)\r\n",
        )
        .on(Rule::prefix(b"echo \"").reply_with(move |typed| {
            let line = String::from_utf8_lossy(typed).trim_end().to_string();
            // echo "key=$(command)"
            let inner = &line["echo \"".len()..line.len() - ")\"".len()];
            let (key, command) = inner.split_once("=$(").unwrap_or(("", ""));
            let output = match command {
                "tr -d '\\0' < /proc/device-tree/model" => "Raspberry Pi 4 Model B Rev 1.4",
                "uname -r" => "6.6.31+rpt-rpi-v8",
                "grep -m1 Serial /proc/cpuinfo | cut -d' ' -f2" => "10000000abcdef01",
                "cat /sys/class/thermal/thermal_zone0/temp" => "48312",
                "vcgencmd get_throttled | cut -d= -f2" => throttled,
                _ => "0", // `run(...)`: exit code
            };
            format!("{line}\r\n{key}={output}\r\n").into_bytes()
        }))
}

#[test]
fn raspberry_pi() -> Result<()> {
    let mut s = Session::example("raspberry_pi.py", pi_sim("0x0"))?;
    s.hook("prepare", &[])?;
    assert_eq!(
        s.of_kind("identity"),
        [Value::dict([
            ("model", "Raspberry Pi 4 Model B Rev 1.4".into()),
            ("kernel", "6.6.31+rpt-rpi-v8".into()),
            ("serial", "10000000abcdef01".into()),
        ])]
    );
    assert_eq!(s.verb("status", &[])?, Value::Bool(true));
    s.pump(11.0)?; // telemetry at t=5 and t=10, kernel warning at t=7
    assert_eq!(s.values("soc_temp_c"), [48.312, 48.312]);
    assert_eq!(s.values("undervoltage"), [1.0]);
    assert!(s.of_kind("parse_error").is_empty() && s.of_kind("timer_error").is_empty());
    // every console line is logged, echoed commands included
    let logs = s.of_kind("log");
    assert!(logs.contains(&Value::dict([("text", "temp=48312".into())])));
    assert!(logs.contains(&Value::dict([(
        "text",
        "echo \"kernel=$(uname -r)\"".into()
    )])));
    Ok(())
}

#[test]
fn raspberry_pi_status_fails_when_throttled() -> Result<()> {
    let mut s = Session::example("raspberry_pi.py", pi_sim("0x50005"))?;
    let e = expect_err(s.verb("status", &[]), "RuntimeError")?;
    assert!(e.message.contains("throttled: 0x50005"), "{e}");
    Ok(())
}
