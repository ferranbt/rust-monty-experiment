//! Runs device scripts in the Monty sandbox **in this process**. The [`Host`] futures
//! are driven with `pollster::block_on`, so no async runtime is needed.
//!
//! Good for tests, the CLI and WASM. A crash inside the interpreter (stack overflow,
//! allocator abort) takes the process down and there is no per-session memory limit:
//! a control plane running untrusted scripts in parallel uses `dut-pool` instead.

use std::time::Duration;

use monty::{MontyRepl, ReplProgress};
use monty_types::{
    CompileOptions, ExcType, ExtFunctionResult, MontyException, MontyObject, NameLookupResult,
    OsFunctionCall, PrintWriter, ResourceLimits, ResourceTracker,
};
use pollster::block_on;

use crate::host::{Host, Seconds, Value};
use crate::protocol::{
    Decoded, HostCall, ImportAnswer, SCRIPT_NAME, SDK_DIR, Snippet, answer_import, decode_call,
    event_object, host_exception, parse_registry, value_of,
};
pub use crate::protocol::{
    MetricInfo, MetricKind, Registry, SDK_MODULE, SUPPORTED_SDK_VERSIONS, ScriptError, VerbInfo,
};

/// Loads a device script and drives it. Monty stays behind this interface (SPEC §7).
pub trait ScriptRunner {
    /// Load SDK + script and configure the host. Returns what the script declared.
    fn load(&mut self, source: &str) -> Result<Registry, ScriptError>;
    fn call_verb(&mut self, name: &str, args: &[(&str, Value)]) -> Result<Value, ScriptError>;
    /// Undefined hooks are optional no-ops that return `Value::None`.
    fn call_hook(&mut self, name: &str, args: &[(&str, Value)]) -> Result<Value, ScriptError>;
    /// Call a `@dev.reader`: one record per call.
    fn read(&mut self, name: &str) -> Result<Value, ScriptError>;
    /// Idle listening: dispatch frames and timer ticks for `seconds` of host time.
    fn pump(&mut self, seconds: Seconds) -> Result<(), ScriptError>;
}

/// Default limits: 5 s of sandbox execution per call (host time excluded).
pub fn default_limits() -> ResourceLimits {
    ResourceLimits {
        max_feed_duration: Some(Duration::from_secs(5)),
        ..Default::default()
    }
}

/// A fresh interpreter session, working directory set so `import dut` finds the SDK.
pub fn new_repl(limits: ResourceLimits) -> MontyRepl {
    let mut repl = MontyRepl::new(
        SCRIPT_NAME,
        ResourceTracker::new(limits),
        CompileOptions::default(),
    );
    repl.set_cwd(SDK_DIR);
    repl
}

pub struct MontyRunner<H: Host> {
    host: H,
    repl: Option<MontyRepl>,
}

impl<H: Host> MontyRunner<H> {
    pub fn new(host: H) -> Self {
        Self::with_limits(host, default_limits())
    }

    pub fn with_limits(host: H, limits: ResourceLimits) -> Self {
        Self {
            host,
            repl: Some(new_repl(limits)),
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn into_host(self) -> H {
        self.host
    }

    /// Run arbitrary sandbox code (tests and debugging).
    pub fn eval(&mut self, code: &str, inputs: &[(&str, Value)]) -> Result<Value, ScriptError> {
        self.run(Snippet::code(code, inputs))
    }

    fn run(&mut self, snippet: Snippet) -> Result<Value, ScriptError> {
        self.feed(snippet).map(|o| value_of(&o))
    }

    fn feed(&mut self, snippet: Snippet) -> Result<MontyObject, ScriptError> {
        let Some(repl) = self.repl.take() else {
            return Err(ScriptError::host("sandbox session lost"));
        };
        let mut step = repl.feed_start(&snippet.code, snippet.inputs, PrintWriter::Stdout);
        loop {
            let progress = match step {
                Ok(p) => p,
                Err(e) => {
                    let err = ScriptError::from_monty(&e.error);
                    self.repl = Some(e.repl);
                    return Err(err);
                }
            };
            step = match progress {
                ReplProgress::Complete { repl, value } => {
                    self.repl = Some(repl);
                    return Ok(value);
                }
                ReplProgress::FunctionCall(call) => {
                    let result = self.host_call(&call.function_name, &call.args);
                    call.resume(result, PrintWriter::Stdout)
                }
                ReplProgress::OsCall(call) => {
                    let result = os_answer(&call.function_call);
                    call.resume(result, PrintWriter::Stdout)
                }
                ReplProgress::NameLookup(lookup) => {
                    lookup.resume(NameLookupResult::Undefined, PrintWriter::Stdout)
                }
                ReplProgress::ResolveFutures(f) => {
                    let exc = MontyException::new(
                        ExcType::RuntimeError,
                        Some("async is not supported".into()),
                    );
                    f.abort(exc, PrintWriter::Stdout)
                }
            };
        }
    }

    fn host_call(&mut self, name: &str, args: &monty_types::CallArgs) -> ExtFunctionResult {
        let call = match decode_call(name, args) {
            Ok(Decoded::Done(value)) => return ExtFunctionResult::Return(value),
            Ok(Decoded::Host(call)) => call,
            Err(exc) => return ExtFunctionResult::Error(exc),
        };
        let result = match call {
            HostCall::Write { channel, data } => {
                block_on(self.host.write(&channel, &data)).map(|()| MontyObject::none())
            }
            HostCall::NextEvent { timeout } => {
                block_on(self.host.next_event(timeout)).map(event_object)
            }
            HostCall::Now => Ok(MontyObject::float(self.host.now())),
            HostCall::Emit { kind, payload } => {
                block_on(self.host.emit(kind, payload)).map(|()| MontyObject::none())
            }
        };
        match result {
            Ok(value) => ExtFunctionResult::Return(value),
            Err(e) => ExtFunctionResult::Error(host_exception(&e)),
        }
    }
}

fn os_answer(call: &OsFunctionCall) -> ExtFunctionResult {
    if let OsFunctionCall::Import(args) = call {
        match answer_import(args) {
            ImportAnswer::Module(module) => return ExtFunctionResult::Return(module),
            ImportAnswer::File(file) => return ExtFunctionResult::ModuleFile(file),
            ImportAnswer::NotFound => {}
        }
    }
    call.no_handler_result()
}

impl<H: Host> ScriptRunner for MontyRunner<H> {
    fn load(&mut self, source: &str) -> Result<Registry, ScriptError> {
        self.feed(Snippet::import_sdk())?;
        self.feed(Snippet::code(source, &[]))?;
        let raw = self.run(Snippet::registry())?;
        let registry = parse_registry(&raw).map_err(ScriptError::host)?;
        block_on(self.host.configure(&registry.framing, &registry.timers))
            .map_err(|e| ScriptError::host(e.to_string()))?;
        Ok(registry)
    }

    fn call_verb(&mut self, name: &str, args: &[(&str, Value)]) -> Result<Value, ScriptError> {
        self.run(Snippet::call_verb(name, args))
    }

    fn call_hook(&mut self, name: &str, args: &[(&str, Value)]) -> Result<Value, ScriptError> {
        self.run(Snippet::call_hook(name, args))
    }

    fn read(&mut self, name: &str) -> Result<Value, ScriptError> {
        self.run(Snippet::read(name))
    }

    fn pump(&mut self, seconds: Seconds) -> Result<(), ScriptError> {
        self.run(Snippet::pump(seconds)).map(|_| ())
    }
}
