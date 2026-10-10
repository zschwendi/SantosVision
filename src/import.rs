// muguet - import.rs
// Constantin Clerc (c22dev) - 0.1

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const SKIPPED_DIRS: &[&str] = &["env_test", "verityradio"];

pub fn import(zip_path: &Path, dest: &Path, keep_code: bool, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let file = File::open(zip_path).with_context(|| format!("{}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::with_capacity(1 << 20, file)).context("not a zip file")?;

    let names: Vec<String> = zip.file_names().map(String::from).collect();
    let manifest = names
        .iter()
        .filter(|n| n.ends_with("data/manifest.json") && (n.len() == "data/manifest.json".len() || n.ends_with("/data/manifest.json")))
        .min_by_key(|n| n.len())
        .context("this zip has no game data (data/manifest.json)")?;
    let prefix = manifest[..manifest.len() - "data/manifest.json".len()].to_string();
    if !names.iter().any(|n| n.starts_with(&format!("{prefix}b/")) && n.ends_with("/shaders/index.json")) {
        bail!("this zip has no shaders (b/<build>/shaders)");
    }
    if keep_code && !names.iter().any(|n| n.starts_with(&format!("{prefix}b/")) && n.ends_with("/game.wasm")) {
        bail!("this zip has no game code (b/<build>/game.wasm)");
    }

    let wanted = |rel: &str| -> bool {
        if !(rel.starts_with("data/") || rel.starts_with("b/")) || rel.ends_with('/') {
            return false;
        }
        if rel.split('/').any(|c| SKIPPED_DIRS.contains(&c.to_ascii_lowercase().as_str())) {
            return false;
        }
        if let Some(build) = rel.strip_prefix("b/") {
            let inner = build.split_once('/').map(|(_, r)| r).unwrap_or("");
            return inner.starts_with("shaders/") || (keep_code && inner == "game.wasm");
        }
        true
    };

    let mut jobs = Vec::new();
    let mut total = 0u64;
    for k in 0..zip.len() {
        let entry = zip.by_index_raw(k)?;
        let Some(name) = entry.enclosed_name() else { continue };
        let name = name.to_string_lossy().replace('\\', "/");
        let Some(rel) = name.strip_prefix(&prefix) else { continue };
        if entry.is_dir() || !wanted(rel) {
            continue;
        }
        total += entry.size();
        jobs.push((k, rel.to_string()));
    }

    let staging = dest.with_extension("importing");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let mut done = 0u64;
    let mut reported = 0u64;
    progress(0, total);
    let mut buf = vec![0u8; 1 << 20];
    for (k, rel) in &jobs {
        let mut entry = zip.by_index(*k)?;
        let out_path = staging.join(rel);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&out_path).with_context(|| format!("{}", out_path.display()))?;
        loop {
            let n = entry.read(&mut buf).with_context(|| format!("reading {rel} from the zip"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            if done - reported >= 64 << 20 {
                reported = done;
                progress(done, total);
            }
        }
    }

    tidy_lists(&staging.join("data"))?;
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(&staging, dest)?;
    progress(total, total);
    Ok(())
}

fn tidy_lists(data: &Path) -> Result<()> {
    let mut modified_sizes = HashMap::new();
    let dlc = data.join("common/data/dlcList.xml");
    if let Ok(text) = std::fs::read_to_string(&dlc) {
        let kept: Vec<&str> = text.lines().filter(|l| !SKIPPED_DIRS.iter().any(|d| l.to_ascii_lowercase().contains(d))).collect();
        std::fs::write(&dlc, kept.join("\n") + "\n")?;
        modified_sizes.insert("common/data/dlcList.xml", std::fs::metadata(&dlc)?.len());
    }
    let manifest_path = data.join("manifest.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?).context("manifest.json")?;
    let present: HashSet<PathBuf> = walk(data);
    if let Some(files) = manifest.get_mut("files").and_then(|f| f.as_array_mut()) {
        files.retain(|f| f.get(0).and_then(|p| p.as_str()).is_some_and(|p| present.contains(&data.join(p))));
        for file in files.iter_mut() {
            let size = file.get(0).and_then(|p| p.as_str()).and_then(|p| modified_sizes.get(p)).copied();
            if let Some(size) = size {
                *file.get_mut(1).context("modified asset manifest entry has no byte size")? = size.into();
            }
        }
    }
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    Ok(())
}

fn walk(dir: &Path) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl Drop for TestDirectory {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn import_updates_only_modified_asset_sizes_and_removes_skipped_assets() -> Result<()> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let temporary = TestDirectory(std::env::temp_dir().join(format!("muguet-import-{}-{unique}", std::process::id())));
        std::fs::create_dir(&temporary.0)?;
        let archive_path = temporary.0.join("game.zip");
        let destination = temporary.0.join("game");
        let original_dlc = b"<Paths>\n<Item>dlcpacks:/env_test/</Item>\n<Item>dlcpacks:/main/</Item>\n</Paths>\r\n";
        let kept_dlc = b"<Paths>\n<Item>dlcpacks:/main/</Item>\n</Paths>\n";
        let regular = b"regular asset\0";
        let regular_row = serde_json::json!(["regular.bin", regular.len(), {"preserved": true}]);
        // An unrelated mismatch must not be silently normalized by this repair.
        let unchanged_row = serde_json::json!(["unchanged-size.bin", 999]);
        let original_manifest = serde_json::json!({
            "mount": "/", "version": "fixture", "custom": {"keep": [1, 2]},
            "files": [
                ["common/data/dlcList.xml", original_dlc.len(), {"preserved": "dlc"}],
                ["dlcpacks/env_test/asset.bin", 7], regular_row.clone(), unchanged_row.clone()
            ]
        });
        let manifest_bytes = serde_json::to_vec(&original_manifest)?;
        let mut archive = zip::ZipWriter::new(File::create(&archive_path)?);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in [
            ("package/data/manifest.json", manifest_bytes.as_slice()),
            ("package/data/common/data/dlcList.xml", original_dlc.as_slice()),
            ("package/data/dlcpacks/env_test/asset.bin", b"skipped".as_slice()),
            ("package/data/regular.bin", regular.as_slice()),
            ("package/data/unchanged-size.bin", b"keep".as_slice()),
            ("package/b/test/shaders/index.json", b"{}".as_slice()),
            ("package/b/test/game.wasm", b"\0asm\x01\0\0\0".as_slice()),
        ] {
            archive.start_file(name, options)?;
            archive.write_all(bytes)?;
        }
        archive.finish()?;

        import(&archive_path, &destination, true, |_, _| {})?;
        let data = destination.join("data");
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(data.join("manifest.json"))?)?;
        assert_eq!(manifest["mount"], original_manifest["mount"]);
        assert_eq!(manifest["version"], original_manifest["version"]);
        assert_eq!(manifest["custom"], original_manifest["custom"]);
        let files = manifest["files"].as_array().unwrap();
        assert_eq!(files.len(), 3);
        assert!(!data.join("dlcpacks/env_test/asset.bin").exists());
        assert!(!files.iter().any(|row| row[0] == "dlcpacks/env_test/asset.bin"));
        assert!(files.contains(&regular_row));
        assert!(files.contains(&unchanged_row));
        assert_eq!(std::fs::read(data.join("regular.bin"))?, regular);
        assert_eq!(std::fs::read(data.join("unchanged-size.bin"))?, b"keep");
        let dlc_row = files.iter().find(|row| row[0] == "common/data/dlcList.xml").unwrap();
        assert_eq!(dlc_row[2], serde_json::json!({"preserved": "dlc"}));
        let advertised_size = dlc_row[1].as_u64().unwrap();
        let dlc_file = File::open(data.join("common/data/dlcList.xml"))?;
        assert_eq!(advertised_size, dlc_file.metadata()?.len());
        assert!(advertised_size < original_dlc.len() as u64);
        let mut read_to_advertised_eof = vec![0; advertised_size as usize];
        dlc_file.read_exact_at(&mut read_to_advertised_eof, 0)?;
        assert_eq!(read_to_advertised_eof, kept_dlc);
        Ok(())
    }
}

#[cfg(target_os = "ios")]
mod ffi {
    use std::ffi::{c_char, CStr, CString};
    use std::sync::Mutex;

    static ERROR: Mutex<Option<CString>> = Mutex::new(None);

    #[no_mangle]
    pub extern "C" fn muguet_import(zip: *const c_char, progress: extern "C" fn(u64, u64)) -> bool {
        let path = unsafe { CStr::from_ptr(zip) }.to_string_lossy().into_owned();
        match super::import(std::path::Path::new(&path), &crate::config::game_dir(), false, |d, t| progress(d, t)) {
            Ok(()) => true,
            Err(e) => {
                *ERROR.lock().unwrap() = CString::new(format!("{e:#}")).ok();
                false
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn muguet_import_error() -> *const c_char {
        ERROR.lock().unwrap().as_ref().map(|s| s.as_ptr()).unwrap_or(std::ptr::null())
    }

    #[no_mangle]
    pub extern "C" fn muguet_game_ready() -> bool {
        crate::config::game_ready()
    }

    #[no_mangle]
    pub extern "C" fn muguet_has_game_code() -> bool {
        crate::has_game_code()
    }
}
