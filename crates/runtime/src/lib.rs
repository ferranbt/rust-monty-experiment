//! Device-script runtime: the [`Host`] interface, framing, data helpers, the
//! Monty [`ScriptRunner`] and an in-memory [`FakeHost`] for tests.
//!
//! A control plane implements [`Host`] over its own transport and drives scripts
//! with [`MontyRunner`].

pub mod fake;
pub mod framing;
pub mod helpers;
pub mod host;
pub mod protocol;
pub mod record;
pub mod runner;

pub use fake::{FakeHost, Rule, SimDevice};
pub use host::{Event, EventKind, Framing, Host, HostError, Seconds, SignalKind, TimerSpec, Value};
pub use record::{Dir, JsonLinesRecorder, MemoryRecorder, RawEntry, Recorder, Signal};
pub use runner::{EventType, MontyRunner, Registry, ScriptError, ScriptRunner, VerbInfo};

#[cfg(test)]
mod tests;
