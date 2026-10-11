//! Actual-GPU fixtures for the two ways the GPU thread writes into guest
//! memory after the fact: texture/buffer readback and query result completion.
//!
//! Every destination sits inside a guarded region. A fixture compares the whole
//! guest memory image before and after, so one byte written anywhere it was not
//! permitted fails the test. These drive the production `readback`, `flush`,
//! `process_maps`, `destroy` and query entry points on a real Metal device.
//! Run serially: `--test-threads=1`.
use super::*;

const PAGES: u32 = 4;
const COMMAND: u64 = 0x1_0000;
const FLAG: u64 = 0x1_0100;
const SOURCE: u64 = 0x1_1000;
/// Guarded region; destinations start well inside it.
const REGION: u64 = 0x2_0000;
const REGION_LEN: u64 = 0x4000;
const DEST: u64 = REGION + 0x1000;
const RECORD_A: u64 = REGION + 0x1000;
const RECORD_B: u64 = REGION + 0x2000;
const GUARD: u8 = 0xa5;
const RGBA8: u32 = 28;
const BC1: u32 = 71;

fn gpu_without(withheld: Features) -> Gpu {
    let mut config = wasmtime::Config::new(); config.wasm_threads(true); config.shared_memory(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let mem = SharedMemory::new(&engine, wasmtime::MemoryType::shared(PAGES, PAGES)).unwrap();
    TEST_WITHHELD_FEATURES.with(|features| features.set(withheld));
    let gpu = Gpu::new(mem, 0, 65536, 0, None);
    TEST_WITHHELD_FEATURES.with(|features| features.set(Features::empty()));
    let gpu = gpu.unwrap();
    assert!(crate::write_bytes(&gpu.mem, REGION, &vec![GUARD; REGION_LEN as usize]));
    gpu
}
fn gpu() -> Gpu { gpu_without(Features::empty()) }

fn memory(g: &Gpu) -> Vec<u8> { crate::read_bytes(&g.mem, 0, g.mem.data_size() as u64) }
fn poke(g: &Gpu, address: u64, bytes: &[u8]) { assert!(crate::write_bytes(&g.mem, address, bytes)); }
/// Distinct, position-dependent source bytes.
fn pattern(count: usize, salt: u8) -> Vec<u8> { (0..count).map(|i| (i as u8).wrapping_mul(7).wrapping_add(salt) | 1).collect() }

/// Asserts that guest memory changed only as `permitted` describes: a list of
/// (address, bytes that must now be there). Everything else must be untouched.
#[track_caller]
fn assert_only(g: &Gpu, before: &[u8], permitted: &[(u64, Vec<u8>)]) {
    assert_eq!(g.errors.load(Ordering::Relaxed), 0, "the GPU reported a validation error");
    let after = memory(g);
    let mut expected = before.to_vec();
    for (address, bytes) in permitted { expected[*address as usize..*address as usize + bytes.len()].copy_from_slice(bytes); }
    let wrong: Vec<usize> = (0..after.len()).filter(|&at| after[at] != expected[at]).collect();
    if let (Some(&at), Some(&last)) = (wrong.first(), wrong.last()) {
        let end = (at + 32).min(after.len());
        panic!("guest memory differs from the permitted image in {} byte(s), 0x{at:x} through 0x{last:x}: found {:02x?}, permitted {:02x?}",
            wrong.len(), &after[at..end], &expected[at..end]);
    }
}

#[derive(Clone, Copy, Default)]
struct Read { src: u32, sub: u32, region: Option<[u32; 6]>, row_pitch: u32, slice_pitch: u32, is_buf: bool, size: u32 }

/// Issues command 42 exactly as the guest's GpuReadback lays it out, with the
/// completion flag armed, and returns the guest memory image before the read.
fn readback(g: &mut Gpu, read: Read) -> Vec<u8> {
    let [l, t, f, r, b, bk] = read.region.unwrap_or_default();
    let words = [read.src, read.sub, read.region.is_some() as u32, l, t, f, r, b, bk, DEST as u32, (DEST >> 32) as u32,
        read.row_pitch, read.slice_pitch, read.is_buf as u32, read.size, FLAG as u32, (FLAG >> 32) as u32];
    poke(g, COMMAND, &words.iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<u8>>());
    poke(g, FLAG, &1u32.to_le_bytes());
    let before = memory(g);
    g.readback(COMMAND / 4);
    before
}
/// Waits for every queued map and applies it through the production path.
fn drain(g: &mut Gpu) {
    for _ in 0..200 { if g.pending_maps.is_empty() { return; } g.process_maps(true); }
    panic!("{} map(s) never completed", g.pending_maps.len());
}
/// Completes a readback and checks the flag, the destination and everything around it.
#[track_caller]
fn finish(g: &mut Gpu, before: &[u8], mut permitted: Vec<(u64, Vec<u8>)>) {
    drain(g);
    permitted.push((FLAG, 0u32.to_le_bytes().to_vec()));
    assert_only(g, before, &permitted);
}

fn rgba_texture(g: &mut Gpu, id: u32, dim: u32, w: u32, h: u32, d: u32, mips: u32, array: u32) {
    g.create_texture(id, dim, w, h, d, mips, array, RGBA8, BIND_SRV, 1);
    assert!(g.tex(id).is_some_and(|texture| texture.format == TextureFormat::Rgba8Unorm && texture.bc_source.is_none()));
}
/// Uploads `data` (tightly packed RGBA rows) through the production upload path.
fn upload(g: &mut Gpu, id: u32, mip: u32, slice: u32, w: u32, h: u32, d: u32, data: &[u8]) {
    poke(g, SOURCE, data);
    g.upload_texture(id, mip, slice, 0, 0, 0, w, h, d, w * 4, h, data.len() as u64, SOURCE);
}
/// Destination image for rows of `row_bytes` laid out at the guest's pitches.
fn laid_out(data: &[u8], row_bytes: usize, rows: usize, slices: usize, row_pitch: u64, slice_pitch: u64) -> Vec<(u64, Vec<u8>)> {
    (0..slices * rows).map(|index| {
        let (z, row) = ((index / rows) as u64, (index % rows) as u64);
        (DEST + z * slice_pitch + row * row_pitch, data[index * row_bytes..(index + 1) * row_bytes].to_vec())
    }).collect()
}

#[test]
fn matching_rgba_layout_reads_back_exactly() {
    let mut g = gpu();
    rgba_texture(&mut g, 1, 2, 8, 4, 1, 1, 1);
    let data = pattern(128, 3);
    upload(&mut g, 1, 0, 0, 8, 4, 1, &data);
    let before = readback(&mut g, Read { src: 1, row_pitch: 32, size: 128, ..Default::default() });
    assert_eq!(g.pending_maps.len(), 1, "a matching readback is queued, not completed early");
    assert_eq!(crate::read_u32(&g.mem, FLAG), Some(1), "the flag stays armed until the copy is done");
    finish(&mut g, &before, vec![(DEST, data)]);
}

#[test]
fn guest_row_padding_is_left_untouched() {
    let mut g = gpu();
    rgba_texture(&mut g, 1, 2, 8, 4, 1, 1, 1);
    let data = pattern(128, 5);
    upload(&mut g, 1, 0, 0, 8, 4, 1, &data);
    // 32 bytes of texels per row at a 48-byte pitch: 16 bytes of padding per row.
    let before = readback(&mut g, Read { src: 1, row_pitch: 48, size: 192, ..Default::default() });
    finish(&mut g, &before, laid_out(&data, 32, 4, 1, 48, 0));
}

#[test]
fn boxed_texture_region_reads_only_that_region() {
    let mut g = gpu();
    rgba_texture(&mut g, 1, 2, 8, 4, 1, 1, 1);
    let data = pattern(128, 9);
    upload(&mut g, 1, 0, 0, 8, 4, 1, &data);
    // Columns 2..6 of rows 1..3.
    let wanted: Vec<u8> = (1..3).flat_map(|row| data[row * 32 + 8..row * 32 + 24].to_vec()).collect();
    let before = readback(&mut g, Read { src: 1, region: Some([2, 1, 0, 6, 3, 1]), row_pitch: 16, size: 32, ..Default::default() });
    finish(&mut g, &before, vec![(DEST, wanted)]);
}

#[test]
fn mip_and_array_layer_subresource_reads_back_exactly() {
    let mut g = gpu();
    rgba_texture(&mut g, 1, 2, 8, 8, 1, 2, 2);
    // Fill every subresource so a wrong mip or layer shows up as wrong bytes.
    for (layer, mip, salt) in [(0u32, 0u32, 11u8), (0, 1, 21), (1, 0, 31), (1, 1, 41)] {
        let side = 8 >> mip;
        upload(&mut g, 1, mip, layer, side, side, 1, &pattern((side * side * 4) as usize, salt));
    }
    // Subresource index = mip + layer * mips: layer 1, mip 1 is 4x4.
    let before = readback(&mut g, Read { src: 1, sub: 3, row_pitch: 16, size: 64, ..Default::default() });
    finish(&mut g, &before, vec![(DEST, pattern(64, 41))]);
}

#[test]
fn volume_slices_keep_guest_row_and_slice_padding() {
    let mut g = gpu();
    rgba_texture(&mut g, 1, 3, 4, 4, 2, 1, 1);
    let data = pattern(128, 13);
    upload(&mut g, 1, 0, 0, 4, 4, 2, &data);
    // 16 texel bytes per row at pitch 24; four rows per slice at slice pitch 120.
    let before = readback(&mut g, Read { src: 1, row_pitch: 24, slice_pitch: 120, size: 240, ..Default::default() });
    finish(&mut g, &before, laid_out(&data, 16, 4, 2, 24, 120));
    // The second slice alone, through a box.
    poke(&g, REGION, &vec![GUARD; REGION_LEN as usize]);
    let before = readback(&mut g, Read { src: 1, region: Some([0, 0, 1, 4, 4, 2]), row_pitch: 16, slice_pitch: 64, size: 64, ..Default::default() });
    finish(&mut g, &before, vec![(DEST, data[64..].to_vec())]);
}

#[test]
fn native_bc_layout_reads_back_the_compressed_blocks_exactly() {
    let mut g = gpu();
    if !g.device.features().contains(Features::TEXTURE_COMPRESSION_BC) {
        eprintln!("SKIPPED: this adapter has no BC support, so there is no native BC layout to read back");
        return;
    }
    g.create_texture(1, 2, 8, 8, 1, 1, 1, BC1, BIND_SRV, 1);
    assert!(g.tex(1).is_some_and(|texture| texture.format == TextureFormat::Bc1RgbaUnorm && texture.bc_source.is_none()));
    // 8x8 BC1 is two rows of two 8-byte blocks; the guest lays it out the same way.
    let blocks = pattern(32, 17);
    poke(&g, SOURCE, &blocks);
    g.upload_texture(1, 0, 0, 0, 0, 0, 8, 8, 1, 16, 2, 32, SOURCE);
    let before = readback(&mut g, Read { src: 1, row_pitch: 16, size: 32, ..Default::default() });
    finish(&mut g, &before, vec![(DEST, blocks)]);
}

#[test]
fn texture_destination_smaller_than_the_host_layout_is_never_overrun() {
    let mut g = gpu();
    rgba_texture(&mut g, 1, 2, 8, 4, 1, 1, 1);
    let data = pattern(128, 19);
    upload(&mut g, 1, 0, 0, 8, 4, 1, &data);
    // The guest says its destination holds 80 bytes: two rows and half of the third.
    let before = readback(&mut g, Read { src: 1, row_pitch: 32, size: 80, ..Default::default() });
    finish(&mut g, &before, vec![(DEST, data[..80].to_vec())]);
    // A zero-byte destination receives nothing and still completes.
    poke(&g, REGION, &vec![GUARD; REGION_LEN as usize]);
    let before = readback(&mut g, Read { src: 1, row_pitch: 32, size: 0, ..Default::default() });
    finish(&mut g, &before, vec![]);
}

fn vertex_buffer(g: &mut Gpu, id: u32, data: &[u8]) {
    g.create_buffer(id, data.len() as u32, 1, 0, 0);
    poke(g, SOURCE, data);
    g.upload_buffer(id, 0, data.len() as u64, SOURCE, false);
}

#[test]
fn buffer_readback_is_exact_and_bounded_by_the_destination() {
    let mut g = gpu();
    let data = pattern(64, 23);
    vertex_buffer(&mut g, 1, &data);
    let whole = Read { src: 1, is_buf: true, size: 64, ..Default::default() };
    let before = readback(&mut g, whole);
    finish(&mut g, &before, vec![(DEST, data.clone())]);
    // A destination shorter than the buffer.
    poke(&g, REGION, &vec![GUARD; REGION_LEN as usize]);
    let before = readback(&mut g, Read { size: 40, ..whole });
    finish(&mut g, &before, vec![(DEST, data[..40].to_vec())]);
    // A boxed range that fits its destination.
    poke(&g, REGION, &vec![GUARD; REGION_LEN as usize]);
    let before = readback(&mut g, Read { region: Some([8, 0, 0, 40, 1, 1]), ..whole });
    finish(&mut g, &before, vec![(DEST, data[8..40].to_vec())]);
}

#[test]
fn boxed_buffer_range_larger_than_the_destination_is_never_overrun() {
    let mut g = gpu();
    let data = pattern(64, 29);
    vertex_buffer(&mut g, 1, &data);
    // The box asks for 64 bytes; the guest destination holds 16.
    let before = readback(&mut g, Read { src: 1, is_buf: true, region: Some([0, 0, 0, 64, 1, 1]), size: 16, ..Default::default() });
    finish(&mut g, &before, vec![(DEST, data[..16].to_vec())]);
}

#[test]
fn software_decoded_bc_readback_is_skipped_without_touching_the_destination() {
    // A device without BC support, as on the visionOS simulator: the host keeps
    // the texture as decoded RGBA while the guest destination is BC-shaped.
    let mut g = gpu_without(Features::TEXTURE_COMPRESSION_BC | Features::TEXTURE_COMPRESSION_BC_SLICED_3D);
    assert!(!g.device.features().contains(Features::TEXTURE_COMPRESSION_BC));
    g.create_texture(1, 2, 8, 8, 1, 1, 1, BC1, BIND_SRV, 1);
    let (format, source) = g.tex(1).map(|texture| (texture.format, texture.bc_source)).unwrap();
    assert_eq!((format, source), (TextureFormat::Rgba8Unorm, Some(TextureFormat::Bc1RgbaUnorm)), "texture must take the software-decoded path");
    poke(&g, SOURCE, &pattern(32, 37));
    g.upload_texture(1, 0, 0, 0, 0, 0, 8, 8, 1, 16, 2, 32, SOURCE);
    // Guest layout for 8x8 BC1: 2 block rows of 16 bytes, 32 bytes in all.
    let before = readback(&mut g, Read { src: 1, row_pitch: 16, size: 32, ..Default::default() });
    // The skip is a safety measure: it completes the request and writes
    // nothing. It does not produce BC data for the guest.
    let (queued, flag) = (g.pending_maps.len(), crate::read_u32(&g.mem, FLAG));
    finish(&mut g, &before, vec![]);
    assert_eq!(queued, 0, "a skipped readback queues no copy");
    assert_eq!(flag, Some(0), "a skipped readback completes immediately");
    // The same device still reads an uncompressed texture back exactly.
    rgba_texture(&mut g, 2, 2, 8, 4, 1, 1, 1);
    let data = pattern(128, 43);
    upload(&mut g, 2, 0, 0, 8, 4, 1, &data);
    let before = readback(&mut g, Read { src: 2, row_pitch: 32, size: 128, ..Default::default() });
    finish(&mut g, &before, vec![(DEST, data)]);
}

// ---- Query result completion ----

const OCCLUSION: u32 = 1;
const EVENT: u32 = 0;
const TIMESTAMP: u32 = 2;

/// Begins and ends an occlusion query that owns `slots`. Slots are assigned
/// directly: these fixtures do not build a guest-shader draw pass. Resolve, map
/// and completion below are the production path on real GPU buffers.
fn occlusion(g: &mut Gpu, id: u32, seq: u32, slots: &[u32]) {
    g.begin_query(id);
    if let Some(Obj::Query(query)) = g.obj_mut(id) { query.slots.extend_from_slice(slots); }
    g.end_query(id, seq);
}
/// The record after a completion: seq, four untouched bytes, 64-bit result.
fn completed(seq: u32, result: u64) -> Vec<(u64, Vec<u8>)> {
    vec![(0, seq.to_le_bytes().to_vec()), (8, result.to_le_bytes().to_vec())]
}
fn at(record: u64, parts: Vec<(u64, Vec<u8>)>) -> Vec<(u64, Vec<u8>)> {
    parts.into_iter().map(|(offset, bytes)| (record + offset, bytes)).collect()
}
/// Runs submitted-work callbacks until none are outstanding.
fn drain_callbacks(g: &mut Gpu) {
    for _ in 0..200 {
        if g.inflight.load(Ordering::SeqCst) == 0 { return; }
        let _ = g.device.poll(PollType::Wait { submission_index: None, timeout: None });
    }
    panic!("submitted-work callback never ran");
}

#[test]
fn live_occlusion_queries_complete_into_their_own_records() {
    let mut g = gpu();
    g.create_query(1, OCCLUSION, RECORD_A);
    g.create_query(2, OCCLUSION, RECORD_B);
    let before = memory(&g);
    // Consecutive slots, and a query that ended without drawing anything.
    occlusion(&mut g, 1, 7, &[0, 1]);
    occlusion(&mut g, 2, 9, &[]);
    g.flush();
    assert_eq!(g.pending_maps.len(), 1);
    drain(&mut g);
    // No draw wrote these slots, so they resolve to zero visible samples.
    let mut permitted = at(RECORD_A, completed(7, 0));
    permitted.extend(at(RECORD_B, completed(9, 0)));
    assert_only(&g, &before, &permitted);
    // Non-consecutive slots take the other resolve layout.
    let before = memory(&g);
    occlusion(&mut g, 1, 8, &[3, 7]);
    g.flush();
    drain(&mut g);
    assert_only(&g, &before, &at(RECORD_A, completed(8, 0)));
}

#[test]
fn query_destroyed_after_submission_is_not_completed() {
    let mut g = gpu();
    g.create_query(1, OCCLUSION, RECORD_A);
    let before = memory(&g);
    occlusion(&mut g, 1, 7, &[0, 1]);
    g.flush();
    assert_eq!(g.pending_maps.len(), 1, "the completion is queued before the destroy");
    g.destroy(1);
    drain(&mut g);
    assert!(g.pending_maps.is_empty(), "the stale map is still unmapped and released");
    assert_only(&g, &before, &[]);
}

#[test]
fn query_destroyed_before_submission_is_not_completed() {
    let mut g = gpu();
    g.create_query(1, OCCLUSION, RECORD_A);
    g.create_query(2, OCCLUSION, RECORD_B);
    let before = memory(&g);
    occlusion(&mut g, 1, 7, &[0, 1]);
    occlusion(&mut g, 2, 9, &[]);
    g.destroy(1);
    g.destroy(2);
    g.flush();
    drain(&mut g);
    assert_only(&g, &before, &[]);
}

#[test]
fn stale_result_is_not_redirected_to_a_query_reusing_the_id_before_submission() {
    let mut g = gpu();
    g.create_query(1, OCCLUSION, RECORD_A);
    let before = memory(&g);
    occlusion(&mut g, 1, 5, &[0, 1]);
    g.destroy(1);
    g.create_query(1, OCCLUSION, RECORD_B);
    g.flush();
    drain(&mut g);
    assert_only(&g, &before, &[]);
    // The new query still completes normally, into its own record only.
    occlusion(&mut g, 1, 6, &[2]);
    g.flush();
    drain(&mut g);
    assert_only(&g, &before, &at(RECORD_B, completed(6, 0)));
}

#[test]
fn stale_result_reaches_neither_record_when_the_id_is_reused_after_submission() {
    let mut g = gpu();
    g.create_query(1, OCCLUSION, RECORD_A);
    let before = memory(&g);
    occlusion(&mut g, 1, 5, &[0, 1]);
    g.flush();
    assert_eq!(g.pending_maps.len(), 1);
    g.destroy(1);
    g.create_query(1, OCCLUSION, RECORD_B);
    drain(&mut g);
    assert_only(&g, &before, &[]);
    occlusion(&mut g, 1, 6, &[2]);
    g.flush();
    drain(&mut g);
    assert_only(&g, &before, &at(RECORD_B, completed(6, 0)));
}

#[test]
fn replacing_a_query_id_without_destroy_ends_the_old_lifetime() {
    let mut g = gpu();
    g.create_query(1, OCCLUSION, RECORD_A);
    let before = memory(&g);
    // Replaced between end and submission.
    occlusion(&mut g, 1, 5, &[0, 1]);
    g.create_query(1, OCCLUSION, RECORD_B);
    g.flush();
    drain(&mut g);
    assert_only(&g, &before, &[]);
    // Replaced after submission, before the map is consumed.
    occlusion(&mut g, 1, 6, &[2, 3]);
    g.flush();
    assert_eq!(g.pending_maps.len(), 1);
    g.create_query(1, OCCLUSION, RECORD_A);
    drain(&mut g);
    assert_only(&g, &before, &[]);
    occlusion(&mut g, 1, 7, &[4]);
    g.flush();
    drain(&mut g);
    assert_only(&g, &before, &at(RECORD_A, completed(7, 0)));
}

#[test]
fn destroying_or_replacing_the_active_query_clears_active_state() {
    let mut g = gpu();
    g.create_query(1, OCCLUSION, RECORD_A);
    g.begin_query(1);
    assert_eq!(g.occlusion_active, Some(1));
    g.destroy(1);
    assert_eq!((g.occlusion_active, g.occlusion_pass_slot), (None, None), "a destroyed query cannot stay active");
    // A new query with the same id does not inherit the old one's open state.
    g.create_query(1, OCCLUSION, RECORD_B);
    g.begin_occlusion_in_pass();
    assert_eq!(g.occlusion_active, None);
    assert!(matches!(g.obj(1), Some(Obj::Query(query)) if query.slots.is_empty()));
    g.begin_query(1);
    assert_eq!(g.occlusion_active, Some(1));
    g.create_query(1, OCCLUSION, RECORD_A);
    assert_eq!(g.occlusion_active, None, "replacing the active query's id ends it");
    // No render pass is opened here; slot allocation inside a real pass is not exercised.
}

#[test]
fn live_event_and_timestamp_queries_complete_from_the_submitted_work_callback() {
    let mut g = gpu();
    g.create_query(1, EVENT, RECORD_A);
    g.create_query(2, TIMESTAMP, RECORD_B);
    let before = memory(&g);
    g.end_query(1, 11);
    g.end_query(2, 12);
    g.encoder();
    g.flush();
    assert_eq!(g.inflight.load(Ordering::SeqCst), 1, "the callback is outstanding until the device is polled");
    assert_only(&g, &before, &[]);
    drain_callbacks(&mut g);
    let stamp = crate::read_u64(&g.mem, RECORD_B + 8).unwrap();
    assert!(stamp != 0 && stamp != u64::from_le_bytes([GUARD; 8]), "timestamp result was written");
    let mut permitted = at(RECORD_A, completed(11, 1));
    permitted.extend(at(RECORD_B, completed(12, stamp)));
    assert_only(&g, &before, &permitted);
}

#[test]
fn event_query_destroyed_or_reused_is_not_completed_by_a_late_callback() {
    let mut g = gpu();
    g.create_query(1, EVENT, RECORD_A);
    let before = memory(&g);
    // Destroyed after submission, while the callback is still outstanding.
    g.end_query(1, 11);
    g.encoder();
    g.flush();
    assert_eq!(g.inflight.load(Ordering::SeqCst), 1);
    g.destroy(1);
    g.create_query(1, EVENT, RECORD_B);
    drain_callbacks(&mut g);
    assert_only(&g, &before, &[]);
    // Destroyed and its id reused between end and submission.
    g.end_query(1, 12);
    g.destroy(1);
    g.create_query(1, EVENT, RECORD_A);
    g.encoder();
    g.flush();
    drain_callbacks(&mut g);
    assert_eq!(g.inflight.load(Ordering::SeqCst), 0, "a cancelled completion still settles the in-flight count");
    assert_only(&g, &before, &[]);
    // The surviving query completes normally.
    g.end_query(1, 13);
    g.encoder();
    g.flush();
    drain_callbacks(&mut g);
    assert_only(&g, &before, &at(RECORD_A, completed(13, 1)));
}
