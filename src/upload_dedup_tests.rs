//! Actual-GPU fixtures for the op47 redundant-upload guard.
//!
//! Each scenario drives the production `create_buffer`, `upload_buffer`,
//! `copy_region`, `clear_uav`, `copy_counter`, `destroy` and `flush` entry
//! points on a real Metal device twice: guard off, then guard on. Both runs
//! must leave the same bytes in every buffer, those bytes must be the ones the
//! scenario states, and the guard-on run must report exactly the expected
//! skips. An upload skipped when it mattered shows up as wrong bytes.
//!
//! The upload route comes from MUGUET_BUFFER_UPLOAD_MODE as usual, so the same
//! fixtures cover queue and pool in separate processes.
//! Run serially: `--test-threads=1`.
use super::*;
use crate::upload_dedup::{Caps, Totals, UploadDedup};

const PAGES: u32 = 4;
const SOURCE: u64 = 0x1_1000;
const VERTEX: u32 = 1;
const INDEX: u32 = 2;

fn gpu(guard: UploadDedup) -> Gpu {
    let mut config = wasmtime::Config::new(); config.wasm_threads(true); config.shared_memory(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let mem = SharedMemory::new(&engine, wasmtime::MemoryType::shared(PAGES, PAGES)).unwrap();
    let mut gpu = Gpu::new(mem, 0, 65536, 0, None).unwrap();
    // Chosen here, whatever MUGUET_OP47_UPLOAD_DEDUP says in this process.
    gpu.upload_dedup = guard;
    gpu
}

/// Distinct, position-dependent, never-zero bytes.
fn pattern(count: usize, salt: u8) -> Vec<u8> { (0..count).map(|i| (i as u8).wrapping_mul(7).wrapping_add(salt) | 1).collect() }

/// `size` zero bytes with each (offset, bytes) laid over them in order.
fn image(size: usize, writes: &[(usize, &[u8])]) -> Vec<u8> {
    let mut image = vec![0u8; size];
    for (offset, bytes) in writes.iter().copied() { image[offset..offset + bytes.len()].copy_from_slice(bytes); }
    image
}

/// One guest upload through the production entry point.
fn upload(g: &mut Gpu, id: u32, offset: u64, data: &[u8], no_overwrite: bool) {
    assert!(crate::write_bytes(&g.mem, SOURCE, data));
    g.upload_buffer(id, offset, data.len() as u64, SOURCE, no_overwrite);
}
fn op47(g: &mut Gpu, id: u32, offset: u64, data: &[u8]) { upload(g, id, offset, data, true); }
fn op12(g: &mut Gpu, id: u32, offset: u64, data: &[u8]) { upload(g, id, offset, data, false); }

/// A UAV view of a buffer, as `clear_uav` and the counter operations expect.
fn buffer_uav(g: &mut Gpu, view: u32, res: u32) {
    g.set_obj(view, Obj::Uav(ViewObj { res, is_buf: true, fmt: 28, vdim: 4, a: 0, b: 4, c: 0, d: 0, views: HashMap::new() }));
}

/// Submits everything pending through the production path, then reads the
/// buffer's real GPU bytes with a copy of the fixture's own.
fn contents(g: &mut Gpu, id: u32) -> Vec<u8> {
    g.flush();
    let (buffer, size) = g.buf(id).and_then(|b| b.gpu.clone().map(|buffer| (buffer, b.bytes))).expect("fixture buffer has GPU storage");
    let output = g.device.create_buffer(&BufferDescriptor { label: Some("op47 dedup fixture readback"), size,
        usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut encoder = g.device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(&buffer, 0, &output, 0, size);
    let (tx, rx) = std::sync::mpsc::channel();
    encoder.map_buffer_on_submit(&output, MapMode::Read, .., move |result| { let _ = tx.send(result); });
    let submitted = g.queue.submit([encoder.finish()]);
    g.device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(Duration::from_secs(10)) }).unwrap();
    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    let bytes = output.slice(..).get_mapped_range().unwrap().to_vec();
    output.unmap();
    bytes
}

struct Outcome { bytes: Vec<Vec<u8>>, totals: Totals }

/// Runs `scenario` on a fresh device with the guard off, then on another with
/// it on, and checks everything that must hold for any scenario.
fn off_then_on(caps: Caps, scenario: impl Fn(&mut Gpu) -> Vec<Vec<u8>>) -> Outcome {
    let run = |enabled: bool| {
        let mut g = gpu(UploadDedup::new(enabled, caps));
        let bytes = scenario(&mut g);
        assert_eq!(g.errors.load(Ordering::Relaxed), 0, "guard enabled={enabled}: the GPU reported a validation error");
        let (discard, keep) = (g.buffer_uploads[UPLOAD_OP12], g.buffer_uploads[UPLOAD_OP47]);
        (bytes, g.upload_dedup.totals(), [discard.api_wall.calls, discard.bytes, keep.api_wall.calls, keep.bytes])
    };
    let (expected, inert, legacy) = run(false);
    let (bytes, totals, guarded) = run(true);
    assert_eq!(inert, Totals::default(), "a disabled guard must not compare, count or record anything");
    assert_eq!(bytes, expected, "the guard changed GPU buffer contents");
    // Every skip is one op47 route call the legacy run made and this run did
    // not. Nothing else about the upload stream may move.
    assert_eq!((guarded[0], guarded[1]), (legacy[0], legacy[1]), "op12 route calls and bytes");
    assert_eq!(legacy[2], guarded[2] + totals.saved_calls, "op47 route calls");
    assert_eq!(legacy[3], guarded[3] + totals.saved_bytes, "op47 route bytes");
    assert_eq!(totals.checked, totals.hits + totals.misses);
    assert_eq!(totals.saved_calls, totals.hits);
    assert!(totals.records <= caps.records && totals.record_bytes <= caps.total_bytes, "caps exceeded: {totals:?}");
    Outcome { bytes, totals }
}

#[test]
fn metal_exact_repeats_are_skipped_and_changes_are_uploaded() {
    let (a, b) = (pattern(16, 3), pattern(16, 5));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 64, VERTEX, 0, 0);
        g.create_buffer(2, 64, INDEX, 0, 0);
        op47(g, 1, 0, &a);
        op47(g, 1, 0, &a); // repeat
        op47(g, 2, 16, &b);
        op47(g, 1, 0, &a); // repeat: another buffer's upload did not disturb it
        let first = contents(g, 1);
        op47(g, 1, 0, &a); // repeat across a submission
        op47(g, 1, 0, &b); // changed bytes
        op47(g, 1, 0, &b); // repeat
        op47(g, 1, 0, &a); // the first bytes again: a change, not a repeat
        op47(g, 2, 16, &b); // repeat
        vec![first, contents(g, 1), contents(g, 2)]
    });
    assert_eq!(outcome.bytes, vec![
        image(64, &[(0, &a[..])]),
        image(64, &[(0, &a[..])]),
        image(64, &[(16, &b[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 9, hits: 5, misses: 4, saved_calls: 5, saved_bytes: 80,
        dropped_changed: 2, records: 2, record_bytes: 32, ..Default::default() });
}

#[test]
fn metal_alternating_disjoint_ranges_are_both_skipped() {
    let (a, b, c) = (pattern(16, 71), pattern(16, 73), pattern(16, 75));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 64, VERTEX, 0, 0);
        op47(g, 1, 0, &a);
        op47(g, 1, 32, &b); // a disjoint range: its own first upload
        op47(g, 1, 0, &a); // repeat
        op47(g, 1, 32, &b); // repeat, with the other range in between
        let first = contents(g, 1);
        // The alternation one record per buffer could never catch.
        for _ in 0..3 {
            op47(g, 1, 0, &a);
            op47(g, 1, 32, &b);
        }
        op47(g, 1, 16, &c); // [16, 32) touches both neighbours and keeps both
        op47(g, 1, 32, &b);
        op47(g, 1, 16, &c);
        op47(g, 1, 0, &a);
        // Equal bytes at another offset are a real upload, not a repeat.
        op47(g, 1, 48, &a);
        vec![first, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![
        image(64, &[(0, &a[..]), (32, &b[..])]),
        image(64, &[(0, &a[..]), (16, &c[..]), (32, &b[..]), (48, &a[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 15, hits: 11, misses: 4, saved_calls: 11, saved_bytes: 176,
        records: 4, record_bytes: 64, ..Default::default() });
}

#[test]
fn metal_changed_writes_drop_intersecting_ranges_and_keep_disjoint_ones() {
    let (low, mid, high) = (pattern(16, 81), pattern(16, 83), pattern(16, 85));
    let (wide, sub, all, gap) = (pattern(32, 87), pattern(4, 89), pattern(80, 91), pattern(16, 93));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 96, VERTEX, 0, 0);
        // Known ranges: [0, 16), [32, 48) and [64, 80).
        for _ in 0..2 {
            op47(g, 1, 0, &low);
            op47(g, 1, 32, &mid);
            op47(g, 1, 64, &high);
        }
        // [8, 40) reaches into the first two.
        op47(g, 1, 8, &wide);
        let after_wide = contents(g, 1);
        op47(g, 1, 64, &high); // untouched, still a repeat
        op47(g, 1, 0, &low); // has to upload: `wide` covered half of it
        op47(g, 1, 32, &mid); // and so does this
        let restored = contents(g, 1);
        // [8, 12) lies inside the first range only.
        op47(g, 1, 8, &sub);
        op47(g, 1, 32, &mid); // repeat
        op47(g, 1, 64, &high); // repeat
        op47(g, 1, 0, &low); // has to upload
        // The same offset with fewer bytes, even identical ones, is no repeat,
        // and it replaces the longer record.
        op47(g, 1, 32, &mid[..8]);
        op47(g, 1, 32, &mid);
        let after_sub = contents(g, 1);
        // [0, 80) covers all three.
        op47(g, 1, 0, &all);
        let after_all = contents(g, 1);
        op47(g, 1, 0, &low);
        op47(g, 1, 32, &mid);
        op47(g, 1, 64, &high);
        let after_three = contents(g, 1);
        // [16, 32) touches two of them and intersects none.
        op47(g, 1, 16, &gap);
        op47(g, 1, 0, &low);
        op47(g, 1, 32, &mid);
        op47(g, 1, 64, &high);
        op47(g, 1, 16, &gap);
        vec![after_wide, restored, after_sub, after_all, after_three, contents(g, 1)]
    });
    let known = image(96, &[(8, &wide[..]), (0, &low[..]), (32, &mid[..]), (64, &high[..])]);
    assert_eq!(outcome.bytes, vec![
        image(96, &[(0, &low[..]), (32, &mid[..]), (64, &high[..]), (8, &wide[..])]),
        known.clone(),
        known,
        image(96, &[(0, &all[..])]),
        image(96, &[(0, &all[..]), (0, &low[..]), (32, &mid[..]), (64, &high[..])]),
        image(96, &[(0, &all[..]), (0, &low[..]), (16, &gap[..]), (32, &mid[..]), (64, &high[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 25, hits: 10, misses: 15, saved_calls: 10, saved_bytes: 160,
        dropped_changed: 11, records: 4, record_bytes: 64, ..Default::default() });
}

#[test]
fn metal_changes_and_repeats_inside_one_submission_keep_upload_order() {
    let (low, mid, wide) = (pattern(16, 95), pattern(16, 97), pattern(32, 99));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 96, VERTEX, 0, 0);
        // Nothing is submitted until the first readback: every upload below
        // sits in the same prelude, in call order.
        op47(g, 1, 0, &low);
        op47(g, 1, 32, &mid);
        op47(g, 1, 8, &wide); // reaches into both
        op47(g, 1, 0, &low); // so these two upload again, after it
        op47(g, 1, 32, &mid);
        op47(g, 1, 0, &low); // repeat
        op47(g, 1, 32, &mid); // repeat
        op47(g, 1, 8, &wide); // and `wide` once more, last
        let one_submission = contents(g, 1);
        op47(g, 1, 8, &wide); // repeat across the submission
        op47(g, 1, 0, &low);
        op47(g, 1, 32, &mid);
        vec![one_submission, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![
        image(96, &[(0, &low[..]), (32, &mid[..]), (8, &wide[..])]),
        image(96, &[(8, &wide[..]), (0, &low[..]), (32, &mid[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 11, hits: 3, misses: 8, saved_calls: 3, saved_bytes: 64,
        dropped_changed: 6, records: 2, record_bytes: 32, ..Default::default() });
}

#[test]
fn metal_op12_anywhere_in_the_buffer_forces_every_range_to_upload_again() {
    let (a, b, z) = (pattern(16, 7), pattern(16, 9), pattern(32, 11));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 96, VERTEX, 0, 0);
        op47(g, 1, 8, &a);
        op47(g, 1, 48, &b);
        op12(g, 1, 0, &z); // overwrites the first range
        op47(g, 1, 8, &a); // the same call as before, but it has to upload
        op47(g, 1, 48, &b); // forgotten too, although the op12 never reached it
        let mixed = contents(g, 1);
        op47(g, 1, 8, &a); // repeat
        op47(g, 1, 48, &b); // repeat
        op12(g, 1, 80, &z[..16]); // touches neither range; both are dropped anyway
        op47(g, 1, 8, &a);
        op47(g, 1, 48, &b);
        op12(g, 1, 8, &a); // identical bytes through op12 still drop the records
        op47(g, 1, 8, &a);
        op47(g, 1, 48, &b);
        op12(g, 1, 0, &[]); // and so does an empty op12
        op47(g, 1, 8, &a);
        op47(g, 1, 48, &b);
        vec![mixed, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![
        image(96, &[(0, &z[..]), (8, &a[..]), (48, &b[..])]),
        image(96, &[(0, &z[..]), (8, &a[..]), (48, &b[..]), (80, &z[..16])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 12, hits: 2, misses: 10, saved_calls: 2, saved_bytes: 32,
        purged: 8, records: 2, record_bytes: 32, ..Default::default() });
}

#[test]
fn metal_padded_tail_op47_keeps_the_legacy_route_and_drops_every_range() {
    let (a, b, tail) = (pattern(16, 61), pattern(16, 65), pattern(6, 63));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 64, VERTEX, 0, 0);
        for _ in 0..2 {
            op47(g, 1, 0, &a);
            op47(g, 1, 32, &b);
        }
        // Six bytes: a 4-byte body and a zero-padded tail word, both inside
        // the first range. Never compared, always uploaded.
        op47(g, 1, 4, &tail);
        let padded = contents(g, 1);
        op47(g, 1, 0, &a); // those bytes are gone, so this uploads
        op47(g, 1, 32, &b); // forgotten with it
        vec![padded, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![
        image(64, &[(0, &a[..]), (32, &b[..]), (4, &tail[..4]), (8, &[tail[4], tail[5], 0, 0][..])]),
        image(64, &[(0, &a[..]), (32, &b[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 6, hits: 2, misses: 4, bypasses: 1, saved_calls: 2, saved_bytes: 32,
        purged: 2, records: 2, record_bytes: 32, ..Default::default() });
}

#[test]
fn metal_oversized_op47_crossing_known_ranges_retains_disjoint_hits() {
    // A 16-byte record cap makes 40- and 20-byte uploads "oversized".
    let caps = Caps { record_bytes: 16, ..Caps::DEFAULT };
    let (a, b, c) = (pattern(16, 101), pattern(16, 103), pattern(16, 105));
    let (wide, far) = (pattern(40, 107), pattern(20, 109));
    let outcome = off_then_on(caps, |g| {
        g.create_buffer(1, 128, VERTEX, 0, 0);
        for _ in 0..2 {
            op47(g, 1, 0, &a);
            op47(g, 1, 32, &b);
            op47(g, 1, 64, &c);
        }
        // [8, 48) is too long to record and crosses the first two ranges.
        op47(g, 1, 8, &wide);
        let crossed = contents(g, 1);
        op47(g, 1, 0, &a); // both have to upload again
        op47(g, 1, 32, &b);
        op47(g, 1, 64, &c); // disjoint: its exact repeat still skips
        let restored = contents(g, 1);
        // [96, 116) is too long as well and reaches none of them.
        op47(g, 1, 96, &far);
        op47(g, 1, 0, &a);
        op47(g, 1, 32, &b);
        op47(g, 1, 64, &c);
        vec![crossed, restored, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![
        image(128, &[(0, &a[..]), (32, &b[..]), (64, &c[..]), (8, &wide[..])]),
        image(128, &[(8, &wide[..]), (0, &a[..]), (32, &b[..]), (64, &c[..])]),
        image(128, &[(8, &wide[..]), (0, &a[..]), (32, &b[..]), (64, &c[..]), (96, &far[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 12, hits: 7, misses: 5, bypasses: 2, saved_calls: 7, saved_bytes: 112,
        purged: 2, records: 3, record_bytes: 48, ..Default::default() });
}

#[test]
fn metal_oversized_single_crossing_budget_boundary_and_padded_purge_keep_exact_bytes() {
    let caps = Caps { record_bytes: 16, total_bytes: 64, ..Caps::DEFAULT };
    let (a,b,c) = (pattern(16,111),pattern(16,113),pattern(16,115));
    let (cross,large,padded) = (pattern(20,117),pattern(160,119),pattern(21,121));
    let outcome = off_then_on(caps, |g| {
        g.create_buffer(1,256,VERTEX,0,0);
        op47(g,1,0,&a); op47(g,1,32,&b); op47(g,1,64,&c);
        op47(g,1,8,&cross); // exact [8,28), drops only A
        let crossed = contents(g,1);
        op47(g,1,0,&a); op47(g,1,32,&b); op47(g,1,64,&c); // one miss, two hits
        // Larger than the entire 64-byte budget, ending exactly at GPU end.
        // It is neither compared nor stored and touches no cached range.
        op47(g,1,96,&large);
        assert_eq!(g.upload_dedup.totals().records, if g.upload_dedup.enabled() {3} else {0});
        op47(g,1,0,&a); op47(g,1,32,&b); op47(g,1,64,&c); // three hits
        let boundary = contents(g,1);
        // Length21 is also oversized but fails alignment before the size gate.
        // Legacy body+zero-padded tail still writes24 bytes and purges all.
        op47(g,1,128,&padded);
        let tail = contents(g,1);
        op47(g,1,0,&a); op47(g,1,32,&b); op47(g,1,64,&c); // all three miss
        vec![crossed,boundary,tail,contents(g,1)]
    });
    assert_eq!(outcome.bytes, vec![
        image(256,&[(0,&a[..]),(32,&b[..]),(64,&c[..]),(8,&cross[..])]),
        image(256,&[(0,&a[..]),(32,&b[..]),(64,&c[..]),(8,&cross[..]),(0,&a[..]),(96,&large[..])]),
        image(256,&[(0,&a[..]),(32,&b[..]),(64,&c[..]),(8,&cross[..]),(0,&a[..]),(96,&large[..]),
            (128,&padded[..20]),(148,&[padded[20],0,0,0][..])]),
        image(256,&[(0,&a[..]),(32,&b[..]),(64,&c[..]),(8,&cross[..]),(0,&a[..]),(96,&large[..]),
            (128,&padded[..20]),(148,&[padded[20],0,0,0][..]),(0,&a[..]),(32,&b[..]),(64,&c[..])]),
    ]);
    // off_then_on also asserts actual route-call/byte reduction equals these
    // saved counters, while every readback matches the unguarded stream.
    assert_eq!(outcome.totals, Totals { checked:12,hits:5,misses:7,bypasses:3,saved_calls:5,saved_bytes:80,
        purged:4,records:3,record_bytes:48,..Default::default() });
}

#[test]
fn metal_destroy_recreate_and_replacement_start_without_records() {
    let (a, b) = (pattern(16, 11), pattern(16, 13));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 64, VERTEX, 0, 0);
        for _ in 0..2 {
            op47(g, 1, 0, &a);
            op47(g, 1, 32, &b);
        }
        let original = contents(g, 1);
        // The same guest id, a new zeroed GPU buffer: nothing is known about it.
        g.destroy(1);
        g.create_buffer(1, 64, VERTEX, 0, 0);
        op47(g, 1, 0, &a);
        op47(g, 1, 32, &b);
        let recreated = contents(g, 1);
        // Replaced without a destroy.
        g.create_buffer(1, 64, VERTEX, 0, 0);
        op47(g, 1, 0, &a);
        op47(g, 1, 32, &b);
        let replaced = contents(g, 1);
        op47(g, 1, 0, &a); // repeats on the newest object
        op47(g, 1, 32, &b);
        vec![original, recreated, replaced, contents(g, 1)]
    });
    let wanted = image(64, &[(0, &a[..]), (32, &b[..])]);
    assert_eq!(outcome.bytes, vec![wanted.clone(), wanted.clone(), wanted.clone(), wanted]);
    assert_eq!(outcome.totals, Totals { checked: 10, hits: 4, misses: 6, saved_calls: 4, saved_bytes: 64,
        purged: 4, records: 2, record_bytes: 32, ..Default::default() });
}

#[test]
fn metal_copy_clear_and_counter_destinations_forget_every_range_for_life() {
    let (a, b, source) = (pattern(16, 13), pattern(16, 15), pattern(64, 17));
    let words = [0x0101_0101u32, 0x0202_0202, 0x0303_0303, 0x0404_0404];
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        // 1: copy_region destination. 2: zero clear. 3: value clear. 4: counter destination.
        for id in 1..=4 {
            g.create_buffer(id, 64, VERTEX, 0, 0);
            for _ in 0..2 {
                op47(g, id, 0, &a);
                op47(g, id, 40, &b);
            }
        }
        g.create_buffer(5, 64, VERTEX, 0, 0);
        op12(g, 5, 0, &source);
        g.create_buffer(6, 16, BIND_UAV, 0, 4);
        buffer_uav(g, 7, 6);
        buffer_uav(g, 8, 2);
        buffer_uav(g, 9, 3);
        g.copy_region(1, 0, 0, 0, 0, 5, 0, false, 0, 0, 0, 0, 0, 0);
        g.clear_uav(8, [0; 4]);
        g.clear_uav(9, words);
        g.set_uav_counter(7, 0x1122_3344);
        g.copy_counter(4, 8, 7); // four bytes inside the first range only
        let mut bytes: Vec<Vec<u8>> = (1..=4).map(|id| contents(g, id)).collect();
        // The same op47 calls as before. Each GPU write replaced bytes, so
        // every one of these has to upload, now and for the object's lifetime,
        // including the second range of buffer 4 that the counter never reached.
        for id in 1..=4 {
            for _ in 0..2 {
                op47(g, id, 0, &a);
                op47(g, id, 40, &b);
            }
        }
        bytes.extend((1..=4).map(|id| contents(g, id)));
        bytes
    });
    let cleared: Vec<u8> = (0..16).flat_map(|word| words[word & 3].to_le_bytes()).collect();
    assert_eq!(outcome.bytes, vec![
        // After the GPU writes.
        source.clone(),
        vec![0u8; 64],
        cleared.clone(),
        image(64, &[(0, &a[..]), (40, &b[..]), (8, &[0x44u8, 0x33, 0x22, 0x11][..])]),
        // After the same op47 calls were issued again.
        image(64, &[(0, &source[..]), (0, &a[..]), (40, &b[..])]),
        image(64, &[(0, &a[..]), (40, &b[..])]),
        image(64, &[(0, &cleared[..]), (0, &a[..]), (40, &b[..])]),
        image(64, &[(0, &a[..]), (40, &b[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 16, hits: 8, misses: 8, bypasses: 16, saved_calls: 8, saved_bytes: 128,
        purged: 8, disqualified: 4, ..Default::default() });
}

#[test]
fn metal_copy_recorded_before_op47s_of_its_submission_is_never_trusted() {
    let (x, w, y) = (pattern(16, 19), pattern(16, 21), pattern(64, 23));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        g.create_buffer(1, 64, VERTEX, 0, 0);
        g.create_buffer(2, 64, VERTEX, 0, 0);
        op12(g, 2, 0, &y);
        // Recorded first, executed last: uploads run in the prelude, so this
        // copy lands AFTER the op47 calls below although it was issued first.
        g.copy_region(1, 0, 0, 0, 0, 2, 0, false, 0, 0, 0, 0, 0, 0);
        op47(g, 1, 0, &x);
        op47(g, 1, 32, &w);
        let after_copy = contents(g, 1);
        // A guard that had recorded those two would skip these two and leave
        // the copy's bytes in place.
        op47(g, 1, 0, &x);
        op47(g, 1, 32, &w);
        vec![after_copy, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![y.clone(), image(64, &[(0, &y[..]), (0, &x[..]), (32, &w[..])])]);
    assert_eq!(outcome.totals, Totals { bypasses: 4, disqualified: 1, ..Default::default() });
}

#[test]
fn metal_consumers_recorded_before_a_repeat_or_a_change_read_legacy_bytes() {
    let (x, w) = (pattern(16, 29), pattern(16, 31));
    let outcome = off_then_on(Caps::DEFAULT, |g| {
        for id in 1..=3 { g.create_buffer(id, 16, VERTEX, 0, 0); }
        op47(g, 1, 0, &x);
        // A consumer of buffer 1 is recorded, then the same bytes arrive
        // again: skipping that upload leaves what the consumer reads as is.
        g.copy_region(2, 0, 0, 0, 0, 1, 0, false, 0, 0, 0, 0, 0, 0);
        op47(g, 1, 0, &x);
        let repeated = contents(g, 2);
        // A consumer is recorded, then different bytes arrive: a no-overwrite
        // upload still precedes that consumer, exactly as without the guard.
        g.copy_region(3, 0, 0, 0, 0, 1, 0, false, 0, 0, 0, 0, 0, 0);
        op47(g, 1, 0, &w);
        let changed = contents(g, 3);
        vec![repeated, changed, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![x.clone(), w.clone(), w.clone()]);
    // Buffer 1 is only ever a copy SOURCE, so it stays tracked; 2 and 3 do not.
    assert_eq!(outcome.totals, Totals { checked: 3, hits: 1, misses: 2, saved_calls: 1, saved_bytes: 16,
        dropped_changed: 1, disqualified: 2, records: 1, record_bytes: 16, ..Default::default() });
}

/// An op47 after which the guard's table must still be inside its caps.
fn capped_op47(g: &mut Gpu, id: u32, offset: u64, data: &[u8], caps: Caps) {
    op47(g, id, offset, data);
    let totals = g.upload_dedup.totals();
    assert!(totals.records <= caps.records && totals.record_bytes <= caps.total_bytes, "caps exceeded: {totals:?}");
}

#[test]
fn metal_global_and_per_buffer_caps_evict_oldest_records_and_stay_exact() {
    // Four records in all, two per buffer, 16 bytes each at most, 48 bytes in all.
    const CAPS: Caps = Caps { records: 4, buffer_records: 2, record_bytes: 16, total_bytes: 48 };
    let (p0, p1, p2) = (pattern(8, 41), pattern(8, 43), pattern(8, 45));
    let (q0, q1, r0, r1) = (pattern(16, 47), pattern(16, 49), pattern(8, 51), pattern(16, 53));
    let wide = pattern(24, 55);
    let outcome = off_then_on(CAPS, |g| {
        for id in 1..=3 { g.create_buffer(id, 64, VERTEX, 0, 0); }
        capped_op47(g, 1, 0, &p0, CAPS);
        capped_op47(g, 1, 16, &p1, CAPS);
        capped_op47(g, 1, 32, &p2, CAPS); // buffer 1's third range: its oldest, [0, 8), goes
        capped_op47(g, 1, 16, &p1, CAPS); // repeat, which makes [32, 40) its oldest
        capped_op47(g, 1, 0, &p0, CAPS); // forgotten, so it uploads again and [32, 40) goes
        capped_op47(g, 2, 0, &q0, CAPS);
        capped_op47(g, 2, 16, &q1, CAPS); // four records, 48 bytes: both global caps full
        capped_op47(g, 3, 0, &r0, CAPS); // a fifth record: the table's oldest (buffer 1) goes
        capped_op47(g, 3, 16, &r1, CAPS); // 16 more bytes: two more records go
        capped_op47(g, 1, 16, &p1, CAPS); // forgotten by then, so it uploads again
        capped_op47(g, 2, 16, &q1, CAPS); // repeat
        capped_op47(g, 3, 0, &r0, CAPS); // repeat
        capped_op47(g, 3, 16, &r1, CAPS); // repeat
        capped_op47(g, 1, 16, &p1, CAPS); // repeat
        capped_op47(g, 2, 32, &wide, CAPS); // oversized but disjoint: q1 stays trusted
        capped_op47(g, 2, 16, &q1, CAPS); // exact repeat still skips
        (1..=3).map(|id| contents(g, id)).collect()
    });
    assert_eq!(outcome.bytes, vec![
        image(64, &[(0, &p0[..]), (16, &p1[..]), (32, &p2[..])]),
        image(64, &[(0, &q0[..]), (16, &q1[..]), (32, &wide[..])]),
        image(64, &[(0, &r0[..]), (16, &r1[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 15, hits: 6, misses: 9, bypasses: 1, saved_calls: 6, saved_bytes: 72,
        evictions: 5, purged: 0, records: 4, record_bytes: 48, ..Default::default() });
}
