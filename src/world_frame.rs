//! Bounded immutable Metal snapshots crossing the game/compositor queue boundary.
//! Only a qualified GPU-ring world view with matching real depth may publish.
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use wgpu::*;

const MAX_SLOTS: usize = 6;
const MAX_LEASES: usize = 12;
const MAX_READY_AGE: Duration = Duration::from_secs(1);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WorldFrame {
    pub abi_version: u32,
    pub byte_size: u32,
    pub lease: u64,
    pub game_frame: u64,
    pub request_frame: u64,
    pub head_frame: u64,
    pub presentation_time: f64,
    pub color_texture: u64,
    pub depth_texture: u64,
    pub view_index: u32,
    pub view_count: u32,
    pub width: u32,
    pub height: u32,
    pub depth_convention: u32,
    pub reserved: u32,
    pub projection_from_eye: [f32; 16],
    pub game_from_eye: [f32; 16],
    pub game_from_stage: [f32; 16],
    pub depth_near: f32,
    pub depth_far: f32,
}

/// Additive ABI: a pair is published by one certified render transaction. The
/// existing per-eye frame ABI and its reserved field remain unchanged.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WorldPair {
    pub abi_version: u32,
    pub byte_size: u32,
    pub scene_epoch: u64,
    pub eyes: [WorldFrame; 2],
}

const PAIR_ABI_VERSION: u32 = 1;
const _: () = assert!(std::mem::size_of::<WorldPair>() == 592);

struct Slot { color: Texture, depth: Texture }
/// Created only for the fresh, pre-UI-clear copy in Gpu::save_world_depth_before_ui.
/// The game never binds this image as a writable target after that copy.
pub(crate) struct FrozenPreUiDepth(Texture);
impl FrozenPreUiDepth {
    pub(crate) fn new_snapshot(texture: Texture) -> Self { Self(texture) }
    pub(crate) fn texture(&self) -> &Texture { &self.0 }
}
#[derive(Clone)]
struct Ready { slot: Arc<Slot>, frame: WorldFrame, generation: u64, provenance_epoch: u64,
    pair_epoch: u64, ready_at: Instant }
impl Ready {
    #[cfg(test)]
    fn depth_texture(&self) -> &Texture { &self.slot.depth }
}
#[derive(Clone)]
struct ReadyPair { eyes: [Ready; 2], scene_epoch: u64, ready_at: Instant }
#[derive(Default)]
struct Frames {
    pool: Vec<Arc<Slot>>,
    latest: [Option<Ready>; 2],
    latest_pair: Option<ReadyPair>,
    leases: HashMap<u64, Ready>,
    next_lease: u64,
    published: u64,
    dropped: u64,
    acquired: u64,
    pairs_published: u64,
    pairs_acquired: u64,
    frozen_depth_handoffs: u64,
    copied_depth_snapshots: u64,
    error: Option<String>,
    generation: u64,
    compositor_frames: u64,
    compositor_views: u32,
    compositor_pair_epoch: Option<u64>,
    compositor_completed_at: Option<Instant>,
}
static FRAMES: LazyLock<Mutex<Frames>> = LazyLock::new(|| Mutex::new(Frames::default()));

pub(crate) fn reset() {
    if let Ok(mut frames) = FRAMES.lock() {
        frames.latest = [None, None];
        frames.latest_pair = None;
        frames.generation = frames.generation.wrapping_add(1);
        frames.compositor_views = 0;
        frames.compositor_pair_epoch = None;
        frames.compositor_completed_at = None;
        // Outstanding compositor leases retain their immutable textures until release.
    }
}

pub(crate) fn status_json() -> serde_json::Value {
    let Ok(frames) = FRAMES.lock() else { return json!({"error": "world frame lock poisoned"}); };
    json!({"abi_version": 1, "pair_abi_version": PAIR_ABI_VERSION,
        "paired_render_requested": paired_render_requested(),
        "storage": "immutable GPU-complete Metal color and real depth",
        "published": frames.published, "dropped": frames.dropped, "acquired": frames.acquired,
        "pairs_published": frames.pairs_published, "pairs_acquired": frames.pairs_acquired,
        "frozen_depth_handoffs": frames.frozen_depth_handoffs,
        "copied_depth_snapshots": frames.copied_depth_snapshots,
        "latest_pair_scene_epoch": frames.latest_pair.as_ref().map(|pair| pair.scene_epoch),
        "outstanding_leases": frames.leases.len(), "slots": frames.pool.len(), "error": frames.error,
        "compositor_completed_frames": frames.compositor_frames, "compositor_completed_views": frames.compositor_views,
        "compositor_completed_pair_epoch": frames.compositor_pair_epoch,
        "compositor_completion_age_seconds": frames.compositor_completed_at.map(|time| time.elapsed().as_secs_f64()),
        "latest_views": frames.latest.iter().map(|r| r.as_ref().map(|r| json!({
            "view_index": r.frame.view_index, "game_frame": r.frame.game_frame,
            "request_frame": r.frame.request_frame, "head_frame": r.frame.head_frame,
            "presentation_time": r.frame.presentation_time, "width": r.frame.width, "height": r.frame.height,
            "age_seconds": r.ready_at.elapsed().as_secs_f64()
        }))).collect::<Vec<_>>()})
}

/// This is a capability request, not evidence that a guest produced a pair.
/// No legacy rendering path changes when it is off.
pub(crate) fn paired_render_requested() -> bool {
    std::env::var("MUGUET_XR_PAIRED_RENDER").is_ok_and(|value| value == "1")
}

fn frozen_depth_transfer_requested() -> bool {
    crate::native::frozen_depth_transfer_requested()
}

fn fresh_completed_views(frames: &Frames) -> bool {
    frames.compositor_views > 0 && frames.compositor_completed_at.is_some_and(|time| time.elapsed() <= MAX_READY_AGE)
        && frames.latest[..frames.compositor_views as usize].iter().all(|ready|
        ready.as_ref().is_some_and(|ready| ready.ready_at.elapsed() <= MAX_READY_AGE
            && crate::xr_view::view_epoch_is_active(ready.provenance_epoch)))
}
pub(crate) fn renderer_connected() -> bool { FRAMES.lock().is_ok_and(|f| fresh_completed_views(&f)) }
pub(crate) fn stereo_connected() -> bool { FRAMES.lock().is_ok_and(|f| {
    if f.compositor_views != 2 { return false; }
    if !paired_render_requested() { return fresh_completed_views(&f); }
    f.compositor_completed_at.is_some_and(|at| at.elapsed() <= MAX_READY_AGE)
        && f.latest_pair.as_ref().is_some_and(|pair| f.compositor_pair_epoch == Some(pair.scene_epoch)
            && pair.ready_at.elapsed() <= MAX_READY_AGE
            && pair.eyes.iter().all(|eye| eye.generation == f.generation
                && crate::xr_view::view_epoch_is_active(eye.provenance_epoch)))
}) }
pub(crate) fn reject(reason: String) {
    if let Ok(mut frames) = FRAMES.lock() { frames.error = Some(reason); frames.dropped += 1; }
}

/// Lighting may use forward Z while the opaque pass writes reversed Z. Only
/// the depth row may differ: color and depth must still describe the same eye
/// and the same screen rays. Unprojection uses the actual depth writer below.
pub(crate) fn shares_screen_rays(color: &crate::xr_view::WorldView, depth: &crate::xr_view::WorldView) -> bool {
    let close = |a: f32, b: f32| a.is_finite() && b.is_finite() && (a - b).abs() <= 0.00002;
    color.provenance_epoch == depth.provenance_epoch
        && color.camera_sequence == depth.camera_sequence
        && color.request_frame == depth.request_frame && color.head_pose_frame == depth.head_pose_frame
        && color.eye_index == depth.eye_index && color.view_count == depth.view_count
        && color.game_from_eye.iter().zip(depth.game_from_eye).all(|(&a, b)| close(a, b))
        && color.game_from_stage.iter().zip(depth.game_from_stage).all(|(&a, b)| close(a, b))
        && color.projection_from_eye.iter().zip(depth.projection_from_eye).enumerate()
            .all(|(index, (&a, b))| index % 4 == 2 || close(a, b))
}

pub(crate) struct PairSource<'a> {
    pub color: &'a Texture,
    pub depth: &'a Texture,
    pub frame: WorldFrame,
}

fn validate_pair_metadata(eyes: &[WorldFrame; 2], scene_epoch: u64) -> anyhow::Result<()> {
    anyhow::ensure!(scene_epoch != 0, "uncertified zero scene epoch");
    for (index, eye) in eyes.iter().enumerate() {
        anyhow::ensure!(eye.view_count == 2 && eye.view_index == index as u32,
            "paired eyes must be ordered left and right");
        anyhow::ensure!(eye.reserved == 0 && eye.game_frame != 0 && eye.request_frame != 0
            && eye.head_frame != 0 && eye.presentation_time.is_finite(),
            "paired eye lacks valid source identity");
        anyhow::ensure!(eye.projection_from_eye.iter().chain(&eye.game_from_eye)
            .chain(&eye.game_from_stage).all(|value| value.is_finite()),
            "paired eye matrix is non-finite");
        anyhow::ensure!(crate::xr_view::rigid(&eye.game_from_eye)
            && crate::xr_view::rigid(&eye.game_from_stage),
            "paired eye camera or stage transform is degenerate");
        let p = &eye.projection_from_eye;
        anyhow::ensure!(p[0] > 0. && p[5] > 0. && p[3].abs() <= 1e-5
            && p[7].abs() <= 1e-5 && (p[11] + 1.).abs() <= 1e-4
            && p[14].abs() > 1e-7 && p[15].abs() <= 1e-5,
            "paired eye projection is degenerate");
        anyhow::ensure!(eye.depth_convention == 0 && eye.depth_near.is_finite()
            && eye.depth_far.is_finite() && eye.depth_near > 0.
            && eye.depth_far > eye.depth_near,
            "paired eye depth convention or range is unsupported");
    }
    let (left, right) = (&eyes[0], &eyes[1]);
    anyhow::ensure!(left.game_frame == right.game_frame
        && left.request_frame == right.request_frame
        && left.head_frame == right.head_frame
        && left.presentation_time.to_bits() == right.presentation_time.to_bits(),
        "paired eyes do not share one scene, request and consumed head pose");
    anyhow::ensure!(left.game_from_stage.iter().zip(right.game_from_stage).all(|(a, b)|
        (a - b).abs() <= 0.00002), "paired eyes disagree on stage mapping");
    let separation_squared: f32 = (12..15).map(|index| {
        let delta = left.game_from_eye[index] - right.game_from_eye[index];
        delta * delta
    }).sum();
    anyhow::ensure!((0.001f32.powi(2)..=0.25f32.powi(2)).contains(&separation_squared),
        "paired eye camera separation is degenerate or implausible");
    anyhow::ensure!((left.depth_near - right.depth_near).abs() <= 0.0001
        && (left.depth_far - right.depth_far).abs() <= 0.1,
        "paired eye depth ranges differ");
    Ok(())
}

fn validate_pair_textures(eyes: &[PairSource<'_>; 2]) -> anyhow::Result<()> {
    for eye in eyes {
        anyhow::ensure!(eye.color.dimension() == TextureDimension::D2 && eye.color.sample_count() == 1
            && eye.color.depth_or_array_layers() == 1, "paired color must be resolved 2D");
        anyhow::ensure!(eye.depth.dimension() == TextureDimension::D2 && eye.depth.sample_count() == 1
            && eye.depth.depth_or_array_layers() == 1 && eye.depth.format() == TextureFormat::R32Float,
            "paired depth must be resolved R32Float");
        anyhow::ensure!(eye.color.width() == eye.depth.width() && eye.color.height() == eye.depth.height(),
            "paired color/depth extent mismatch");
        anyhow::ensure!(eye.color.usage().contains(TextureUsages::COPY_SRC)
            && eye.depth.usage().contains(TextureUsages::COPY_SRC), "paired source lacks COPY_SRC");
    }
    anyhow::ensure!(eyes[0].color.width() == eyes[1].color.width()
        && eyes[0].color.height() == eyes[1].color.height()
        && eyes[0].color.format() == eyes[1].color.format(), "paired eye texture layouts differ");
    Ok(())
}

fn reserve_snapshot_slot(frames: &mut Frames, device: &Device, color: &Texture,
    mut frozen_depth: Option<Texture>) -> Option<Arc<Slot>> {
    frames.pool.retain(|slot| Arc::strong_count(slot) > 1
        || (slot.color.width() == color.width() && slot.color.height() == color.height()
            && slot.color.format() == color.format()));
    if let Some(index) = frames.pool.iter().position(|slot| Arc::strong_count(slot) == 1
        && slot.color.width() == color.width() && slot.color.height() == color.height()
        && slot.color.format() == color.format()) {
        if let Some(depth) = frozen_depth.take() {
            // This Arc exists only in the pool. Replacing its old depth cannot
            // affect a queued copy, published Ready, or compositor lease.
            let Some(slot) = Arc::get_mut(&mut frames.pool[index]) else { return None; };
            slot.depth = depth;
        }
        return Some(frames.pool[index].clone());
    }
    if frames.pool.len() >= MAX_SLOTS { return None; }
    let size = Extent3d { width: color.width(), height: color.height(), depth_or_array_layers: 1 };
    let make = |format, label| device.create_texture(&TextureDescriptor {
        label: Some(label), size, mip_level_count: 1, sample_count: 1,
        dimension: TextureDimension::D2, format,
        usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let slot = Arc::new(Slot { color: make(color.format(), "Immersive world color snapshot"),
        depth: frozen_depth.unwrap_or_else(|| make(TextureFormat::R32Float,
            "Immersive actual world depth snapshot")) });
    frames.pool.push(slot.clone());
    Some(slot)
}

/// Future certified render-only producer entry. Both GPU source images must
/// already be independently rendered from the same frozen scene. A token from
/// natural alternating camera updates cannot call this API.
pub(crate) fn publish_pair(device: &Device, queue: &Queue, sources: [PairSource<'_>; 2],
    proof: crate::xr_view::CertifiedPairEpoch) {
    let result = (|| -> anyhow::Result<()> {
        anyhow::ensure!(paired_render_requested(), "paired render flag is off");
        let scene_epoch = proof.scene_epoch();
        let provenance_epoch = proof.provenance_epoch();
        let source_frames = [sources[0].frame, sources[1].frame];
        validate_pair_metadata(&source_frames, scene_epoch)?;
        validate_pair_textures(&sources)?;
        let color_handles = [metal_pointer(sources[0].color), metal_pointer(sources[1].color)];
        let depth_handles = [metal_pointer(sources[0].depth), metal_pointer(sources[1].depth)];
        anyhow::ensure!(color_handles[0].is_some() && color_handles[1].is_some()
            && color_handles[0] != color_handles[1],
            "paired eyes must have distinct native color targets");
        anyhow::ensure!(depth_handles[0].is_some() && depth_handles[1].is_some()
            && depth_handles[0] != depth_handles[1],
            "paired eyes must have distinct native depth targets");
        anyhow::ensure!(crate::xr_view::view_epoch_is_active(provenance_epoch),
            "paired source provenance expired");
        let (slots, generation) = {
            let mut frames = FRAMES.lock().map_err(|_| anyhow::anyhow!("world frame lock poisoned"))?;
            let left = reserve_snapshot_slot(&mut frames, device, sources[0].color, None)
                .context("no left-eye snapshot slot")?;
            let right = reserve_snapshot_slot(&mut frames, device, sources[1].color, None)
                .context("no right-eye snapshot slot")?;
            ([left, right], frames.generation)
        };
        let mut published = source_frames;
        for index in 0..2 {
            published[index].abi_version = 1;
            published[index].byte_size = std::mem::size_of::<WorldFrame>() as u32;
            published[index].width = sources[index].color.width();
            published[index].height = sources[index].color.height();
            published[index].color_texture = metal_pointer(&slots[index].color)
                .context("paired Metal color unavailable")?;
            published[index].depth_texture = metal_pointer(&slots[index].depth)
                .context("paired Metal depth unavailable")?;
        }
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("Immutable immersive paired-eye snapshot") });
        for index in 0..2 {
            let size = Extent3d { width: sources[index].color.width(), height: sources[index].color.height(),
                depth_or_array_layers: 1 };
            for (source, destination) in [(sources[index].color, &slots[index].color),
                (sources[index].depth, &slots[index].depth)] {
                encoder.copy_texture_to_texture(TexelCopyTextureInfo { texture: source, mip_level: 0,
                    origin: Origin3d::ZERO, aspect: TextureAspect::All }, TexelCopyTextureInfo {
                    texture: destination, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All }, size);
            }
        }
        queue.submit([encoder.finish()]);
        queue.on_submitted_work_done(move || {
            if let Ok(mut frames) = FRAMES.lock() {
                if generation != frames.generation || !crate::xr_view::view_epoch_is_active(provenance_epoch)
                    || frames.latest_pair.as_ref().is_some_and(|pair| pair.scene_epoch >= scene_epoch) { return; }
                let ready_at = Instant::now();
                let eyes = std::array::from_fn(|index| Ready { slot: slots[index].clone(),
                    frame: published[index], generation, provenance_epoch, pair_epoch: scene_epoch,
                    ready_at });
                frames.latest = [Some(eyes[0].clone()), Some(eyes[1].clone())];
                frames.latest_pair = Some(ReadyPair { eyes, scene_epoch, ready_at });
                frames.published += 2;
                frames.pairs_published += 1;
                frames.copied_depth_snapshots += 2;
                frames.error = None;
            }
        });
        let _ = device.poll(PollType::Poll);
        Ok(())
    })();
    if let Err(error) = result { reject(format!("paired source rejected: {error:#}")); }
}

pub(crate) fn publish(device: &Device, queue: &Queue, color: &Texture, depth: &Texture,
    frame: WorldFrame, provenance_epoch: u64) {
    publish_with_frozen_depth(device, queue, color, depth, None, frame, provenance_epoch);
}

/// A frozen depth may replace only the second, bridge-owned depth copy. The
/// producer's pre-UI copy and this color snapshot remain ordered on one queue.
pub(crate) fn publish_with_frozen_depth(device: &Device, queue: &Queue, color: &Texture,
    depth: &Texture, frozen_depth: Option<FrozenPreUiDepth>, mut frame: WorldFrame,
    provenance_epoch: u64) {
    let result = (|| -> anyhow::Result<()> {
        anyhow::ensure!(frame.view_index < frame.view_count && (1..=2).contains(&frame.view_count), "invalid world view count");
        anyhow::ensure!(frame.presentation_time.is_finite() && frame.projection_from_eye.iter()
            .chain(&frame.game_from_eye).chain(&frame.game_from_stage).all(|n| n.is_finite()), "non-finite world metadata");
        anyhow::ensure!(color.dimension() == TextureDimension::D2 && color.sample_count() == 1
            && color.depth_or_array_layers() == 1, "world color must be a resolved 2D image");
        anyhow::ensure!(depth.format() == TextureFormat::R32Float && depth.sample_count() == 1
            && depth.dimension() == TextureDimension::D2 && depth.depth_or_array_layers() == 1,
            "world depth must preserve actual 32-bit NDC depth");
        anyhow::ensure!(color.width() == depth.width() && color.height() == depth.height(), "world color/depth dimensions differ");
        anyhow::ensure!(color.usage().contains(TextureUsages::COPY_SRC) && depth.usage().contains(TextureUsages::COPY_SRC), "world snapshot sources lack COPY_SRC");
        // A bad or foreign candidate retains the original copy path. The
        // source and candidate must be the identical native image on the
        // color target's Metal device; metadata alone cannot prove ownership.
        let frozen_depth = frozen_depth.filter(|candidate| frozen_depth_transfer_requested() && {
            let texture = candidate.texture();
            let native = metal_pointer(texture);
            let native_device = metal_device_pointer(texture);
            texture.dimension() == TextureDimension::D2
                && texture.mip_level_count() == 1
                && texture.sample_count() == 1
                && texture.depth_or_array_layers() == 1
                && texture.format() == TextureFormat::R32Float
                && texture.width() == color.width()
                && texture.height() == color.height()
                && texture.usage().contains(TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING)
                && native.is_some() && native == metal_pointer(depth)
                && native_device.is_some() && native_device == metal_device_pointer(color)
        });
        let used_frozen_depth = frozen_depth.is_some();
        let (slot, generation) = {
            let mut frames = FRAMES.lock().map_err(|_| anyhow::anyhow!("world frame lock poisoned"))?;
            // A GPU command dequeued before close/recenter must not capture the
            // reopened generation after those operations reset its provenance.
            anyhow::ensure!(crate::xr_view::view_epoch_is_active(provenance_epoch), "world camera provenance expired before snapshot");
            let slot = match reserve_snapshot_slot(&mut frames, device, color,
                frozen_depth.map(|candidate| candidate.0)) {
                Some(slot) => slot,
                None => { frames.dropped += 1; return Ok(()); }
            };
            (slot, frames.generation)
        };
        frame.abi_version = 1;
        frame.byte_size = std::mem::size_of::<WorldFrame>() as u32;
        frame.width = color.width(); frame.height = color.height();
        frame.color_texture = metal_pointer(&slot.color).context("native Metal color unavailable")?;
        frame.depth_texture = metal_pointer(&slot.depth).context("native Metal depth unavailable")?;
        let size = Extent3d { width: color.width(), height: color.height(), depth_or_array_layers: 1 };
        let mut commands = device.create_command_encoder(&CommandEncoderDescriptor { label: Some("Immutable immersive world snapshot") });
        let mut copy = |source: &Texture, destination: &Texture| {
            commands.copy_texture_to_texture(TexelCopyTextureInfo { texture: source, mip_level: 0,
                origin: Origin3d::ZERO, aspect: TextureAspect::All }, TexelCopyTextureInfo {
                texture: destination, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All }, size);
        };
        copy(color, &slot.color);
        if !used_frozen_depth { copy(depth, &slot.depth); }
        queue.submit([commands.finish()]);
        // Separate compositor queues may read only after the producer's copy finishes.
        queue.on_submitted_work_done(move || {
            if let Ok(mut frames) = FRAMES.lock() {
                if generation != frames.generation || !crate::xr_view::view_epoch_is_active(provenance_epoch) { return; }
                let index = frame.view_index as usize;
                frames.latest[index] = Some(Ready { slot, frame, generation, provenance_epoch,
                    pair_epoch: 0, ready_at: Instant::now() });
                frames.published += 1;
                if used_frozen_depth { frames.frozen_depth_handoffs += 1; }
                else { frames.copied_depth_snapshots += 1; }
                frames.error = None;
            }
        });
        let _ = device.poll(PollType::Poll);
        Ok(())
    })();
    if let Err(error) = result { if let Ok(mut frames) = FRAMES.lock() { frames.error = Some(format!("{error:#}")); } }
}

use anyhow::Context;
fn metal_pointer(texture: &Texture) -> Option<u64> {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "visionos"))]
    {
        // The borrowed Objective-C address remains alive through the slot's Texture.
        let metal = unsafe { texture.as_hal::<wgpu::hal::api::Metal>() }?;
        Some(std::ptr::from_ref(metal.raw_handle()).cast::<()>() as usize as u64)
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "visionos")))]
    { let _ = texture; None }
}

fn metal_device_pointer(texture: &Texture) -> Option<usize> {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "visionos"))]
    {
        // MTLTexture.device is a borrowed id<MTLDevice>. Compare identity only;
        // the producer and compositor already retain their owning devices.
        let raw = metal_pointer(texture)? as usize as *mut objc2::runtime::AnyObject;
        let device: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![raw, device] };
        (!device.is_null()).then_some(device as usize)
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "visionos")))]
    { let _ = texture; None }
}

#[no_mangle]
pub extern "C" fn sv_world_acquire_frame(view_index: u32, output: *mut WorldFrame) -> i32 {
    if output.is_null() || (output as usize) % std::mem::align_of::<WorldFrame>() != 0 || view_index >= 2 { return -1; }
    // The caller supplies a valid pointer to a complete WorldFrame, as in WorldBridge.h.
    let output = unsafe { &mut *output };
    if output.abi_version != 1 || output.byte_size != std::mem::size_of::<WorldFrame>() as u32 { return -1; }
    let Ok(mut frames) = FRAMES.lock() else { return -1; };
    if frames.leases.len() >= MAX_LEASES { return 1; }
    let Some(mut ready) = frames.latest[view_index as usize].clone() else { return 1; };
    if !crate::xr_view::view_epoch_is_active(ready.provenance_epoch) || ready.ready_at.elapsed() > MAX_READY_AGE { return 1; }
    let Some(token) = frames.next_lease.checked_add(1) else { return -1; };
    frames.next_lease = token;
    ready.frame.lease = token;
    *output = ready.frame;
    frames.leases.insert(token, ready);
    frames.acquired += 1;
    0
}

/// Acquires both eye leases under one lock from one GPU-complete transaction.
/// Independent `latest[0]` and `latest[1]` can never satisfy this operation.
#[no_mangle]
pub extern "C" fn sv_world_acquire_pair(output: *mut WorldPair) -> i32 {
    if output.is_null() || (output as usize) % std::mem::align_of::<WorldPair>() != 0 { return -1; }
    let output = unsafe { &mut *output };
    if output.abi_version != PAIR_ABI_VERSION || output.byte_size != std::mem::size_of::<WorldPair>() as u32 {
        return -1;
    }
    // Every pending/error return leaves no stale textures or lease tokens in
    // caller memory. The caller must reinitialize version/size for each poll.
    *output = WorldPair { abi_version: PAIR_ABI_VERSION,
        byte_size: std::mem::size_of::<WorldPair>() as u32, ..Default::default() };
    if !paired_render_requested() { return 1; }
    let Ok(mut frames) = FRAMES.lock() else { return -1; };
    if frames.leases.len() > MAX_LEASES - 2 { return 1; }
    let Some(pair) = frames.latest_pair.clone() else { return 1; };
    if pair.ready_at.elapsed() > MAX_READY_AGE
        || pair.eyes.iter().any(|eye| eye.generation != frames.generation
            || !crate::xr_view::view_epoch_is_active(eye.provenance_epoch)) { return 1; }
    let Some(last_token) = frames.next_lease.checked_add(2) else { return -1; };
    let mut returned = WorldPair { abi_version: PAIR_ABI_VERSION,
        byte_size: std::mem::size_of::<WorldPair>() as u32, scene_epoch: pair.scene_epoch,
        eyes: [pair.eyes[0].frame, pair.eyes[1].frame] };
    for index in 0..2 {
        let token = last_token - (1 - index as u64);
        returned.eyes[index].lease = token;
        frames.leases.insert(token, pair.eyes[index].clone());
    }
    frames.next_lease = last_token;
    frames.acquired += 2;
    frames.pairs_acquired += 1;
    *output = returned;
    0
}

#[no_mangle]
pub extern "C" fn sv_world_release_frame(lease: u64) -> i32 {
    let Ok(mut frames) = FRAMES.lock() else { return -1; };
    if frames.leases.remove(&lease).is_some() { 0 } else { -1 }
}

#[no_mangle]
pub extern "C" fn sv_world_complete_frame(leases: *const u64, count: u32, metal_status: u32) -> i32 {
    if leases.is_null() || (leases as usize) % std::mem::align_of::<u64>() != 0 || !(1..=2).contains(&count) { return -1; }
    let tokens = unsafe { std::slice::from_raw_parts(leases, count as usize) };
    let Ok(mut frames) = FRAMES.lock() else { return -1; };
    let mut eyes = 0u32;
    let mut pair_epoch = None;
    for token in tokens {
        let Some(ready) = frames.leases.get(token) else { return -1; };
        if ready.generation != frames.generation || !crate::xr_view::view_epoch_is_active(ready.provenance_epoch) { return 1; }
        if ready.frame.view_count != count || ready.frame.view_index >= count { return -1; }
        if count == 2 {
            match pair_epoch {
                None => pair_epoch = Some(ready.pair_epoch),
                Some(epoch) if epoch != ready.pair_epoch => return -1,
                _ => {}
            }
        }
        eyes |= 1 << ready.frame.view_index;
    }
    if eyes != (1 << count) - 1 { return -1; }
    if metal_status != 4 {
        frames.error = Some(format!("compositor Metal command ended with status {metal_status}"));
        frames.compositor_views = 0;
        frames.compositor_pair_epoch = None;
        frames.compositor_completed_at = None;
        return 1;
    }
    frames.compositor_frames += 1;
    frames.compositor_views = count;
    frames.compositor_pair_epoch = pair_epoch.filter(|epoch| *epoch != 0);
    frames.compositor_completed_at = Some(Instant::now());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    #[test]
    fn depth_remapping_preserves_screen_rays_but_rejects_other_cameras() {
        let identity = [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.];
        let color = crate::xr_view::WorldView { provenance_epoch: 4, camera_sequence: 9,
            request_frame: 8, head_pose_frame: 8, presentation_time_seconds: 1.,
            eye_index: 0, view_count: 2, game_from_eye: identity, game_from_stage: identity,
            projection_from_eye: [1.,0.,0.,0., 0.,1.777778,0.,0., 0.,0.,-1.0001,-1., 0.,0.,-0.10001,0.],
            depth_reverse_z: false, headset_applied: true };
        let mut depth = color;
        for i in [2, 6, 10, 14] { depth.projection_from_eye[i] = color.projection_from_eye[i + 1] - color.projection_from_eye[i]; }
        depth.depth_reverse_z = true;
        assert!(shares_screen_rays(&color, &depth));
        depth.projection_from_eye[0] = 1.206285; // Uncorrected original game field of view.
        assert!(!shares_screen_rays(&color, &depth));
        depth.projection_from_eye[0] = color.projection_from_eye[0];
        depth.game_from_eye[12] = 0.032; // Opposite-eye or stale head translation.
        assert!(!shares_screen_rays(&color, &depth));
        depth.game_from_eye = color.game_from_eye;
        depth.camera_sequence += 1;
        assert!(!shares_screen_rays(&color, &depth));
    }
    #[test]
    fn frame_abi_layout_and_invalid_leases() {
        let _serial = TEST_LOCK.lock().unwrap();
        *FRAMES.lock().unwrap() = Frames::default();
        assert_eq!(std::mem::size_of::<WorldFrame>(), 288);
        assert_eq!(std::mem::offset_of!(WorldFrame, projection_from_eye), 88);
        assert_eq!(std::mem::offset_of!(WorldFrame, game_from_stage), 216);
        assert_eq!(sv_world_acquire_frame(0, std::ptr::null_mut()), -1);
        assert_eq!(sv_world_release_frame(0), -1);
        let mut frame = WorldFrame::default();
        assert_eq!(sv_world_acquire_frame(0, &mut frame), -1);
        frame.abi_version = 1; frame.byte_size = 288;
        assert_eq!(sv_world_acquire_frame(0, &mut frame), 1);
        assert_eq!(std::mem::size_of::<WorldPair>(), 592);
        assert_eq!(std::mem::offset_of!(WorldPair, eyes), 16);
        assert_eq!(sv_world_acquire_pair(std::ptr::null_mut()), -1);
        let mut pair = WorldPair::default();
        assert_eq!(sv_world_acquire_pair(&mut pair), -1);
        pair.abi_version = PAIR_ABI_VERSION;
        pair.byte_size = std::mem::size_of::<WorldPair>() as u32;
        assert_eq!(sv_world_acquire_pair(&mut pair), 1);
        assert_eq!(FRAMES.lock().unwrap().leases.len(), 0);
    }

    #[test]
    fn pair_metadata_requires_one_scene_pose_and_distinct_cameras() {
        let identity = [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.];
        let projection = [1.,0.,0.,0., 0.,1.5,0.,0., 0.,0.,-1.0001,-1., 0.,0.,-0.10001,0.];
        let left = WorldFrame { view_index: 0, view_count: 2, game_frame: 41,
            request_frame: 29, head_frame: 27, presentation_time: 3.,
            game_from_eye: identity, game_from_stage: identity,
            projection_from_eye: projection, depth_near: 0.1, depth_far: 1000., ..Default::default() };
        let mut right = left;
        right.view_index = 1;
        right.game_from_eye[12] = 0.064;
        assert!(validate_pair_metadata(&[left, right], 0).is_err());
        assert!(validate_pair_metadata(&[left, right], 5).is_ok());
        right.game_frame += 1;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.game_frame = left.game_frame;
        right.head_frame += 1;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.head_frame = left.head_frame;
        right.request_frame += 1;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.request_frame = left.request_frame;
        right.presentation_time += 0.001;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.presentation_time = left.presentation_time;
        right.game_from_stage[12] = 0.01;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.game_from_stage = left.game_from_stage;
        right.game_from_eye = left.game_from_eye;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.game_from_eye[12] = 0.064;
        right.projection_from_eye[0] = f32::NAN;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.projection_from_eye = left.projection_from_eye;
        right.projection_from_eye[11] = 0.;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.projection_from_eye = left.projection_from_eye;
        right.depth_convention = 1;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.depth_convention = 0;
        right.depth_far = right.depth_near;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
        right.depth_far = left.depth_far;
        right.game_from_eye[0] = 0.;
        assert!(validate_pair_metadata(&[left, right], 5).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "explicit Metal resource-lifetime fixture; not part of CPU-only checks"]
    fn metal_pair_acquisition_retains_both_eyes_across_resize_and_epoch_reset() {
        let _serial = TEST_LOCK.lock().unwrap();
        let old_flag = std::env::var_os("MUGUET_XR_PAIRED_RENDER");
        std::env::set_var("MUGUET_XR_PAIRED_RENDER", "1");
        *FRAMES.lock().unwrap() = Frames::default();
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(1), 0);
        let provenance = crate::xr_view::view_epoch();
        let mut descriptor = InstanceDescriptor::new_without_display_handle();
        descriptor.backends = Backends::METAL;
        let instance = Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).unwrap();
        let make = |extent: u32, format| device.create_texture(&TextureDescriptor {
            label: Some("Coherent eye fixture"),
            size: Extent3d { width: extent, height: extent, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2, format,
            usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST, view_formats: &[],
        });
        let upload = |texture: &Texture, pixel: [u8; 4]| {
            let extent = texture.width();
            queue.write_texture(TexelCopyTextureInfo { texture, mip_level: 0,
                origin: Origin3d::ZERO, aspect: TextureAspect::All },
                &pixel.repeat((extent * extent) as usize),
                TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(extent * 4),
                    rows_per_image: Some(extent) },
                Extent3d { width: extent, height: extent, depth_or_array_layers: 1 });
        };
        let read_pixel = |texture: &Texture| -> [u8; 4] {
            let extent = texture.width();
            let output = device.create_buffer(&BufferDescriptor {
                label: Some("Coherent eye readback"), size: 256 * extent as u64,
                usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
                mapped_at_creation: false });
            let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
            encoder.copy_texture_to_buffer(TexelCopyTextureInfo { texture, mip_level: 0,
                origin: Origin3d::ZERO, aspect: TextureAspect::All },
                TexelCopyBufferInfo { buffer: &output,
                    layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256),
                        rows_per_image: Some(extent) } },
                Extent3d { width: extent, height: extent, depth_or_array_layers: 1 });
            let index = queue.submit([encoder.finish()]);
            let (send, receive) = std::sync::mpsc::channel();
            output.slice(..).map_async(MapMode::Read, move |result| { let _ = send.send(result); });
            device.poll(PollType::Wait { submission_index: Some(index),
                timeout: Some(Duration::from_secs(10)) }).unwrap();
            receive.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
            let mapped = output.slice(..).get_mapped_range().unwrap();
            let pixel = mapped[..4].try_into().unwrap();
            drop(mapped); output.unmap();
            pixel
        };
        let identity = [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.];
        let projection = [1.,0.,0.,0., 0.,1.5,0.,0., 0.,0.,-1.0001,-1., 0.,0.,-0.10001,0.];
        let mut left = WorldFrame { view_count: 2, game_frame: 10, request_frame: 8, head_frame: 7,
            presentation_time: 3., projection_from_eye: projection, game_from_eye: identity,
            game_from_stage: identity, depth_near: 0.1, depth_far: 1000., ..Default::default() };
        let mut right = left;
        right.view_index = 1;
        right.game_from_eye[12] = 0.064;
        let colors = [make(2, TextureFormat::Rgba8Unorm), make(2, TextureFormat::Rgba8Unorm)];
        let depths = [make(2, TextureFormat::R32Float), make(2, TextureFormat::R32Float)];
        upload(&colors[0], [255, 0, 0, 255]);
        upload(&colors[1], [0, 255, 0, 255]);
        upload(&depths[0], 0.25f32.to_le_bytes());
        upload(&depths[1], 0.5f32.to_le_bytes());
        let sources = std::array::from_fn(|index| PairSource { color: &colors[index],
            depth: &depths[index], frame: [left, right][index] });
        publish_pair(&device, &queue, sources,
            crate::xr_view::fixture_certified_pair_epoch(1, provenance));
        device.poll(PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(10)) }).unwrap();
        let mut pair = WorldPair { abi_version: PAIR_ABI_VERSION, byte_size: 592, ..Default::default() };
        assert_eq!(sv_world_acquire_pair(&mut pair), 0);
        assert_eq!(pair.scene_epoch, 1);
        assert_ne!(pair.eyes[0].color_texture, pair.eyes[1].color_texture);
        assert_ne!(pair.eyes[0].depth_texture, pair.eyes[1].depth_texture);
        let first_tokens = [pair.eyes[0].lease, pair.eyes[1].lease];
        assert_ne!(first_tokens[0], first_tokens[1]);
        let first_slots = {
            let frames = FRAMES.lock().unwrap();
            [frames.leases[&first_tokens[0]].slot.clone(),
             frames.leases[&first_tokens[1]].slot.clone()]
        };
        upload(&colors[0], [0, 0, 255, 255]);
        upload(&colors[1], [255, 255, 0, 255]);
        upload(&depths[0], 0.75f32.to_le_bytes());
        upload(&depths[1], 0.875f32.to_le_bytes());
        left.game_frame += 1; right.game_frame += 1;
        let larger_colors = [make(4, TextureFormat::Rgba8Unorm), make(4, TextureFormat::Rgba8Unorm)];
        let larger_depths = [make(4, TextureFormat::R32Float), make(4, TextureFormat::R32Float)];
        upload(&larger_colors[0], [255, 0, 255, 255]);
        upload(&larger_colors[1], [0, 255, 255, 255]);
        upload(&larger_depths[0], 0.125f32.to_le_bytes());
        upload(&larger_depths[1], 0.625f32.to_le_bytes());
        let larger_sources = std::array::from_fn(|index| PairSource { color: &larger_colors[index],
            depth: &larger_depths[index], frame: [left, right][index] });
        publish_pair(&device, &queue, larger_sources,
            crate::xr_view::fixture_certified_pair_epoch(2, provenance));
        device.poll(PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(10)) }).unwrap();
        let old_lease_colors = {
            let frames = FRAMES.lock().unwrap();
            [frames.leases[&first_tokens[0]].slot.color.width(),
             frames.leases[&first_tokens[1]].slot.color.width()]
        };
        assert_eq!(old_lease_colors, [2, 2]);
        pair = WorldPair { abi_version: PAIR_ABI_VERSION, byte_size: 592, ..Default::default() };
        assert_eq!(sv_world_acquire_pair(&mut pair), 0);
        assert_eq!(pair.scene_epoch, 2);
        assert_eq!([pair.eyes[0].width, pair.eyes[1].width], [4, 4]);
        let new_tokens = [pair.eyes[0].lease, pair.eyes[1].lease];
        let new_slots = {
            let frames = FRAMES.lock().unwrap();
            [frames.leases[&new_tokens[0]].slot.clone(),
             frames.leases[&new_tokens[1]].slot.clone()]
        };
        let mixed_tokens = [first_tokens[0], new_tokens[1]];
        assert_eq!(sv_world_complete_frame(mixed_tokens.as_ptr(), 2, 4), -1);
        let mut extra_tokens = Vec::new();
        for _ in 0..4 {
            let mut extra = WorldPair { abi_version: PAIR_ABI_VERSION, byte_size: 592, ..Default::default() };
            assert_eq!(sv_world_acquire_pair(&mut extra), 0);
            extra_tokens.extend(extra.eyes.map(|eye| eye.lease));
        }
        assert_eq!(FRAMES.lock().unwrap().leases.len(), MAX_LEASES);
        let mut pending = WorldPair { abi_version: PAIR_ABI_VERSION, byte_size: 592,
            scene_epoch: 999, eyes: pair.eyes };
        assert_eq!(sv_world_acquire_pair(&mut pending), 1);
        assert_eq!(pending.scene_epoch, 0);
        assert_eq!(pending.eyes[0].lease, 0);
        assert_eq!(pending.eyes[1].lease, 0);
        assert_eq!(FRAMES.lock().unwrap().leases.len(), MAX_LEASES);
        assert_eq!(sv_world_complete_frame(new_tokens.as_ptr(), 2, 4), 0);
        assert!(stereo_connected());
        for (slot, expected_color, expected_depth) in [
            (&first_slots[0], [255, 0, 0, 255], 0.25f32),
            (&first_slots[1], [0, 255, 0, 255], 0.5f32),
            (&new_slots[0], [255, 0, 255, 255], 0.125f32),
            (&new_slots[1], [0, 255, 255, 255], 0.625f32)] {
            assert_eq!(read_pixel(&slot.color), expected_color);
            assert_eq!(f32::from_le_bytes(read_pixel(&slot.depth)), expected_depth);
        }
        reset();
        assert!(!stereo_connected());
        assert_eq!(sv_world_complete_frame(new_tokens.as_ptr(), 2, 4), 1);
        assert_eq!(sv_world_acquire_pair(&mut pair), 1);
        assert_eq!(pair.scene_epoch, 0);
        assert_eq!(pair.eyes[0].lease, 0);
        assert_eq!(pair.eyes[1].lease, 0);
        for token in first_tokens.into_iter().chain(new_tokens).chain(extra_tokens) {
            assert_eq!(sv_world_release_frame(token), 0);
            assert_eq!(sv_world_release_frame(token), -1);
        }
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(0), 0);
        if let Some(value) = old_flag { std::env::set_var("MUGUET_XR_PAIRED_RENDER", value); }
        else { std::env::remove_var("MUGUET_XR_PAIRED_RENDER"); }
        *FRAMES.lock().unwrap() = Frames::default();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_snapshot_is_immutable_and_leases_are_bounded() {
        let _serial = TEST_LOCK.lock().unwrap();
        *FRAMES.lock().unwrap() = Frames::default();
        let mut descriptor = InstanceDescriptor::new_without_display_handle(); descriptor.backends = Backends::METAL;
        let instance = Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).unwrap();
        let make = |format| device.create_texture(&TextureDescriptor { label: Some("World bridge test source"),
            size: Extent3d { width: 2, height: 2, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: TextureDimension::D2, format, usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
            view_formats: &[] });
        let color = make(TextureFormat::Rgba8Unorm); let depth = make(TextureFormat::R32Float);
        let upload = |texture: &Texture, bytes: &[u8]| queue.write_texture(TexelCopyTextureInfo { texture,
            mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All }, bytes,
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(2) },
            Extent3d { width: 2, height: 2, depth_or_array_layers: 1 });
        upload(&color, &[255, 0, 0, 255].repeat(4)); upload(&depth, &0.25f32.to_le_bytes().repeat(4));
        let mut frame = WorldFrame { view_count: 1, game_frame: 42, request_frame: 40, head_frame: 40,
            presentation_time: 1., ..Default::default() };
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(1), 0);
        let epoch = crate::xr_view::view_epoch();
        publish(&device, &queue, &color, &depth, frame, epoch);
        device.poll(PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        frame.abi_version = 1; frame.byte_size = 288;
        assert_eq!(sv_world_acquire_frame(0, &mut frame), 0);
        assert_eq!(frame.game_frame, 42); assert_ne!(frame.color_texture, 0); assert_ne!(frame.depth_texture, 0);
        let token = frame.lease;
        // The live game target changes while the compositor holds the prior image.
        upload(&color, &[0, 0, 255, 255].repeat(4)); upload(&depth, &0.75f32.to_le_bytes().repeat(4));
        let slot = FRAMES.lock().unwrap().leases[&token].slot.clone();
        let output = device.create_buffer(&BufferDescriptor { label: Some("World bridge immutable readback"),
            size: 1024, usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        for (texture, offset) in [(&slot.color, 0), (&slot.depth, 512)] {
            encoder.copy_texture_to_buffer(TexelCopyTextureInfo { texture, mip_level: 0, origin: Origin3d::ZERO,
                aspect: TextureAspect::All }, TexelCopyBufferInfo { buffer: &output,
                layout: TexelCopyBufferLayout { offset, bytes_per_row: Some(256), rows_per_image: Some(2) } },
                Extent3d { width: 2, height: 2, depth_or_array_layers: 1 });
        }
        let index = queue.submit([encoder.finish()]);
        let (send, receive) = std::sync::mpsc::channel();
        output.slice(..).map_async(MapMode::Read, move |result| { let _ = send.send(result); });
        device.poll(PollType::Wait { submission_index: Some(index), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        receive.recv_timeout(std::time::Duration::from_secs(1)).unwrap().unwrap();
        let bytes = output.slice(..).get_mapped_range().unwrap();
        assert_eq!(&bytes[..4], &[255, 0, 0, 255]);
        assert_eq!(f32::from_le_bytes(bytes[512..516].try_into().unwrap()), 0.25);
        drop(bytes); output.unmap();
        let mut tokens = vec![token];
        for _ in 1..MAX_LEASES { assert_eq!(sv_world_acquire_frame(0, &mut frame), 0); tokens.push(frame.lease); }
        assert_eq!(sv_world_acquire_frame(0, &mut frame), 1);
        assert_eq!(sv_world_complete_frame(&token, 1, 4), 0);
        assert!(renderer_connected()); assert!(!stereo_connected());
        FRAMES.lock().unwrap().compositor_completed_at = Some(Instant::now() - Duration::from_secs(2));
        assert!(!renderer_connected()); // A fresh producer cannot hide a stalled compositor.
        FRAMES.lock().unwrap().compositor_completed_at = Some(Instant::now());
        FRAMES.lock().unwrap().latest[0].as_mut().unwrap().ready_at = Instant::now() - Duration::from_secs(2);
        assert!(!renderer_connected()); // Historical completion is not a fresh live connection.
        FRAMES.lock().unwrap().latest[0].as_mut().unwrap().ready_at = Instant::now();
        reset(); assert!(!renderer_connected());
        assert_eq!(sv_world_complete_frame(&token, 1, 4), 1); // Completion from a closed scene is stale.
        for token in tokens { assert_eq!(sv_world_release_frame(token), 0); assert_eq!(sv_world_release_frame(token), -1); }
        // A previously dequeued game image cannot adopt a reopened generation.
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(0), 0);
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(1), 0);
        publish(&device, &queue, &color, &depth, frame, epoch);
        assert_eq!(sv_world_acquire_frame(0, &mut frame), 1);
        assert!(FRAMES.lock().unwrap().error.as_deref().unwrap().contains("provenance expired"));
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(0), 0);
        *FRAMES.lock().unwrap() = Frames::default();
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "explicit Metal content and cross-queue lifetime fixture"]
    fn frozen_pre_ui_depth_is_owned_until_lease_release_and_invalid_handoff_copies() {
        let _serial = TEST_LOCK.lock().unwrap();
        let old_flag = std::env::var_os("MUGUET_TRANSFER_FROZEN_DEPTH");
        std::env::set_var("MUGUET_TRANSFER_FROZEN_DEPTH", "1");
        *FRAMES.lock().unwrap() = Frames::default();
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(1), 0);
        let epoch = crate::xr_view::view_epoch();
        let mut descriptor = InstanceDescriptor::new_without_display_handle();
        descriptor.backends = Backends::METAL;
        let instance = Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).unwrap();
        let make = |format| device.create_texture(&TextureDescriptor {
            label: Some("Pre-UI depth handoff fixture"),
            size: Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2, format,
            usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let upload = |texture: &Texture, pixel: [u8; 4]| {
            queue.write_texture(TexelCopyTextureInfo { texture, mip_level: 0,
                origin: Origin3d::ZERO, aspect: TextureAspect::All }, &pixel.repeat(4),
                TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(2) },
                Extent3d { width: 2, height: 2, depth_or_array_layers: 1 });
        };
        let read_pixel = |texture: &Texture| -> [u8; 4] {
            let output = device.create_buffer(&BufferDescriptor {
                label: Some("Frozen native depth readback"), size: 512,
                usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
            let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
            encoder.copy_texture_to_buffer(TexelCopyTextureInfo { texture, mip_level: 0,
                origin: Origin3d::ZERO, aspect: TextureAspect::All }, TexelCopyBufferInfo {
                buffer: &output, layout: TexelCopyBufferLayout { offset: 0,
                    bytes_per_row: Some(256), rows_per_image: Some(2) } },
                Extent3d { width: 2, height: 2, depth_or_array_layers: 1 });
            let index = queue.submit([encoder.finish()]);
            let (send, receive) = std::sync::mpsc::channel();
            output.slice(..).map_async(MapMode::Read, move |result| { let _ = send.send(result); });
            device.poll(PollType::Wait { submission_index: Some(index),
                timeout: Some(Duration::from_secs(10)) }).unwrap();
            receive.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
            let mapped = output.slice(..).get_mapped_range().unwrap();
            let pixel = mapped[..4].try_into().unwrap();
            drop(mapped); output.unmap();
            pixel
        };
        let copy_depth = |source: &Texture, destination: &Texture| {
            let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
            encoder.copy_texture_to_texture(TexelCopyTextureInfo { texture: source, mip_level: 0,
                origin: Origin3d::ZERO, aspect: TextureAspect::All }, TexelCopyTextureInfo {
                texture: destination, mip_level: 0, origin: Origin3d::ZERO,
                aspect: TextureAspect::All }, Extent3d { width: 2, height: 2, depth_or_array_layers: 1 });
            queue.submit([encoder.finish()]);
        };
        let color = make(TextureFormat::Rgba8Unorm);
        let game_depth = make(TextureFormat::R32Float);
        let pre_ui_depth = make(TextureFormat::R32Float);
        upload(&color, [255, 0, 0, 255]);
        upload(&game_depth, 0.25f32.to_le_bytes());
        copy_depth(&game_depth, &pre_ui_depth);
        upload(&game_depth, 0.75f32.to_le_bytes()); // Simulated UI clear after frozen copy.
        let base_frame = WorldFrame { view_count: 1, game_frame: 40, request_frame: 39,
            head_frame: 39, presentation_time: 1., ..Default::default() };
        publish_with_frozen_depth(&device, &queue, &color, &pre_ui_depth,
            Some(FrozenPreUiDepth::new_snapshot(pre_ui_depth.clone())), base_frame, epoch);
        device.poll(PollType::Wait { submission_index: None,
            timeout: Some(Duration::from_secs(10)) }).unwrap();
        let mut frame = base_frame;
        frame.abi_version = 1; frame.byte_size = 288;
        assert_eq!(sv_world_acquire_frame(0, &mut frame), 0);
        let first_token = frame.lease;
        assert_eq!(frame.depth_texture, metal_pointer(&pre_ui_depth).unwrap());
        assert_eq!(FRAMES.lock().unwrap().pool.len(), 1);
        let first_slot_ptr = {
            let mut frames = FRAMES.lock().unwrap();
            let slot = Arc::as_ptr(&frames.leases[&first_token].slot) as usize;
            // Remove latest so only the compositor lease can protect this slot.
            frames.latest[0] = None;
            slot
        };
        drop(pre_ui_depth); // No external handle keeps the frozen image alive.
        assert_eq!(FRAMES.lock().unwrap().frozen_depth_handoffs, 1);

        // A later world frame cannot reuse the color slot while the compositor
        // still owns its first lease; the frozen depth also remains unchanged.
        let next_pre_ui_depth = make(TextureFormat::R32Float);
        upload(&game_depth, 0.5f32.to_le_bytes());
        copy_depth(&game_depth, &next_pre_ui_depth);
        upload(&game_depth, 0.875f32.to_le_bytes());
        let next_frame = WorldFrame { game_frame: 41, ..base_frame };
        publish_with_frozen_depth(&device, &queue, &color, &next_pre_ui_depth,
            Some(FrozenPreUiDepth::new_snapshot(next_pre_ui_depth.clone())), next_frame, epoch);
        device.poll(PollType::Wait { submission_index: None,
            timeout: Some(Duration::from_secs(10)) }).unwrap();
        let second_ready = FRAMES.lock().unwrap().latest[0].as_ref().unwrap().clone();
        assert_ne!(first_slot_ptr, Arc::as_ptr(&second_ready.slot) as usize);
        let first_ready = FRAMES.lock().unwrap().leases[&first_token].clone();
        assert_eq!(first_ready.frame.depth_texture, metal_pointer(&first_ready.slot.depth).unwrap());
        assert_eq!(f32::from_le_bytes(read_pixel(first_ready.depth_texture())), 0.25);
        assert_eq!(f32::from_le_bytes(read_pixel(second_ready.depth_texture())), 0.5);
        assert_eq!(FRAMES.lock().unwrap().frozen_depth_handoffs, 2);

        // A candidate that is not the selected source must take the original
        // copy path even with the flag on; no foreign pointer reaches the ABI.
        let invalid_candidate = make(TextureFormat::R32Float);
        let fallback_frame = WorldFrame { game_frame: 42, ..base_frame };
        publish_with_frozen_depth(&device, &queue, &color, &game_depth,
            Some(FrozenPreUiDepth::new_snapshot(invalid_candidate)), fallback_frame, epoch);
        device.poll(PollType::Wait { submission_index: None,
            timeout: Some(Duration::from_secs(10)) }).unwrap();
        let fallback_ready = FRAMES.lock().unwrap().latest[0].as_ref().unwrap().clone();
        assert_eq!(fallback_ready.frame.depth_texture, metal_pointer(&fallback_ready.slot.depth).unwrap());
        assert_eq!(f32::from_le_bytes(read_pixel(fallback_ready.depth_texture())), 0.875);
        assert_eq!(FRAMES.lock().unwrap().copied_depth_snapshots, 1);

        std::env::set_var("MUGUET_TRANSFER_FROZEN_DEPTH", "0");
        let off_frame = WorldFrame { game_frame: 43, ..base_frame };
        publish_with_frozen_depth(&device, &queue, &color, &next_pre_ui_depth,
            Some(FrozenPreUiDepth::new_snapshot(next_pre_ui_depth.clone())), off_frame, epoch);
        device.poll(PollType::Wait { submission_index: None,
            timeout: Some(Duration::from_secs(10)) }).unwrap();
        let off_ready = FRAMES.lock().unwrap().latest[0].as_ref().unwrap().clone();
        assert_ne!(off_ready.frame.depth_texture, metal_pointer(&next_pre_ui_depth).unwrap());
        assert_eq!(f32::from_le_bytes(read_pixel(off_ready.depth_texture())), 0.5);
        assert_eq!(FRAMES.lock().unwrap().copied_depth_snapshots, 2);

        assert_eq!(sv_world_complete_frame(&first_token, 1, 4), 0);
        drop(first_ready);
        reset(); // Existing compositor lease still owns its frozen image.
        let leased_depth = FRAMES.lock().unwrap().leases[&first_token].slot.depth.clone();
        assert_eq!(f32::from_le_bytes(read_pixel(&leased_depth)), 0.25);
        drop(leased_depth);
        assert_eq!(sv_world_release_frame(first_token), 0);
        assert_eq!(sv_world_release_frame(first_token), -1);
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(0), 0);
        if let Some(value) = old_flag { std::env::set_var("MUGUET_TRANSFER_FROZEN_DEPTH", value); }
        else { std::env::remove_var("MUGUET_TRANSFER_FROZEN_DEPTH"); }
        *FRAMES.lock().unwrap() = Frames::default();
    }
}
