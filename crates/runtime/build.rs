//! Embeds every `.py` file of the sandbox SDK package (`sdk/<SDK_MODULE>/`) into the binary.
//! Generates `$OUT_DIR/sdk.rs` with `SDK_MODULE` and `SDK_FILES`, included by `src/runner.rs`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::{env, fs};

/// Import name of the SDK package inside the sandbox (`from dut import Device`).
const SDK_MODULE: &str = "dut";

fn main() -> Result<(), Box<dyn Error>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sdk")
        .join(SDK_MODULE);
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "py") {
            files.push(path);
        }
    }
    files.sort();
    if !files.iter().any(|p| p.ends_with("__init__.py")) {
        return Err(format!("{} has no __init__.py", dir.display()).into());
    }

    let mut out = format!("pub const SDK_MODULE: &str = {SDK_MODULE:?};\n");
    out.push_str("const SDK_FILES: &[(&str, &str)] = &[\n");
    for path in &files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| non_utf8(path))?;
        let full = path.to_str().ok_or_else(|| non_utf8(path))?;
        out.push_str(&format!("    ({name:?}, include_str!({full:?})),\n"));
    }
    out.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR")?).join("sdk.rs"), out)?;
    Ok(())
}

fn non_utf8(path: &Path) -> String {
    format!("non-UTF-8 path: {}", path.display())
}
