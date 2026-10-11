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

struct Slot { color: Texture, depth: Texture }
#[derive(Clone)]
struct Ready { slot: Arc<Slot>, frame: WorldFrame, generation: u64, provenance_epoch: u64, ready_at: Instant }
#[derive(Default)]
struct Frames {
    pool: Vec<Arc<Slot>>,
    latest: [Option<Ready>; 2],
    leases: HashMap<u64, Ready>,
    next_lease: u64,
    published: u64,
    dropped: u64,
    acquired: u64,
    error: Option<String>,
    generation: u64,
    compositor_frames: u64,
    compositor_views: u32,
    compositor_completed_at: Option<Instant>,
}
static FRAMES: LazyLock<Mutex<Frames>> = LazyLock::new(|| Mutex::new(Frames::default()));

pub(crate) fn reset() {
    if let Ok(mut frames) = FRAMES.lock() {
        frames.latest = [None, None];
        frames.generation = frames.generation.wrapping_add(1);
        frames.compositor_views = 0;
        frames.compositor_completed_at = None;
        // Outstanding compositor leases retain their immutable textures until release.
    }
}

pub(crate) fn status_json() -> serde_json::Value {
    let Ok(frames) = FRAMES.lock() else { return json!({"error": "world frame lock poisoned"}); };
    json!({"abi_version": 1, "storage": "immutable GPU-complete Metal color and real depth",
        "published": frames.published, "dropped": frames.dropped, "acquired": frames.acquired,
        "outstanding_leases": frames.leases.len(), "slots": frames.pool.len(), "error": frames.error,
        "compositor_completed_frames": frames.compositor_frames, "compositor_completed_views": frames.compositor_views,
        "compositor_completion_age_seconds": frames.compositor_completed_at.map(|time| time.elapsed().as_secs_f64()),
        "latest_views": frames.latest.iter().map(|r| r.as_ref().map(|r| json!({
            "view_index": r.frame.view_index, "game_frame": r.frame.game_frame,
            "request_frame": r.frame.request_frame, "head_frame": r.frame.head_frame,
            "presentation_time": r.frame.presentation_time, "width": r.frame.width, "height": r.frame.height,
            "age_seconds": r.ready_at.elapsed().as_secs_f64()
        }))).collect::<Vec<_>>()})
}

fn fresh_completed_views(frames: &Frames) -> bool {
    frames.compositor_views > 0 && frames.compositor_completed_at.is_some_and(|time| time.elapsed() <= MAX_READY_AGE)
        && frames.latest[..frames.compositor_views as usize].iter().all(|ready|
        ready.as_ref().is_some_and(|ready| ready.ready_at.elapsed() <= MAX_READY_AGE
            && crate::xr_view::view_epoch_is_active(ready.provenance_epoch)))
}
pub(crate) fn renderer_connected() -> bool { FRAMES.lock().is_ok_and(|f| fresh_completed_views(&f)) }
pub(crate) fn stereo_connected() -> bool { FRAMES.lock().is_ok_and(|f| f.compositor_views == 2 && fresh_completed_views(&f)) }
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

pub(crate) fn publish(device: &Device, queue: &Queue, color: &Texture, depth: &Texture, mut frame: WorldFrame, provenance_epoch: u64) {
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
        let (slot, generation) = {
            let mut frames = FRAMES.lock().map_err(|_| anyhow::anyhow!("world frame lock poisoned"))?;
            // A GPU command dequeued before close/recenter must not capture the
            // reopened generation after those operations reset its provenance.
            anyhow::ensure!(crate::xr_view::view_epoch_is_active(provenance_epoch), "world camera provenance expired before snapshot");
            frames.pool.retain(|slot| Arc::strong_count(slot) > 1
                || (slot.color.width() == color.width() && slot.color.height() == color.height()
                    && slot.color.format() == color.format()));
            let available = frames.pool.iter().find(|slot| Arc::strong_count(slot) == 1
                && slot.color.width() == color.width() && slot.color.height() == color.height()
                && slot.color.format() == color.format()).cloned();
            let slot = match available {
                Some(slot) => slot,
                None if frames.pool.len() < MAX_SLOTS => {
                    let size = Extent3d { width: color.width(), height: color.height(), depth_or_array_layers: 1 };
                    let make = |format, label| device.create_texture(&TextureDescriptor {
                        label: Some(label), size, mip_level_count: 1, sample_count: 1,
                        dimension: TextureDimension::D2, format,
                        usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING, view_formats: &[],
                    });
                    let slot = Arc::new(Slot { color: make(color.format(), "Immersive world color snapshot"),
                        depth: make(TextureFormat::R32Float, "Immersive actual world depth snapshot") });
                    frames.pool.push(slot.clone());
                    slot
                }
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
        for (source, destination) in [(color, &slot.color), (depth, &slot.depth)] {
            commands.copy_texture_to_texture(TexelCopyTextureInfo { texture: source, mip_level: 0,
                origin: Origin3d::ZERO, aspect: TextureAspect::All }, TexelCopyTextureInfo {
                texture: destination, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All }, size);
        }
        queue.submit([commands.finish()]);
        // Separate compositor queues may read only after the producer's copy finishes.
        queue.on_submitted_work_done(move || {
            if let Ok(mut frames) = FRAMES.lock() {
                if generation != frames.generation || !crate::xr_view::view_epoch_is_active(provenance_epoch) { return; }
                let index = frame.view_index as usize;
                frames.latest[index] = Some(Ready { slot, frame, generation, provenance_epoch, ready_at: Instant::now() });
                frames.published += 1;
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
    for token in tokens {
        let Some(ready) = frames.leases.get(token) else { return -1; };
        if ready.generation != frames.generation || !crate::xr_view::view_epoch_is_active(ready.provenance_epoch) { return 1; }
        if ready.frame.view_count != count || ready.frame.view_index >= count { return -1; }
        eyes |= 1 << ready.frame.view_index;
    }
    if eyes != (1 << count) - 1 { return -1; }
    if metal_status != 4 {
        frames.error = Some(format!("compositor Metal command ended with status {metal_status}"));
        frames.compositor_views = 0;
        frames.compositor_completed_at = None;
        return 1;
    }
    frames.compositor_frames += 1;
    frames.compositor_views = count;
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
}
