//! Default-off, one complete host ring interval. This is an ordered metadata
//! observation, not a replayable command buffer or an immutable GPU scene.
use crate::xr_view::WorldView;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, Ordering}, LazyLock, Mutex};

pub(crate) const MAX_OPERAND_BYTES: usize = 256 * 1024;
const MAX_RECORDS: usize = 32_768;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_HASH_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPORT_BYTES: usize = 17 * 1024 * 1024;
const MAX_STATUS_BYTES: usize = 2048;
static CLAIMED: AtomicBool = AtomicBool::new(false);
static NATIVE_TEXTURE_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static STATUS: LazyLock<Mutex<Value>> = LazyLock::new(|| Mutex::new(json!({
    "schema":1,"enabled":false,"state":"disabled"
})));

pub fn status_json() -> Value {
    STATUS.lock().map(|value| value.clone()).unwrap_or_else(|_| json!({"state":"status_lock_poisoned"}))
}

fn marker(view: &WorldView) -> Value {
    json!({"epoch":view.provenance_epoch,"camera":view.camera_sequence,
        "request":view.request_frame,"head":view.head_pose_frame,"eye":view.eye_index,
        "views":view.view_count,"time":view.presentation_time_seconds,
        "headset_applied":view.headset_applied,"reverse_z":view.depth_reverse_z,
        "game_from_eye":view.game_from_eye,"game_from_stage":view.game_from_stage,
        "projection_from_eye":view.projection_from_eye})
}

pub(crate) struct CommandTrace {
    enabled: bool,
    finished: bool,
    path: Option<PathBuf>,
    pending_present: Option<Value>,
    start: Option<(u64, WorldView)>,
    records: Vec<Value>,
    record_bytes: usize,
    observed: u64,
    omitted: u64,
    incomplete_operands: u64,
    hash_bytes: usize,
    first_omitted: Option<u64>,
    max_records: usize,
    max_bytes: usize,
}

impl CommandTrace {
    pub(crate) fn from_env() -> Self {
        let requested = std::env::var("MUGUET_XR_COMMAND_TRACE").as_deref() == Ok("1");
        let enabled = requested && !CLAIMED.swap(true, Ordering::Relaxed);
        let path = std::env::var_os("MUGUET_XR_COMMAND_TRACE_PATH").map(PathBuf::from);
        let trace = Self::new(enabled, path);
        if enabled { trace.status(json!({"schema":1,"enabled":true,"state":"waiting_for_immersive_present"})); }
        trace
    }

    fn new(enabled: bool, path: Option<PathBuf>) -> Self {
        Self { enabled, finished:false, path, pending_present:None, start:None,
            records:Vec::new(), record_bytes:0, observed:0, omitted:0,
            incomplete_operands:0, hash_bytes:0, first_omitted:None,
            max_records:MAX_RECORDS, max_bytes:MAX_BYTES }
    }

    /// The disabled path does not copy ring operands, inspect resources or build JSON.
    pub(crate) fn wants_command(&self, opcode: u32) -> bool {
        self.enabled && !self.finished && (self.start.is_some() || opcode == 7)
    }

    pub(crate) fn captures_native_identities(&self) -> bool { self.enabled && !self.finished }

    #[cfg(test)]
    pub(crate) fn fixture(enabled: bool) -> Self { Self::new(enabled,None) }

    pub(crate) fn retaining_commands(&self) -> bool {
        self.records.len() < self.max_records && self.record_bytes < self.max_bytes
    }

    pub(crate) fn hash_allowance(&self, wanted: usize) -> usize {
        wanted.min(MAX_HASH_BYTES.saturating_sub(self.hash_bytes))
    }

    pub(crate) fn note_hash_bytes(&mut self, bytes: usize) { self.hash_bytes += bytes; }

    /// `record` must own every copied word; guest memory is never retained here.
    pub(crate) fn command(&mut self, opcode: u32, mut record: Value) {
        if !self.wants_command(opcode) { return; }
        if self.start.is_none() {
            if opcode == 7 { self.pending_present = Some(record); }
            return;
        }
        self.observed += 1;
        record["ordinal"] = self.observed.into();
        if record["operands_complete"] != true { self.incomplete_operands += 1; }
        let bytes = serde_json::to_vec(&record).map_or(usize::MAX, |value| value.len());
        if self.records.len() >= self.max_records || bytes > self.max_bytes.saturating_sub(self.record_bytes) {
            self.omitted += 1;
            self.first_omitted.get_or_insert(self.observed);
        } else {
            self.record_bytes += bytes;
            self.records.push(record);
        }
    }

    /// Called at the existing present consumer, with its actual queued marker.
    /// Both endpoints are included; no pre-start commands are fabricated.
    pub(crate) fn present(&mut self, game_frame: u64, view: Option<&WorldView>, immersive: bool) {
        if !self.enabled || self.finished { return; }
        let qualified = immersive && view.is_some_and(|v| v.headset_applied
            && v.provenance_epoch != 0 && v.camera_sequence != 0 && v.request_frame != 0 && v.head_pose_frame != 0);
        if self.start.is_none() {
            let pending = self.pending_present.take();
            let (Some(view), Some(mut record)) = (view.filter(|_| qualified), pending) else { return; };
            self.start = Some((game_frame, *view));
            record["present_view"] = marker(view);
            self.command(7, record);
            self.status(json!({"schema":1,"enabled":true,"state":"recording",
                "start_game_frame":game_frame,"start_epoch":view.provenance_epoch}));
            return;
        }
        self.finished = true;
        let (start_frame, start_view) = self.start.unwrap();
        let records = std::mem::take(&mut self.records);
        let records_retained = records.len();
        let mut report = json!({"schema":1,"state":"captured","scope":"one host ring present-to-present interval, endpoints inclusive",
            "start_game_frame":start_frame,"end_game_frame":game_frame,
            "start_view":marker(&start_view),"end_view":view.map(marker),
            "end_immersive_qualified":qualified,
            "endpoint_epoch_matches":view.is_some_and(|v|v.provenance_epoch==start_view.provenance_epoch),
            "commands_observed":self.observed,"records_retained":records_retained,
            "records_omitted":self.omitted,"first_omitted_ordinal":self.first_omitted,
            "command_sequence_complete":self.omitted==0,
            "records_with_incomplete_operands":self.incomplete_operands,
            "all_operands_complete":self.omitted==0 && self.incomplete_operands==0,
            "record_bytes":self.record_bytes,"upload_bytes_hashed":self.hash_bytes,
            "limits":{"records":MAX_RECORDS,"record_bytes":MAX_BYTES,"full_report_bytes":MAX_REPORT_BYTES,"operand_bytes_per_command":MAX_OPERAND_BYTES,"upload_hash_bytes":MAX_HASH_BYTES},
            "qualification_caveat":"opening queued headset-camera marker; not proof that publish_world_frame or compositor acknowledgement succeeded",
            "snapshot_complete":false,
            "missing":["pre-existing resource contents; native identities only where actual allocation evidence is retained",
                "GPU-written buffer/texture contents, query results and asynchronous host operations",
                "native derived pipeline/shader variants, implicit passes/clears/proxies and resource transitions",
                "retained native GPU resources and frozen simulation epoch; this file cannot certify replay or stereo"],
            "upload_hash_algorithm":"fnv1a64_noncryptographic_correlation_only"});
        report["records"] = Value::Array(records);
        let summary = json!({"schema":1,"enabled":true,"state":"captured",
            "start_game_frame":start_frame,"end_game_frame":game_frame,
            "commands_observed":self.observed,"records_retained":records_retained,
            "records_omitted":self.omitted,"records_with_incomplete_operands":self.incomplete_operands,
            "command_sequence_complete":self.omitted==0,"snapshot_complete":false,
            "persistence":{"state":"pending_background_write"}});
        self.status(summary.clone());
        let path = self.path.take();
        // One process-wide claimed trace creates at most one writer. Disk IO
        // and full-report serialization do not block GPU command execution.
        if let Err(error) = std::thread::Builder::new().name("world-command-trace-writer".into()).spawn(move || {
            let mut summary = summary;
            summary["persistence"] = persist(path.as_deref(), &report);
            update_status(summary);
        }) {
            self.status(json!({"schema":1,"enabled":true,"state":"captured",
                "persistence":{"state":"error","reason":error.to_string()}}));
        }
    }

    fn status(&self, value: Value) {
        update_status(value);
    }
}

/// Minted for a returned native wgpu Texture handle at its allocation site.
/// This is a process-scoped handle association, never a guest ID or GPU content
/// generation. It retains no Texture handle and never changes render UIDs.
pub(crate) fn native_texture_tag(enabled: bool) -> Option<u64> {
    if !enabled { return None; }
    NATIVE_TEXTURE_TAG.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|tag|tag.checked_add(1)).ok()
}

fn update_status(value: Value) {
    let value = if serde_json::to_vec(&value).is_ok_and(|bytes|bytes.len()<=MAX_STATUS_BYTES) { value }
        else { json!({"schema":1,"enabled":true,"state":"summary_exceeds_2_KiB"}) };
    if let Ok(mut status) = STATUS.lock() { *status = value; }
}

pub(crate) fn upload_fingerprint(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ *byte as u64).wrapping_mul(0x100000001b3));
    format!("{hash:016x}")
}

fn persist(path: Option<&Path>, report: &Value) -> Value {
    let Some(path) = path else { return json!({"state":"path_not_configured"}); };
    if !path.is_absolute() { return json!({"state":"error","reason":"trace path must be absolute"}); }
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut writer = LimitedWriter { file, remaining:MAX_REPORT_BYTES };
        serde_json::to_writer(&mut writer, report)?;
        writer.write_all(b"\n")?;
        writer.flush()
    })();
    match result {
        Ok(()) => json!({"state":"written","path":path.to_string_lossy()}),
        Err(error) => json!({"state":"error","reason":error.to_string()}),
    }
}

struct LimitedWriter { file: std::fs::File, remaining:usize }
impl std::io::Write for LimitedWriter {
    fn write(&mut self, bytes:&[u8]) -> std::io::Result<usize> {
        if bytes.len()>self.remaining { return Err(std::io::Error::other("full report byte cap exceeded; file may be incomplete")); }
        let count=self.file.write(bytes)?; self.remaining-=count; Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.file.flush() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> WorldView { WorldView { provenance_epoch:1,camera_sequence:2,request_frame:3,head_pose_frame:4,
        presentation_time_seconds:1.,eye_index:1,view_count:2,game_from_eye:[0.;16],game_from_stage:[0.;16],
        projection_from_eye:[0.;16],depth_reverse_z:true,headset_applied:true } }
    fn record(op:u32) -> Value { json!({"opcode":op,"operands":[7,8,9],"operands_complete":true}) }
    fn arm(trace:&mut CommandTrace) { trace.command(7,record(7)); trace.present(10,Some(&view()),true); }

    #[test] fn off_and_unqualified_present_do_not_arm() {
        let mut off=CommandTrace::new(false,None); arm(&mut off);
        assert!(!off.wants_command(7)); assert!(off.records.is_empty());
        let mut trace=CommandTrace::new(true,None);
        trace.command(7,record(7)); trace.present(10,Some(&view()),false);
        assert!(trace.start.is_none()); assert!(trace.records.is_empty());
        trace.command(7,record(7)); trace.present(11,None,true); assert!(trace.start.is_none());
    }

    #[test] fn both_present_markers_and_wrap_are_ordered_and_one_shot() {
        let mut trace=CommandTrace::new(true,None); arm(&mut trace);
        for op in [1,12,32,39,35,7] { trace.command(op,record(op)); }
        assert_eq!(trace.records.iter().map(|r|r["opcode"].as_u64().unwrap()).collect::<Vec<_>>(),[7,1,12,32,39,35,7]);
        assert_eq!(trace.records.last().unwrap()["ordinal"],7);
        trace.present(11,Some(&view()),true); assert!(trace.finished); assert!(!trace.wants_command(7));
        trace.command(7,record(7)); assert_eq!(trace.observed,7);
    }

    #[test] fn record_and_byte_caps_count_omissions_without_sampling() {
        let mut trace=CommandTrace::new(true,None); trace.max_records=2; arm(&mut trace);
        for op in [32,1,7] { trace.command(op,record(op)); }
        assert_eq!(trace.observed,4); assert_eq!(trace.records.len(),2); assert_eq!(trace.omitted,2);
        assert_eq!(trace.first_omitted,Some(3));
        let mut bytes=CommandTrace::new(true,None); bytes.max_bytes=1; arm(&mut bytes);
        assert_eq!(bytes.omitted,1); assert!(bytes.records.is_empty());
    }

    #[test] fn operand_and_hash_limits_are_explicit() {
        let mut trace=CommandTrace::new(true,None); arm(&mut trace);
        trace.command(12,json!({"opcode":12,"operands_complete":false}));
        assert_eq!(trace.incomplete_operands,1);
        trace.note_hash_bytes(MAX_HASH_BYTES-2); assert_eq!(trace.hash_allowance(8),2);
        assert_eq!(upload_fingerprint(b"hello"),"a430d84680aabd0b");
        assert_ne!(upload_fingerprint(b"hello"),upload_fingerprint(b"hellp"));
    }

    #[test] fn create_new_preserves_existing_and_rejects_relative_path() {
        let path=std::env::temp_dir().join(format!("sv-command-trace-test-{}-{}.json",std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        assert_eq!(persist(Some(&path),&json!({"first":true}))["state"],"written");
        let before=std::fs::read(&path).unwrap();
        assert_eq!(persist(Some(&path),&json!({"second":true}))["state"],"error");
        assert_eq!(std::fs::read(&path).unwrap(),before); std::fs::remove_file(path).unwrap();
        assert_eq!(persist(Some(Path::new("relative.json")),&json!({}))["state"],"error");
    }
}
