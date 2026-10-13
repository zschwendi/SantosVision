// muguet - audio.rs
// Constantin Clerc (c22dev) - 0.1

use crate::RT;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

static UNDERRUN_FRAMES: AtomicU64 = AtomicU64::new(0);
static CALLBACKS: AtomicU64 = AtomicU64::new(0);

static STARTED: AtomicBool = AtomicBool::new(false);

/// Unity restores the game's normal mix. The visionOS launcher migrates the
/// earlier temporary mute once, then preserves subsequent volume selections.
const DEFAULT_MASTER_VOLUME: f32 = 1.0;

/// Launch override, then the persistent setting, then the default. Anything
/// that is not a finite gain in 0...1 is ignored rather than played.
fn resolve_master_volume(env: Option<&str>, setting: Option<&str>) -> f32 {
    let valid = |v: f64| (v.is_finite() && (0.0..=1.0).contains(&v)).then_some(v as f32);
    env.and_then(|v| v.trim().parse::<f64>().ok()).and_then(valid)
        .or_else(|| setting.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|v| v.get("master_volume").and_then(|v| v.as_f64())).and_then(valid))
        .unwrap_or(DEFAULT_MASTER_VOLUME)
}

/// Game master gain, fixed for the process before the first output sample.
pub fn master_volume() -> f32 {
    static VOLUME: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *VOLUME.get_or_init(|| {
        let env = std::env::var("MUGUET_MASTER_VOLUME").ok();
        let setting = std::env::var_os("MUGUET_FSROOT")
            .and_then(|root| std::fs::read_to_string(std::path::Path::new(&root).join("native-audio.json")).ok());
        let volume = resolve_master_volume(env.as_deref(), setting.as_deref());
        eprintln!("[audio] game master volume {volume}");
        volume
    })
}

pub fn start(ring: u64, capacity: u32) {
    if STARTED.swap(true, Ordering::SeqCst) || std::env::var("MUGUET_MUTE").is_ok() {
        return;
    }

    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || match play(ring, capacity) {
            Ok(stream) => {
                eprintln!("[audio] CoreAudio output started (ring 0x{ring:x}, {capacity} frames)");
                let _keep = stream;
                loop { std::thread::park(); }
            }
            Err(e) => eprintln!("[audio] no output: {e:#}"),
        })
        .expect("spawn audio thread");
}

fn play(ring: u64, capacity: u32) -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no default output device"))?;
    let config = cpal::StreamConfig { channels: 2, sample_rate: 48000, buffer_size: cpal::BufferSize::Default };
    let (base, _) = crate::mem_base_len(&RT.get().unwrap().mem);
    let base = base as usize;
    let hdr = move |i: usize| -> &'static AtomicI32 { unsafe { &*((base + ring as usize + 4 * i) as *const AtomicI32) } };
    let data = base + ring as usize + 64;
    let mask = capacity.wrapping_sub(1);
    let volume = master_volume();
    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], _| {
            let frames = out.len() / 2;
            let read = hdr(1).load(Ordering::SeqCst) as u32;
            let write = hdr(0).load(Ordering::SeqCst) as u32;
            let take = (write.wrapping_sub(read) as usize).min(frames);
            #[cfg(target_os = "visionos")]
            let audible = crate::native::is_active();
            #[cfg(not(target_os = "visionos"))]
            let audible = true;
            for i in 0..take {
                let at = ((read.wrapping_add(i as u32) & mask) * 2) as usize;
                unsafe {
                    let p = (data as *const f32).add(at);
                    // The ring is still drained at zero gain, so game timing is unchanged.
                    out[2 * i] = if audible && volume > 0.0 { *p * volume } else { 0.0 };
                    out[2 * i + 1] = if audible && volume > 0.0 { *p.add(1) * volume } else { 0.0 };
                }
            }
            for s in &mut out[take * 2..] { *s = 0.0; }
            UNDERRUN_FRAMES.fetch_add((frames - take) as u64, Ordering::Relaxed);
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
            hdr(1).store(read.wrapping_add(take as u32) as i32, Ordering::SeqCst);
            hdr(3).fetch_add(1, Ordering::SeqCst);
        },
        |e| eprintln!("[audio] stream error: {e}"),
        None,
    )?;
    stream.play()?;

    std::thread::spawn(move || {
        let mut last_w = hdr(0).load(Ordering::SeqCst) as u32;
        let mut last_u = 0;
        let mut last_c = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let w = hdr(0).load(Ordering::SeqCst) as u32;
            let (u, c) = (UNDERRUN_FRAMES.load(Ordering::Relaxed), CALLBACKS.load(Ordering::Relaxed));
            eprintln!("[audio] mixer produced {:.0} frames/s (needs 48000), underrun {:.0} frames/s, {} callbacks/s, ring fill {}",
                w.wrapping_sub(last_w) as f64 / 5.0, (u - last_u) as f64 / 5.0, (c - last_c) / 5, w.wrapping_sub(hdr(1).load(Ordering::SeqCst) as u32));
            last_w = w; last_u = u; last_c = c;
        }
    });
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_volume_prefers_launch_override_then_setting_then_default() {
        assert_eq!(resolve_master_volume(None, None), DEFAULT_MASTER_VOLUME);
        assert_eq!(resolve_master_volume(None, Some(r#"{"master_volume":0.25}"#)), 0.25);
        assert_eq!(resolve_master_volume(Some(" 1 "), Some(r#"{"master_volume":0.25}"#)), 1.0);
        assert_eq!(resolve_master_volume(Some("0"), Some(r#"{"master_volume":1.0}"#)), 0.0);
    }

    #[test]
    fn master_volume_ignores_values_that_are_not_a_gain() {
        for bad in ["", "loud", "NaN", "inf", "-0.5", "1.5"] {
            assert_eq!(resolve_master_volume(Some(bad), None), DEFAULT_MASTER_VOLUME, "{bad}");
            assert_eq!(resolve_master_volume(Some(bad), Some(r#"{"master_volume":0.5}"#)), 0.5, "{bad}");
        }
        for bad in ["bad JSON", "{}", r#"{"master_volume":"1"}"#, r#"{"master_volume":2}"#, r#"{"master_volume":-1}"#] {
            assert_eq!(resolve_master_volume(None, Some(bad)), DEFAULT_MASTER_VOLUME, "{bad}");
        }
    }
}
