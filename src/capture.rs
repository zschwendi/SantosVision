//! One-shot readback of the game's actual presented texture for runtime verification.
//! This does not submit a game image to the immersive compositor.
use anyhow::{Context, Result};
use serde_json::json;
use std::ffi::{c_char, CStr};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use wgpu::*;

#[derive(Default)]
struct Capture {
    pending: Option<PathBuf>,
    state: &'static str,
    path: Option<PathBuf>,
    frame: u64,
    width: u32,
    height: u32,
    error: Option<String>,
}
static CAPTURE: LazyLock<Mutex<Capture>> = LazyLock::new(|| Mutex::new(Capture { state: "idle", ..Default::default() }));

#[no_mangle]
pub extern "C" fn sv_gpu_capture_next(path: *const c_char) -> i32 {
    if path.is_null() { return -1; }
    let Ok(text) = (unsafe { CStr::from_ptr(path) }).to_str() else { return -1; };
    let path = PathBuf::from(text);
    // A verification capture must never replace a user's existing file.
    if !path.is_absolute() || path.exists() || !path.parent().is_some_and(|parent| parent.is_dir()) { return -1; }
    let Ok(mut capture) = CAPTURE.lock() else { return -1; };
    if matches!(capture.state, "pending" | "reading") { return -1; }
    *capture = Capture { pending: Some(path.clone()), state: "pending", path: Some(path), ..Default::default() };
    0
}

pub fn status_json() -> serde_json::Value {
    let Ok(c) = CAPTURE.lock() else { return json!({"state": "failed", "error": "capture lock poisoned"}); };
    json!({"state": c.state, "path": c.path, "frame": c.frame,
        "width": c.width, "height": c.height, "error": c.error})
}

pub(crate) fn on_present(device: &Device, queue: &Queue, texture: &Texture, frame: u64) {
    let path = {
        let Ok(mut capture) = CAPTURE.lock() else { return; };
        let Some(path) = capture.pending.take() else { return; };
        capture.state = "reading";
        capture.frame = frame;
        capture.width = texture.width();
        capture.height = texture.height();
        path
    };
    let result = begin_readback(device, queue, texture, path);
    if let Err(error) = result { record_result(Err(error)); }
}

fn record_result(result: Result<()>) {
    if let Ok(mut capture) = CAPTURE.lock() {
        match result {
            Ok(()) => { capture.state = "written"; eprintln!("[capture] saved actual game frame {} ({}x{}) to {:?}", capture.frame, capture.width, capture.height, capture.path); }
            Err(error) => { capture.state = "failed"; capture.error = Some(format!("{error:#}")); eprintln!("[capture] {error:#}"); }
        }
    }
}

fn begin_readback(device: &Device, queue: &Queue, texture: &Texture, path: PathBuf) -> Result<()> {
    let format = texture.format();
    anyhow::ensure!(matches!(format, TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb | TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb), "capture needs an RGBA8/BGRA8 final texture, got {format:?}");
    anyhow::ensure!(texture.sample_count() == 1 && texture.dimension() == TextureDimension::D2, "capture needs a resolved 2D texture");
    anyhow::ensure!(texture.usage().contains(TextureUsages::COPY_SRC), "presented texture lacks COPY_SRC");
    let (width, height) = (texture.width(), texture.height());
    let row_bytes = width.checked_mul(4).context("capture width overflow")?;
    let padded = row_bytes.checked_add(COPY_BYTES_PER_ROW_ALIGNMENT - 1).context("capture stride overflow")? / COPY_BYTES_PER_ROW_ALIGNMENT * COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&BufferDescriptor {
        label: Some("SantosVision actual game capture"), size: padded as u64 * height as u64,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor { label: Some("Game texture verification readback") });
    encoder.copy_texture_to_buffer(
        TexelCopyTextureInfo { texture, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
        TexelCopyBufferInfo { buffer: &buffer, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(height) } },
        Extent3d { width, height, depth_or_array_layers: 1 },
    );
    let submission = queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(MapMode::Read, move |result| { let _ = sender.send(result); });
    let device = device.clone();
    std::thread::Builder::new().name("game-frame-capture".into()).spawn(move || {
        let result = (|| -> Result<()> {
            device.poll(PollType::Wait { submission_index: Some(submission), timeout: Some(Duration::from_secs(10)) }).context("poll game capture")?;
            receiver.recv_timeout(Duration::from_secs(2)).context("wait for capture mapping")?.context("map capture")?;
            let pixels = buffer.slice(..).get_mapped_range().context("read mapped capture range")?;
            let rgb = rgba_rows_to_rgb(&pixels, width, height, padded, matches!(format, TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb))?;
            // create_new repeats the no-overwrite check atomically after GPU completion.
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).with_context(|| format!("create {}", path.display()))?;
            write!(file, "P6\n{width} {height}\n255\n")?;
            file.write_all(&rgb)?;
            file.sync_all()?;
            drop(pixels);
            buffer.unmap();
            Ok(())
        })();
        record_result(result);
    }).context("spawn capture worker")?;
    Ok(())
}

fn rgba_rows_to_rgb(bytes: &[u8], width: u32, height: u32, stride: u32, bgra: bool) -> Result<Vec<u8>> {
    let width = width as usize;
    let height = height as usize;
    let stride = stride as usize;
    anyhow::ensure!(stride >= width * 4 && bytes.len() >= stride * height, "short capture buffer");
    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        for pixel in bytes[row * stride..row * stride + width * 4].chunks_exact(4) {
            if bgra { rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]); }
            else { rgb.extend_from_slice(&pixel[..3]); }
        }
    }
    Ok(rgb)
}

#[cfg(test)]
mod tests {
    #[test]
    fn bgra_capture_preserves_color_and_skips_row_padding() {
        let bytes = [30, 20, 10, 255, 6, 5, 4, 255, 99, 99, 99, 99,
            3, 2, 1, 255, 9, 8, 7, 255, 88, 88, 88, 88];
        assert_eq!(super::rgba_rows_to_rgb(&bytes, 2, 2, 12, true).unwrap(), vec![10, 20, 30, 4, 5, 6, 1, 2, 3, 7, 8, 9]);
    }

    #[test]
    fn truncated_readback_is_rejected() {
        assert!(super::rgba_rows_to_rgb(&[0; 7], 2, 1, 8, false).is_err());
    }
}
