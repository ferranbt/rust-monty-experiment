//! Host-side framing (SPEC §6.2): turns a byte stream into frames.
//!
//! A framer is fed raw chunks and returns frames plus error strings. Errors
//! never lose data: the bytes are already in the raw log.

use std::collections::HashMap;

use crate::helpers::{DataError, cobs_decode, slip_decode};
use crate::host::{Event, EventKind, Framing, Seconds, SignalKind, Value};
use crate::record::Signal;

#[derive(Debug, Default, PartialEq)]
pub struct Fed {
    pub frames: Vec<Vec<u8>>,
    pub errors: Vec<String>,
}

pub trait Framer: Send {
    fn feed(&mut self, data: &[u8]) -> Fed;
}

/// One framer per channel, all using the script's declared framing.
/// Shared by every host so frames and `framing_error` signals look the same everywhere.
pub struct ChannelFramers {
    framing: Framing,
    framers: HashMap<String, Box<dyn Framer>>,
}

/// What one received chunk produced.
#[derive(Debug, Default, PartialEq)]
pub struct Deframed {
    pub events: Vec<Event>,
    /// `framing_error` signals for bytes that could not be framed.
    pub errors: Vec<Signal>,
}

impl Default for ChannelFramers {
    /// Text lines until the script declares its framing.
    fn default() -> Self {
        Self::new(Framing::Delimiter {
            delimiter: b"\n".to_vec(),
            keep_empty: false,
            max_len: 4096,
        })
    }
}

impl ChannelFramers {
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            framers: HashMap::new(),
        }
    }

    /// Switch framing; partial frames buffered under the old framing are dropped.
    pub fn reset(&mut self, framing: &Framing) {
        self.framing = framing.clone();
        self.framers.clear();
    }

    /// Feed bytes received on `channel` at host time `t`.
    pub fn feed(&mut self, channel: &str, data: &[u8], t: Seconds) -> Deframed {
        let framing = &self.framing;
        let framer = self
            .framers
            .entry(channel.to_string())
            .or_insert_with(|| make_framer(framing));
        let fed = framer.feed(data);
        Deframed {
            events: fed
                .frames
                .into_iter()
                .map(|frame| Event {
                    kind: EventKind::Frame(frame),
                    channel: Some(channel.to_string()),
                    t,
                })
                .collect(),
            errors: fed
                .errors
                .into_iter()
                .map(|error| Signal {
                    t,
                    kind: SignalKind::FramingError,
                    payload: Value::dict([("channel", channel.into()), ("error", error.into())]),
                })
                .collect(),
        }
    }
}

pub fn make_framer(spec: &Framing) -> Box<dyn Framer> {
    match spec.clone() {
        Framing::Delimiter {
            delimiter,
            keep_empty,
            max_len,
        } => Box::new(Delimited {
            delim: delimiter,
            max_len,
            keep_empty,
            decode: None,
            buf: vec![],
        }),
        Framing::Cobs { max_len } => Box::new(Delimited::decoded(vec![0x00], max_len, cobs_decode)),
        Framing::Slip { max_len } => Box::new(Delimited::decoded(vec![0xC0], max_len, slip_decode)),
        Framing::Hdlc { max_len } => {
            Box::new(Delimited::decoded(vec![0x7E], max_len, hdlc_unescape))
        }
        Framing::LengthPrefixed {
            header,
            length_offset,
            length_size,
            trailer_len,
            big_endian,
            max_len,
        } => Box::new(LengthPrefixed {
            header,
            length_offset,
            length_size,
            trailer_len,
            big_endian,
            max_len,
            buf: vec![],
        }),
        Framing::Fixed { size } => Box::new(Fixed { size, buf: vec![] }),
        Framing::Raw => Box::new(Raw),
    }
}

type Decode = fn(&[u8]) -> Result<Vec<u8>, DataError>;

struct Delimited {
    delim: Vec<u8>,
    max_len: usize,
    keep_empty: bool,
    decode: Option<Decode>,
    buf: Vec<u8>,
}

impl Delimited {
    fn decoded(delim: Vec<u8>, max_len: usize, decode: Decode) -> Self {
        Self {
            delim,
            max_len,
            keep_empty: false,
            decode: Some(decode),
            buf: vec![],
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

impl Framer for Delimited {
    fn feed(&mut self, data: &[u8]) -> Fed {
        let mut out = Fed::default();
        self.buf.extend_from_slice(data);
        while let Some(i) = find(&self.buf, &self.delim) {
            let chunk: Vec<u8> = self.buf.drain(..i + self.delim.len()).take(i).collect();
            if chunk.is_empty() && !self.keep_empty {
                continue;
            }
            if chunk.len() > self.max_len {
                out.errors.push(format!(
                    "frame longer than max_len={}, dropped",
                    self.max_len
                ));
                continue;
            }
            match self.decode {
                None => out.frames.push(chunk),
                Some(decode) => match decode(&chunk) {
                    Ok(frame) => out.frames.push(frame),
                    Err(e) => out.errors.push(e.to_string()),
                },
            }
        }
        if self.buf.len() > self.max_len {
            out.errors.push(format!(
                "no delimiter within max_len={}, {} bytes dropped",
                self.max_len,
                self.buf.len()
            ));
            self.buf.clear();
        }
        out
    }
}

fn hdlc_unescape(chunk: &[u8]) -> Result<Vec<u8>, DataError> {
    let mut out = Vec::with_capacity(chunk.len());
    let mut esc = false;
    for &b in chunk {
        if esc {
            out.push(b ^ 0x20);
            esc = false;
        } else if b == 0x7D {
            esc = true;
        } else {
            out.push(b);
        }
    }
    if esc {
        return Err("hdlc: truncated escape sequence".into());
    }
    Ok(out)
}

/// Delivers whole frames, header and length field included.
struct LengthPrefixed {
    header: Vec<u8>,
    length_offset: usize,
    length_size: usize,
    trailer_len: usize,
    big_endian: bool,
    max_len: usize,
    buf: Vec<u8>,
}

impl Framer for LengthPrefixed {
    fn feed(&mut self, data: &[u8]) -> Fed {
        let mut out = Fed::default();
        self.buf.extend_from_slice(data);
        loop {
            if !self.header.is_empty() {
                match find(&self.buf, &self.header) {
                    Some(0) => {}
                    Some(i) => {
                        out.errors.push(format!("{i} bytes before header skipped"));
                        self.buf.drain(..i);
                    }
                    None => {
                        // keep a possible partial header at the tail
                        let junk = self.buf.len().saturating_sub(self.header.len() - 1);
                        if junk > 0 {
                            out.errors
                                .push(format!("{junk} bytes before header skipped"));
                            self.buf.drain(..junk);
                        }
                        break;
                    }
                }
            }
            let need = self.length_offset + self.length_size;
            if self.buf.len() < need {
                break;
            }
            let field = &self.buf[self.length_offset..need];
            let length = if self.big_endian {
                field.iter().fold(0usize, |acc, b| (acc << 8) | *b as usize)
            } else {
                field
                    .iter()
                    .rev()
                    .fold(0usize, |acc, b| (acc << 8) | *b as usize)
            };
            let total = need + length + self.trailer_len;
            if total > self.max_len {
                out.errors.push(format!(
                    "declared frame length {total} > max_len={}",
                    self.max_len
                ));
                if self.header.is_empty() {
                    self.buf.clear();
                    break;
                }
                self.buf.drain(..1); // resync on the next header
                continue;
            }
            if self.buf.len() < total {
                break;
            }
            out.frames.push(self.buf.drain(..total).collect());
        }
        out
    }
}

struct Fixed {
    size: usize,
    buf: Vec<u8>,
}

impl Framer for Fixed {
    fn feed(&mut self, data: &[u8]) -> Fed {
        self.buf.extend_from_slice(data);
        let mut out = Fed::default();
        while self.buf.len() >= self.size {
            out.frames.push(self.buf.drain(..self.size).collect());
        }
        out
    }
}

struct Raw;

impl Framer for Raw {
    fn feed(&mut self, data: &[u8]) -> Fed {
        let frames = if data.is_empty() {
            vec![]
        } else {
            vec![data.to_vec()]
        };
        Fed {
            frames,
            errors: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{cobs_encode, slip_encode};

    fn feed_all(f: &mut dyn Framer, chunks: impl IntoIterator<Item = Vec<u8>>) -> Fed {
        let mut all = Fed::default();
        for c in chunks {
            let fed = f.feed(&c);
            all.frames.extend(fed.frames);
            all.errors.extend(fed.errors);
        }
        all
    }

    fn bytewise(data: &[u8], n: usize) -> Vec<Vec<u8>> {
        data.chunks(n).map(<[u8]>::to_vec).collect()
    }

    fn delim(d: &[u8], max_len: usize, keep_empty: bool) -> Box<dyn Framer> {
        make_framer(&Framing::Delimiter {
            delimiter: d.to_vec(),
            keep_empty,
            max_len,
        })
    }

    fn lp(
        header: &[u8],
        off: usize,
        size: usize,
        trailer: usize,
        big: bool,
        max_len: usize,
    ) -> Box<dyn Framer> {
        make_framer(&Framing::LengthPrefixed {
            header: header.to_vec(),
            length_offset: off,
            length_size: size,
            trailer_len: trailer,
            big_endian: big,
            max_len,
        })
    }

    #[test]
    fn delimiter_split_across_chunks() {
        let mut f = delim(b"\r\n", 4096, false);
        let fed = feed_all(f.as_mut(), bytewise(b"a=1\r\n\r\nb=2\r\nparti", 1));
        assert_eq!(fed.frames, [b"a=1".to_vec(), b"b=2".to_vec()]);
        assert!(fed.errors.is_empty());
        assert_eq!(f.feed(b"al\r\n").frames, [b"partial".to_vec()]);
    }

    #[test]
    fn delimiter_keep_empty() {
        assert_eq!(
            delim(b"\n", 4096, true).feed(b"\n\nx\n").frames,
            [vec![], vec![], b"x".to_vec()]
        );
    }

    #[test]
    fn delimiter_overflow_is_reported_and_dropped() {
        let mut f = delim(b"\n", 8, false);
        let fed = feed_all(f.as_mut(), [b"0123456789ab".to_vec(), b"ok\n".to_vec()]);
        assert_eq!(fed.frames, [b"ok".to_vec()]);
        assert_eq!(fed.errors.len(), 1);
    }

    #[test]
    fn cobs_frames_and_junk() {
        let mut f = make_framer(&Framing::Cobs { max_len: 4096 });
        let mut stream = cobs_encode(b"\x10\x00\x01");
        stream.extend(b"\x05\x11\x00");
        stream.extend(cobs_encode(b"\x00"));
        let fed = feed_all(f.as_mut(), bytewise(&stream, 2));
        assert_eq!(fed.frames, [b"\x10\x00\x01".to_vec(), b"\x00".to_vec()]);
        assert_eq!(fed.errors.len(), 1);
        assert!(fed.errors[0].contains("truncated"));
    }

    #[test]
    fn slip_and_hdlc() {
        let mut f = make_framer(&Framing::Slip { max_len: 4096 });
        let mut stream = slip_encode(b"\xc0a");
        stream.extend(slip_encode(b"b"));
        let fed = feed_all(f.as_mut(), bytewise(&stream, 3));
        assert_eq!(fed.frames, [b"\xc0a".to_vec(), b"b".to_vec()]);
        let mut h = make_framer(&Framing::Hdlc { max_len: 4096 });
        assert_eq!(
            h.feed(b"\x7e\x01\x7d\x5e\x02\x7e").frames,
            [b"\x01\x7e\x02".to_vec()]
        );
    }

    #[test]
    fn length_prefixed_with_header_resync() {
        let mut f = lp(b"\xaa", 1, 1, 2, false, 64);
        let good = b"\xaa\x03xyzCC".to_vec();
        let mut stream = b"junk".to_vec();
        stream.extend(&good);
        stream.extend(&good[..3]);
        let fed = feed_all(f.as_mut(), bytewise(&stream, 1));
        assert_eq!(fed.frames, std::slice::from_ref(&good));
        assert!(!fed.errors.is_empty());
        assert_eq!(f.feed(&good[3..]).frames, [good]);
    }

    #[test]
    fn length_prefixed_big_endian_and_oversize() {
        let mut f = lp(b"", 0, 2, 0, true, 64);
        assert_eq!(
            f.feed(b"\x00\x02hi\x00\x01!").frames,
            [b"\x00\x02hi".to_vec(), b"\x00\x01!".to_vec()]
        );
        let mut f = lp(b"\xaa", 1, 1, 0, false, 8);
        let fed = f.feed(b"\xaa\xff\xaa\x01z");
        assert_eq!(fed.frames, [b"\xaa\x01z".to_vec()]);
        assert!(!fed.errors.is_empty());
    }

    #[test]
    fn fixed_and_raw() {
        let mut f = make_framer(&Framing::Fixed { size: 3 });
        let fed = feed_all(f.as_mut(), [b"ab".to_vec(), b"cdefg".to_vec()]);
        assert_eq!(fed.frames, [b"abc".to_vec(), b"def".to_vec()]);
        let mut r = make_framer(&Framing::Raw);
        let fed = feed_all(r.as_mut(), [b"ab".to_vec(), vec![], b"c".to_vec()]);
        assert_eq!(fed.frames, [b"ab".to_vec(), b"c".to_vec()]);
    }
}
