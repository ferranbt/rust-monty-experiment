//! [`Host`] over a real serial port (UART adapter, USB CDC-ACM). macOS and Linux.
//!
//! - Port paths: Linux `/dev/ttyUSB0`, `/dev/ttyACM0`, `/dev/serial0` (Raspberry Pi
//!   GPIO UART; your user needs the `dialout` group). macOS `/dev/cu.usbserial-*` or
//!   `/dev/cu.usbmodem*`: use `cu.*`, not `tty.*`, which can block waiting for carrier.
//! - The port is the `"main"` channel. 8N1, no flow control.
//! - `now()` is a monotonic clock starting when the host is created.
//! - Every byte both ways goes to the [`Recorder`] raw log with its host timestamp,
//!   then through the script's framing.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use dut_runtime::framing::ChannelFramers;
use dut_runtime::host::{
    Event, EventKind, Framing, Host, HostError, Seconds, SignalKind, TimerSpec, Value,
};
use dut_runtime::record::{Dir, RawEntry, Recorder, Signal};

/// Longest single blocking read; longer waits are split so the loop stays responsive.
const MAX_READ_WAIT: Duration = Duration::from_secs(1);
const CHANNEL: &str = "main";

/// A byte pipe to the device. Implemented for serial ports; tests use in-memory ones.
pub trait Transport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    /// Read what is available, waiting at most `timeout`. `Ok(0)` when nothing arrived.
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

impl Transport for Box<dyn serialport::SerialPort> {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        // No flush(): on a serial port that is tcdrain(), which blocks until the bytes
        // are physically sent and can hang (flow control, pseudo-terminals). write(2)
        // already hands the bytes to the kernel; there is no userspace buffer.
        Write::write_all(self, data)
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.set_timeout(timeout)?;
        match self.read(buf) {
            Ok(n) => Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}

/// A device reached over TCP: an emulator's serial console (QEMU `-serial tcp::4321,server`,
/// Renode, Wokwi) or a serial-to-network bridge. The same bytes as a serial port.
impl Transport for TcpStream {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        Write::write_all(self, data)
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        // a zero read timeout means "block forever" to the OS
        self.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        match self.read(buf) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            )),
            Ok(n) => Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}

/// Where the device is: a serial port or a TCP address.
pub enum Link {
    Serial(Box<dyn serialport::SerialPort>),
    Tcp(TcpStream),
}

impl Transport for Link {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Link::Serial(port) => Transport::write_all(port, data),
            Link::Tcp(stream) => Transport::write_all(stream, data),
        }
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        match self {
            Link::Serial(port) => port.read_timeout(buf, timeout),
            Link::Tcp(stream) => stream.read_timeout(buf, timeout),
        }
    }
}

/// Connect to a device's TCP serial stream, e.g. `localhost:4321`.
pub fn connect_tcp(address: &str) -> Result<TcpStream, HostError> {
    let stream =
        TcpStream::connect(address).map_err(|e| HostError::Transport(format!("{address}: {e}")))?;
    // interactive console traffic: send small writes at once
    stream
        .set_nodelay(true)
        .map_err(|e| HostError::Transport(format!("{address}: {e}")))?;
    Ok(stream)
}

/// Open a serial port by path at `baud`, 8N1, no flow control.
pub fn open_port(path: &str, baud: u32) -> Result<Box<dyn serialport::SerialPort>, HostError> {
    serialport::new(path, baud)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| HostError::Transport(format!("{path}: {e}")))
}

struct Timer {
    id: u32,
    period: Seconds,
    due: Seconds,
}

pub struct SerialHost<T: Transport, R: Recorder> {
    port: T,
    recorder: R,
    start: Instant,
    framers: ChannelFramers,
    timers: Vec<Timer>,
    ready: VecDeque<Event>,
    buf: Vec<u8>,
}

impl<T: Transport, R: Recorder> SerialHost<T, R> {
    pub fn new(port: T, recorder: R) -> Self {
        Self {
            port,
            recorder,
            start: Instant::now(),
            framers: ChannelFramers::default(),
            timers: vec![],
            ready: VecDeque::new(),
            buf: vec![0; 4096],
        }
    }

    #[cfg(test)]
    pub fn recorder(&self) -> &R {
        &self.recorder
    }

    pub fn into_parts(self) -> (T, R) {
        (self.port, self.recorder)
    }

    /// Queue ticks for timers that are due. A timer that fell behind fires once and
    /// skips the missed periods (ticks are coalesced, never bursted).
    fn fire_due_timers(&mut self, now: Seconds) {
        for timer in &mut self.timers {
            if timer.due <= now {
                let missed = ((now - timer.due) / timer.period).floor() + 1.0;
                timer.due += missed * timer.period;
                self.ready.push_back(Event {
                    kind: EventKind::Tick { timer: timer.id },
                    channel: None,
                    t: now,
                });
            }
        }
    }

    fn next_timer_due(&self) -> Option<Seconds> {
        self.timers.iter().map(|t| t.due).min_by(f64::total_cmp)
    }
}

fn wait_for(seconds: Seconds) -> Duration {
    // f64::max maps NaN to 0; the cap also covers +inf.
    Duration::from_secs_f64(seconds.max(0.0)).min(MAX_READ_WAIT)
}

impl<T: Transport + Send, R: Recorder + Send> Host for SerialHost<T, R> {
    async fn configure(
        &mut self,
        framing: &Framing,
        timers: &[TimerSpec],
    ) -> Result<(), HostError> {
        TimerSpec::check(timers)?;
        self.framers.reset(framing);
        let now = self.now();
        self.timers = timers
            .iter()
            .map(|t| Timer {
                id: t.id,
                period: t.seconds,
                due: now + t.seconds,
            })
            .collect();
        Ok(())
    }

    async fn write(&mut self, channel: &str, data: &[u8]) -> Result<(), HostError> {
        if channel != CHANNEL {
            return Err(HostError::UnknownChannel(channel.into()));
        }
        let t = self.now();
        self.recorder.raw(RawEntry {
            t,
            dir: Dir::Tx,
            channel: channel.into(),
            data: data.to_vec(),
        });
        self.port
            .write_all(data)
            .map_err(|e| HostError::Transport(format!("write: {e}")))
    }

    async fn next_event(&mut self, timeout: Seconds) -> Result<Option<Event>, HostError> {
        let deadline = self.now() + timeout.max(0.0);
        loop {
            if let Some(ev) = self.ready.pop_front() {
                return Ok(Some(ev));
            }
            let now = self.now();
            self.fire_due_timers(now);
            if !self.ready.is_empty() {
                continue;
            }
            if now >= deadline {
                return Ok(None);
            }
            let until = self
                .next_timer_due()
                .map_or(deadline, |due| due.min(deadline));
            let n = self
                .port
                .read_timeout(&mut self.buf, wait_for(until - now))
                .map_err(|e| HostError::Transport(format!("read: {e}")))?;
            if n > 0 {
                let t = self.now();
                let data = self.buf[..n].to_vec();
                let out = self.framers.feed(CHANNEL, &data, t);
                self.recorder.raw(RawEntry {
                    t,
                    dir: Dir::Rx,
                    channel: CHANNEL.into(),
                    data,
                });
                for error in out.errors {
                    self.recorder.signal(error);
                }
                self.ready.extend(out.events);
            }
        }
    }

    fn now(&self) -> Seconds {
        self.start.elapsed().as_secs_f64()
    }

    async fn emit(&mut self, kind: SignalKind, payload: Value) -> Result<(), HostError> {
        let t = self.now();
        self.recorder.signal(Signal { t, kind, payload });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use eyre::{OptionExt, Result, bail};

    use pollster::block_on;

    use super::*;
    use dut_runtime::record::MemoryRecorder;
    use dut_runtime::{MontyRunner, ScriptRunner};

    /// In-memory port: queued incoming chunks, and canned replies to writes.
    #[derive(Default)]
    struct MockPort {
        incoming: VecDeque<Vec<u8>>,
        replies: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
        written: Vec<Vec<u8>>,
    }

    impl MockPort {
        fn reply(mut self, prefix: &[u8], chunks: &[&[u8]]) -> Self {
            self.replies
                .push((prefix.to_vec(), chunks.iter().map(|c| c.to_vec()).collect()));
            self
        }
    }

    impl Transport for MockPort {
        fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
            self.written.push(data.to_vec());
            if let Some((_, chunks)) = self.replies.iter().find(|(p, _)| data.starts_with(p)) {
                self.incoming.extend(chunks.iter().cloned());
            }
            Ok(())
        }

        fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
            match self.incoming.pop_front() {
                Some(chunk) => {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                None => {
                    thread::sleep(timeout);
                    Ok(0)
                }
            }
        }
    }

    fn host(port: MockPort) -> SerialHost<MockPort, MemoryRecorder> {
        SerialHost::new(port, MemoryRecorder::default())
    }

    fn frame(ev: Option<Event>) -> Result<Vec<u8>> {
        match ev.ok_or_eyre("no event")?.kind {
            EventKind::Frame(data) => Ok(data),
            other => bail!("not a frame: {other:?}"),
        }
    }

    #[test]
    fn frames_split_across_reads_are_reassembled_and_logged() -> Result<()> {
        let mut port = MockPort::default();
        port.incoming
            .extend([b"T=2".to_vec(), b"1\nT=".to_vec(), b"22\n".to_vec()]);
        let mut h = host(port);
        assert_eq!(frame(block_on(h.next_event(1.0))?)?, b"T=21");
        assert_eq!(frame(block_on(h.next_event(1.0))?)?, b"T=22");
        let rx: Vec<_> = h
            .recorder()
            .raw
            .iter()
            .map(|e| (e.dir, e.data.clone()))
            .collect();
        assert_eq!(
            rx,
            [
                (Dir::Rx, b"T=2".to_vec()),
                (Dir::Rx, b"1\nT=".to_vec()),
                (Dir::Rx, b"22\n".to_vec())
            ]
        );
        Ok(())
    }

    #[test]
    fn timeout_returns_none_after_the_deadline() -> Result<()> {
        let mut h = host(MockPort::default());
        let start = h.now();
        assert_eq!(block_on(h.next_event(0.05))?, None);
        assert!(h.now() - start >= 0.05);
        Ok(())
    }

    #[test]
    fn timers_tick_on_the_real_clock() -> Result<()> {
        let mut h = host(MockPort::default());
        block_on(h.configure(
            &Framing::Raw,
            &[TimerSpec {
                id: 3,
                seconds: 0.02,
            }],
        ))?;
        let ev = block_on(h.next_event(1.0))?.ok_or_eyre("no tick")?;
        assert_eq!(ev.kind, EventKind::Tick { timer: 3 });
        assert!(ev.t >= 0.02);
        Ok(())
    }

    #[test]
    fn framing_errors_and_tx_are_recorded() -> Result<()> {
        let mut port = MockPort::default();
        port.incoming.push_back(b"\x05\x11\x00".to_vec());
        let mut h = host(port);
        block_on(h.configure(&Framing::Cobs { max_len: 4096 }, &[]))?;
        block_on(h.write("main", b"ping"))?;
        assert_eq!(block_on(h.next_event(0.02))?, None);
        let signals = &h.recorder().signals;
        assert_eq!(
            (signals.len(), &signals[0].kind),
            (1, &SignalKind::FramingError)
        );
        assert_eq!(h.recorder().raw[0].dir, Dir::Tx);
        assert!(matches!(
            block_on(h.write("aux", b"x")),
            Err(HostError::UnknownChannel(_))
        ));
        Ok(())
    }

    #[test]
    fn script_request_reply_over_the_port() -> Result<()> {
        let port = MockPort::default().reply(b"radio ch", &[b"[log] busy\n", b"ch=", b"19\n"]);
        let mut runner = MontyRunner::new(host(port));
        runner.load(
            "from dut import Device, Text, request\n\
             dev = Device(\"pi\", sdk=\"0.1\", format=Text())\n\
             @dev.set_channel\n\
             def set_channel(channel):\n    return request(f\"radio ch {channel}\\n\", match=\"ch=\", timeout=1)\n",
        )?;
        assert_eq!(
            runner.call_verb("set_channel", &[("channel", Value::Int(19))])?,
            Value::from("ch=19")
        );
        let (port, rec) = runner.into_host().into_parts();
        assert_eq!(port.written, [b"radio ch 19\n".to_vec()]);
        // text mode logs every line, the unsolicited one and the reply
        let lines: Vec<_> = rec.signals.iter().filter_map(|s| s.log_text()).collect();
        assert_eq!(lines, ["[log] busy", "ch=19"]);
        Ok(())
    }

    /// A real pseudo-terminal pair: exercises the serialport code path on macOS and Linux.
    #[cfg(unix)]
    #[test]
    fn real_pty_round_trip() -> Result<()> {
        use serialport::SerialPort;

        let (host_end, mut device_end) = serialport::TTYPort::pair()?;
        let device = thread::spawn(move || -> io::Result<()> {
            let mut buf = [0u8; 64];
            device_end.set_timeout(Duration::from_secs(2))?;
            let n = device_end.read(&mut buf)?;
            if buf[..n].starts_with(b"ping") {
                device_end.write_all(b"po")?;
                thread::sleep(Duration::from_millis(20));
                device_end.write_all(b"ng\n")?;
            }
            Ok(())
        });
        let port: Box<dyn serialport::SerialPort> = Box::new(host_end);
        let mut h = SerialHost::new(port, MemoryRecorder::default());
        block_on(h.write("main", b"ping\n"))?;
        assert_eq!(frame(block_on(h.next_event(2.0))?)?, b"pong");
        device
            .join()
            .map_err(|_| eyre::eyre!("device thread panicked"))??;
        Ok(())
    }
}
