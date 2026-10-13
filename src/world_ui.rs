//! One opt-in, bounded draw-state trace for identifying the final color/HUD boundary.
//! The existing "UI depth clear" only proves a depth-clear mechanism, not that
//! the color image is post-tonemap or that following draws are HUD (vs weapons).
//! This module never changes GPU state, guest memory, or a published texture.
use crate::xr_view::WorldView;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, Ordering}, LazyLock, Mutex};

const FIRST: usize = 16;
const BOUNDARIES: usize = 16;
const TAIL: usize = 224;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const MAX_STATUS_BYTES: usize = 24 * 1024;
static CLAIMED: AtomicBool = AtomicBool::new(false);
static STATUS: LazyLock<Mutex<Value>> = LazyLock::new(|| Mutex::new(json!({
    "schema":1,"enabled":false,"state":"not_started"
})));

pub fn status_json() -> Value {
    STATUS.lock().map(|status| status.clone())
        .unwrap_or_else(|_| json!({"schema":1,"state":"status_lock_poisoned"}))
}

pub(crate) fn marker(view: &WorldView) -> Value {
    json!({"epoch":view.provenance_epoch,"camera":view.camera_sequence,
        "request":view.request_frame,"head":view.head_pose_frame,
        "time":view.presentation_time_seconds,"eye":view.eye_index,"views":view.view_count,
        "headset_applied":view.headset_applied,"reverse_z":view.depth_reverse_z})
}

pub(crate) struct ColorPhaseTrace {
    enabled: bool,
    finished: bool,
    path: Option<PathBuf>,
    start: Option<(u64, WorldView)>,
    draws: u64,
    discarded_presents: u64,
    boundary_count: u64,
    first: Vec<Value>,
    boundaries: Vec<Value>,
    tail: VecDeque<Value>,
}

impl ColorPhaseTrace {
    pub(crate) fn from_env() -> Self {
        let requested = std::env::var("MUGUET_XR_COLOR_PHASE_TRACE").is_ok_and(|value| value == "1");
        let enabled = requested && !CLAIMED.swap(requested, Ordering::Relaxed);
        let path = std::env::var_os("MUGUET_XR_COLOR_PHASE_TRACE_PATH").map(PathBuf::from);
        let trace = Self::new(enabled, path);
        if enabled {
            if let Ok(mut status) = STATUS.lock() {
                *status = json!({"schema":1,"enabled":true,"state":"waiting_for_qualified_world_draw"});
            }
        }
        trace
    }

    fn new(enabled: bool, path: Option<PathBuf>) -> Self {
        Self { enabled, finished:false, path, start:None, draws:0, discarded_presents:0,
            boundary_count:0, first:Vec::new(), boundaries:Vec::new(), tail:VecDeque::new() }
    }

    /// Called before building any JSON so the normal rendering path allocates nothing.
    pub(crate) fn wants_draw(&self, qualified_start: bool) -> bool {
        self.enabled && !self.finished && (self.start.is_some() || qualified_start)
    }

    pub(crate) fn record_draw(&mut self, game_frame: u64, qualified: Option<WorldView>,
        mut record: Value, depth_clear_boundary: bool) {
        if !self.wants_draw(qualified.is_some()) { return; }
        if self.start.is_none() {
            let Some(view) = qualified.filter(|view| view.headset_applied) else { return; };
            self.start = Some((game_frame, view));
            if let Ok(mut status) = STATUS.lock() {
                *status = json!({"schema":1,"enabled":true,"state":"recording",
                    "game_frame":game_frame,"start_view":marker(&view)});
            }
        }
        self.draws += 1;
        record["ordinal"] = self.draws.into();
        // A malformed/unusually broad state must not defeat the memory bound.
        if serde_json::to_vec(&record).map_or(true, |bytes| bytes.len() > MAX_RECORD_BYTES) {
            record = json!({"ordinal":self.draws,"record_omitted":"state_exceeds_16_KiB",
                "depth_clear_boundary":depth_clear_boundary});
        }
        if self.first.len() < FIRST { self.first.push(record); }
        else if depth_clear_boundary && self.boundaries.len() < BOUNDARIES {
            self.boundaries.push(record);
        } else {
            if self.tail.len() == TAIL { self.tail.pop_front(); }
            self.tail.push_back(record);
        }
        if depth_clear_boundary { self.boundary_count += 1; }
    }

    pub(crate) fn present(&mut self, game_frame: u64, view: Option<&WorldView>, final_target: Value) {
        if !self.enabled || self.finished || self.start.is_none() { return; }
        let (started_frame, start) = self.start.unwrap();
        if started_frame != game_frame || !view.is_some_and(|view|
            view.headset_applied && crate::world_frame::shares_screen_rays(view, &start)) {
            // Loading, closing and camera changes cannot certify an unrelated present.
            self.discarded_presents += 1;
            self.start = None; self.draws = 0; self.boundary_count = 0;
            self.first.clear(); self.boundaries.clear(); self.tail.clear();
            if let Ok(mut status) = STATUS.lock() {
                *status = json!({"schema":1,"enabled":true,"state":"waiting_for_matching_present",
                    "discarded_presents":self.discarded_presents});
            }
            return;
        }
        self.finished = true;
        let mut records: Vec<Value> = self.first.drain(..).chain(self.boundaries.drain(..))
            .chain(self.tail.drain(..)).collect();
        records.sort_by_key(|record| record["ordinal"].as_u64().unwrap_or(0));
        let report = json!({"schema":1,"enabled":true,"state":"captured","game_frame":game_frame,
            "scope":"successful draw-state samples; not a full copy/compute/clear dependency trace",
            "boundary_caveat":"depth-clear shader match does not certify HUD or post-tonemap color",
            "start_view":marker(&start),"present_view":view.map(marker),
            "source_projection":start.projection_from_eye,"source_game_from_eye":start.game_from_eye,
            "draws_observed":self.draws,"records_retained":records.len(),
            "draws_omitted":self.draws.saturating_sub(records.len() as u64),
            "retention":{"first":FIRST,"depth_clear_boundaries":BOUNDARIES,"tail":TAIL},
            "depth_clear_boundaries_observed":self.boundary_count,"discarded_presents":self.discarded_presents,
            "final_target":final_target,"records":records});
        let persistence = persist(self.path.as_deref(), &report);
        let summary = bounded_summary(&report, persistence);
        if let Ok(mut status) = STATUS.lock() { *status = summary; }
    }
}

fn persist(path: Option<&std::path::Path>, report: &Value) -> Value {
    let Some(path) = path else { return json!({"state":"path_not_configured"}); };
    if !path.is_absolute() { return json!({"state":"error","reason":"trace path must be absolute"}); }
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        serde_json::to_writer(&mut file, report)?;
        file.write_all(b"\n")?;
        file.flush()
    })();
    match result {
        Ok(()) => json!({"state":"written","path":path.to_string_lossy()}),
        Err(error) => json!({"state":"error","reason":error.to_string()}),
    }
}

fn bounded_summary(report: &Value, persistence: Value) -> Value {
    let mut result = report.clone();
    result.as_object_mut().unwrap().remove("records");
    result["persistence"] = persistence;
    let all = report["records"].as_array().unwrap();
    // Prefer final draws and the last clear boundary. The complete trace stays in its own file.
    let mut selected: Vec<Value> = all.iter().rev().take(4).cloned().collect();
    if let Some(boundary) = all.iter().rev().find(|row| row["depth_clear_boundary"] == true) {
        if !selected.iter().any(|row| row["ordinal"] == boundary["ordinal"]) { selected.push(boundary.clone()); }
    }
    if let Some(first) = all.first() {
        if !selected.iter().any(|row| row["ordinal"] == first["ordinal"]) { selected.push(first.clone()); }
    }
    while !selected.is_empty() {
        result["selected_records"] = json!(selected);
        if serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() <= MAX_STATUS_BYTES) { return result; }
        selected.pop();
    }
    result["selected_records"] = json!([]);
    // Metadata is small in production; guard against unexpectedly long environment paths too.
    if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > MAX_STATUS_BYTES) {
        return json!({"schema":1,"enabled":true,"state":"captured","summary_omitted":"metadata_exceeds_24_KiB"});
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> WorldView { WorldView { headset_applied:true, camera_sequence:7,
        request_frame:4, head_pose_frame:3, provenance_epoch:2, presentation_time_seconds:1.,
        eye_index:0, view_count:2, game_from_eye:[0.;16], game_from_stage:[0.;16],
        projection_from_eye:[0.;16], depth_reverse_z:true } }

    #[test]
    fn disabled_trace_and_unqualified_draws_do_not_arm() {
        let mut trace = ColorPhaseTrace::new(false, None);
        trace.record_draw(1, Some(view()), json!({}), false);
        assert!(trace.start.is_none()); assert!(!trace.wants_draw(true));
        let mut trace = ColorPhaseTrace::new(true, None);
        trace.record_draw(1, None, json!({}), false); assert!(trace.start.is_none());
    }

    #[test]
    fn bounded_retention_keeps_first_boundaries_and_final_draws() {
        let mut trace = ColorPhaseTrace::new(true, None);
        for ordinal in 1..=2000 { trace.record_draw(5, Some(view()),
            json!({"depth_clear_boundary":ordinal==1200}), ordinal==1200); }
        assert_eq!(trace.first.len(), FIRST); assert_eq!(trace.boundaries.len(), 1);
        assert_eq!(trace.tail.len(), TAIL); assert_eq!(trace.tail.back().unwrap()["ordinal"], 2000);
        assert!(trace.first.len()+trace.boundaries.len()+trace.tail.len() <= 256);
        trace.present(5, Some(&view()), json!({"id":1}));
        assert!(trace.finished); assert!(!trace.wants_draw(true));
    }

    #[test]
    fn foreign_present_discards_and_next_matching_present_finishes_once() {
        let mut trace = ColorPhaseTrace::new(true, None);
        trace.record_draw(5, Some(view()), json!({}), false);
        let foreign = WorldView { camera_sequence:8, ..view() };
        trace.present(5, Some(&foreign), json!({}));
        assert!(trace.start.is_none()); assert!(!trace.finished); assert_eq!(trace.discarded_presents, 1);
        trace.record_draw(6, Some(view()), json!({}), false);
        trace.present(6, Some(&view()), json!({"id":9})); assert!(trace.finished);
        trace.record_draw(7, Some(view()), json!({}), false); assert_eq!(trace.draws, 1);
    }

    #[test]
    fn status_hard_cap_and_trace_file_never_overwrites() {
        let report = json!({"records":(0..256).map(|n|json!({"ordinal":n,
            "payload":"x".repeat(15000),"depth_clear_boundary":n==250})).collect::<Vec<_>>()});
        let summary = bounded_summary(&report, json!({"state":"path_not_configured"}));
        assert!(serde_json::to_vec(&summary).unwrap().len() <= MAX_STATUS_BYTES);
        let path = std::env::temp_dir().join(format!("sv-color-phase-test-{}-{}.json", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        assert_eq!(persist(Some(&path), &json!({"original":true}))["state"], "written");
        assert_eq!(persist(Some(&path), &json!({"overwrite":true}))["state"], "error");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"original\":true}\n");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn oversized_draw_is_explicitly_omitted_and_all_three_retention_caps_hold() {
        let mut trace = ColorPhaseTrace::new(true, None);
        trace.record_draw(5, Some(view()), json!({"payload":"x".repeat(MAX_RECORD_BYTES)}), false);
        assert_eq!(trace.first[0]["record_omitted"], "state_exceeds_16_KiB");
        for _ in 1..2000 { trace.record_draw(5, Some(view()), json!({}), true); }
        assert_eq!(trace.first.len()+trace.boundaries.len()+trace.tail.len(), 256);
        assert_eq!(trace.boundary_count, 1999);
        assert_eq!(trace.tail.back().unwrap()["ordinal"], 2000);
    }
}
