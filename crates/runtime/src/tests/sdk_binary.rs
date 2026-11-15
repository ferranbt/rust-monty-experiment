//! `with_crc` / `check_crc`: the generic "body + CRC trailer" envelope.

use super::common::*;
use crate::helpers::{crc, pack};
use crate::{SimDevice, Value};
use eyre::{Result, WrapErr};

fn session() -> Result<Session> {
    let body = "from dut import with_crc, check_crc\n";
    Session::new(body, SimDevice::new())
}

fn eval(s: &mut Session, code: &str) -> Result<Value> {
    s.eval(code).wrap_err_with(|| code.to_string())
}

#[test]
fn with_crc_appends_little_endian_crc16_by_default() -> Result<()> {
    let mut s = session()?;
    let expected = [
        b"\x10\x02".as_slice(),
        &pack("<H", &[Value::Int(crc("crc16-ccitt", b"\x10\x02")? as i64)])?,
    ]
    .concat();
    assert_eq!(eval(&mut s, "with_crc(b'\\x10\\x02')")?, b(&expected));
    Ok(())
}

#[test]
fn check_crc_returns_body_on_match() -> Result<()> {
    let mut s = session()?;
    assert_eq!(eval(&mut s, "check_crc(with_crc(b'hello'))")?, b(b"hello"));
    Ok(())
}

#[test]
fn check_crc_rejects_bad_crc() -> Result<()> {
    let mut s = session()?;
    assert_eq!(
        eval(
            &mut s,
            "f = with_crc(b'hello')\ncheck_crc(b'jello' + f[-2:])"
        )?,
        Value::None
    );
    assert_eq!(
        eval(
            &mut s,
            "check_crc(f[:-1] + bytes.fromhex('%02x' % (f[-1] ^ 1)))"
        )?,
        Value::None
    );
    Ok(())
}

#[test]
fn check_crc_rejects_short_and_junk_frames() -> Result<()> {
    let mut s = session()?;
    for frame in ["b''", "b'\\x01'", "b'\\x01\\x02'", "b'\\xff\\xfe\\xfd'"] {
        assert_eq!(
            eval(&mut s, &format!("check_crc({frame})"))?,
            Value::None,
            "{frame}"
        );
    }
    Ok(())
}

#[test]
fn widths_and_byte_orders() -> Result<()> {
    let mut s = session()?;
    for (kind, fmt, len) in [
        ("crc8-maxim", "None", 1),
        ("crc16-modbus", "'>H'", 2),
        ("crc32", "None", 4),
        ("crc32", "'>I'", 4),
    ] {
        let code = format!(
            "f = with_crc(b'123456789', kind='{kind}', fmt={fmt})\n[len(f) - 9, check_crc(f, kind='{kind}', fmt={fmt})]"
        );
        assert_eq!(
            eval(&mut s, &code)?,
            Value::List(vec![Value::Int(len), b(b"123456789")]),
            "{kind} {fmt}"
        );
    }
    // big-endian trailer bytes really are big-endian
    let crc32 = crc("crc32", b"123456789")? as u32;
    assert_eq!(
        eval(
            &mut s,
            "with_crc(b'123456789', kind='crc32', fmt='>I')[-4:]"
        )?,
        b(&crc32.to_be_bytes())
    );
    // a wrong byte order fails the check
    assert_eq!(
        eval(&mut s, "check_crc(with_crc(b'ab', fmt='>H'), fmt='<H')")?,
        Value::None
    );
    Ok(())
}

#[test]
fn bad_arguments_raise() -> Result<()> {
    let mut s = session()?;
    for code in [
        "with_crc(b'x', fmt='<Q')",
        "with_crc(b'x', kind='nope')",
        "with_crc(b'x', kind='nope', fmt='<H')",
    ] {
        expect_err(s.eval(code), "ValueError")?;
    }
    Ok(())
}
