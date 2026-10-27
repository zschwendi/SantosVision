//! Private Mac capture input admission. No game execution, cache replacement or
//! compilation fallback. Absent from ordinary and visionOS builds.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{fs::File, io::Read, path::{Path, PathBuf}, sync::OnceLock};
use wasmtime::{Engine, Module};

const WASM_SHA256: &str = "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0";
const AOT_SHA256: &str = "dc4806cba7b44366fd039dc5212a20fbfcc96139d3208047b5e52e41db2fdfff";
const WASM_BYTES: u64 = 63_201_802;
const AOT_BYTES: u64 = 212_445_264;

struct Prepared {
    wasm_path: PathBuf,
    code: Vec<u8>,
}
static PREPARED: OnceLock<Prepared> = OnceLock::new();

fn read_pinned(path: &Path, length: u64, expected: &str) -> Result<Vec<u8>> {
    ensure!(path.is_absolute(), "capture input must be absolute");
    ensure!(path.canonicalize()? == path, "capture input must be its canonical path");
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(meta.file_type().is_file() && meta.len() == length,
        "capture input must be a regular file with the exact admitted length");
    let file = File::open(path)?;
    let opened = file.metadata()?;
    ensure!(opened.is_file() && opened.len() == length, "capture input changed before open");
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(length + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 == length, "capture input changed size while reading");
    ensure!(crate::xr_motion_patch::sha256(&bytes)? == expected, "capture input hash differs from the admitted Mac fixture");
    Ok(bytes)
}

/// Admit the exact original WASM and already-compiled Mac AOT before starting a
/// private runtime. Returns a receipt; writes no files and executes no guest.
/// The caller supplies a separately created, disposable save directory later.
pub fn prepare(wasm_path: &Path, aot_path: &Path) -> Result<Value> {
    ensure!(PREPARED.get().is_none(), "only one capture AOT preparation per process");
    ensure!(option_env!("MUGUET_MOTION_AOT_SHA256") == Some(AOT_SHA256),
        "the private capture binary needs the exact compile-time Mac AOT certificate");
    let wasm = read_pinned(wasm_path, WASM_BYTES, WASM_SHA256).context("original Mac WASM admission")?;
    drop(wasm);
    let code = read_pinned(aot_path, AOT_BYTES, AOT_SHA256).context("compiled Mac AOT admission")?;
    crate::xr_motion_aim::qualify_embedded_aot(&code)?;
    let engine = Engine::new(&crate::engine_config())?;
    // SAFETY: deserialize only the exact previously qualified private Mac AOT,
    // after both its content hash and this binary's certificate have matched.
    let module = unsafe { Module::deserialize(&engine, &code) }
        .map_err(anyhow::Error::from)
        .context("private Mac AOT incompatible with the actual engine; compilation fallback forbidden")?;
    let receipt = json!({
        "schema":1,
        "wasm_path":wasm_path,"wasm_sha256":WASM_SHA256,"wasm_bytes":WASM_BYTES,
        "aot_path":aot_path,"aot_sha256":AOT_SHA256,"aot_bytes":AOT_BYTES,
        "imports":module.imports().len(),"exports":module.exports().len(),
        "engine":"current production Mac engine_config with default jit feature",
        "storage":"immutable in-process bytes; no cache file is created or replaced",
        "guest_executed":false,"fallback_compilation_allowed":false
    });
    PREPARED.set(Prepared { wasm_path: wasm_path.to_path_buf(), code })
        .map_err(|_| anyhow::anyhow!("concurrent capture AOT preparation rejected"))?;
    Ok(receipt)
}

pub(crate) fn load(engine: &Engine, startup: &crate::config::Startup) -> Result<Module> {
    let prepared = PREPARED.get().context("capture AOT was not explicitly prepared; compilation fallback forbidden")?;
    ensure!(startup.wasm_path == prepared.wasm_path,
        "native startup selected a different WASM than the admitted capture input");
    // Verify the original identity at the actual load boundary as well. The
    // compiled bytes are already retained privately and cannot be replaced by a
    // cache writer or by later changes to the source AOT file.
    let wasm = read_pinned(&startup.wasm_path, WASM_BYTES, WASM_SHA256)?;
    drop(wasm);
    // SAFETY: Prepared can only contain the content-hash-pinned code admitted
    // above. Re-deserialization binds it to the actual runtime's Engine.
    unsafe { Module::deserialize(engine, &prepared.code) }
        .map_err(anyhow::Error::from)
        .context("capture AOT failed actual-engine deserialization; compilation fallback forbidden")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_and_wrong_length_inputs_without_execution() {
        assert!(read_pinned(Path::new("relative.cwasm"), AOT_BYTES, AOT_SHA256).is_err());
        let temp = std::env::temp_dir().join(format!("native-capture-invalid-{}", std::process::id()));
        std::fs::create_dir(&temp).unwrap();
        let temp = temp.canonicalize().unwrap();
        let file = temp.join("wrong.cwasm");
        std::fs::write(&file, b"not compiled code").unwrap();
        assert!(read_pinned(&file, AOT_BYTES, AOT_SHA256).is_err());
        assert!(read_pinned(&file, 17, AOT_SHA256).unwrap_err().to_string().contains("hash differs"));
        let link = temp.join("link.cwasm");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_pinned(&link, 17, AOT_SHA256).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(temp).unwrap();
    }

    #[test]
    #[ignore = "requires the private hash-pinned Mac inputs; CPU deserialization only"]
    fn exact_private_aot_load_is_fail_closed() {
        let wasm = PathBuf::from(std::env::var_os("SV_NATIVE_CAPTURE_TEST_WASM").unwrap());
        let aot = PathBuf::from(std::env::var_os("SV_NATIVE_CAPTURE_TEST_AOT").unwrap());
        let engine = Engine::new(&crate::engine_config()).unwrap();
        let mut startup = crate::config::Startup { wasm_path: wasm.clone(), cache_dir: PathBuf::new(),
            engine_args: vec![], native_graphics: None };
        assert!(load(&engine, &startup).is_err(), "unprepared load must fail");
        let receipt = prepare(&wasm, &aot).unwrap();
        assert_eq!(receipt["guest_executed"], false);
        assert_eq!(receipt["fallback_compilation_allowed"], false);
        let loaded = load(&engine, &startup).unwrap();
        assert_eq!(loaded.imports().len(), receipt["imports"].as_u64().unwrap() as usize);
        assert!(prepare(&wasm, &aot).is_err(), "a second preparation cannot replace immutable code");
        startup.wasm_path = PathBuf::from("/different/game.wasm");
        assert!(load(&engine, &startup).is_err(), "a different runtime source must fail");
    }
}
