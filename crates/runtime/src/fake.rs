//! In-memory [`Host`] with a virtual clock, plus a simulated device (SPEC §12).
//!
//! Byte deliveries and timer ticks are scheduled actions; `next_event` jumps
//! the clock to the next action (or to the deadline) without sleeping, so
//! timeouts run instantly and repeatably.
//!
//! ```ignore
//! let sim = SimDevice::new()
//!     .chunked(3)
//!     .at(0.0, b"FW 1.2\n")
//!     .every(2.0, b"T=21.5\n")
//!     .on(Rule::prefix(b"radio ch ").reply_with(|cmd| [b"ch=", cmd.rsplit(|b| *b == b' ').next().unwrap_or_default()].concat()))
//!     .on(Rule::prefix(b"reboot").reply_at(1.0, b"FW 1.2\n"));
//! let host = FakeHost::with_device(sim);
//! ```

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

use crate::framing::ChannelFramers;
use crate::host::{
    Event, EventKind, Framing, Host, HostError, Seconds, SignalKind, TimerSpec, Value,
};
pub use crate::record::{Dir, RawEntry, Signal};

enum Action {
    Rx { channel: String, data: Vec<u8> },
    Tick { timer: u32, period: Seconds },
    Repeat { data: Vec<u8>, period: Seconds },
}

struct Scheduled {
    t: Seconds,
    seq: u64,
    action: Action,
}

// Min-heap on (t, seq).
impl Ord for Scheduled {
    fn cmp(&self, other: &Self) -> Ordering {
        other.t.total_cmp(&self.t).then(other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Scheduled {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Scheduled {}

pub struct FakeHost {
    t: Seconds,
    seq: u64,
    queue: BinaryHeap<Scheduled>,
    ready: VecDeque<Event>,
    framers: ChannelFramers,
    device: Option<SimDevice>,
    pub raw_log: Vec<RawEntry>,
    pub signals: Vec<Signal>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeHost {
    pub fn new() -> Self {
        Self {
            t: 0.0,
            seq: 0,
            queue: BinaryHeap::new(),
            ready: VecDeque::new(),
            framers: ChannelFramers::default(),
            device: None,
            raw_log: vec![],
            signals: vec![],
        }
    }

    pub fn with_device(device: SimDevice) -> Self {
        let mut host = Self::new();
        for (t, data) in device.at.clone() {
            host.send_from_device(&device, t, &data);
        }
        for (period, data) in device.every.clone() {
            host.schedule(period, Action::Repeat { data, period });
        }
        host.device = Some(device);
        host
    }

    /// Device -> host bytes arriving at absolute time `at`.
    pub fn deliver(&mut self, data: &[u8], at: Seconds, channel: &str) {
        self.schedule(
            at,
            Action::Rx {
                channel: channel.into(),
                data: data.to_vec(),
            },
        );
    }

    /// Move the virtual clock (tests only; never backwards).
    pub fn set_time(&mut self, t: Seconds) {
        self.t = self.t.max(t);
    }

    pub fn device(&self) -> Option<&SimDevice> {
        self.device.as_ref()
    }

    /// Payloads of the signals of one kind, in order.
    pub fn of_kind(&self, kind: impl Into<SignalKind>) -> Vec<&Value> {
        let kind = kind.into();
        self.signals
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| &s.payload)
            .collect()
    }

    /// Bytes written to the device on `channel`, one entry per write.
    pub fn tx(&self, channel: &str) -> Vec<Vec<u8>> {
        self.raw_log
            .iter()
            .filter(|e| e.dir == Dir::Tx && e.channel == channel)
            .map(|e| e.data.clone())
            .collect()
    }

    fn schedule(&mut self, t: Seconds, action: Action) {
        self.seq += 1;
        self.queue.push(Scheduled {
            t,
            seq: self.seq,
            action,
        });
    }

    fn send_from_device(&mut self, device: &SimDevice, t: Seconds, data: &[u8]) {
        for (at, chunk) in device.chunks(t, data) {
            self.deliver(&chunk, at, &device.channel.clone());
        }
    }

    fn run(&mut self, action: Action) {
        match action {
            Action::Rx { channel, data } => self.rx(channel, data),
            Action::Tick { timer, period } => {
                self.ready.push_back(Event {
                    kind: EventKind::Tick { timer },
                    channel: None,
                    t: self.t,
                });
                self.schedule(self.t + period, Action::Tick { timer, period });
            }
            Action::Repeat { data, period } => {
                if let Some(device) = self.device.take() {
                    self.send_from_device(&device, self.t, &data);
                    self.device = Some(device);
                }
                self.schedule(self.t + period, Action::Repeat { data, period });
            }
        }
    }

    fn rx(&mut self, channel: String, data: Vec<u8>) {
        let out = self.framers.feed(&channel, &data, self.t);
        self.raw_log.push(RawEntry {
            t: self.t,
            dir: Dir::Rx,
            channel,
            data,
        });
        self.signals.extend(out.errors);
        self.ready.extend(out.events);
    }
}

impl Host for FakeHost {
    async fn configure(
        &mut self,
        framing: &Framing,
        timers: &[TimerSpec],
    ) -> Result<(), HostError> {
        TimerSpec::check(timers)?;
        self.framers.reset(framing);
        for timer in timers {
            self.schedule(
                self.t + timer.seconds,
                Action::Tick {
                    timer: timer.id,
                    period: timer.seconds,
                },
            );
        }
        Ok(())
    }

    async fn write(&mut self, channel: &str, data: &[u8]) -> Result<(), HostError> {
        self.raw_log.push(RawEntry {
            t: self.t,
            dir: Dir::Tx,
            channel: channel.into(),
            data: data.to_vec(),
        });
        if let Some(mut device) = self.device.take() {
            for (at, chunk) in device.on_write(self.t, channel, data) {
                self.deliver(&chunk, at, channel);
            }
            self.device = Some(device);
        }
        Ok(())
    }

    async fn next_event(&mut self, timeout: Seconds) -> Result<Option<Event>, HostError> {
        let deadline = self.t + timeout.max(0.0);
        loop {
            if let Some(ev) = self.ready.pop_front() {
                return Ok(Some(ev));
            }
            if !self.queue.peek().is_some_and(|next| next.t <= deadline) {
                self.t = deadline;
                return Ok(None);
            }
            if let Some(next) = self.queue.pop() {
                self.t = self.t.max(next.t);
                self.run(next.action);
            }
        }
    }

    fn now(&self) -> Seconds {
        self.t
    }

    async fn emit(&mut self, kind: SignalKind, payload: Value) -> Result<(), HostError> {
        self.signals.push(Signal {
            t: self.t,
            kind,
            payload,
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Simulated device
// ---------------------------------------------------------------------------

type Matcher = Box<dyn Fn(&[u8]) -> bool + Send>;
type Producer = Box<dyn Fn(&[u8]) -> Vec<u8> + Send>;

enum Reply {
    Bytes(Vec<u8>),
    With(Producer),
}

/// "When the device receives X, reply Y after Z seconds."
pub struct Rule {
    matcher: Matcher,
    replies: Vec<(Seconds, Reply)>,
    delay: Seconds,
    once: bool,
    used: bool,
}

impl Rule {
    /// Matches writes starting with `prefix`.
    pub fn prefix(prefix: &[u8]) -> Self {
        let p = prefix.to_vec();
        Self::when(move |d| d.starts_with(&p))
    }

    pub fn when(f: impl Fn(&[u8]) -> bool + Send + 'static) -> Self {
        Self {
            matcher: Box::new(f),
            replies: vec![],
            delay: 0.01,
            once: false,
            used: false,
        }
    }

    /// Reply `data` after the rule's delay.
    pub fn reply(self, data: &[u8]) -> Self {
        self.reply_at(0.0, data)
    }

    /// Reply `data` at `offset` seconds after the rule's delay.
    pub fn reply_at(mut self, offset: Seconds, data: &[u8]) -> Self {
        self.replies.push((offset, Reply::Bytes(data.to_vec())));
        self
    }

    /// Reply with bytes computed from the written data.
    pub fn reply_with(mut self, f: impl Fn(&[u8]) -> Vec<u8> + Send + 'static) -> Self {
        self.replies.push((0.0, Reply::With(Box::new(f))));
        self
    }

    pub fn delay(mut self, delay: Seconds) -> Self {
        self.delay = delay;
        self
    }

    pub fn once(mut self) -> Self {
        self.once = true;
        self
    }
}

pub struct SimDevice {
    channel: String,
    echo: bool,
    chunk_size: Option<usize>,
    chunk_gap: Seconds,
    rules: Vec<Rule>,
    at: Vec<(Seconds, Vec<u8>)>,
    every: Vec<(Seconds, Vec<u8>)>,
    /// Everything the device received, one entry per write.
    pub received: Vec<Vec<u8>>,
}

impl Default for SimDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl SimDevice {
    pub fn new() -> Self {
        Self {
            channel: "main".into(),
            echo: false,
            chunk_size: None,
            chunk_gap: 0.001,
            rules: vec![],
            at: vec![],
            every: vec![],
            received: vec![],
        }
    }

    /// The channel the device talks on (default `"main"`).
    pub fn channel(mut self, channel: &str) -> Self {
        self.channel = channel.into();
        self
    }

    /// Echo every write back (1 ms later) before any reply.
    pub fn echo(mut self) -> Self {
        self.echo = true;
        self
    }

    /// Split all output into `size`-byte chunks, 1 ms apart, to exercise framing.
    pub fn chunked(mut self, size: usize) -> Self {
        self.chunk_size = Some(size);
        self
    }

    /// The first matching rule answers a write.
    pub fn on(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Unsolicited output at absolute time `t`.
    pub fn at(mut self, t: Seconds, data: &[u8]) -> Self {
        self.at.push((t, data.to_vec()));
        self
    }

    /// Unsolicited output every `period` seconds, starting at `period`.
    pub fn every(mut self, period: Seconds, data: &[u8]) -> Self {
        self.every.push((period, data.to_vec()));
        self
    }

    fn chunks(&self, t: Seconds, data: &[u8]) -> Vec<(Seconds, Vec<u8>)> {
        match self.chunk_size {
            None => vec![(t, data.to_vec())],
            Some(n) => data
                .chunks(n)
                .enumerate()
                .map(|(i, c)| (t + i as Seconds * self.chunk_gap, c.to_vec()))
                .collect(),
        }
    }

    fn on_write(&mut self, t: Seconds, channel: &str, data: &[u8]) -> Vec<(Seconds, Vec<u8>)> {
        if channel != self.channel {
            return vec![];
        }
        self.received.push(data.to_vec());
        let mut t = t;
        let mut out = vec![];
        if self.echo {
            t += 0.001;
            out.extend(self.chunks(t, data));
        }
        let Some(rule) = self
            .rules
            .iter_mut()
            .find(|r| !(r.once && r.used) && (r.matcher)(data))
        else {
            return out;
        };
        rule.used = true;
        let mut replies = vec![];
        for (offset, reply) in &rule.replies {
            let bytes = match reply {
                Reply::Bytes(b) => b.clone(),
                Reply::With(f) => f(data),
            };
            replies.push((t + rule.delay + offset, bytes));
        }
        for (at, bytes) in replies {
            out.extend(self.chunks(at, &bytes));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use eyre::{OptionExt, Result, bail};

    use pollster::block_on;

    use super::*;

    /// The next event, which must be a frame: (data, t).
    fn next_frame(host: &mut FakeHost, timeout: Seconds) -> Result<(Vec<u8>, Seconds)> {
        let ev = block_on(host.next_event(timeout))?.ok_or_eyre("no event before the timeout")?;
        let EventKind::Frame(data) = ev.kind else {
            bail!("not a frame: {ev:?}")
        };
        Ok((data, ev.t))
    }

    #[test]
    fn next_event_advances_virtual_clock_to_deadline() -> Result<()> {
        let mut host = FakeHost::new();
        assert_eq!(block_on(host.next_event(2.5))?, None);
        assert_eq!(host.now(), 2.5);
        Ok(())
    }

    #[test]
    fn frames_arrive_at_their_time() -> Result<()> {
        let mut host = FakeHost::with_device(SimDevice::new().at(1.0, b"a\n").at(3.0, b"b\n"));
        assert_eq!(next_frame(&mut host, 10.0)?, (b"a".to_vec(), 1.0));
        assert_eq!(block_on(host.next_event(1.0))?, None);
        assert_eq!(host.now(), 2.0);
        assert_eq!(next_frame(&mut host, 5.0)?, (b"b".to_vec(), 3.0));
        Ok(())
    }

    #[test]
    fn ticks_repeat() -> Result<()> {
        let mut host = FakeHost::new();
        block_on(host.configure(
            &Framing::Raw,
            &[TimerSpec {
                id: 0,
                seconds: 2.0,
            }],
        ))?;
        let mut times = vec![];
        for _ in 0..3 {
            times.push(block_on(host.next_event(10.0))?.ok_or_eyre("no tick")?.t);
        }
        assert_eq!(times, [2.0, 4.0, 6.0]);
        Ok(())
    }

    #[test]
    fn write_is_raw_logged_and_reaches_sim() -> Result<()> {
        let mut host = FakeHost::with_device(
            SimDevice::new().on(Rule::prefix(b"ping").reply(b"pong\n").delay(0.5)),
        );
        block_on(host.write("main", b"ping\n"))?;
        assert_eq!(next_frame(&mut host, 1.0)?, (b"pong".to_vec(), 0.5));
        let log: Vec<_> = host
            .raw_log
            .iter()
            .map(|e| (e.dir, e.data.clone(), e.t))
            .collect();
        assert_eq!(
            log,
            [
                (Dir::Tx, b"ping\n".to_vec(), 0.0),
                (Dir::Rx, b"pong\n".to_vec(), 0.5)
            ]
        );
        Ok(())
    }

    #[test]
    fn chunked_output_is_reassembled_by_framing() -> Result<()> {
        let mut host = FakeHost::with_device(SimDevice::new().chunked(2).at(0.1, b"hello\n"));
        assert_eq!(next_frame(&mut host, 1.0)?.0, b"hello");
        assert_eq!(host.raw_log.iter().filter(|e| e.dir == Dir::Rx).count(), 3);
        Ok(())
    }

    #[test]
    fn framing_errors_become_host_signals() -> Result<()> {
        let mut host = FakeHost::with_device(SimDevice::new().at(0.1, b"\x05\x11\x00"));
        block_on(host.configure(&Framing::Cobs { max_len: 4096 }, &[]))?;
        assert_eq!(block_on(host.next_event(1.0))?, None);
        assert_eq!(host.of_kind("framing_error").len(), 1);
        Ok(())
    }

    #[test]
    fn once_rules_and_echo() -> Result<()> {
        let sim = SimDevice::new()
            .echo()
            .on(Rule::prefix(b"x").reply(b"first\n").once());
        let mut host = FakeHost::with_device(sim);
        block_on(host.write("main", b"x\n"))?;
        block_on(host.write("main", b"x\n"))?;
        let mut frames = vec![];
        while let Some(ev) = block_on(host.next_event(1.0))? {
            let EventKind::Frame(data) = ev.kind else {
                bail!("not a frame")
            };
            frames.push(data);
        }
        assert_eq!(frames, [b"x".to_vec(), b"x".to_vec(), b"first".to_vec()]);
        Ok(())
    }
}
