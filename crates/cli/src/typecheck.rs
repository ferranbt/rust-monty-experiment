//! Type-check a device script against the embedded SDK before running it.

use dut_runtime::protocol::{SDK_MODULE, sdk_files};
use monty_type_checking::{SourceFile, TypeCheckContext, TypeChecker};
use monty_types::{ModuleStub, TypeCheckingConfig, TypeCheckingFormat};

/// What the sandbox's `_host` module provides (see `dut_runtime::protocol`).
const HOST_STUB: &str = r#"
def write(data: bytes, channel: str = "main") -> None: ...
def next_event(timeout: float) -> dict | None: ...
def now() -> float: ...
def emit(kind: str, payload: dict) -> None: ...
def pack(fmt: str, values: list) -> bytes: ...
def unpack(fmt: str, data: bytes) -> list: ...
def crc(kind: str, data: bytes) -> int: ...
def cobs_encode(data: bytes) -> bytes: ...
def cobs_decode(data: bytes) -> bytes: ...
def slip_encode(data: bytes) -> bytes: ...
def slip_decode(data: bytes) -> bytes: ...
def bytes_from_list(values: list) -> bytes: ...
"#;

/// `Ok(None)` when clean, `Ok(Some(diagnostics))` when the script has typing errors.
pub fn check(source: &str, path: &str) -> Result<Option<String>, String> {
    let mut stubs = vec![ModuleStub::new("_host", HOST_STUB).map_err(|e| e.to_string())?];
    for (file, code) in sdk_files() {
        let module = match file.strip_suffix(".py") {
            Some("__init__") => SDK_MODULE.to_string(),
            Some(name) => format!("{SDK_MODULE}.{name}"),
            None => continue,
        };
        stubs.push(ModuleStub::new(module, *code).map_err(|e| e.to_string())?);
    }
    let context = TypeCheckContext {
        stubs: None,
        module_stubs: &stubs,
        prelude: "",
    };
    let config = TypeCheckingConfig {
        format: TypeCheckingFormat::Concise,
        color: false,
    };
    let mut checker = TypeChecker::default();
    let diagnostics = checker.run_with(&SourceFile::new(source, path), &context, config)?;
    Ok(diagnostics.map(|d| d.to_string()))
}
