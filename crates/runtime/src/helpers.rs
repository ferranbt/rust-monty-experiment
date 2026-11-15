//! Data-only helpers exposed to the sandbox (SPEC §6.1): `pack`, `unpack`,
//! `crc`, `cobs_*`, `slip_*`, `bytes_from_list`.
//!
//! Pure functions, no host state, so they are not part of the [`Host`](crate::Host)
//! trait: every host gets the same implementation.

use std::fmt;

use crate::host::Value;

/// Bad input to a data helper. Raised in the sandbox as `ValueError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataError(pub String);

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DataError {}

impl From<String> for DataError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DataError {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}

// ---------------------------------------------------------------------------
// pack / unpack (struct-style, standard sizes, no alignment)
// ---------------------------------------------------------------------------

struct Fmt {
    little: bool,
    items: Vec<(char, usize)>, // (code, count)
}

fn parse_fmt(fmt: &str) -> Result<Fmt, DataError> {
    let mut chars = fmt.chars().peekable();
    let little = match chars.next() {
        Some('<') | Some('=') => true,
        Some('>') | Some('!') => false,
        _ => return Err("format must start with a byte-order char (<, >, ! or =)".into()),
    };
    let mut items = Vec::new();
    while let Some(c) = chars.next() {
        let mut count = None;
        let mut c = c;
        while c.is_ascii_digit() {
            count = Some(count.unwrap_or(0) * 10 + (c as u8 - b'0') as usize);
            c = chars.next().ok_or("format ends with a repeat count")?;
        }
        if !"xbBhHiIlLqQfd?s".contains(c) {
            return Err(format!("unsupported format character {c:?}").into());
        }
        items.push((c, count.unwrap_or(1)));
    }
    if items.is_empty() {
        return Err("empty format".into());
    }
    Ok(Fmt { little, items })
}

fn size_of(code: char) -> usize {
    match code {
        'x' | 'b' | 'B' | '?' | 's' => 1,
        'h' | 'H' => 2,
        'i' | 'I' | 'l' | 'L' | 'f' => 4,
        _ => 8, // q Q d
    }
}

fn put(out: &mut Vec<u8>, bytes: &[u8], little: bool) {
    if little {
        out.extend(bytes.iter().rev());
    } else {
        out.extend(bytes);
    }
}

pub fn pack(fmt: &str, values: &[Value]) -> Result<Vec<u8>, DataError> {
    let f = parse_fmt(fmt)?;
    let mut out = Vec::new();
    let mut vals = values.iter();
    let mut next = |code: char| {
        vals.next()
            .ok_or_else(|| DataError(format!("pack: not enough values for {code:?}")))
    };
    for (code, count) in f.items {
        match code {
            'x' => out.extend(std::iter::repeat_n(0u8, count)),
            's' => {
                let Value::Bytes(b) = next(code)? else {
                    return Err("pack: 's' needs bytes".into());
                };
                let mut b = b.clone();
                b.resize(count, 0);
                out.extend(b);
            }
            _ => {
                for _ in 0..count {
                    let v = next(code)?;
                    match code {
                        'f' | 'd' => {
                            let x = v.as_float().ok_or("pack: expected a number")?;
                            if code == 'f' {
                                put(&mut out, &(x as f32).to_be_bytes(), f.little);
                            } else {
                                put(&mut out, &x.to_be_bytes(), f.little);
                            }
                        }
                        _ => {
                            let x = match v {
                                Value::Bool(b) => *b as i64,
                                Value::Int(i) => *i,
                                _ => return Err(format!("pack: {code:?} needs an int").into()),
                            };
                            let size = size_of(code);
                            let signed = code.is_ascii_lowercase() && code != '?';
                            let bits = size as u32 * 8;
                            let (lo, hi) = if signed {
                                (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
                            } else {
                                (0, (1i128 << bits) - 1)
                            };
                            if (x as i128) < lo || (x as i128) > hi {
                                return Err(format!("pack: {x} out of range for {code:?}").into());
                            }
                            let be = (x as i128 as u128).to_be_bytes();
                            put(&mut out, &be[16 - size..], f.little);
                        }
                    }
                }
            }
        }
    }
    if vals.next().is_some() {
        return Err("pack: too many values".into());
    }
    Ok(out)
}

pub fn unpack(fmt: &str, data: &[u8]) -> Result<Vec<Value>, DataError> {
    let f = parse_fmt(fmt)?;
    let total: usize = f.items.iter().map(|(c, n)| size_of(*c) * n).sum();
    if total != data.len() {
        return Err(format!("unpack: format needs {total} bytes, got {}", data.len()).into());
    }
    let mut out = Vec::new();
    let mut pos = 0;
    for (code, count) in f.items {
        if code == 'x' {
            pos += count;
            continue;
        }
        if code == 's' {
            out.push(Value::Bytes(data[pos..pos + count].to_vec()));
            pos += count;
            continue;
        }
        for _ in 0..count {
            let size = size_of(code);
            let mut be = data[pos..pos + size].to_vec();
            if f.little {
                be.reverse();
            }
            pos += size;
            let mut raw = 0u64;
            for b in &be {
                raw = (raw << 8) | *b as u64;
            }
            out.push(match code {
                'f' => Value::Float(f32::from_bits(raw as u32) as f64),
                'd' => Value::Float(f64::from_bits(raw)),
                '?' => Value::Bool(raw != 0),
                'Q' if raw > i64::MAX as u64 => {
                    return Err("unpack: 'Q' value exceeds 2**63-1".into());
                }
                c if c.is_ascii_lowercase() => {
                    let shift = 64 - size * 8;
                    Value::Int(((raw << shift) as i64) >> shift)
                }
                _ => Value::Int(raw as i64),
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// CRC
// ---------------------------------------------------------------------------

struct CrcSpec {
    width: u32,
    poly: u64,
    init: u64,
    refin: bool,
    refout: bool,
    xorout: u64,
}

const CRCS: &[(&str, CrcSpec)] = &[
    (
        "crc8",
        CrcSpec {
            width: 8,
            poly: 0x07,
            init: 0,
            refin: false,
            refout: false,
            xorout: 0,
        },
    ),
    (
        "crc8-maxim",
        CrcSpec {
            width: 8,
            poly: 0x31,
            init: 0,
            refin: true,
            refout: true,
            xorout: 0,
        },
    ),
    // "crc16-ccitt" means CCITT-FALSE.
    (
        "crc16-ccitt",
        CrcSpec {
            width: 16,
            poly: 0x1021,
            init: 0xFFFF,
            refin: false,
            refout: false,
            xorout: 0,
        },
    ),
    (
        "crc16-ccitt-false",
        CrcSpec {
            width: 16,
            poly: 0x1021,
            init: 0xFFFF,
            refin: false,
            refout: false,
            xorout: 0,
        },
    ),
    (
        "crc16-xmodem",
        CrcSpec {
            width: 16,
            poly: 0x1021,
            init: 0,
            refin: false,
            refout: false,
            xorout: 0,
        },
    ),
    (
        "crc16-kermit",
        CrcSpec {
            width: 16,
            poly: 0x1021,
            init: 0,
            refin: true,
            refout: true,
            xorout: 0,
        },
    ),
    (
        "crc16-modbus",
        CrcSpec {
            width: 16,
            poly: 0x8005,
            init: 0xFFFF,
            refin: true,
            refout: true,
            xorout: 0,
        },
    ),
    (
        "crc32",
        CrcSpec {
            width: 32,
            poly: 0x04C1_1DB7,
            init: 0xFFFF_FFFF,
            refin: true,
            refout: true,
            xorout: 0xFFFF_FFFF,
        },
    ),
];

fn reflect(mut v: u64, width: u32) -> u64 {
    let mut r = 0;
    for _ in 0..width {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    r
}

pub fn crc(kind: &str, data: &[u8]) -> Result<u64, DataError> {
    let (_, s) = CRCS.iter().find(|(name, _)| *name == kind).ok_or_else(|| {
        let names: Vec<_> = CRCS.iter().map(|(n, _)| *n).collect();
        format!("crc: unknown kind {kind:?}, expected one of {names:?}")
    })?;
    let top = 1u64 << (s.width - 1);
    let mask = if s.width == 64 {
        u64::MAX
    } else {
        (1u64 << s.width) - 1
    };
    let mut reg = s.init;
    for &b in data {
        let b = if s.refin {
            reflect(b as u64, 8)
        } else {
            b as u64
        };
        reg ^= b << (s.width - 8);
        for _ in 0..8 {
            reg = if reg & top != 0 {
                (reg << 1) ^ s.poly
            } else {
                reg << 1
            } & mask;
        }
    }
    if s.refout {
        reg = reflect(reg, s.width);
    }
    Ok(reg ^ s.xorout)
}

// ---------------------------------------------------------------------------
// COBS / SLIP
// ---------------------------------------------------------------------------

/// COBS-encode `data` and append the 0x00 frame delimiter.
pub fn cobs_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 254 + 2);
    let mut block: Vec<u8> = Vec::with_capacity(254);
    for &b in data {
        if b == 0 {
            out.push(block.len() as u8 + 1);
            out.append(&mut block);
        } else {
            block.push(b);
            if block.len() == 254 {
                out.push(255);
                out.append(&mut block);
            }
        }
    }
    out.push(block.len() as u8 + 1);
    out.append(&mut block);
    out.push(0);
    out
}

/// Decode one COBS frame. A trailing 0x00 delimiter is accepted.
pub fn cobs_decode(data: &[u8]) -> Result<Vec<u8>, DataError> {
    let data = data.strip_suffix(&[0]).unwrap_or(data);
    if data.contains(&0) {
        return Err("cobs_decode: unexpected 0x00 inside frame".into());
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let code = data[i] as usize;
        let end = i + code;
        if end > data.len() {
            return Err("cobs_decode: truncated frame".into());
        }
        out.extend_from_slice(&data[i + 1..end]);
        i = end;
        if code < 255 && i < data.len() {
            out.push(0);
        }
    }
    Ok(out)
}

const SLIP_END: u8 = 0xC0;
const SLIP_ESC: u8 = 0xDB;
const SLIP_ESC_END: u8 = 0xDC;
const SLIP_ESC_ESC: u8 = 0xDD;

/// SLIP-encode `data`, wrapped in END bytes on both sides.
pub fn slip_encode(data: &[u8]) -> Vec<u8> {
    let mut out = vec![SLIP_END];
    for &b in data {
        match b {
            SLIP_END => out.extend([SLIP_ESC, SLIP_ESC_END]),
            SLIP_ESC => out.extend([SLIP_ESC, SLIP_ESC_ESC]),
            _ => out.push(b),
        }
    }
    out.push(SLIP_END);
    out
}

/// Decode one SLIP frame. Leading/trailing END bytes are accepted.
pub fn slip_decode(data: &[u8]) -> Result<Vec<u8>, DataError> {
    let start = data
        .iter()
        .position(|&b| b != SLIP_END)
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|&b| b != SLIP_END)
        .map_or(start, |i| i + 1);
    let mut out = Vec::new();
    let mut esc = false;
    for &b in &data[start..end] {
        if esc {
            out.push(match b {
                SLIP_ESC_END => SLIP_END,
                SLIP_ESC_ESC => SLIP_ESC,
                _ => return Err("slip_decode: invalid escape sequence".into()),
            });
            esc = false;
        } else if b == SLIP_ESC {
            esc = true;
        } else if b == SLIP_END {
            return Err("slip_decode: END byte inside frame".into());
        } else {
            out.push(b);
        }
    }
    if esc {
        return Err("slip_decode: truncated escape sequence".into());
    }
    Ok(out)
}

pub fn bytes_from_list(values: &[Value]) -> Result<Vec<u8>, DataError> {
    values
        .iter()
        .map(|v| match v {
            Value::Int(i) if (0..=255).contains(i) => Ok(*i as u8),
            other => Err(format!("bytes_from_list: {other:?} is not a byte (0..255)").into()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use eyre::{OptionExt, Result};

    use super::*;

    #[test]
    fn crc_check_values() -> Result<()> {
        for (kind, expected) in [
            ("crc8", 0xF4),
            ("crc8-maxim", 0xA1),
            ("crc16-ccitt", 0x29B1),
            ("crc16-ccitt-false", 0x29B1),
            ("crc16-xmodem", 0x31C3),
            ("crc16-kermit", 0x2189),
            ("crc16-modbus", 0x4B37),
            ("crc32", 0xCBF4_3926),
        ] {
            assert_eq!(crc(kind, b"123456789")?, expected, "{kind}");
        }
        let err = crc("crc99", b"x")
            .err()
            .ok_or_eyre("unknown CRC kind accepted")?;
        assert!(err.0.contains("unknown kind"));
        Ok(())
    }

    #[test]
    fn cobs_roundtrip() -> Result<()> {
        let long: Vec<u8> = (0..=255u8).cycle().take(768).collect();
        let no_zero: Vec<u8> = (1..=254u8).collect();
        for data in [
            &b""[..],
            b"\x00",
            b"\x00\x00",
            b"\x11\x22\x00\x33",
            &long,
            &no_zero,
            &[1u8; 254],
        ] {
            let enc = cobs_encode(data);
            let (last, body) = enc.split_last().ok_or_eyre("empty encoding")?;
            assert_eq!(*last, 0);
            assert!(!body.contains(&0));
            assert_eq!(cobs_decode(&enc)?, data);
            assert_eq!(cobs_decode(body)?, data);
        }
        assert_eq!(
            cobs_encode(b"\x11\x22\x00\x33"),
            b"\x03\x11\x22\x02\x33\x00"
        );
        Ok(())
    }

    #[test]
    fn cobs_rejects_junk() {
        assert!(cobs_decode(b"\x05\x11").is_err());
        assert!(cobs_decode(b"\x03\x00\x01").is_err());
    }

    #[test]
    fn slip_roundtrip_and_junk() -> Result<()> {
        let all: Vec<u8> = (0..=255u8).collect();
        for data in [&b""[..], b"abc", b"\xc0\xdb\xc0", &all] {
            let enc = slip_encode(data);
            assert_eq!(
                (enc.first(), enc.last()),
                (Some(&SLIP_END), Some(&SLIP_END))
            );
            assert!(!enc[1..enc.len() - 1].contains(&SLIP_END));
            assert_eq!(slip_decode(&enc)?, data);
        }
        assert!(slip_decode(b"\xdb\x01").is_err());
        assert!(slip_decode(b"ab\xdb").is_err());
        Ok(())
    }

    #[test]
    fn pack_unpack() -> Result<()> {
        let v = [Value::Int(1), Value::Int(0x1234), Value::Int(-2)];
        assert_eq!(pack("<BHh", &v)?, b"\x01\x34\x12\xfe\xff");
        assert_eq!(unpack("<BHh", b"\x01\x34\x12\xfe\xff")?, v);
        assert_eq!(unpack(">I", b"\x00\x00\x01\x00")?, [Value::Int(256)]);
        assert_eq!(pack(">2B", &[Value::Int(1), Value::Int(2)])?, b"\x01\x02");
        assert_eq!(unpack("<3sx", b"abc\x00")?, [Value::Bytes(b"abc".to_vec())]);
        assert_eq!(
            unpack("<d", &pack("<d", &[Value::Float(1.5)])?)?,
            [Value::Float(1.5)]
        );
        Ok(())
    }

    #[test]
    fn pack_errors() -> Result<()> {
        for fmt in ["BH", "<P", "<e", ""] {
            assert!(pack(fmt, &[Value::Int(1)]).is_err(), "{fmt}");
        }
        let err = pack("<B", &[Value::Int(256)])
            .err()
            .ok_or_eyre("256 packed into a byte")?;
        assert!(err.0.contains("out of range"));
        assert!(pack("<B", &[]).is_err());
        assert!(pack("<B", &[Value::Int(1), Value::Int(2)]).is_err());
        assert!(unpack("<H", b"\x01").is_err());
        Ok(())
    }

    #[test]
    fn bytes_from_list_checks_range() -> Result<()> {
        assert_eq!(
            bytes_from_list(&[Value::Int(0), Value::Int(255)])?,
            b"\x00\xff"
        );
        assert!(bytes_from_list(&[Value::Int(256)]).is_err());
        Ok(())
    }
}
