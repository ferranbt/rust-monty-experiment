//! The SPEC §6.4 text device and §6.6 binary device, end to end in Monty.

use super::common::*;
use crate::helpers::{cobs_decode, cobs_encode, crc, pack};
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
    assert!(
        s.of_kind("record")
            .contains(&Value::dict([("temp_c", 21.5.into())]))
    );
    assert!(
        s.of_kind("heartbeat")
            .contains(&Value::dict([("ok", false.into())]))
    );
    assert_eq!(
        s.of_kind("event"),
        [Value::dict([
            ("text", "ERR overheat".into()),
            ("name", "error".into())
        ])]
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

fn meter_frame(op: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let mut body = vec![op, payload.len() as u8];
    body.extend(payload);
    let crc = crc("crc16-ccitt", &body)? as i64;
    body.extend(pack("<H", &[Value::Int(crc)])?);
    Ok(cobs_encode(&body))
}

#[test]
fn meter_x() -> Result<()> {
    let telemetry = meter_frame(0x10, &pack("<hH", &[Value::Int(-1250), Value::Int(3300)])?)?;
    let mut corrupt = telemetry.clone();
    corrupt[3] ^= 0x01;
    assert!(
        !corrupt[..corrupt.len() - 1].contains(&0),
        "corruption must keep COBS valid"
    );
    let sim = SimDevice::new()
        .chunked(4)
        .at(0.1, &telemetry)
        .at(0.2, &corrupt)
        .at(0.3, b"\x05\x11\x00") // undecodable COBS junk
        .at(0.4, b"\x02\x01\x00") // decodes to a 1-byte frame -> too short for a CRC
        .on(
            Rule::when(|d| cobs_decode(d).is_ok_and(|b| b.first() == Some(&0x21)))
                .reply(&meter_frame(0x21, b"\x01")?),
        );
    let mut s = Session::example("meter_x.py", sim)?;

    s.pump(1.0)?;
    assert_eq!(
        s.of_kind("record"),
        [Value::dict([
            ("temp_c", Value::Float(-12.5)),
            ("vbat_mv", Value::Int(3300))
        ])]
    );
    assert_eq!(s.of_kind("heartbeat"), [Value::dict([("ok", true.into())])]);
    let crc_error = Value::dict([("name", "crc_error".into())]);
    assert_eq!(s.of_kind("event"), [crc_error.clone(), crc_error]); // corrupt + short frame
    assert_eq!(s.of_kind("framing_error").len(), 1);
    assert!(s.of_kind("parse_error").is_empty());

    s.verb("tx_on", &[])?;
    let tx = s.tx();
    assert_eq!(tx.len(), 1);
    assert_eq!(tx[0].last(), Some(&0));
    let body = cobs_decode(&tx[0])?;
    assert_eq!(&body[..body.len() - 2], b"\x21\x01\x01");
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
    let temp = Value::dict([("soc_temp_c", 48.312.into())]);
    assert_eq!(s.of_kind("record"), [temp.clone(), temp]);
    assert_eq!(
        s.of_kind("event"),
        [Value::dict([
            (
                "text",
                "[   12.345] hwmon hwmon1: Under-voltage detected! (0x00050005)".into()
            ),
            ("name", "undervoltage".into()),
        ])]
    );
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
