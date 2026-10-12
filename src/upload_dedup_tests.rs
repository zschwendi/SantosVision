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
use crate::upload_dedup::{Totals, UploadDedup, MAX_RECORDS, MAX_RECORD_BYTES, MAX_TOTAL_BYTES};

const PAGES: u32 = 4;
const SOURCE: u64 = 0x1_1000;
const VERTEX: u32 = 1;
const INDEX: u32 = 2;
const DEFAULT_CAPS: (usize, usize, usize) = (MAX_RECORDS, MAX_RECORD_BYTES, MAX_TOTAL_BYTES);

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
fn off_then_on(caps: (usize, usize, usize), scenario: impl Fn(&mut Gpu) -> Vec<Vec<u8>>) -> Outcome {
    let run = |enabled: bool| {
        let mut g = gpu(UploadDedup::new(enabled, caps.0, caps.1, caps.2));
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
    Outcome { bytes, totals }
}

#[test]
fn metal_exact_repeats_are_skipped_and_changes_are_uploaded() {
    let (a, b) = (pattern(16, 3), pattern(16, 5));
    let outcome = off_then_on(DEFAULT_CAPS, |g| {
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
        op47(g, 1, 16, &a); // same bytes, another range
        op47(g, 1, 16, &a[..8]); // same offset, shorter
        op47(g, 1, 16, &a[..8]); // repeat
        op47(g, 2, 16, &b); // repeat
        vec![first, contents(g, 1), contents(g, 2)]
    });
    assert_eq!(outcome.bytes, vec![
        image(64, &[(0, &a[..])]),
        image(64, &[(0, &a[..]), (16, &a[..])]),
        image(64, &[(16, &b[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 12, hits: 6, misses: 6, saved_calls: 6, saved_bytes: 88,
        records: 2, record_bytes: 24, ..Default::default() });
}

#[test]
fn metal_op12_between_repeats_forces_the_next_op47() {
    let (a, z) = (pattern(16, 7), pattern(32, 9));
    let outcome = off_then_on(DEFAULT_CAPS, |g| {
        g.create_buffer(1, 32, VERTEX, 0, 0);
        op47(g, 1, 8, &a);
        op12(g, 1, 0, &z); // overwrites the recorded range
        op47(g, 1, 8, &a); // same call as before, but it has to upload
        let mixed = contents(g, 1);
        op47(g, 1, 8, &a); // repeat
        op12(g, 1, 8, &a); // identical bytes through op12 still drop the record
        op47(g, 1, 8, &a);
        op12(g, 1, 0, &[]); // and so does an empty op12
        op47(g, 1, 8, &a);
        vec![mixed, contents(g, 1)]
    });
    let wanted = image(32, &[(0, &z[..]), (8, &a[..])]);
    assert_eq!(outcome.bytes, vec![wanted.clone(), wanted]);
    assert_eq!(outcome.totals, Totals { checked: 5, hits: 1, misses: 4, saved_calls: 1, saved_bytes: 16,
        records: 1, record_bytes: 16, ..Default::default() });
}

#[test]
fn metal_padded_tail_op47_keeps_the_legacy_route_and_drops_the_record() {
    let (a, tail) = (pattern(16, 61), pattern(6, 63));
    let outcome = off_then_on(DEFAULT_CAPS, |g| {
        g.create_buffer(1, 32, VERTEX, 0, 0);
        op47(g, 1, 0, &a);
        op47(g, 1, 0, &a); // repeat
        // Six bytes: a 4-byte body and a zero-padded tail word, both inside
        // the recorded range. Never compared, always uploaded.
        op47(g, 1, 4, &tail);
        let padded = contents(g, 1);
        op47(g, 1, 0, &a); // the recorded bytes are gone, so this uploads
        vec![padded, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![
        image(32, &[(0, &a[..]), (4, &tail[..4]), (8, &[tail[4], tail[5], 0, 0][..])]),
        image(32, &[(0, &a[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 3, hits: 1, misses: 2, bypasses: 1, saved_calls: 1, saved_bytes: 16,
        records: 1, record_bytes: 16, ..Default::default() });
}

#[test]
fn metal_destroy_recreate_and_replacement_start_without_a_record() {
    let a = pattern(16, 11);
    let outcome = off_then_on(DEFAULT_CAPS, |g| {
        g.create_buffer(1, 32, VERTEX, 0, 0);
        op47(g, 1, 0, &a);
        op47(g, 1, 0, &a); // repeat
        let original = contents(g, 1);
        // The same guest id, a new zeroed GPU buffer: nothing is known about it.
        g.destroy(1);
        g.create_buffer(1, 32, VERTEX, 0, 0);
        op47(g, 1, 0, &a);
        let recreated = contents(g, 1);
        // Replaced without a destroy.
        g.create_buffer(1, 32, VERTEX, 0, 0);
        op47(g, 1, 0, &a);
        let replaced = contents(g, 1);
        op47(g, 1, 0, &a); // repeat on the newest object
        vec![original, recreated, replaced, contents(g, 1)]
    });
    let wanted = image(32, &[(0, &a[..])]);
    assert_eq!(outcome.bytes, vec![wanted.clone(), wanted.clone(), wanted.clone(), wanted]);
    assert_eq!(outcome.totals, Totals { checked: 5, hits: 2, misses: 3, saved_calls: 2, saved_bytes: 32,
        records: 1, record_bytes: 16, ..Default::default() });
}

#[test]
fn metal_copy_clear_and_counter_destinations_are_disqualified_for_life() {
    let (a, source) = (pattern(16, 13), pattern(32, 17));
    let words = [0x0101_0101u32, 0x0202_0202, 0x0303_0303, 0x0404_0404];
    let outcome = off_then_on(DEFAULT_CAPS, |g| {
        // 1: copy_region destination. 2: zero clear. 3: value clear. 4: counter destination.
        for id in 1..=4 {
            g.create_buffer(id, 32, VERTEX, 0, 0);
            op47(g, id, 0, &a);
            op47(g, id, 0, &a); // repeat
        }
        g.create_buffer(5, 32, VERTEX, 0, 0);
        op12(g, 5, 0, &source);
        g.create_buffer(6, 16, BIND_UAV, 0, 4);
        buffer_uav(g, 7, 6);
        buffer_uav(g, 8, 2);
        buffer_uav(g, 9, 3);
        g.copy_region(1, 0, 0, 0, 0, 5, 0, false, 0, 0, 0, 0, 0, 0);
        g.clear_uav(8, [0; 4]);
        g.clear_uav(9, words);
        g.set_uav_counter(7, 0x1122_3344);
        g.copy_counter(4, 8, 7);
        let mut bytes: Vec<Vec<u8>> = (1..=4).map(|id| contents(g, id)).collect();
        // The same op47 as before. Each GPU write replaced those bytes, so
        // every one of these has to upload, now and for the object's lifetime.
        for id in 1..=4 {
            op47(g, id, 0, &a);
            op47(g, id, 0, &a);
        }
        bytes.extend((1..=4).map(|id| contents(g, id)));
        bytes
    });
    let cleared: Vec<u8> = (0..8).flat_map(|word| words[word & 3].to_le_bytes()).collect();
    assert_eq!(outcome.bytes, vec![
        // After the GPU writes.
        source.clone(),
        vec![0u8; 32],
        cleared.clone(),
        image(32, &[(0, &a[..]), (8, &[0x44u8, 0x33, 0x22, 0x11][..])]),
        // After the same op47 was issued again.
        image(32, &[(0, &source[..]), (0, &a[..])]),
        image(32, &[(0, &a[..])]),
        image(32, &[(0, &cleared[..]), (0, &a[..])]),
        image(32, &[(0, &a[..])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 8, hits: 4, misses: 4, bypasses: 8, saved_calls: 4, saved_bytes: 64,
        disqualified: 4, ..Default::default() });
}

#[test]
fn metal_copy_recorded_before_an_op47_of_its_submission_is_never_trusted() {
    let (x, y) = (pattern(16, 19), pattern(32, 23));
    let outcome = off_then_on(DEFAULT_CAPS, |g| {
        g.create_buffer(1, 32, VERTEX, 0, 0);
        g.create_buffer(2, 32, VERTEX, 0, 0);
        op12(g, 2, 0, &y);
        // Recorded first, executed last: uploads run in the prelude, so this
        // copy lands AFTER the op47 below although it was issued before it.
        g.copy_region(1, 0, 0, 0, 0, 2, 0, false, 0, 0, 0, 0, 0, 0);
        op47(g, 1, 0, &x);
        let after_copy = contents(g, 1);
        // A guard that had recorded that op47 would skip this one and leave
        // the copy's bytes in place.
        op47(g, 1, 0, &x);
        vec![after_copy, contents(g, 1)]
    });
    assert_eq!(outcome.bytes, vec![y.clone(), image(32, &[(0, &y[..]), (0, &x[..])])]);
    assert_eq!(outcome.totals, Totals { bypasses: 2, disqualified: 1, ..Default::default() });
}

#[test]
fn metal_consumers_recorded_before_a_repeat_or_a_change_read_legacy_bytes() {
    let (x, w) = (pattern(16, 29), pattern(16, 31));
    let outcome = off_then_on(DEFAULT_CAPS, |g| {
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
        disqualified: 2, records: 1, record_bytes: 16, ..Default::default() });
}

/// An op47 after which the guard's table must still be inside its caps.
fn capped_op47(g: &mut Gpu, id: u32, offset: u64, data: &[u8], caps: (usize, usize, usize)) {
    op47(g, id, offset, data);
    let totals = g.upload_dedup.totals();
    assert!(totals.records <= caps.0 && totals.record_bytes <= caps.2, "caps exceeded: {totals:?}");
}

#[test]
fn metal_aggregate_cap_evicts_oldest_records_and_stays_exact() {
    // Three records, 16 bytes each at most, 40 bytes in all.
    const CAPS: (usize, usize, usize) = (3, 16, 40);
    let data: Vec<Vec<u8>> = (0..4u8).map(|index| pattern(16, 41 + 2 * index)).collect();
    let wide = pattern(24, 51);
    let outcome = off_then_on(CAPS, |g| {
        for id in 1..=4 { g.create_buffer(id, 32, VERTEX, 0, 0); }
        capped_op47(g, 1, 0, &data[0], CAPS);
        capped_op47(g, 2, 0, &data[1], CAPS);
        capped_op47(g, 3, 0, &data[2], CAPS); // 48 bytes would not fit: buffer 1's record goes
        capped_op47(g, 1, 0, &data[0], CAPS); // so this uploads again, evicting buffer 2's
        capped_op47(g, 3, 0, &data[2], CAPS); // repeat
        capped_op47(g, 4, 0, &data[3][..8], CAPS); // 40 bytes, three records: both caps full
        capped_op47(g, 2, 0, &data[1], CAPS); // a fourth record: the oldest (buffer 1) goes
        capped_op47(g, 1, 8, &wide, CAPS); // over the per-record cap: legacy route
        capped_op47(g, 3, 0, &data[2], CAPS); // repeat
        capped_op47(g, 4, 0, &data[3][..8], CAPS); // repeat
        (1..=4).map(|id| contents(g, id)).collect()
    });
    assert_eq!(outcome.bytes, vec![
        image(32, &[(0, &data[0][..]), (8, &wide[..])]),
        image(32, &[(0, &data[1][..])]),
        image(32, &[(0, &data[2][..])]),
        image(32, &[(0, &data[3][..8])]),
    ]);
    assert_eq!(outcome.totals, Totals { checked: 9, hits: 3, misses: 6, bypasses: 1, saved_calls: 3, saved_bytes: 40,
        evictions: 3, records: 3, record_bytes: 40, ..Default::default() });
}
