//! Run device scripts in a pool of `monty` worker processes, one session per device.
//!
//! A crash, memory blow-up or hang ends only that session; the control plane keeps
//! running. The [`Host`] (your transport) stays in your process: the worker sends each
//! host call back here and the session answers it with your host.
//!
//! ```ignore
//! let pool = ScriptPool::new(PoolSettings::new("/usr/local/bin/monty")).await?;
//! let mut session = pool.open(&script, my_host).await?;      // one per device, in parallel
//! match session.call_verb("tx_on", &[]).await {
//!     Ok(value) => {}
//!     Err(e) if e.ends_session() => { /* report; open a new session */ }
//!     Err(e) => { /* script or host error; session still usable */ }
//! }
//! session.close().await;
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use dut_runtime::protocol::{
    Decoded, HostCall, ImportAnswer, SCRIPT_NAME, SDK_DIR, Snippet, answer_import, decode_call,
    event_object, host_exception, parse_registry, value_of,
};
use dut_runtime::{Host, HostError, Registry, ScriptError, Seconds, Value};
use monty_pool::{
    Checkout, Pool, PoolConfig, PoolError, ReplConfig, ResumeValue, TurnEvent, on_print_sync,
};
use monty_types::{
    ExcType, IMPORT_CALL, ImportArgs, MONTY_VERSION, MontyException, MontyObject, NameLookupResult,
    ResourceLimits,
};

#[derive(Debug, Clone)]
pub struct PoolSettings {
    /// Path to the `monty` worker binary (`cargo install monty-runtime --version =1.6.0 --locked`).
    pub worker: PathBuf,
    /// Workers kept warm so sessions start without waiting for a spawn.
    pub min_workers: usize,
    /// Most sessions at once; `open` waits for a free worker beyond this.
    pub max_workers: usize,
    /// Memory one session may use, in bytes. A loaded script needs ~250 KB.
    pub max_memory: usize,
    /// Longest one call (verb, hook, reader, pump) may take on the host clock,
    /// waiting on the device included.
    pub call_timeout: Duration,
    /// Longest the interpreter may compute within one call (host waits excluded).
    pub max_cpu_per_call: Duration,
}

impl PoolSettings {
    pub fn new(worker: impl Into<PathBuf>) -> Self {
        Self {
            worker: worker.into(),
            min_workers: 1,
            max_workers: 64,
            max_memory: 1024 * 1024,
            call_timeout: Duration::from_secs(60),
            max_cpu_per_call: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    /// The script raised an exception. The session is still usable.
    Script(ScriptError),
    /// Your host failed during the call. The session is still usable.
    Host(HostError),
    /// The call exceeded `call_timeout` or `max_cpu_per_call`. Session ended.
    Timeout,
    /// The session exceeded `max_memory`. Session ended.
    MemoryLimit,
    /// The worker process died. Session ended.
    Crashed(String),
    /// The pool failed: worker missing or wrong version, spawn or protocol error.
    Pool(String),
    /// An earlier error ended this session; open a new one.
    Ended,
}

impl SessionError {
    pub fn ends_session(&self) -> bool {
        !matches!(self, SessionError::Script(_) | SessionError::Host(_))
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Script(e) => write!(f, "script error: {e}"),
            SessionError::Host(e) => write!(f, "host error: {e}"),
            SessionError::Timeout => write!(f, "call exceeded its time limit"),
            SessionError::MemoryLimit => write!(f, "session exceeded its memory limit"),
            SessionError::Crashed(why) => write!(f, "worker crashed: {why}"),
            SessionError::Pool(why) => write!(f, "pool error: {why}"),
            SessionError::Ended => write!(f, "session already ended"),
        }
    }
}

impl std::error::Error for SessionError {}

/// One per control plane; share it.
pub struct ScriptPool {
    pool: Pool,
    settings: PoolSettings,
}

impl ScriptPool {
    /// Checks the worker binary is the same Monty version as this crate, then starts it.
    pub async fn new(settings: PoolSettings) -> Result<Self, SessionError> {
        check_worker(&settings.worker).await?;
        let mut config = PoolConfig::subprocess(&settings.worker);
        config.min_processes = settings.min_workers;
        config.max_processes = settings.max_workers.max(1);
        // Backstop for a worker hung in a way its own limits cannot see.
        config.request_timeout = Some(settings.max_cpu_per_call + Duration::from_secs(5));
        let pool = Pool::new(config)
            .await
            .map_err(|e| SessionError::Pool(e.to_string()))?;
        Ok(Self { pool, settings })
    }

    /// Load `script` into a fresh worker and configure `host` with its framing and timers.
    pub async fn open<H: Host + Send>(
        &self,
        script: &str,
        host: H,
    ) -> Result<Session<H>, SessionError> {
        let repl = ReplConfig {
            script_name: SCRIPT_NAME.into(),
            limits: Some(ResourceLimits {
                max_memory: Some(self.settings.max_memory),
                max_feed_duration: Some(self.settings.max_cpu_per_call),
                // Every host call counts; long listens make thousands. The call
                // deadline is the real bound, this is a safety net.
                max_suspensions: 10_000_000,
                ..Default::default()
            }),
            ..ReplConfig::default()
        };
        let checkout = self
            .pool
            .checkout(&repl)
            .await
            .map_err(|e| SessionError::Pool(e.to_string()))?;
        let mut session = Session {
            host,
            checkout: Some(checkout),
            registry: None,
            call_timeout: self.settings.call_timeout.as_secs_f64(),
        };
        session.run(Snippet::import_sdk()).await?;
        session.run(Snippet::code(script, &[])).await?;
        let raw = session.run(Snippet::registry()).await?;
        let registry = parse_registry(&value_of(&raw))
            .map_err(|e| SessionError::Script(ScriptError::host(e)))?;
        session
            .host
            .configure(&registry.framing, &registry.timers)
            .await
            .map_err(SessionError::Host)?;
        session.registry = Some(registry);
        Ok(session)
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

async fn check_worker(worker: &Path) -> Result<(), SessionError> {
    let path = worker.to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&path).arg("--version").output()
    })
    .await
    .map_err(|e| SessionError::Pool(e.to_string()))?
    .map_err(|e| SessionError::Pool(format!("worker {}: {e}", worker.display())))?;
    let version = String::from_utf8_lossy(&output.stdout);
    let found = version.split_whitespace().last().unwrap_or_default();
    if found != MONTY_VERSION {
        return Err(SessionError::Pool(format!(
            "worker {} is Monty {found:?}, this build needs {MONTY_VERSION}",
            worker.display()
        )));
    }
    Ok(())
}

/// One device's loaded script, in its own worker. Calls run one at a time.
pub struct Session<H: Host + Send> {
    host: H,
    /// `None` once the session has ended.
    checkout: Option<Checkout>,
    registry: Option<Registry>,
    call_timeout: Seconds,
}

impl<H: Host + Send> Session<H> {
    /// What the script declared (framing, verbs, hooks, timers, ...).
    pub fn registry(&self) -> Option<&Registry> {
        self.registry.as_ref()
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn is_alive(&self) -> bool {
        self.checkout.is_some()
    }

    /// The worker's process id, for diagnostics.
    pub fn worker_pid(&self) -> Option<u32> {
        self.checkout.as_ref().and_then(Checkout::pid)
    }

    pub async fn call_verb(
        &mut self,
        name: &str,
        args: &[(&str, Value)],
    ) -> Result<Value, SessionError> {
        self.run(Snippet::call_verb(name, args))
            .await
            .map(|v| value_of(&v))
    }

    /// Undefined hooks are optional no-ops that return `Value::None`.
    pub async fn call_hook(
        &mut self,
        name: &str,
        args: &[(&str, Value)],
    ) -> Result<Value, SessionError> {
        self.run(Snippet::call_hook(name, args))
            .await
            .map(|v| value_of(&v))
    }

    pub async fn read(&mut self, name: &str) -> Result<Value, SessionError> {
        self.run(Snippet::read(name)).await.map(|v| value_of(&v))
    }

    pub async fn pump(&mut self, seconds: Seconds) -> Result<(), SessionError> {
        self.run(Snippet::pump(seconds)).await.map(|_| ())
    }

    /// Return the worker to the pool. Returns the host.
    pub async fn close(mut self) -> H {
        if let Some(checkout) = self.checkout.take() {
            // A worker that cannot be returned is discarded by the pool.
            let _ = checkout.finish().await;
        }
        self.host
    }

    async fn run(&mut self, snippet: Snippet) -> Result<MontyObject, SessionError> {
        let deadline = self.host.now() + self.call_timeout;
        let mut host_error = None;
        let mut on_print = on_print_sync(|_, _| {});
        let Some(checkout) = self.checkout.as_mut() else {
            return Err(SessionError::Ended);
        };
        let mut event = checkout
            .feed_with_cwd(
                snippet.code,
                snippet.inputs,
                vec![],
                Some(SDK_DIR),
                true,
                &mut on_print,
            )
            .await;
        loop {
            let answer = match event {
                Err(e) => return Err(self.fail(e, host_error)),
                Ok(TurnEvent::Complete(value)) => {
                    return match host_error {
                        Some(e) => Err(SessionError::Host(e)),
                        None => Ok(value),
                    };
                }
                Ok(TurnEvent::FunctionCall {
                    function_name,
                    args,
                    ..
                }) => match decode_call(&function_name, &args) {
                    Err(exc) => ResumeValue::Error(exc),
                    Ok(Decoded::Done(value)) => ResumeValue::Return(value),
                    Ok(Decoded::Host(call)) => {
                        let remaining = deadline - self.host.now();
                        if remaining <= 0.0 {
                            self.kill().await;
                            return Err(SessionError::Timeout);
                        }
                        match self.host_call(call, remaining).await {
                            Ok(value) => ResumeValue::Return(value),
                            Err(e) => {
                                let exc = host_exception(&e);
                                host_error.get_or_insert(e);
                                ResumeValue::Error(exc)
                            }
                        }
                    }
                },
                Ok(TurnEvent::OsCall {
                    function_name,
                    args,
                    ..
                }) if function_name == IMPORT_CALL => {
                    match ImportArgs::from_call_args(&args).map(|a| answer_import(&a)) {
                        Some(ImportAnswer::Module(module)) => ResumeValue::Return(module),
                        Some(ImportAnswer::File(file)) => ResumeValue::ModuleFile(file),
                        Some(ImportAnswer::NotFound) | None => ResumeValue::NotHandled,
                    }
                }
                Ok(TurnEvent::OsCall { .. }) => ResumeValue::NotHandled,
                Ok(TurnEvent::NameLookup { .. }) => {
                    event = self
                        .checkout()?
                        .resume_name_lookup(NameLookupResult::Undefined, &mut on_print)
                        .await;
                    continue;
                }
                Ok(TurnEvent::ResolveFutures { .. }) => {
                    self.kill().await;
                    return Err(SessionError::Pool("async code is not supported".into()));
                }
            };
            event = self.checkout()?.resume(answer, &mut on_print).await;
        }
    }

    fn checkout(&mut self) -> Result<&mut Checkout, SessionError> {
        self.checkout.as_mut().ok_or(SessionError::Ended)
    }

    /// A host call, with waits capped at what is left of the call deadline.
    async fn host_call(
        &mut self,
        call: HostCall,
        remaining: Seconds,
    ) -> Result<MontyObject, HostError> {
        Ok(match call {
            HostCall::Write { channel, data } => {
                self.host.write(&channel, &data).await?;
                MontyObject::none()
            }
            HostCall::NextEvent { timeout } => {
                event_object(self.host.next_event(timeout.min(remaining)).await?)
            }
            HostCall::Now => MontyObject::float(self.host.now()),
            HostCall::Emit { kind, payload } => {
                self.host.emit(kind, payload).await?;
                MontyObject::none()
            }
        })
    }

    async fn kill(&mut self) {
        if let Some(checkout) = self.checkout.take() {
            checkout.terminate().await;
        }
    }

    /// Classify a pool error; errors that end the session drop the worker.
    fn fail(&mut self, e: PoolError, host_error: Option<HostError>) -> SessionError {
        let error = match e {
            PoolError::Runtime(exc) => classify(&exc, host_error),
            PoolError::Typing(msg) => SessionError::Script(ScriptError {
                exc_type: "SyntaxError".into(),
                message: msg.clone(),
                traceback: msg,
            }),
            PoolError::Timeout { .. } | PoolError::CpuLimit { .. } => SessionError::Timeout,
            PoolError::Crashed { .. } => SessionError::Crashed(e.to_string()),
            PoolError::Finished => SessionError::Ended,
            other => SessionError::Pool(other.to_string()),
        };
        if error.ends_session() {
            // The pool already discarded the worker for these; drop our handle.
            self.checkout = None;
        }
        error
    }
}

fn classify(exc: &MontyException, host_error: Option<HostError>) -> SessionError {
    match exc.exc_type() {
        ExcType::MemoryError => SessionError::MemoryLimit,
        // The interpreter's own CPU budget, not a script-level TimeoutError.
        ExcType::TimeoutError
            if exc
                .message()
                .unwrap_or_default()
                .contains("time limit exceeded") =>
        {
            SessionError::Timeout
        }
        _ => match host_error {
            Some(e) => SessionError::Host(e),
            None => SessionError::Script(ScriptError::from_monty(exc)),
        },
    }
}

#[cfg(test)]
mod tests;
