//! Default-off, bounded source-to-compositor timing observation.
//! All stage times share this module's native Instant origin. Presentation time
//! is only identifying metadata; it is never subtracted from native elapsed time.
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Mutex, OnceLock,
    },
    time::Instant,
};

pub(crate) const MAX_EVENTS: usize = 8192;
const MAX_EXPORT_BYTES: usize = 4 * 1024 * 1024;
const MAX_STATUS_BYTES: usize = 2048;
static ENABLED: AtomicBool = AtomicBool::new(false);
static CAPTURE: OnceLock<Mutex<Capture>> = OnceLock::new();
static EXPORT_TX: OnceLock<SyncSender<Export>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Identity {
    pub epoch: u64,
    pub request: Option<u64>,
    pub head: Option<u64>,
    pub camera: Option<u64>,
    pub game: Option<u64>,
    pub eye: Option<u32>,
    pub views: Option<u32>,
    pub lease: Option<u64>,
    pub presentation_time: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Metrics {
    pub ring_bytes: Option<u64>,
    pub ring_head: Option<u64>,
    pub ring_tail: Option<u64>,
    pub ring_size: Option<u64>,
    pub pending_snapshots: Option<u64>,
    pub dropped_snapshots: Option<u64>,
    pub slots: Option<u64>,
    pub leases: Option<u64>,
    pub metal_status: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Event {
    sequence: u64,
    elapsed_ns: u64,
    session: u64,
    stage: &'static str,
    id: Identity,
    metrics: Metrics,
}
impl Event {
    fn json(self) -> Value {
        json!({"type":"event","sequence":self.sequence,"elapsed_ns":self.elapsed_ns,
        "session_id":self.session,"stage":self.stage,"epoch":self.id.epoch,"request":self.id.request,
        "head":self.id.head,"camera":self.id.camera,"game":self.id.game,"eye":self.id.eye,
        "views":self.id.views,"lease":self.id.lease,"presentation_time":self.id.presentation_time,
        "ring_bytes":self.metrics.ring_bytes,"pending_snapshots":self.metrics.pending_snapshots,
        "ring_head":self.metrics.ring_head,"ring_tail":self.metrics.ring_tail,"ring_size":self.metrics.ring_size,
        "dropped_snapshots":self.metrics.dropped_snapshots,"slots":self.metrics.slots,
        "leases":self.metrics.leases,"metal_status":self.metrics.metal_status})
    }
}

struct Capture {
    origin: Instant,
    session: u64,
    records: Vec<Event>,
    path: Option<PathBuf>,
    active: bool,
    truncated: bool,
    observed_dropped: u64,
    export_state: &'static str,
    error: Option<String>,
    recorded: usize,
    export_taken: bool,
}

impl Capture {
    fn new(session: u64, path: Option<PathBuf>) -> Self {
        Self {
            origin: Instant::now(),
            session,
            records: Vec::with_capacity(MAX_EVENTS),
            path,
            active: true,
            truncated: false,
            observed_dropped: 0,
            export_state: "collecting",
            error: None,
            recorded: 0,
            export_taken: false,
        }
    }
    fn push(
        &mut self,
        stage: &'static str,
        id: Identity,
        metrics: Metrics,
        elapsed_ns: u64,
    ) -> bool {
        if !self.active {
            return false;
        }
        // The lock serializes sequence/time observation; tests may inject a
        // regressing clock, which is rejected rather than silently relabeled.
        if self
            .records
            .last()
            .is_some_and(|last| elapsed_ns < last.elapsed_ns)
        {
            self.observed_dropped += 1;
            return false;
        }
        self.records.push(Event {
            sequence: self.recorded as u64,
            elapsed_ns,
            session: self.session,
            stage,
            id,
            metrics,
        });
        self.recorded += 1;
        if self.records.len() == MAX_EVENTS {
            self.active = false;
            self.truncated = true;
        }
        true
    }
    fn take_export(&mut self) -> Option<Export> {
        if self.export_taken {
            return None;
        }
        self.export_taken = true;
        self.active = false;
        self.export_state = if self.path.is_some() {
            "pending"
        } else {
            "not_requested"
        };
        Some(Export {
            session: self.session,
            path: self.path.take(),
            records: std::mem::take(&mut self.records),
            truncated: self.truncated,
            observed_dropped: self.observed_dropped,
        })
    }
}

/// Cold startup only. Disabled hooks never initialize a OnceLock/Mutex, read an
/// environment variable, allocate, or query a clock.
pub(crate) fn initialize_from_env() {
    if std::env::var("MUGUET_XR_LATENCY_TRACE").as_deref() != Ok("1") {
        return;
    }
    if CAPTURE.get().is_some() {
        return;
    }
    let path = std::env::var_os("MUGUET_XR_LATENCY_TRACE_PATH").map(PathBuf::from);
    let session = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut capture = Capture::new(session, path);
    if capture
        .path
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        capture.active = false;
        capture.export_state = "rejected";
        capture.error = Some("Latency trace path must be absolute".into());
    }
    let active = capture.active;
    if CAPTURE.set(Mutex::new(capture)).is_ok() && active {
        // The only export worker/channel allocation happens at cold startup,
        // before any producer/frame locks or tracing are active.
        match export_worker() {
            Ok(sender) => {
                let _ = EXPORT_TX.set(sender);
                ENABLED.store(true, Ordering::Release);
            }
            Err(error) => {
                if let Ok(mut capture) = CAPTURE.get().unwrap().lock() {
                    capture.active = false;
                    capture.export_state = "failed";
                    capture.error = Some(error.to_string());
                }
            }
        }
    }
}

#[inline]
pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Nonwrapped position distance only. The original start size argument's wrap
/// extent is not certified here; preserve unknown wrapped occupancy explicitly.
pub(crate) fn ring_bytes(head: i32, tail: i32) -> Option<u64> {
    let head = u64::try_from(head).ok()?;
    let tail = u64::try_from(tail).ok()?;
    if head < 64 || tail < 64 {
        return None;
    }
    (head >= tail).then_some(head.saturating_sub(tail))
}

pub(crate) fn record(stage: &'static str, id: Identity, metrics: Metrics) {
    if !enabled() {
        return;
    }
    let Some(capture) = CAPTURE.get() else {
        return;
    };
    let export = {
        let Ok(mut capture) = capture.lock() else {
            return;
        };
        if !capture.active {
            return;
        }
        let elapsed_ns = capture.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        capture.push(stage, id, metrics, elapsed_ns);
        if !capture.active {
            capture.take_export()
        } else {
            None
        }
    };
    if let Some(export) = export {
        ENABLED.store(false, Ordering::Release);
        enqueue_export(export);
    }
}

pub(crate) fn finish() {
    if !ENABLED.swap(false, Ordering::AcqRel) {
        return;
    }
    let Some(capture) = CAPTURE.get() else {
        return;
    };
    let export = {
        let Ok(mut capture) = capture.lock() else {
            return;
        };
        if !capture.active {
            return;
        }
        capture.take_export()
    };
    if let Some(export) = export {
        enqueue_export(export);
    }
}

struct Export {
    session: u64,
    path: Option<PathBuf>,
    records: Vec<Event>,
    truncated: bool,
    observed_dropped: u64,
}
fn export_worker() -> std::io::Result<SyncSender<Export>> {
    let (sender, receiver) = mpsc::sync_channel::<Export>(1);
    std::thread::Builder::new()
        .name("world-latency-export".into())
        .spawn(move || {
            let Ok(export) = receiver.recv() else {
                return;
            };
            let result = if export.path.is_some() {
                write_export(&export)
            } else {
                Ok(())
            };
            if let Some(capture) = CAPTURE.get() {
                if let Ok(mut capture) = capture.lock() {
                    match result {
                        Ok(()) => {
                            capture.export_state = if export.path.is_some() {
                                "written"
                            } else {
                                "not_requested"
                            }
                        }
                        Err(error) => {
                            capture.export_state = "failed";
                            capture.error = Some(error.to_string());
                        }
                    }
                }
            }
        })?;
    Ok(sender)
}

fn enqueue_export(export: Export) {
    // A single preallocated channel slot receives at most one snapshot. The
    // caller may still hold a world/frame lock: never spawn, serialize or do
    // filesystem work here. Only the cold-start worker performs persistence.
    let result = EXPORT_TX
        .get()
        .ok_or("export worker unavailable")
        .and_then(|sender| {
            sender
                .try_send(export)
                .map_err(|_| "export channel unavailable")
        });
    if let Err(error) = result {
        if let Some(capture) = CAPTURE.get() {
            if let Ok(mut capture) = capture.lock() {
                capture.export_state = "failed";
                capture.error = Some(error.into());
            }
        }
    }
}

fn write_export(export: &Export) -> std::io::Result<()> {
    use std::io::Write;
    let path = export
        .path
        .as_ref()
        .ok_or_else(|| std::io::Error::other("no export path"))?;
    if !path.is_absolute() {
        return Err(std::io::Error::other("export path must be absolute"));
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let header = json!({"type":"header","schema":1,"session_id":export.session,"pid":std::process::id(),
        "clock":"native Instant elapsed nanoseconds","presentation_clock":"metadata only; not native elapsed",
        "max_events":MAX_EVENTS,"max_file_bytes":MAX_EXPORT_BYTES,
        "scope":"observed source pipeline stages; not GPU-only or motion-to-photon timing; independent eyes"});
    let mut bytes = serde_json::to_vec(&header).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    let mut written = bytes.len();
    let mut exported = 0;
    for event in &export.records {
        let mut line = serde_json::to_vec(&event.json()).map_err(std::io::Error::other)?;
        line.push(b'\n');
        // Reserve a bounded footer. No partial event or over-limit file.
        if line.len() > MAX_EXPORT_BYTES.saturating_sub(written + 1024) {
            break;
        }
        file.write_all(&line)?;
        written += line.len();
        exported += 1;
    }
    let mut footer=serde_json::to_vec(&json!({"type":"footer","session_id":export.session,
        "recorded":export.records.len(),"exported":exported,"persistence_omitted":export.records.len()-exported,
        "record_limit_reached":export.truncated,"observed_dropped_events":export.observed_dropped,
        "events_after_stop_unobserved":true,"same_epoch_stereo_certified":false})).map_err(std::io::Error::other)?;
    footer.push(b'\n');
    if footer.len() > 1024 || written + footer.len() > MAX_EXPORT_BYTES {
        return Err(std::io::Error::other("footer exceeds export bound"));
    }
    file.write_all(&footer)?;
    file.sync_all()
}

pub(crate) fn status_json() -> Value {
    let Some(capture) = CAPTURE.get() else {
        return json!({"schema":1,"enabled":false,"state":"disabled"});
    };
    let Ok(capture) = capture.lock() else {
        return json!({"schema":1,"state":"lock_failed"});
    };
    let status = json!({"schema":1,"enabled":capture.active,"session_id":capture.session,
        "state":capture.export_state,"recorded":capture.recorded,"max_events":MAX_EVENTS,
        "record_limit_reached":capture.truncated,"observed_dropped_events":capture.observed_dropped,
        "events_after_stop_unobserved":!capture.active,"error":capture.error,
        "clock":"native Instant elapsed nanoseconds","max_file_bytes":MAX_EXPORT_BYTES});
    if serde_json::to_vec(&status).is_ok_and(|bytes| bytes.len() <= MAX_STATUS_BYTES) {
        status
    } else {
        json!({"schema":1,"state":"status_limit","session_id":capture.session})
    }
}

#[no_mangle]
pub extern "C" fn sv_world_latency_enabled() -> u32 {
    u32::from(enabled())
}
#[no_mangle]
pub extern "C" fn sv_world_latency_finish() {
    finish();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(epoch: u64, eye: u32) -> Identity {
        Identity {
            epoch,
            request: Some(3),
            head: Some(7),
            camera: Some(8),
            game: Some(9),
            eye: Some(eye),
            views: Some(2),
            lease: Some(10),
            presentation_time: Some(100.),
            ..Default::default()
        }
    }
    #[test]
    fn disabled_hooks_do_not_initialize_capture() {
        assert!(!enabled());
        assert!(CAPTURE.get().is_none());
        record("probe", Identity::default(), Metrics::default());
        finish();
        assert!(CAPTURE.get().is_none());
    }
    #[test]
    fn cap_stops_without_growing() {
        let mut c = Capture::new(5, None);
        for n in 0..MAX_EVENTS + 10 {
            c.push("camera", id(1, 0), Metrics::default(), n as u64);
        }
        assert_eq!(c.records.len(), MAX_EVENTS);
        assert!(!c.active);
        assert!(c.truncated);
    }
    #[test]
    fn backwards_clock_is_rejected() {
        let mut c = Capture::new(5, None);
        c.push("a", id(1, 0), Metrics::default(), 10);
        assert!(!c.push("b", id(1, 0), Metrics::default(), 9));
        assert_eq!(c.observed_dropped, 1);
        assert_eq!(c.records.len(), 1);
    }
    #[test]
    fn request_unknowns_are_not_invented() {
        let mut c = Capture::new(5, None);
        c.push(
            "request",
            Identity {
                epoch: 2,
                request: Some(3),
                ..Default::default()
            },
            Metrics::default(),
            0,
        );
        let j = c.records[0].json();
        assert!(j["head"].is_null());
        assert!(j["eye"].is_null());
        assert_eq!(j["session_id"], 5);
    }
    #[test]
    fn old_async_identity_never_becomes_new_lifecycle() {
        let mut c = Capture::new(5, None);
        let pending = id(1, 0);
        c.push("camera_consumed", pending, Metrics::default(), 1);
        c.push("camera_consumed", id(2, 0), Metrics::default(), 2);
        c.push("snapshot_completed", pending, Metrics::default(), 3);
        assert_eq!(c.records[2].id.epoch, 1);
        assert_eq!(c.records[2].session, 5);
        assert_ne!(c.records[1].id, c.records[2].id);
    }
    #[test]
    fn captured_sessions_are_separate_even_if_ids_match() {
        let mut a = Capture::new(5, None);
        let mut b = Capture::new(6, None);
        a.push("camera", id(1, 0), Metrics::default(), 1);
        b.push("camera", id(1, 0), Metrics::default(), 1);
        assert_ne!(a.records[0].session, b.records[0].session);
        assert_eq!(a.records[0].id, b.records[0].id);
    }
    #[test]
    fn occupancy_preserves_unknown_wrap_and_invalid_positions() {
        assert_eq!(ring_bytes(100, 80), Some(20));
        assert_eq!(ring_bytes(80, 1000), None);
        assert_eq!(ring_bytes(80, 80), Some(0));
        assert_eq!(ring_bytes(-1, 80), None);
        assert_eq!(ring_bytes(1024, 80), Some(944));
        assert_eq!(ring_bytes(80, 10), None);
    }
    #[test]
    fn independent_eye_and_epoch_identities_survive() {
        let mut c = Capture::new(5, None);
        for (n, (epoch, eye)) in [(1, 0), (1, 1), (2, 0)].into_iter().enumerate() {
            c.push("acquire", id(epoch, eye), Metrics::default(), n as u64);
        }
        assert_ne!(c.records[0].id, c.records[1].id);
        assert_ne!(c.records[0].id, c.records[2].id);
    }
    #[test]
    fn one_clock_sequences_equal_time_calls() {
        let mut c = Capture::new(5, None);
        for stage in [
            "camera", "ring", "snapshot", "acquire", "commit", "complete",
        ] {
            c.push(stage, id(1, 0), Metrics::default(), 3);
        }
        for (n, e) in c.records.iter().enumerate() {
            assert_eq!(e.sequence, n as u64);
            assert_eq!(e.session, 5);
            assert_eq!(e.id, id(1, 0));
        }
    }
    #[test]
    fn export_snapshot_moves_records_and_stops() {
        let mut c = Capture::new(5, None);
        c.push("acquire", id(1, 0), Metrics::default(), 1);
        let export = c.take_export().unwrap();
        assert!(!c.active);
        assert!(c.records.is_empty());
        assert_eq!(export.records.len(), 1);
        assert_eq!(c.recorded, 1);
        assert!(c.take_export().is_none());
    }
    #[test]
    fn cap_and_finish_cannot_export_twice() {
        let mut c = Capture::new(5, None);
        for n in 0..MAX_EVENTS {
            c.push("camera", id(1, 0), Metrics::default(), n as u64);
        }
        assert!(c.take_export().is_some());
        assert!(c.take_export().is_none());
        assert!(!c.push("complete", id(1, 0), Metrics::default(), MAX_EVENTS as u64));
    }
    #[test]
    fn create_new_preserves_existing_file() {
        let path = std::env::temp_dir().join(format!("sv-latency-existing-{}", std::process::id()));
        std::fs::write(&path, b"preserve").unwrap();
        let export = Export {
            session: 5,
            path: Some(path.clone()),
            records: vec![],
            truncated: false,
            observed_dropped: 0,
        };
        assert!(write_export(&export).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn maximal_export_is_bounded_and_parseable() {
        let path = std::env::temp_dir().join(format!("sv-latency-bounded-{}", std::process::id()));
        let mut c = Capture::new(u64::MAX, Some(path.clone()));
        for n in 0..MAX_EVENTS {
            c.push(
                "snapshot_complete",
                id(u64::MAX, 1),
                Metrics {
                    ring_bytes: Some(u64::MAX),
                    ..Default::default()
                },
                n as u64,
            );
        }
        write_export(&c.take_export().unwrap()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= MAX_EXPORT_BYTES);
        let lines: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.last().unwrap()["type"], "footer");
        assert_eq!(lines.last().unwrap()["same_epoch_stereo_certified"], false);
        assert_eq!(
            lines.len() - 2,
            lines.last().unwrap()["exported"].as_u64().unwrap() as usize
        );
        std::fs::remove_file(path).unwrap();
    }
}
