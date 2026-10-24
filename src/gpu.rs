// muguet - gpu.rs
// Constantin Clerc (c22dev) - 0.1

use crate::mem_base_len;
use regex::Regex;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use wasmtime::SharedMemory;
use wgpu::*;
use crate::xr_view::foreground_depth as fg;

#[path = "render_prepare.rs"]
mod render_prepare;
use render_prepare::{PassKey, RenderPrepare};

#[path = "uniform_pack.rs"]
mod uniform_pack;
use uniform_pack::UniformPack;

#[path = "uniform_snapshot.rs"]
mod uniform_snapshot;
use uniform_snapshot::UniformSnapshot;

#[path = "bind_group_prepare.rs"]
mod bind_group_prepare;
use bind_group_prepare::{BindGroupPrepare, EntryScratch, KeyScratch, NonBindingKind};

#[cfg(all(test, target_os = "macos"))]
#[path = "guest_write_tests.rs"]
mod guest_write_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "upload_dedup_tests.rs"]
mod upload_dedup_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "texture_view_cache_tests.rs"]
mod texture_view_cache_tests;

#[cfg(all(test,target_os="macos"))]
#[path="xr_hud_gpu_tests.rs"]
mod xr_hud_gpu_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "xr_foreground_depth_metal_tests.rs"]
mod xr_foreground_depth_metal_tests;

/// A frozen pre-UI depth copy belongs to its qualified camera/present, not to
/// subsequent contents of the live source texture. Ordinary source clears do
/// not modify the copy. Other mutations and unqualified geometry still revoke
/// it; publication also requires the exact camera/eye provenance and consumes
/// every saved copy at each present.
#[derive(Clone, Copy)]
enum WorldDepthChange { LiveClear, ProvenUiClear, Other }

impl WorldDepthChange {
    fn retains_frozen_snapshot(self) -> bool { matches!(self, Self::LiveClear | Self::ProvenUiClear) }
}

fn invalidate_world_depth_records<Snapshot>(
    live: &mut HashMap<u32, (crate::xr_view::WorldView, u64)>,
    frozen: &mut HashMap<u32, (crate::xr_view::WorldView, Snapshot, u64)>,
    id: u32, change: WorldDepthChange,
) -> Option<(crate::xr_view::WorldView, u64)> {
    let previous_live = live.remove(&id);
    let previous_saved = if change.retains_frozen_snapshot() {
        frozen.get(&id).map(|(view, _, draws)| (*view, *draws))
    } else {
        frozen.remove(&id).map(|(view, _, draws)| (view, draws))
    };
    previous_live.or(previous_saved)
}

// Test-only: adapter features withheld from the next Gpu::new on this thread,
// so fixtures can reach the paths of devices that lack them.
#[cfg(test)]
thread_local! {
    static TEST_WITHHELD_FEATURES: std::cell::Cell<Features> = std::cell::Cell::new(Features::empty());
}

const RING_DATA: u64 = 64;
const BIND_SRV: u32 = 0x8;
const BIND_RT: u32 = 0x20;
const BIND_DS: u32 = 0x40;
const BIND_UAV: u32 = 0x80;
const RING_CHUNK: u64 = 8 << 20;
const UBO_WINDOW: u64 = 65536;
const FRAME_SLOTS: usize = 3;

pub(crate) fn completion_poll_needed(query_events_pending: bool) -> bool {
    query_events_pending || crate::world_frame::has_pending_callbacks()
}

pub(crate) fn producer_idle_timeout_millis(pending_maps: bool, query_events_pending: bool,
    pending_upload_callbacks: bool) -> u64 {
    if pending_maps || completion_poll_needed(query_events_pending) || pending_upload_callbacks { 1 } else { 100 }
}

const ZERO_VB_SIZE: u64 = 4096;
const OCCLUSION_CAPACITY: u32 = 2048;
const OCCLUSION_VISIBLE_COUNT: u64 = 65536;
const BG_LIMIT: usize = 24000;
const UPLOAD_OP12: usize = 0;
const UPLOAD_OP47: usize = 1;
const UPLOAD_UNIFORMS: usize = 2;
const UPLOAD_SHADER_CONSTS: usize = 3;
const UPLOAD_UAV_CLEAR: usize = 4;
const UPLOAD_UAV_COUNTER: usize = 5;
const UPLOAD_SOURCE_COUNT: usize = 6;
const THREAD_REPLAY: usize = 0;
const THREAD_UNIFORM_WAIT: usize = 1;
const THREAD_GUEST_WAIT: usize = 2;
const THREAD_MAP_PROCESS: usize = 3;
const THREAD_QUERY_POLL: usize = 4;
const THREAD_PRODUCER_IDLE: usize = 5;
const THREAD_SUSPEND_WAIT: usize = 6;
const THREAD_SUSPEND_SLEEP: usize = 7;
const THREAD_PRESENT_BEFORE_WAIT: usize = 8;
const THREAD_WORLD_PUBLICATION: usize = 9;
const THREAD_PIPELINE_COLLECTION: usize = 10;
const THREAD_TIMING_COUNT: usize = 11;
const GEOMETRY_DIAG_PAIR_CAP: usize = 32;

static GEOMETRY_DIAG_STATUS: LazyLock<Mutex<serde_json::Value>> =
    LazyLock::new(|| Mutex::new(serde_json::json!({"enabled":false})));

pub(crate) fn geometry_capture_diagnostics_json() -> serde_json::Value {
    GEOMETRY_DIAG_STATUS.lock().map(|status| status.clone())
        .unwrap_or_else(|_| serde_json::json!({"enabled":true,"error":"diagnostic lock poisoned"}))
}

#[derive(Default)]
struct GeometryCaptureDiagnostics {
    considered: u64,
    vertex_match: u64,
    pair_match: u64,
    view_match: u64,
    target_match: u64,
    depth_write_match: u64,
    surface_match: u64,
    draw_match: u64,
    eligible_attempts: u64,
    qualified_world_depth_draws: u64,
    other_qualified_pairs: u64,
    qualified_pairs: Vec<GeometryPairCount>,
    first_pair_state: Option<serde_json::Value>,
}

struct GeometryPairCount {
    vertex_hash: String,
    pixel_hash: String,
    vertex_label: String,
    pixel_label: String,
    draws: u64,
}
const API_WALL_NS_BOUNDS: [u64; 4] = [10_000, 100_000, 1_000_000, 10_000_000];
const UPLOAD_BYTE_BOUNDS: [u64; 4] = [256, 4096, 65536, 1048576];

#[derive(Clone, Copy, Default)]
struct ApiWallStats {
    calls: u64,
    total_ns: u64,
    max_ns: u64,
    buckets: [u64; 5],
}

impl ApiWallStats {
    #[inline]
    fn record(&mut self, elapsed: Duration) {
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.calls = self.calls.saturating_add(1);
        self.total_ns = self.total_ns.saturating_add(nanos);
        self.max_ns = self.max_ns.max(nanos);
        let bucket = API_WALL_NS_BOUNDS.iter().position(|bound| nanos <= *bound).unwrap_or(4);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
    }

    fn metrics(&self) -> serde_json::Value {
        serde_json::json!({"calls": self.calls, "wall_ns": self.total_ns, "max_wall_ns": self.max_ns,
            "wall_ns_bins_le_10000_100000_1000000_10000000_inf": self.buckets})
    }

    fn accumulate(&mut self, other: &Self) {
        self.calls = self.calls.saturating_add(other.calls);
        self.total_ns = self.total_ns.saturating_add(other.total_ns);
        self.max_ns = self.max_ns.max(other.max_ns);
        for index in 0..self.buckets.len() {
            self.buckets[index] = self.buckets[index].saturating_add(other.buckets[index]);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct BufferUploadStats {
    bytes: u64,
    byte_buckets: [u64; 5],
    api_wall: ApiWallStats,
}

impl BufferUploadStats {
    #[inline]
    fn record(&mut self, bytes: usize, elapsed: Duration) {
        let bytes = bytes as u64;
        self.bytes = self.bytes.saturating_add(bytes);
        let bucket = UPLOAD_BYTE_BOUNDS.iter().position(|bound| bytes <= *bound).unwrap_or(4);
        self.byte_buckets[bucket] = self.byte_buckets[bucket].saturating_add(1);
        self.api_wall.record(elapsed);
    }

    fn accumulate(&mut self, other: &Self) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.api_wall.accumulate(&other.api_wall);
        for index in 0..self.byte_buckets.len() {
            self.byte_buckets[index] = self.byte_buckets[index].saturating_add(other.byte_buckets[index]);
        }
    }
}

#[derive(Default)]
struct IndirectReadStats {
    attempts: u64,
    successes: u64,
    poll_errors: u64,
    map_errors: u64,
    wait_wall: ApiWallStats,
}

// Fixed-size GPU-thread-owned route timer. Queue mode times queue.write_buffer;
// pool mode includes source allocation/copy and prelude recording. The guest
// Vec snapshot and any earlier flush remain outside this timer. Not GPU time.
#[inline]
fn measured_write_buffer(uploads: &mut crate::buffer_upload::BufferUploads, device: &Device, queue: &Queue, stats: &mut BufferUploadStats, buffer: &Buffer, offset: u64, data: &[u8]) {
    let started = Instant::now();
    uploads.write(device, queue, buffer, offset, data);
    stats.record(data.len(), started.elapsed());
}

#[cfg(not(target_os = "visionos"))]
type SurfaceWindow = Arc<winit::window::Window>;
#[cfg(target_os = "visionos")]
type SurfaceWindow = ();

pub struct SurfaceHandoff {
    pub instance: Instance,
    pub surface: Surface<'static>,
    pub window: SurfaceWindow,
}
pub static SURFACE: Mutex<Option<SurfaceHandoff>> = Mutex::new(None);
pub static STARTED: AtomicBool = AtomicBool::new(false);

pub static OCCLUDED: AtomicBool = AtomicBool::new(false);

pub static SUSPENDED: AtomicBool = AtomicBool::new(false);

pub static PRESENTS: AtomicU64 = AtomicU64::new(0);
pub static WINDOW_PRESENTS: AtomicU64 = AtomicU64::new(0);

pub fn start(mem: &SharedMemory, ring: u64, size: u64, dbg: u64) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mem = mem.clone();
    std::thread::Builder::new()
        .name("gpu".into())
        .stack_size(32 << 20)
        .spawn(move || {
            crate::set_interactive_qos();
            let handoff = SURFACE.lock().unwrap().take();
            let mut gpu = match Gpu::new(mem.clone(), ring, size, dbg, handoff) {
                Ok(g) => g,
                Err(e) => {
                    log(&format!("init failed: {e:#}"));
                    #[cfg(target_os = "visionos")]
                    crate::native::report_error(format!("native GPU initialization failed: {e:#}"));
                    drain_forever(&mem, ring);
                }
            };
            gpu.run();
        })
        .expect("spawn gpu thread");
}

fn log(s: &str) {
    eprintln!("[wgpu] {s}");
}

fn drain_forever(mem: &SharedMemory, ring: u64) -> ! {
    let a = |w: u64| -> &AtomicI32 { atomic(mem, ring + 4 * w) };
    a(2).store(2, Ordering::SeqCst);
    loop {
        let h = a(0).load(Ordering::SeqCst);
        a(1).store(h, Ordering::SeqCst);
        let _ = mem.atomic_notify(ring + 4, u32::MAX);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn atomic(mem: &SharedMemory, addr: u64) -> &AtomicI32 {
    let (base, _) = mem_base_len(mem);
    unsafe { &*(base.add(addr as usize) as *const AtomicI32) }
}

struct Fmt {
    format: TextureFormat,
    views: Vec<TextureFormat>,
    a8: bool,
}
fn map_format(f: u32, bind: u32) -> Fmt {
    use TextureFormat as T;
    let ds = bind & BIND_DS != 0;
    let (format, views, a8) = match f {
        2 => (T::Rgba32Float, vec![], false),
        9 | 10 | 11 => (T::Rgba16Float, vec![], false),
        16 => (T::Rg32Float, vec![], false),
        23 | 24 => (T::Rgb10a2Unorm, vec![], false),
        26 => (T::Rg11b10Ufloat, vec![], false),
        27 | 28 => (T::Rgba8Unorm, vec![T::Rgba8UnormSrgb], false),
        29 => (T::Rgba8UnormSrgb, vec![T::Rgba8Unorm], false),
        90 | 87 | 88 => (T::Bgra8Unorm, vec![T::Bgra8UnormSrgb], false),
        91 => (T::Bgra8UnormSrgb, vec![T::Bgra8Unorm], false),
        33 | 34 | 35 => (T::Rg16Float, vec![], false),
        39 => (if ds { T::Depth32Float } else { T::R32Float }, vec![], false),
        40 => (T::Depth32Float, vec![], false),
        41 => (T::R32Float, vec![], false),
        19 | 20 => (T::Depth32FloatStencil8, vec![], false),
        44 | 45 => (T::Depth24PlusStencil8, vec![], false),
        48 | 49 => (T::Rg8Unorm, vec![], false),
        53 => (if ds { T::Depth16Unorm } else { T::R16Float }, vec![], false),
        55 => (T::Depth16Unorm, vec![], false),
        54 => (T::R16Float, vec![], false),
        60 | 61 => (T::R8Unorm, vec![], false),
        63 => (T::R8Snorm, vec![], false),
        51 => (T::Rg8Snorm, vec![], false),
        31 => (T::Rgba8Snorm, vec![], false),
        67 => (T::Rgb9e5Ufloat, vec![], false),
        65 => (T::Rgba8Unorm, vec![], true),
        56 => (T::Depth16Unorm, vec![], false),
        21 | 22 | 46 | 47 => (T::Depth24PlusStencil8, vec![], false),
        70 | 71 => (T::Bc1RgbaUnorm, vec![T::Bc1RgbaUnormSrgb], false),
        72 => (T::Bc1RgbaUnormSrgb, vec![T::Bc1RgbaUnorm], false),
        73 | 74 => (T::Bc2RgbaUnorm, vec![T::Bc2RgbaUnormSrgb], false),
        75 => (T::Bc2RgbaUnormSrgb, vec![T::Bc2RgbaUnorm], false),
        76 | 77 => (T::Bc3RgbaUnorm, vec![T::Bc3RgbaUnormSrgb], false),
        78 => (T::Bc3RgbaUnormSrgb, vec![T::Bc3RgbaUnorm], false),
        79 | 80 => (T::Bc4RUnorm, vec![], false),
        82 | 83 => (T::Bc5RgUnorm, vec![], false),
        94 | 95 => (T::Bc6hRgbUfloat, vec![], false),
        97 | 98 => (T::Bc7RgbaUnorm, vec![T::Bc7RgbaUnormSrgb], false),
        99 => (T::Bc7RgbaUnormSrgb, vec![T::Bc7RgbaUnorm], false),
        other => {
            log_once(&format!("fmt{other}"), &format!("no WebGPU format for DXGI {other} (using rgba8unorm)"));
            (T::Rgba8Unorm, vec![], false)
        }
    };
    Fmt { format, views, a8 }
}

fn is_depth(f: TextureFormat) -> bool {
    f.is_depth_stencil_format() || matches!(f, TextureFormat::Stencil8)
}

const STENCIL_PROXY: TextureFormat = TextureFormat::Rg8Uint;

fn has_stencil(f: TextureFormat) -> bool {
    matches!(f, TextureFormat::Depth24PlusStencil8 | TextureFormat::Depth32FloatStencil8 | TextureFormat::Stencil8)
}
fn is_bc(f: TextureFormat) -> bool {
    f.is_compressed()
}
fn block_bytes(f: TextureFormat) -> u32 {
    if !is_bc(f) { return 0; }
    f.block_copy_size(None).unwrap_or(16)
}
fn texel_bytes(f: TextureFormat) -> u32 {
    if is_bc(f) || is_depth(f) { return 0; }
    f.block_copy_size(None).unwrap_or(0)
}

static LOGGED: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(|| Mutex::new(Default::default()));
/// Keeps a readback inside the guest destination. `rows` are (source offset,
/// guest address, byte count); the guest sends the destination subresource's
/// own byte size, laid out in the guest's format. Returns the rows that fit
/// and the number of bytes withheld.
fn bounded_readback_rows(rows: Vec<(u64, u64, u64)>, addr: u64, size: u64, memory: u64) -> (Vec<(u64, u64, u64)>, u64) {
    let limit = addr.saturating_add(size).min(memory);
    let mut withheld = 0u64;
    let kept = rows.into_iter().filter_map(|(from, to, n)| {
        let fits = if to < addr { 0 } else { n.min(limit.saturating_sub(to)) };
        withheld += n - fits;
        (fits != 0).then_some((from, to, fits))
    }).collect();
    (kept, withheld)
}

fn log_once(key: &str, msg: &str) {
    if LOGGED.lock().unwrap().insert(key.to_string()) {
        log(msg);
    }
}

fn cmp(i: u32) -> Option<CompareFunction> {
    use CompareFunction as C;
    Some(match i { 1 => C::Never, 2 => C::Less, 3 => C::Equal, 4 => C::LessEqual, 5 => C::Greater, 6 => C::NotEqual, 7 => C::GreaterEqual, 8 => C::Always, _ => return None })
}
fn stencil_op(i: u32) -> StencilOperation {
    use StencilOperation as S;
    match i { 2 => S::Zero, 3 => S::Replace, 4 => S::IncrementClamp, 5 => S::DecrementClamp, 6 => S::Invert, 7 => S::IncrementWrap, 8 => S::DecrementWrap, _ => S::Keep }
}
fn blend_factor(i: u32, default: BlendFactor) -> BlendFactor {
    use BlendFactor as B;
    match i {
        1 => B::Zero, 2 => B::One, 3 => B::Src, 4 => B::OneMinusSrc, 5 => B::SrcAlpha, 6 => B::OneMinusSrcAlpha,
        7 => B::DstAlpha, 8 => B::OneMinusDstAlpha, 9 => B::Dst, 10 => B::OneMinusDst, 11 => B::SrcAlphaSaturated,
        14 => B::Constant, 15 => B::OneMinusConstant, 16 => B::Src1, 17 => B::OneMinusSrc1, 18 => B::Src1Alpha, 19 => B::OneMinusSrc1Alpha,
        _ => default,
    }
}
fn blend_op(i: u32) -> BlendOperation {
    use BlendOperation as O;
    match i { 2 => O::Subtract, 3 => O::ReverseSubtract, 4 => O::Min, 5 => O::Max, _ => O::Add }
}
fn topology(i: u32) -> (PrimitiveTopology, bool) {
    use PrimitiveTopology as P;
    match i { 1 => (P::PointList, false), 2 => (P::LineList, false), 3 => (P::LineStrip, true), 5 => (P::TriangleStrip, true), _ => (P::TriangleList, false) }
}
fn address(i: u32) -> AddressMode {
    match i { 2 | 5 => AddressMode::MirrorRepeat, 3 | 4 => AddressMode::ClampToEdge, _ => AddressMode::Repeat }
}
fn vertex_format(dxgi: u32) -> Option<VertexFormat> {
    use VertexFormat as V;
    Some(match dxgi {
        2 => V::Float32x4, 6 => V::Float32x3, 16 => V::Float32x2, 41 => V::Float32,
        3 => V::Uint32x4, 7 => V::Uint32x3, 17 => V::Uint32x2, 42 => V::Uint32,
        4 => V::Sint32x4, 8 => V::Sint32x3, 18 => V::Sint32x2, 43 => V::Sint32,
        10 => V::Float16x4, 34 => V::Float16x2,
        11 => V::Unorm16x4, 35 => V::Unorm16x2,
        12 => V::Uint16x4, 36 => V::Uint16x2,
        13 => V::Snorm16x4, 37 => V::Snorm16x2,
        14 => V::Sint16x4, 38 => V::Sint16x2,
        28 => V::Unorm8x4, 30 => V::Uint8x4, 31 => V::Snorm8x4, 32 => V::Sint8x4,
        49 => V::Unorm8x2, 50 => V::Uint8x2, 51 => V::Snorm8x2, 52 => V::Sint8x2,
        87 => V::Unorm8x4Bgra, 24 => V::Unorm10_10_10_2,
        _ => return None,
    })
}
fn wf(w: u32) -> f32 { f32::from_bits(w) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind { Uniform, Storage, ReadOnlyStorage, Sampler, SamplerCmp, Texture, StorageTexture }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sample { Float, Uint, Sint, Depth, Unfilterable }

#[derive(Clone, Debug)]
struct Binding {
    group: u32,
    binding: u32,
    kind: Kind,
    dim: TextureViewDimension,
    sample: Sample,
    ms: bool,
    format: Option<TextureFormat>,
    format_name: String,
    access: StorageTextureAccess,
}
struct Info {
    bindings: Vec<Binding>,
    outputs: Vec<u32>,
    writes_fragment_depth: bool,
}

static RE_DECL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@group\((\d+)u?\)\s*@binding\((\d+)u?\)\s*var(?:<([^>]*)>)?\s+(\w+)\s*:\s*([^;]+);").unwrap());
static RE_TEXTYPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^texture_(\w+?)(?:<([^>]*)>)?$").unwrap());
static RE_FRAG_SIG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@fragment\s*fn\s+main\s*\((?:[^()]|\([^)]*\))*\)\s*->\s*([^{]+)\{").unwrap());
static RE_LOC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@location\((\d+)u?\)").unwrap());
static RE_UNIFORM_DECL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@group\((\d+)u?\)\s*@binding\((\d+)u?\)\s*var<uniform>\s+(\w+)\s*:\s*(\w+)\s*;").unwrap());
static RE_ARRAY_N: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"array<[^>]*>,\s*(\d+)u?>").unwrap());
static RE_FIRST_FN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n(?:@\w+|fn)\s").unwrap());
static RE_FLAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@location\((\d+)u?\)\s*@interpolate\(flat[^)]*\)").unwrap());
static RE_VS_LOC_MEMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@location\((\d+)u\)(\s*\n\s*\w+\s*:)").unwrap());
static RE_VS_MAIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@vertex\s*fn\s+main\s*\([\s\S]*?\)\s*->\s*(\w+)\s*\{").unwrap());
static RE_PS_MAIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(@fragment\s*fn\s+main\s*\()([\s\S]*?)(\)\s*->)").unwrap());
static RE_MAIN_INNER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\n\s*main_inner\([^;]*\);)").unwrap());

fn view_dim(name: &str) -> TextureViewDimension {
    match name {
        "2d_array" | "2d-array" => TextureViewDimension::D2Array,
        "3d" => TextureViewDimension::D3,
        "cube" => TextureViewDimension::Cube,
        "cube_array" | "cube-array" => TextureViewDimension::CubeArray,
        "1d" => TextureViewDimension::D1,
        _ => TextureViewDimension::D2,
    }
}

fn wgsl_storage_format(name: &str) -> Option<TextureFormat> {
    use TextureFormat as T;
    Some(match name {
        "rgba8unorm" => T::Rgba8Unorm, "rgba8snorm" => T::Rgba8Snorm, "rgba8uint" => T::Rgba8Uint, "rgba8sint" => T::Rgba8Sint,
        "bgra8unorm" => T::Bgra8Unorm, "rgba16uint" => T::Rgba16Uint, "rgba16sint" => T::Rgba16Sint, "rgba16float" => T::Rgba16Float,
        "r32uint" => T::R32Uint, "r32sint" => T::R32Sint, "r32float" => T::R32Float, "rg32uint" => T::Rg32Uint, "rg32sint" => T::Rg32Sint,
        "rg32float" => T::Rg32Float, "rgba32uint" => T::Rgba32Uint, "rgba32sint" => T::Rgba32Sint, "rgba32float" => T::Rgba32Float,
        "r8unorm" => T::R8Unorm, "rg8unorm" => T::Rg8Unorm, "r16float" => T::R16Float, "rg16float" => T::Rg16Float,
        "rgb10a2unorm" => T::Rgb10a2Unorm, "rg11b10ufloat" => T::Rg11b10Ufloat, "r16uint" => T::R16Uint, "rg16uint" => T::Rg16Uint,
        _ => return None,
    })
}
fn wgsl_format_name(f: TextureFormat) -> Option<&'static str> {
    use TextureFormat as T;
    Some(match f {
        T::Rgba8Unorm => "rgba8unorm", T::Rgba8Snorm => "rgba8snorm", T::Rgba8Uint => "rgba8uint", T::Rgba8Sint => "rgba8sint",
        T::Bgra8Unorm => "bgra8unorm", T::Rgba16Uint => "rgba16uint", T::Rgba16Sint => "rgba16sint", T::Rgba16Float => "rgba16float",
        T::R32Uint => "r32uint", T::R32Sint => "r32sint", T::R32Float => "r32float", T::Rg32Uint => "rg32uint", T::Rg32Sint => "rg32sint",
        T::Rg32Float => "rg32float", T::Rgba32Uint => "rgba32uint", T::Rgba32Sint => "rgba32sint", T::Rgba32Float => "rgba32float",
        T::R8Unorm => "r8unorm", T::Rg8Unorm => "rg8unorm", T::R16Float => "r16float", T::Rg16Float => "rg16float",
        T::Rgb10a2Unorm => "rgb10a2unorm", T::Rg11b10Ufloat => "rg11b10ufloat",
        _ => return None,
    })
}

fn parse_wgsl(text: &str) -> Info {
    let mut bindings = Vec::new();
    for m in RE_DECL.captures_iter(text) {
        let group: u32 = m[1].parse().unwrap_or(0);
        let binding: u32 = m[2].parse().unwrap_or(0);
        let space: String = m.get(3).map(|x| x.as_str()).unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        let ty = m[5].trim().to_string();
        let mut b = Binding {
            group, binding, kind: Kind::Texture, dim: TextureViewDimension::D2, sample: Sample::Float, ms: false,
            format: None, format_name: String::new(), access: StorageTextureAccess::WriteOnly,
        };
        if space == "uniform" {
            b.kind = Kind::Uniform;
        } else if space.starts_with("storage") {
            b.kind = if space.contains("read_write") { Kind::Storage } else { Kind::ReadOnlyStorage };
        } else if ty == "sampler" {
            b.kind = Kind::Sampler;
        } else if ty == "sampler_comparison" {
            b.kind = Kind::SamplerCmp;
        } else {
            let (name, arg) = match RE_TEXTYPE.captures(&ty) {
                Some(t) => (t[1].to_string(), t.get(2).map(|x| x.as_str().to_string()).unwrap_or_else(|| "f32".into())),
                None => ("2d".to_string(), "f32".to_string()),
            };
            if let Some(d) = name.strip_prefix("storage_") {
                b.kind = Kind::StorageTexture;
                b.dim = view_dim(d);
                b.format_name = arg.split(',').next().unwrap_or("").trim().to_string();
                b.format = wgsl_storage_format(&b.format_name);
                b.access = if arg.contains("read_write") { StorageTextureAccess::ReadWrite } else if arg.contains("read") { StorageTextureAccess::ReadOnly } else { StorageTextureAccess::WriteOnly };
            } else if let Some(d) = name.strip_prefix("depth_") {
                b.dim = view_dim(d.strip_prefix("multisampled_").unwrap_or(d));
                b.sample = Sample::Depth;
                b.ms = d.starts_with("multisampled");
            } else if name.starts_with("multisampled_") {
                b.dim = TextureViewDimension::D2;
                b.ms = true;
                b.sample = Sample::Unfilterable;
            } else {
                b.dim = view_dim(&name);
                b.sample = match arg.as_str() { "u32" => Sample::Uint, "i32" => Sample::Sint, _ => Sample::Float };
            }
        }
        bindings.push(b);
    }
    let mut outputs = Vec::new();
    if let Some(sig) = RE_FRAG_SIG.captures(text) {
        let r = sig[1].trim();
        if let Some(l) = RE_LOC.captures(r) {
            outputs.push(l[1].parse().unwrap_or(0));
        } else if let Ok(st) = Regex::new(&format!(r"struct\s+{}\s*\{{([^}}]*)\}}", regex::escape(r))) {
            if let Some(s) = st.captures(text) {
                for m in RE_LOC.captures_iter(&s[1]) {
                    outputs.push(m[1].parse().unwrap_or(0));
                }
            }
        }
    }
    // Conservative: any frag_depth token disables far-background retention,
    // including unused helpers or comments, rather than risking an override.
    Info { bindings, outputs, writes_fragment_depth: text.contains("frag_depth") }
}

#[derive(Clone)]
struct PackMember { binding: u32, size: u64, offset: u64 }
#[derive(Clone)]
struct Pack { members: Vec<PackMember>, size: u64 }

fn pack_uniforms(text: &str) -> (String, Option<Pack>) {
    struct M { binding: u32, name: String, ty: String, size: u64 }
    let mut members: Vec<M> = Vec::new();
    let mut group = 0;
    let out = RE_UNIFORM_DECL.replace_all(text, |c: &regex::Captures| {
        let b: u32 = c[2].parse().unwrap_or(0);
        if b == 15 { return c[0].to_string(); }
        group = c[1].parse().unwrap_or(0);
        let ty = c[4].to_string();
        let size = Regex::new(&format!(r"struct\s+{}\s*\{{([^}}]*)\}}", regex::escape(&ty)))
            .ok()
            .and_then(|st| st.captures(text).map(|s| s[1].to_string()))
            .and_then(|body| RE_ARRAY_N.captures(&body).map(|a| a[1].parse::<u64>().unwrap_or(1) * 16))
            .unwrap_or(16);
        members.push(M { binding: b, name: c[3].to_string(), ty, size });
        String::new()
    });
    if members.is_empty() {
        return (text.to_string(), None);
    }
    let mut out = out.into_owned();
    members.sort_by_key(|m| m.binding);
    let mut off = 0;
    let mut pm = Vec::new();
    for m in &members {
        pm.push(PackMember { binding: m.binding, size: m.size, offset: off });
        off += m.size;
    }
    for m in &members {
        let re = Regex::new(&format!(r"\b{}\b", regex::escape(&m.name))).unwrap();
        out = re.replace_all(&out, format!("cbpack_v.{}", m.name).as_str()).into_owned();
    }
    let st = format!(
        "struct cbpack_t {{\n{}\n}}\n@group({}u) @binding(0u) var<uniform> cbpack_v : cbpack_t;\n",
        members.iter().map(|m| format!("  {} : {},", m.name, m.ty)).collect::<Vec<_>>().join("\n"),
        group
    );
    let out = match RE_FIRST_FN.find(&out) {
        Some(at) => format!("{}\n{}{}", &out[..at.start()], st, &out[at.start()..]),
        None => out + &st,
    };
    (out, Some(Pack { members: pm, size: off }))
}

fn vs_output_struct(text: &str) -> Option<Regex> {
    let m = RE_VS_MAIN.captures(text)?;
    Regex::new(&format!(r"(struct\s+{}\s*\{{)([^}}]*)(\}})", regex::escape(&m[1]))).ok()
}
fn locations_in(text: &str, vs: bool) -> Vec<u32> {
    let body = if vs {
        vs_output_struct(text).and_then(|re| re.captures(text).map(|m| m[2].to_string())).unwrap_or_default()
    } else {
        RE_PS_MAIN.captures(text).map(|m| m[2].to_string()).unwrap_or_default()
    };
    RE_LOC.captures_iter(&body).filter_map(|x| x[1].parse().ok()).collect()
}
fn remap_locations(text: &str, vs: bool, map: &HashMap<u32, u32>) -> String {
    let swap = |s: &str| -> String {
        RE_LOC_U.replace_all(s, |c: &regex::Captures| {
            let n: u32 = c[1].parse().unwrap_or(0);
            match map.get(&n) { Some(t) => format!("@location({}{})", t, &c[2]), None => c[0].to_string() }
        }).into_owned()
    };
    let re = if vs { vs_output_struct(text) } else { Some(RE_PS_MAIN.clone()) };
    match re {
        Some(re) => re.replace(text, |c: &regex::Captures| format!("{}{}{}", &c[1], swap(&c[2]), &c[3])).into_owned(),
        None => text.to_string(),
    }
}
static RE_LOC_U: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@location\((\d+)(u?)\)").unwrap());

#[derive(Clone, Copy)]
struct TexDesc { w: u32, h: u32, d: u32, mips: u32, samples: u32, dim: TextureDimension }
struct Proxy {
    tex: Texture,
    trace_native_identity: Option<u64>,
    format: TextureFormat,
    dirty: bool,
    passes: Vec<(TextureView, BindGroup)>,
}

fn trace_native_texture(tag:Option<u64>,kind:&str)->serde_json::Value {
    tag.map(|tag|serde_json::json!({"kind":kind,"tag":tag,"scope":"this_process",
        "basis":"tag minted for returned wgpu Texture handle at allocation site; parent Eq where view reported",
        "identity_kind":"wgpu_texture_handle_lifetime","backend_allocation_validated":false,"backend_id":null}))
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(all(test,target_os="macos"))]
mod native_trace_evidence_tests {
    use super::*;
    fn gpu(enabled:bool)->Gpu {
        let mut config=wasmtime::Config::new(); config.wasm_threads(true); config.shared_memory(true);
        let engine=wasmtime::Engine::new(&config).unwrap();
        let mem=SharedMemory::new(&engine,wasmtime::MemoryType::shared(1,1)).unwrap();
        let mut gpu=Gpu::new(mem,0,65536,0,None).unwrap();
        gpu.command_trace=crate::world_command_trace::CommandTrace::fixture(enabled); gpu
    }
    fn texture(g:&mut Gpu,id:u32) { g.create_texture(id,2,1,1,1,1,1,28,BIND_RT|BIND_SRV,1); }

    #[test]
    fn allocation_tags_match_retained_parent_not_reused_guest_id_or_render_uid() {
        let mut g=gpu(true); let uid=g.next_uid; texture(&mut g,1);
        assert_eq!(g.next_uid,uid);
        let first=g.tex(1).unwrap().trace_native_identity.unwrap();
        let view=g.tex(1).unwrap().tex.create_view(&TextureViewDescriptor::default());
        let clone=view.clone();
        assert_eq!(g.command_trace_view_parent(1,&clone)["native_allocation"]["tag"],first);
        texture(&mut g,1);
        assert_ne!(g.tex(1).unwrap().trace_native_identity.unwrap(),first);
        assert_eq!(g.command_trace_view_parent(1,&view)["resolved"],false);
        assert_eq!(g.command_trace_view_parent(1,&view)["parent_eq"],false);
        g.create_buffer(7,16,1,0,4);
        let buffer=g.command_trace_resource(7);
        assert_eq!(buffer["native_allocation"]["identity_kind"],"wgpu_buffer_handle_lifetime");
        assert_eq!(buffer["native_allocation"]["backend_allocation_validated"],false);
        assert_eq!(buffer["native_allocation"]["tag"],g.buf(7).unwrap().identity);
        let mut off=gpu(false); let uid=off.next_uid; texture(&mut off,1);
        assert!(off.tex(1).unwrap().trace_native_identity.is_none()); assert_eq!(off.next_uid,uid);
    }

    #[test]
    fn proxy_allocation_is_distinct_and_parent_eq_identifies_it() {
        let mut g=gpu(true); texture(&mut g,1);
        g.float_to_depth_proxy(1).unwrap();
        let texture=g.tex(1).unwrap(); let proxy=texture.proxy_fdepth.as_ref().unwrap();
        let view=proxy.tex.create_view(&TextureViewDescriptor::default());
        assert_ne!(texture.trace_native_identity,proxy.trace_native_identity);
        let evidence=g.command_trace_view_parent(1,&view);
        assert_eq!(evidence["native_allocation"]["kind"],"texture_proxy");
        assert_eq!(evidence["native_allocation"]["tag"],proxy.trace_native_identity.unwrap());
        g.flush();
    }

    #[test]
    fn retained_pass_uses_its_key_and_pending_clear_enumeration_is_bounded() {
        let mut g=gpu(true); texture(&mut g,1); texture(&mut g,2);
        g.s.rt[0]=Rt{tex:1,mip:0,slice:0,fmt:28}; assert!(g.ensure_pass());
        let tag=g.tex(1).unwrap().trace_native_identity.unwrap();
        g.s.rt[0].tex=2;
        let native=g.command_trace_native_context();
        assert_eq!(native["attachments"][0]["guest_id"],1);
        assert_eq!(native["attachments"][0]["retained_parent"]["native_allocation"]["tag"],tag);
        for i in 0..70 { g.pending_clears.entry(9).or_default().push((0,0,PendingClear{color:None,depth:Some(i as f32),stencil:Some(i)})); }
        let native=g.command_trace_native_context();
        assert_eq!(native["pending_clears_observed"],70);
        assert_eq!(native["pending_clears_omitted"],6);
        assert_eq!(native["pending_clears"].as_array().unwrap().len(),64);
        assert_eq!(native["pending_clears"][63]["queue_index"],63);
        assert_eq!(native["pending_clears"][0]["current_target_native_allocation"],serde_json::Value::Null);
        assert_eq!(g.pending_clears[&9].len(),70);
        assert_eq!(g.s.rt[0].tex,2);
        g.pending_clears.remove(&9); g.flush();
    }
}
struct Tex {
    tex: Texture,
    trace_native_identity: Option<u64>,
    desc: TexDesc,
    format: TextureFormat,
    // Original compressed format when the adapter uses decoded storage.
    bc_source: Option<TextureFormat>,
    views: HashMap<(TextureFormat, u32, u32), TextureView>,
    a8: bool,
    enc: u64,
    in_bg: bool,
    proxy_depth: Option<Proxy>,
    proxy_stencil: Option<Proxy>,
    proxy_fdepth: Option<Proxy>,
    blit_bg: Option<BindGroup>,
    mip_passes: Option<Vec<(TextureView, BindGroup)>>,
}
struct Buf {
    identity: u64,
    size: u64,
    stride: u32,
    gpu: Option<Buffer>,
    bytes: u64,
    shadow: Option<Vec<u8>>,
    version: u64,
    enc: u64,
    in_bg: bool,
    counter: Option<Buffer>,
    dedup: crate::upload_dedup::Track,
}
#[derive(Clone, Copy)]
struct SigEl { sem: u32, idx: u32, reg: u32 }
struct ShaderObj {
    stage: u32,
    hash: String,
    sig: Vec<SigEl>,
    program: Option<Option<Rc<Program>>>,
}
struct Program {
    hud_kind: Option<crate::xr_hud::ShaderKind>,
    module: ShaderModule,
    info: Info,
    consts: Option<Buffer>,
    pack: Option<Pack>,
    text: String,
    flat: Vec<u32>,
    group_bindings: BTreeMap<u32, Vec<Binding>>,
    variants: RefCell<HashMap<String, Option<ShaderModule>>>,
    label: String,
    uid: u64,
    vid: bool,
    iid: bool,
    out_locs: Vec<u32>,
    in_locs: Vec<u32>,
    clip: bool,
}
#[derive(Clone, Copy)]
struct El { sem: u32, idx: u32, fmt: u32, slot: u32, offset: u32, cls: u32 }
struct LayoutObj { els: Vec<El>, slots: Vec<(u32, u32)> }
struct StateObj { words: Vec<u32>, sampler: Option<Sampler> }
struct ViewObj {
    res: u32,
    is_buf: bool,
    fmt: u32,
    vdim: u32,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    views: HashMap<String, TextureView>,
}
/// Lifetime of one guest query's result record. Queued completions hold this
/// token, never an object id or a raw address, so work left over from a query
/// that was destroyed or replaced can neither write the old record nor be
/// redirected to a newer query that reuses the id.
struct QueryLife(Mutex<Option<u64>>);
impl QueryLife {
    fn new(addr: u64) -> Arc<Self> { Arc::new(QueryLife(Mutex::new(Some(addr)))) }
    fn record(&self) -> std::sync::MutexGuard<'_, Option<u64>> { self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
    fn is_live(&self) -> bool { self.record().is_some() }
    /// Ends the lifetime. Returns only after any completion in progress has finished.
    fn retire(&self) { *self.record() = None; }
    /// Publishes a result to the guest unless the query is gone. The lock is
    /// held across the liveness check and every store, so a completion on
    /// another thread cannot write after `retire` has returned.
    fn complete(&self, mem: &SharedMemory, result: u64, seq: u32) -> bool {
        let record = self.record();
        let Some(addr) = *record else { return false };
        atomic(mem, addr + 8).store(result as u32 as i32, Ordering::SeqCst);
        atomic(mem, addr + 12).store((result >> 32) as u32 as i32, Ordering::SeqCst);
        atomic(mem, addr).store(seq as i32, Ordering::SeqCst);
        let _ = mem.atomic_notify(addr, u32::MAX);
        true
    }
}
struct QueryObj { qtype: u32, life: Arc<QueryLife>, slots: Vec<u32> }
// Destroying or replacing the object in any way ends its lifetime.
impl Drop for QueryObj {
    fn drop(&mut self) { self.life.retire(); }
}

enum Obj {
    Tex(Tex),
    Buf(Buf),
    Shader(ShaderObj),
    Layout(Rc<LayoutObj>),
    State(Rc<StateObj>),
    Srv(ViewObj),
    Uav(ViewObj),
    Query(QueryObj),
}

impl Obj {
    // stage_bind_group can observe these objects directly, or reach a Tex/Buf
    // through a view whose guest ID has not changed. Non-sampler states do not
    // supply a native sampler; shader/layout identity is in the program/BGL key.
    fn bind_group_relevant(&self) -> bool {
        match self {
            Self::Tex(_) | Self::Buf(_) | Self::Srv(_) | Self::Uav(_) => true,
            Self::State(state) => state.sampler.is_some(),
            Self::Shader(_) | Self::Layout(_) | Self::Query(_) => false,
        }
    }

    fn nonbinding_kind(&self) -> Option<NonBindingKind> {
        match self {
            Self::Query(_) => Some(NonBindingKind::Query),
            Self::Layout(_) => Some(NonBindingKind::Layout),
            Self::Shader(_) => Some(NonBindingKind::Shader),
            Self::State(state) if state.sampler.is_none() => Some(NonBindingKind::State),
            _ => None,
        }
    }
}

fn binding_cache_needs_invalidation(old: Option<&Obj>, new: Option<&Obj>) -> bool {
    old.is_some_and(Obj::bind_group_relevant) || new.is_some_and(Obj::bind_group_relevant)
}

#[derive(Clone, Default)]
struct Vb { buf: u32, stride: u32, offset: u32 }
#[derive(Clone, Default)]
struct Cb { buf: u32, first: u32 }
#[derive(Clone, Default, PartialEq)]
struct Rt { tex: u32, mip: u32, slice: u32, fmt: u32 }
#[derive(Clone, Default, PartialEq)]
struct Ds { tex: u32, mip: u32, slice: u32, fmt: u32, flags: u32 }

struct State {
    shader: [u32; 3],
    layout: u32,
    topo: u32,
    vb: Vec<Vb>,
    ib: (u32, u32, u32),
    cb: [Vec<Cb>; 3],
    srv: [Vec<u32>; 3],
    uav: [Vec<u32>; 3],
    smp: [Vec<u32>; 3],
    rt: Vec<Rt>,
    ds: Ds,
    blend: (u32, [f64; 4]),
    dss: (u32, u32),
    rs: u32,
    viewports: Vec<[f32; 6]>,
    scissors: Vec<[u32; 4]>,
}
impl State {
    fn new() -> Self {
        State {
            shader: [0; 3], layout: 0, topo: 4,
            vb: vec![Vb::default(); 16], ib: (0, 0, 0),
            cb: std::array::from_fn(|_| vec![Cb::default(); 16]),
            srv: std::array::from_fn(|_| vec![0; 128]),
            uav: std::array::from_fn(|_| vec![0; 8]),
            smp: std::array::from_fn(|_| vec![0; 16]),
            rt: vec![Rt::default(); 8], ds: Ds::default(),
            blend: (0, [1.0; 4]), dss: (0, 0), rs: 0,
            viewports: vec![], scissors: vec![],
        }
    }
}

#[derive(Clone)]
struct PassInfo {
    rt_formats: Vec<Option<TextureFormat>>,
    samples: u32,
    ds_format: Option<TextureFormat>,
    ro_depth: bool,
    ro_stencil: bool,
    width: u32,
    height: u32,
}
#[derive(Default)]
struct PassSt {
    pl: Option<u64>,
    bg: [Option<(u64, u32)>; 2],
    vb: Vec<Option<(u64, u64)>>,
    ib: Option<(u64, IndexFormat, u64)>,
    vp: Option<[f32; 6]>,
    sc: Option<[u32; 4]>,
    bf: Option<[f64; 4]>,
    sref: Option<u32>,
}

struct Pl {
    pipeline: RenderPipeline,
    uid: u64,
    vb_req: Vec<VbReq>,
    label: String,
}

struct PipeJob {
    label: String,
    layout: PipelineLayout,
    vmod: ShaderModule,
    fmod: Option<ShaderModule>,
    vbs: Vec<Option<(u64, VertexStepMode, Vec<VertexAttribute>)>>,
    primitive: PrimitiveState,
    depth_stencil: Option<DepthStencilState>,
    multisample: MultisampleState,
    targets: Vec<Option<ColorTargetState>>,
}
enum PipeEntry { Pending, Done(Option<Rc<Pl>>) }

fn compile_pipeline(device: &Device, job: &PipeJob) -> Option<RenderPipeline> {
    let vbs: Vec<Option<VertexBufferLayout>> = job.vbs.iter().map(|v| v.as_ref().map(|(stride, step, attrs)| VertexBufferLayout { array_stride: *stride, step_mode: *step, attributes: attrs })).collect();
    let desc = RenderPipelineDescriptor {
        label: Some(&job.label),
        layout: Some(&job.layout),
        vertex: VertexState { module: &job.vmod, entry_point: Some("main"), compilation_options: Default::default(), buffers: &vbs },
        primitive: job.primitive,
        depth_stencil: job.depth_stencil.clone(),
        multisample: job.multisample,
        fragment: job.fmod.as_ref().map(|m| FragmentState { module: m, entry_point: Some("main"), compilation_options: Default::default(), targets: &job.targets }),
        multiview_mask: None,
        cache: None,
    };
    let scope = device.push_error_scope(ErrorFilter::Validation);
    let p = device.create_render_pipeline(&desc);
    match pollster::block_on(scope.pop()) {
        None => Some(p),
        Some(e) => {
            let msg = error_chain(&e);
            log_once(&format!("pipeline:{}", &msg[..msg.len().min(160)]), &format!("pipeline {} failed: {}", job.label, &msg[..msg.len().min(1500)]));
            None
        }
    }
}

#[derive(Clone, Copy)]
struct VbReq { slot: u32, instance: bool, stride: u64, last: u64 }

#[derive(Clone, Copy, Default)]
struct PendingClear { color: Option<Color>, depth: Option<f32>, stencil: Option<u32> }

struct Chunk { buf: Buffer, id: u64, base: u64, stage: Vec<u8>, lo: u64, hi: u64 }
struct BgResult { bg: BindGroup, uid: u64, dyn_off: Option<u32> }

struct PendingMap {
    buf: Buffer,
    done: Arc<AtomicBool>,
    kind: MapKind,
}
enum MapKind {
    Readback { flag: u64, rows: Vec<(u64, u64, u64)> },
    Occlusion { list: Vec<(u64, u32, Arc<QueryLife>, bool, usize)> },
}

pub struct HudCapture {
    final_id:u32, final_texture:Texture, view:crate::xr_view::WorldView,
    images:Option<Arc<crate::xr_hud::HudImages>>, draws:u64,
    scratch:HashMap<u32,Texture>, rejected:Option<String>,
}

struct Gpu {
    mem: SharedMemory,
    base: *mut u8,
    ring: u64,
    device: Device,
    queue: Queue,
    adapter: Adapter,
    surface: Option<(Surface<'static>, SurfaceWindow)>,
    surface_format: TextureFormat,
    surface_size: (u32, u32),
    objs: Vec<Option<Obj>>,
    enc: Option<CommandEncoder>,
    enc_epoch: u64,
    pass: Option<RenderPass<'static>>,
    pass_key: Option<PassKey>,
    pass_info: Option<PassInfo>,
    pass_st: PassSt,
    s: State,
    world_view: Option<crate::xr_view::WorldView>,
    world_depths: HashMap<u32, (crate::xr_view::WorldView, u64)>,
    world_depth_before_ui: HashMap<u32, (crate::xr_view::WorldView, crate::world_frame::FrozenPreUiDepth, u64)>,
    world_depth_snapshot_overflow: bool,
    world_depth_snapshots_this_present: u8,
    world_depth_trace: HashMap<(u32, u64, bool, u32, bool), (u64, [f32; 6])>,
    world_depth_anomalies: Vec<serde_json::Value>,
    color_phase_trace: crate::world_ui::ColorPhaseTrace,
    hud_capture:Option<HudCapture>, hud_draw:Option<crate::xr_hud::HudBlend>, hud_pool:Vec<Arc<crate::xr_hud::HudImages>>,
    // Producer-attributed first-person foreground (default off; see fg).
    foreground: Option<fg::ForegroundMark>,
    fp_sessions: fg::logic::Sessions,
    fp_images: HashMap<u32, ForegroundImages>,
    fp_pool: Vec<Texture>,
    fp_merge: Option<(RenderPipeline, BindGroupLayout)>,
    foreground_capture: Option<ForegroundCapture>,
    command_trace: crate::world_command_trace::CommandTrace,
    pdirty: bool,
    last_pl: Option<Rc<Pl>>,
    last_program_uids: Option<(u64, u64)>,
    float_programs: HashMap<(u64, Vec<u32>), Option<Rc<Program>>>,
    shader_index: HashMap<String, serde_json::Value>,
    shader_dir: PathBuf,
    next_uid: u64,
    bgl_cache: HashMap<(String, u32, u32), (BindGroupLayout, u64)>,
    numeric_bgl_cache: HashMap<(u64, u32, u32), (BindGroupLayout, u64)>,
    render_prepare: RenderPrepare,
    uniform_pack: UniformPack,
    uniform_snapshot: UniformSnapshot,
    bind_group_prepare: BindGroupPrepare,
    empty_bgl: Option<(BindGroupLayout, u64)>,
    pipelines: HashMap<Vec<i64>, PipeEntry>,
    pipe_jobs: Option<std::sync::mpsc::Sender<(u64, PipeJob)>>,
    pipe_results: std::sync::mpsc::Receiver<(u64, Option<RenderPipeline>)>,
    pipe_pending: HashMap<u64, (Vec<i64>, Vec<VbReq>, String)>,
    pending_skips: u64,
    pending_skips0: u64,
    compute: HashMap<String, Option<(ComputePipeline, BindGroupLayout, u64, Rc<Program>)>>,
    bind_groups: HashMap<Vec<u64>, (BindGroup, u64)>,
    ubo_slots: Vec<Vec<Chunk>>,
    ubo_shared: Vec<(Buffer, u64)>,
    ubo_slot: usize,
    ubo_chunk: usize,
    ubo_off: u64,
    dummy_tex: HashMap<String, TextureView>,
    dummy_smp: Option<Sampler>,
    dummy_cmp: Option<Sampler>,
    dummy_bufs: HashMap<u32, Buffer>,
    zero_vb: Option<Buffer>,
    zero_src: Option<Buffer>,
    scratch: Option<Buffer>,
    proxy_pipelines: HashMap<&'static str, RenderPipeline>,
    f2d_pipeline: Option<RenderPipeline>,
    blit: Option<RenderPipeline>,
    mip_pipelines: HashMap<TextureFormat, RenderPipeline>,
    mip_sampler: Option<Sampler>,
    occlusion_set: Option<QuerySet>,
    occlusion_next: u32,
    occlusion_active: Option<u32>,
    occlusion_pass_slot: Option<u32>,
    pending_resolve: Vec<(Arc<QueryLife>, u32, Vec<u32>)>,
    pending_events: Vec<(Arc<QueryLife>, u32, u64)>,
    pending_maps: Vec<PendingMap>,
    counter_src: Option<Buffer>,
    counter_slot: u32,
    frame_slot_done: Vec<Option<SubmissionIndex>>,
    last_submit: Option<SubmissionIndex>,
    vb_avail: [u64; 16],
    dbg_idx: u64,
    geometry_stereo_capture: bool,
    geometry_stereo_capture_done: bool,
    geometry_capture_diagnostics: GeometryCaptureDiagnostics,

    draws: u64,
    skipped: u64,
    presents: u64,
    pipe_creates: u64,
    fps_t0: Instant,
    fps_n0: u64,
    fps_d0: u64,
    buffer_uploads: [BufferUploadStats; UPLOAD_SOURCE_COUNT],
    uploads: crate::buffer_upload::BufferUploads,
    upload_dedup: crate::upload_dedup::UploadDedup,
    indirect_reads: IndirectReadStats,
    thread_wall: [ApiWallStats; THREAD_TIMING_COUNT],
    errors: Arc<AtomicU64>,

    inflight: Arc<AtomicU64>,
    instance: Instance,

    pending_clears: HashMap<u32, Vec<(u32, u32, PendingClear)>>,
    clears_folded: u64,
    submits: u64,
    submits0: u64,
    passes: u64,
    passes0: u64,
}

unsafe impl Send for Gpu {}

pub fn max_msaa() -> u32 {
    let mut d = InstanceDescriptor::new_without_display_handle();
    d.backends = Backends::METAL;
    let Ok(adapter) = pollster::block_on(Instance::new(d).request_adapter(&RequestAdapterOptions::default())) else { return 4 };
    let formats = [TextureFormat::Rgba8Unorm, TextureFormat::Rgba16Float, TextureFormat::Rg11b10Ufloat, TextureFormat::Depth32FloatStencil8];
    [16, 8, 4, 2]
        .into_iter()
        .find(|&n| formats.iter().all(|f| adapter.get_texture_format_features(*f).flags.sample_count_supported(n)))
        .unwrap_or(1)
}

impl Gpu {
    fn new(mem: SharedMemory, ring: u64, _size: u64, dbg: u64, handoff: Option<SurfaceHandoff>) -> anyhow::Result<Gpu> {
        let (base, _) = mem_base_len(&mem);
        let (instance, surface) = match handoff {
            Some(h) => (h.instance, Some((h.surface, h.window))),
            None => { let mut d = InstanceDescriptor::new_without_display_handle(); d.backends = Backends::METAL; (Instance::new(d), None) }
        };
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            power_preference: PowerPreference::HighPerformance,
            compatible_surface: surface.as_ref().map(|s| &s.0),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))?;
        let want = Features::TEXTURE_COMPRESSION_BC | Features::TEXTURE_COMPRESSION_BC_SLICED_3D | Features::DEPTH32FLOAT_STENCIL8
            | Features::FLOAT32_FILTERABLE | Features::INDIRECT_FIRST_INSTANCE | Features::RG11B10UFLOAT_RENDERABLE
            | Features::BGRA8UNORM_STORAGE | Features::DEPTH_CLIP_CONTROL | Features::CLIP_DISTANCES | Features::DUAL_SOURCE_BLENDING

            | Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let features = want & adapter.features();
        #[cfg(test)]
        let features = features.difference(TEST_WITHHELD_FEATURES.with(|withheld| withheld.get()));
        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
            label: Some("muguet"),
            required_features: features,
            required_limits: adapter.limits(),
            ..Default::default()
        }))?;
        let errors = Arc::new(AtomicU64::new(0));
        {
            let errors = errors.clone();
            let mem2 = mem.clone();
            let kinds: Mutex<HashMap<String, u64>> = Mutex::new(HashMap::new());
            device.on_uncaptured_error(Arc::new(move |e: Error| {
                errors.fetch_add(1, Ordering::Relaxed);
                atomic(&mem2, ring + 16).fetch_add(1, Ordering::Relaxed);
                let msg = error_chain(&e);
                let key: String = msg.chars().filter(|c| !c.is_ascii_digit()).take(200).collect();
                let mut k = kinds.lock().unwrap();
                let n = k.entry(key).or_insert(0);
                *n += 1;
                if *n == 1 && k.len() <= 60 {
                    log(&format!("validation error: {}", &msg[..msg.len().min(900)]));
                }
            }));
        }
        let info = adapter.get_info();
        log(&format!("device ready: {} ({:?}), features {:?}", info.name, info.backend, features));
        let surface_format = surface
            .as_ref()
            .map(|(s, _)| {
                let caps = s.get_capabilities(&adapter);
                caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0])
            })
            .unwrap_or(TextureFormat::Bgra8Unorm);
        let shader_dir = PathBuf::from(std::env::var("MUGUET_SHADERS").unwrap_or_default());
        let index_text = std::fs::read_to_string(shader_dir.join("index.json")).unwrap_or_else(|e| {
            log(&format!("no shader index at {}: {e}", shader_dir.display()));
            "{}".into()
        });
        let shader_index: HashMap<String, serde_json::Value> = serde_json::from_str(&index_text).unwrap_or_default();
        log(&format!("shader index: {} entries from {}", shader_index.len(), shader_dir.display()));

        let (res_tx, pipe_results) = std::sync::mpsc::channel::<(u64, Option<RenderPipeline>)>();
        let pipe_jobs = {
            let (job_tx, job_rx) = std::sync::mpsc::channel::<(u64, PipeJob)>();
            let job_rx = Arc::new(Mutex::new(job_rx));
            for i in 0..4 {
                let (dev, rx, tx) = (device.clone(), job_rx.clone(), res_tx.clone());
                std::thread::Builder::new().name(format!("pipeline-compile-{i}")).spawn(move || loop {
                    let job = rx.lock().unwrap().recv();
                    let Ok((id, job)) = job else { return };
                    let p = compile_pipeline(&dev, &job);
                    if tx.send((id, p)).is_err() { return; }
                }).expect("spawn compile thread");
            }
            Some(job_tx)
        };
        let render_prepare = RenderPrepare::from_environment();
        log(&format!("render prepare: {}", render_prepare.metrics(0, 0, 0, 0, 0)));
        let uniform_snapshot = UniformSnapshot::from_environment();
        log(&format!("uniform snapshot cache: {}", uniform_snapshot.metrics()));
        let uniform_pack = UniformPack::from_environment();
        log(&format!("uniform stage pack: {}", uniform_pack.metrics(0, 0, 0, 0)));
        let bind_group_prepare = BindGroupPrepare::from_environment();
        log(&format!("bind group prepare: {}", bind_group_prepare.metrics()));
        let upload_dedup = crate::upload_dedup::UploadDedup::from_environment();
        log(&format!("op47 upload dedup: {}", upload_dedup.metrics(0.0)));
        Ok(Gpu {
            instance: instance.clone(), pending_clears: HashMap::new(), clears_folded: 0, pipe_jobs, pipe_results, pipe_pending: HashMap::new(), pending_skips: 0, pending_skips0: 0,
            base, mem, ring, device, queue, adapter, surface, surface_format, surface_size: (0, 0),
            objs: Vec::new(), enc: None, enc_epoch: 1, pass: None, pass_key: None, pass_info: None, pass_st: PassSt::default(),
            s: State::new(), world_view: None, world_depths: HashMap::new(), world_depth_before_ui: HashMap::new(), world_depth_snapshot_overflow: false, world_depth_snapshots_this_present: 0, world_depth_trace: HashMap::new(), world_depth_anomalies: Vec::new(),
            color_phase_trace: crate::world_ui::ColorPhaseTrace::from_env(), hud_capture:None, hud_draw:None, hud_pool:Vec::new(),
            foreground: None, fp_sessions: Default::default(), fp_images: HashMap::new(), fp_pool: Vec::new(), fp_merge: None,
            foreground_capture: ForegroundCapture::from_env(),
            command_trace: crate::world_command_trace::CommandTrace::from_env(), pdirty: true, last_pl: None, last_program_uids: None,
            float_programs: HashMap::new(), shader_index, shader_dir, next_uid: 1,
            bgl_cache: HashMap::new(), empty_bgl: None, pipelines: HashMap::new(), compute: HashMap::new(), bind_groups: HashMap::new(),
            numeric_bgl_cache: HashMap::new(), render_prepare, uniform_pack, uniform_snapshot, bind_group_prepare,
            ubo_slots: (0..FRAME_SLOTS).map(|_| Vec::new()).collect(), ubo_shared: Vec::new(), ubo_slot: 0, ubo_chunk: 0, ubo_off: 0,
            dummy_tex: HashMap::new(), dummy_smp: None, dummy_cmp: None, dummy_bufs: HashMap::new(), zero_vb: None, zero_src: None, scratch: None,
            proxy_pipelines: HashMap::new(), f2d_pipeline: None, blit: None, mip_pipelines: HashMap::new(), mip_sampler: None,
            occlusion_set: None, occlusion_next: 0, occlusion_active: None, occlusion_pass_slot: None,
            pending_resolve: Vec::new(), pending_events: Vec::new(), pending_maps: Vec::new(), counter_src: None, counter_slot: 0,
            frame_slot_done: (0..FRAME_SLOTS).map(|_| None).collect(), last_submit: None, vb_avail: [0; 16],
            dbg_idx: if dbg != 0 { dbg + 424 } else { 0 },
            geometry_stereo_capture: std::env::var("MUGUET_GEOMETRY_STEREO_CAPTURE").as_deref() == Ok("1"),
            geometry_stereo_capture_done: false,
            geometry_capture_diagnostics: GeometryCaptureDiagnostics::default(),
            draws: 0, skipped: 0, presents: 0, pipe_creates: 0, fps_t0: Instant::now(), fps_n0: 0, fps_d0: 0,
            buffer_uploads: [BufferUploadStats::default(); UPLOAD_SOURCE_COUNT], indirect_reads: IndirectReadStats::default(),
            uploads: crate::buffer_upload::BufferUploads::from_environment(), upload_dedup,
            thread_wall: [ApiWallStats::default(); THREAD_TIMING_COUNT],
            errors, inflight: Arc::new(AtomicU64::new(0)), submits: 0, submits0: 0, passes: 0, passes0: 0,
        })
    }

    #[inline]
    fn u(&self, w: u64) -> u32 {
        unsafe { std::ptr::read_volatile(self.base.add((w * 4) as usize) as *const u32) }
    }
    #[inline]
    fn f(&self, w: u64) -> f32 {
        f32::from_bits(self.u(w))
    }
    fn bytes(&self, addr: u64, n: u64) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.base.add(addr as usize), n as usize) }
    }
    fn hdr(&self, w: u64) -> &AtomicI32 {
        atomic(&self.mem, self.ring + 4 * w)
    }
    fn uid(&mut self) -> u64 {
        // Resource identities must never alias after integer wrap.
        self.next_uid = self.next_uid.checked_add(1).expect("GPU resource identity exhausted");
        self.next_uid
    }

    fn invalidate_world_depth(&mut self, id: u32, reason: serde_json::Value) {
        self.invalidate_world_depth_inner(id, reason, WorldDepthChange::Other);
    }

    fn invalidate_world_depth_inner(&mut self, id: u32, reason: serde_json::Value, change: WorldDepthChange) {
        // Every caller runs before its mutation is encoded: an open foreground
        // overlay snapshots its actual post-write depth first.
        self.foreground_foreign_write(id, "depth_target_mutation");
        if self.foreground_capture_live() && (self.fp_sessions.overlay(id).is_some() || self.tex(id).is_some_and(|t| is_depth(t.format))) {
            self.foreground_capture_event(serde_json::json!({"event":"depth_mutation","target":id,"reason":reason.clone(),"draw":self.draws}));
        }
        let previous = invalidate_world_depth_records(&mut self.world_depths,
            &mut self.world_depth_before_ui, id, change);
        if let Some((view, draws)) = previous {
            if self.world_depth_anomalies.len() >= 8 { self.world_depth_anomalies.remove(0); }
            self.world_depth_anomalies.push(serde_json::json!({"depth":id,"invalidation":reason,
                "previous_sequence":view.camera_sequence,"previous_draws":draws,
                "ui_snapshot_retained":change.retains_frozen_snapshot() && self.world_depth_before_ui.contains_key(&id)}));
        }
    }

    fn save_world_depth_before_ui(&mut self, id: u32) {
        let Some((view, draws)) = self.world_depths.get(&id).copied() else { return };
        if self.world_depth_snapshots_this_present >= 4
            || (self.world_depth_before_ui.len() >= 2 && !self.world_depth_before_ui.contains_key(&id)) {
            self.world_depth_snapshot_overflow = true; return;
        }
        // End all scene writes and preserve real raster depth before the
        // clip-space UI clear. The reusable depth proxy is never leased here.
        self.end_pass();
        let Some((depth, _)) = self.depth_proxy(id, false) else { return };
        let snapshot = self.device.create_texture(&TextureDescriptor {
            label: Some("Qualified GTA scene depth before UI clear"),
            size: Extent3d { width: depth.width(), height: depth.height(), depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2,
            format: TextureFormat::R32Float,
            usage: TextureUsages::COPY_DST | TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING,
            view_formats: &[] });
        self.end_pass();
        self.encoder().copy_texture_to_texture(
            TexelCopyTextureInfo { texture: &depth, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
            TexelCopyTextureInfo { texture: &snapshot, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
            Extent3d { width: depth.width(), height: depth.height(), depth_or_array_layers: 1 });
        self.world_depth_snapshots_this_present += 1;
        self.world_depth_before_ui.insert(id, (view,
            crate::world_frame::FrozenPreUiDepth::new_snapshot(snapshot), draws));
        self.foreground_capture_event(serde_json::json!({"event":"world_depth_frozen_before_ui_clear","target":id,
            "camera_sequence":view.camera_sequence,"world_draws":draws,"draw":self.draws}));
        log_once("world-depth-before-ui", "qualified real scene depth copied before GTA's clip-space UI clear; final color must retain the same camera rays");
    }

    fn hud_reject(&mut self,reason:&str) {
        if let Some(h)=&mut self.hud_capture { h.rejected.get_or_insert_with(||reason.to_owned()); }
    }

    /// Execute before every mutation opcode, irrespective of tracing flags.
    /// IDs are resolved against retained native texture handles at draws/present;
    /// replacing a guest ID cannot preserve a candidate by numerical coincidence.
    fn hud_non_draw_command(&mut self,op:u32,p:u64,_n:u64) {
        if self.hud_capture.is_none(){return;}
        let u=|i|self.u(p+i);
        let mut writes=Vec::new();let mut reads=Vec::new();
        match op {
            3|4|5|6|13=>writes.push(u(0)),
            34=>{writes.push(u(0));reads.push(u(5));},
            35=>{writes.push(u(0));reads.push(u(1));},
            36=>{writes.push(u(0));reads.push(u(2));},
            42=>reads.push(u(0)),
            40|41=>{let view=u(0);if let Some(v)=self.view_obj(view){writes.push(v.res);reads.push(v.res);}},
            39=>{for &id in &self.s.uav[2] {if let Some(v)=self.view_obj(id){writes.push(v.res);reads.push(v.res);}}
                for &id in &self.s.srv[2]{if let Some(v)=self.view_obj(id){reads.push(v.res);}}},
            0..=47=>{},
            _=>self.hud_reject("unknown GPU opcode after final postFX"),
        }
        if let Some(h)=&mut self.hud_capture {
            for id in writes { h.scratch.remove(&id);if id==h.final_id {h.rejected.get_or_insert_with(||format!("opcode{op} mutates final HUD target"));} }
            if h.images.is_some() && reads.contains(&h.final_id){h.rejected.get_or_insert_with(||format!("opcode{op} reads HUD-bearing final target"));}
        }
    }

    fn reserve_hud_images(&mut self,source:&Texture)->Option<Arc<crate::xr_hud::HudImages>> {
        self.hud_pool.retain(|images|Arc::strong_count(images)>1 || (images.world.width()==source.width()
            && images.world.height()==source.height() && images.world.format()==source.format()));
        if let Some(images)=self.hud_pool.iter().find(|images|Arc::strong_count(images)==1) {return Some(images.clone());}
        if self.hud_pool.len()>=6{return None;}
        // Bound both the live pool and pixel allocation before allocating.
        if source.width() as u64*source.height() as u64>16_777_216{return None;}
        let make=|format,label|self.device.create_texture(&TextureDescriptor{label:Some(label),size:Extent3d{width:source.width(),height:source.height(),depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:TextureDimension::D2,format,usage:TextureUsages::COPY_SRC|TextureUsages::COPY_DST|TextureUsages::TEXTURE_BINDING|TextureUsages::RENDER_ATTACHMENT,view_formats:&[]});
        let images=Arc::new(crate::xr_hud::HudImages{world:make(source.format(),"HUD-free final world color"),contribution:make(TextureFormat::Rgba16Float,"Actual HUD premultiplied contribution and coverage")});
        self.hud_pool.push(images.clone());Some(images)
    }

    fn prepare_hud_draw(&mut self,vs:&Program,ps:Option<&Program>)->Option<crate::xr_hud::HudBlend> {
        use crate::xr_hud::ShaderKind as K;
        let Some(capture)=self.hud_capture.as_ref()else{return None;};
        if capture.rejected.is_some(){return None;}
        if !crate::xr_hud::enabled(){self.hud_reject("HUD frontend inactive");return None;}
        if self.s.rt.iter().skip(1).any(|r|r.tex==capture.final_id
            || self.tex(r.tex).is_some_and(|t|t.tex==capture.final_texture)) {
            self.hud_reject("final target bound outside sole attachment zero");return None;
        }
        let secondary_targets:Vec<u32>=self.s.rt.iter().skip(1).filter(|r|r.tex!=0).map(|r|r.tex).collect();
        if let Some(c)=&mut self.hud_capture{for id in secondary_targets{c.scratch.remove(&id);}}
        let capture=self.hud_capture.as_ref().unwrap();
        let graphics_uav_resources:Vec<u32>=self.s.uav[..2].iter().flatten().filter_map(|id|self.view_obj(*id).map(|v|v.res)).collect();
        if graphics_uav_resources.iter().any(|id|*id==capture.final_id
            || self.tex(*id).is_some_and(|t|t.tex==capture.final_texture)) {
            self.hud_reject("graphics UAV aliases final target");return None;
        }
        if !graphics_uav_resources.is_empty(){if let Some(c)=&mut self.hud_capture{for id in &graphics_uav_resources{c.scratch.remove(id);}}}
        let capture=self.hud_capture.as_ref().unwrap();
        if capture.images.is_some() {
            let reads_final=[(0,Some(vs)),(1,ps)].into_iter().any(|(stage,program)|program.is_some_and(|program|
                program.info.bindings.iter().any(|b|matches!(b.kind,Kind::Texture|Kind::StorageTexture|Kind::Storage|Kind::ReadOnlyStorage)
                  && b.binding>=32 && self.s.srv[stage].get((b.binding-32)as usize).and_then(|id|self.view_obj(*id))
                    .is_some_and(|v|v.res==capture.final_id))));
            if reads_final{self.hud_reject("draw reads HUD-bearing final target");return None;}
        }
        let id=self.s.rt[0].tex;
        let final_target=id==capture.final_id;
        let valid_final=final_target && self.tex(id).is_some_and(|t|t.tex==capture.final_texture);
        let pair=(vs.hud_kind,ps.and_then(|p|p.hud_kind));
        let one_target=self.s.rt.iter().skip(1).all(|r|r.tex==0)
            && self.s.rt[0].mip==0 && self.s.rt[0].slice==0;
        let (blend,mask)=blend_for(self.state(self.s.blend.0).as_deref(),0);
        let writes_depth=self.s.ds.flags&1==0 && self.state(self.s.dss.0).is_some_and(|d|d.words.first()==Some(&1)&&d.words.get(1)==Some(&1));
        if !final_target {
            // The minimap scratch has an original full clear and a closed set of
            // source-pinned UI/map writers. Its final blit alpha is used exactly
            // as that original fragment emits it; raw scratch alpha is not a
            // claimed coverage mask (notably PS_BlitDepthToAlpha).
            let clear=pair==(Some(K::ClearVertex),Some(K::ClearPixel)) && one_target
                && blend.is_none() && mask==ColorWrites::ALL && self.tex(id).is_some_and(|t|{
                    let vp=self.s.viewports.first().copied().unwrap_or([0.,0.,t.desc.w as f32,t.desc.h as f32,0.,1.]);
                    let sc=self.s.scissors.first().copied().unwrap_or([0,0,t.desc.w,t.desc.h]);
                    vp[..4]==[0.,0.,t.desc.w as f32,t.desc.h as f32] && sc==[0,0,t.desc.w,t.desc.h]
                });
            let blit_sources_certified=ps.is_some_and(|program|program.info.bindings.iter().filter(|b|b.kind==Kind::Texture).all(|b|
                self.s.srv[1].get(b.binding.wrapping_sub(32)as usize).and_then(|id|self.view_obj(*id))
                    .is_some_and(|v|self.tex(v.res).is_some_and(|t|capture.scratch.get(&v.res)==Some(&t.tex)))));
            let rgb_empty=(mask&(ColorWrites::RED|ColorWrites::GREEN|ColorWrites::BLUE)).is_empty();
            let known=matches!(pair,(Some(K::ScaleformVertex),Some(K::ScaleformPixel))|(Some(K::MapVertex),Some(K::MapPixel))|(Some(K::ClearVertex),Some(K::ClearPixel)))
                || matches!(pair,(Some(K::BlitVertex),Some(K::BlitPixel|K::DepthToAlpha)))&&(rgb_empty||blit_sources_certified);
            let texture=self.tex(id).map(|t|t.tex.clone());
            if let Some(c)=&mut self.hud_capture {
                if clear {if let Some(texture)=texture {if c.scratch.len()<8 || c.scratch.contains_key(&id){c.scratch.insert(id,texture);}}}
                else if !known || !one_target || texture.as_ref()!=c.scratch.get(&id) {c.scratch.remove(&id);}
            }
            return None;
        }
        if !valid_final || !one_target || writes_depth || ps.is_none_or(|p|p.info.writes_fragment_depth||p.info.outputs!=[0]) {
            self.hud_reject("unqualified final-target layout/depth/fragment output");return None;
        }
        let scaleform=matches!(pair,(Some(K::ScaleformVertex),Some(K::ScaleformPixel)));
        let blit=pair==(Some(K::BlitVertex),Some(K::BlitPixel)) && ps.is_some_and(|p|{
            let mut textures=p.info.bindings.iter().filter(|b|b.kind==Kind::Texture);
            let Some(binding)=textures.next()else{return false;};if textures.next().is_some(){return false;}
            let Some(&view_id)=self.s.srv[1].get(binding.binding.wrapping_sub(32) as usize)else{return false;};
            let Some(view)=self.view_obj(view_id).filter(|v|!v.is_buf)else{return false;};
            self.tex(view.res).is_some_and(|t|capture.scratch.get(&view.res)==Some(&t.tex))
        });
        if !scaleform && !blit {self.hud_reject("final target write is not a pinned Scaleform writer or qualified minimap composite");return None;}
        let Some(mode)=crate::xr_hud::qualify_blend(blend,mask)else{self.hud_reject("unsupported HUD partial RGB mask or destination-dependent blend");return None;};
        if !ps.is_some_and(|p|crate::xr_hud::fragment_abi_supported(&p.text)){self.hud_reject("HUD fragment return ABI differs");return None;}
        if capture.images.is_none() {
            let source=capture.final_texture.clone();
            self.end_pass();self.materialize_clears(id);
            let Some(images)=self.reserve_hud_images(&source)else{self.hud_reject("bounded HUD snapshot pool full");return None;};
            self.encoder().copy_texture_to_texture(TexelCopyTextureInfo{texture:&source,mip_level:0,origin:Origin3d::ZERO,aspect:TextureAspect::All},TexelCopyTextureInfo{texture:&images.world,mip_level:0,origin:Origin3d::ZERO,aspect:TextureAspect::All},Extent3d{width:source.width(),height:source.height(),depth_or_array_layers:1});
            let view=images.contribution.create_view(&TextureViewDescriptor::default());
            {let _clear=self.encoder().begin_render_pass(&RenderPassDescriptor{label:Some("Clear real HUD contribution"),color_attachments:&[Some(RenderPassColorAttachment{view:&view,depth_slice:None,resolve_target:None,ops:Operations{load:LoadOp::Clear(Color::TRANSPARENT),store:StoreOp::Store}})],depth_stencil_attachment:None,timestamp_writes:None,occlusion_query_set:None,multiview_mask:None});}
            self.hud_capture.as_mut().unwrap().images=Some(images);
        }
        Some(mode)
    }

    fn finish_hud_draw(&mut self,vs:&Program,ps:Option<&Program>) {
        use crate::xr_hud::ShaderKind as K;
        if self.hud_draw.is_some(){if let Some(c)=&mut self.hud_capture{c.draws+=1;}return;}
        if !crate::xr_hud::enabled() || self.hud_capture.is_some()
            || (vs.hud_kind,ps.and_then(|p|p.hud_kind))!=(Some(K::LensVertex),Some(K::LensPixel)) {return;}
        let Some(view)=self.world_view.filter(|v|v.headset_applied&&crate::xr_view::view_epoch_is_active(v.provenance_epoch))else{return;};
        let r=&self.s.rt[0];
        let Some(t)=self.tex(r.tex)else{return;};
        let (blend,mask)=blend_for(self.state(self.s.blend.0).as_deref(),0);
        let expected=[0.,0.,t.desc.w as f32,t.desc.h as f32,0.,1.];
        if r.mip!=0 || r.slice!=0 || self.s.rt.iter().skip(1).any(|r|r.tex!=0)
            || t.desc.samples!=1 || t.desc.d!=1 || !matches!(t.format,TextureFormat::Bgra8Unorm|TextureFormat::Rgba8Unorm)
            || blend.is_some() || mask!=ColorWrites::ALL
            || self.s.viewports.first().copied().unwrap_or(expected)!=expected
            || self.s.scissors.first().copied().unwrap_or([0,0,t.desc.w,t.desc.h])!=[0,0,t.desc.w,t.desc.h] {return;}
        self.hud_capture=Some(HudCapture{final_id:r.tex,final_texture:t.tex.clone(),view,images:None,draws:0,scratch:HashMap::new(),rejected:None});
    }

    fn color_phase_target(&self, id: u32) -> serde_json::Value {
        serde_json::json!({"id":id,"texture":self.tex(id).map(|t| serde_json::json!({
            "format":format!("{:?}",t.format),"width":t.desc.w,"height":t.desc.h,
            "layers":t.desc.d,"samples":t.desc.samples}))})
    }

    /// Diagnostic only. A clip-space depth clear is not a certified HUD boundary:
    /// weapons and subsequent post-processing can use the same untagged phase.
    fn trace_color_draw(&mut self, vs: &Program, ps: Option<&Program>, viewport: [f32;6],
        scissor: [u32;4], writes_depth: bool, full_world_viewport: bool,
        depth_clear_boundary: bool, command_word: u64, count: u32, instances: u32) {
        if !self.color_phase_trace.wants_draw(true) { return; }
        let qualified = self.world_view.filter(|view| view.headset_applied && writes_depth
            && full_world_viewport && self.s.ds.mip == 0 && self.s.ds.slice == 0
            && self.tex(self.s.ds.tex).is_some_and(|t| is_depth(t.format)
                && t.desc.samples == 1 && t.desc.d == 1));
        if !self.color_phase_trace.wants_draw(qualified.is_some()) { return; }
        let shader = |stage: usize, program: &Program| serde_json::json!({
            "id":self.s.shader[stage],"label":program.label.chars().take(160).collect::<String>(),
            "hash":match self.obj(self.s.shader[stage]) { Some(Obj::Shader(s))=>Some(&s.hash), _=>None },
            "outputs":program.info.outputs,"fragment_depth":program.info.writes_fragment_depth});
        let blend = self.state(self.s.blend.0);
        let targets: Vec<_> = self.s.rt.iter().enumerate().filter(|(_,rt)|rt.tex != 0).map(|(slot,rt)| {
            let (bl, mask) = blend_for(blend.as_deref(), slot);
            let output = ps.is_some_and(|program|program.info.outputs.contains(&(slot as u32)));
            let format = self.pass_info.as_ref().and_then(|pi|pi.rt_formats.get(slot)).copied().flatten();
            let blendable = format.is_some_and(|format|self.adapter.get_texture_format_features(format)
                .flags.contains(TextureFormatFeatureFlags::BLENDABLE));
            serde_json::json!({"slot":slot,"target":self.color_phase_target(rt.tex),
                "mip":rt.mip,"slice":rt.slice,"dxgi_view":rt.fmt,
                "actual_view_format":format.map(|format|format!("{format:?}")),
                "fragment_output":output,"write_mask":if output {mask.bits()} else {0},
                "blend_requested":bl.map(|state|format!("{state:?}")),
                "blend_applied":bl.filter(|_|output && blendable).map(|state|format!("{state:?}"))})
        }).collect();
        let mut srvs = Vec::new();
        let mut srv_count = 0;
        for (stage, program) in [(0,Some(vs)),(1,ps)] {
            let Some(program) = program else {continue};
            for binding in &program.info.bindings {
                if binding.group != stage as u32 || !(32..160).contains(&binding.binding)
                    || !matches!(binding.kind, Kind::Texture | Kind::ReadOnlyStorage | Kind::Storage) { continue; }
                srv_count += 1;
                if srvs.len() >= 64 { continue; }
                let slot = (binding.binding - 32) as usize;
                let id = self.s.srv[stage][slot];
                srvs.push(serde_json::json!({"stage":stage,"slot":slot,"binding":binding.binding,"view":id,
                    "resource":self.view_obj(id).map(|v|v.res),
                    "view_desc":self.view_obj(id).map(|v|[v.fmt,v.vdim,v.a,v.b,v.c,v.d]),
                    "is_buffer":self.view_obj(id).map(|v|v.is_buf),
                    "texture":self.view_obj(id).filter(|v|!v.is_buf).map(|v|self.color_phase_target(v.res))}));
            }
        }
        let record = serde_json::json!({"draw":self.draws,"command_word":command_word,
            "count":count,"instances":instances,"world":self.world_view.as_ref().map(crate::world_ui::marker),
            "shader":{"vertex":shader(0,vs),"pixel":ps.map(|program|shader(1,program))},
            "render_targets":targets,"srv_bindings":srvs,"srv_bindings_omitted":srv_count-srvs.len(),
            "blend_state":blend.map(|state|state.words.iter().take(66).copied().collect::<Vec<_>>()),
            "blend_constant":self.s.blend.1,"depth_target":self.color_phase_target(self.s.ds.tex),
            "depth_subresource":[self.s.ds.mip,self.s.ds.slice,self.s.ds.fmt,self.s.ds.flags],
            "depth_state":self.state(self.s.dss.0).map(|state|state.words.iter().take(13).copied().collect::<Vec<_>>()),
            "stencil_reference":self.s.dss.1,"viewport":viewport,"scissor":scissor,
            "rasterizer_state":self.state(self.s.rs).map(|state|state.words.iter().take(10).copied().collect::<Vec<_>>()),
            "writes_depth":writes_depth,"full_world_viewport":full_world_viewport,
            "depth_clear_boundary":depth_clear_boundary});
        self.color_phase_trace.record_draw(self.presents+1, qualified, record, depth_clear_boundary);
    }

    fn obj(&self, id: u32) -> Option<&Obj> {
        self.objs.get(id as usize).and_then(|o| o.as_ref())
    }
    fn obj_mut(&mut self, id: u32) -> Option<&mut Obj> {
        self.objs.get_mut(id as usize).and_then(|o| o.as_mut())
    }
    // A guest view ID can outlive the texture allocation it references. Its
    // cached native views hold the OLD allocation strongly, so clearing only
    // bind_groups cannot make that view follow a reused resource ID.
    // Call only for texture creation/replacement/destruction, not ordinary
    // buffer/shader/state churn. Already encoded commands keep their own refs.
    fn invalidate_texture_views(&mut self, id: u32) {
        for object in self.objs.iter_mut().flatten() {
            if let Obj::Srv(view) | Obj::Uav(view) = object {
                if !view.is_buf && view.res == id { view.views.clear(); }
            }
        }
    }

    fn set_obj(&mut self, id: u32, o: Obj) {
        let i = id as usize;
        if i > 64 << 20 {
            log_once("hugeid", &format!("object id {id} out of range: ignored"));
            return;
        }
        if self.objs.len() <= i {
            self.objs.resize_with(i + 1, || None);
        }
        // A previously empty resource ID may already be referenced by a view
        // whose unchanged ID cached a dummy binding. An old binding-relevant
        // object also requires invalidation when replaced by a nonbinding one.
        let texture_lifetime = matches!(self.objs[i].as_ref(), Some(Obj::Tex(_))) || matches!(&o, Obj::Tex(_));
        if texture_lifetime {
            // Attachment keys and deferred clears name guest IDs, not native
            // allocations. Retire only this texture's old lifetime; material
            // texture creation must not split an unrelated render pass.
            if id != 0 && self.in_open_pass(id) { self.end_pass(); }
            self.pending_clears.remove(&id);
            self.invalidate_texture_views(id);
        }
        // Texture identity correctness is independent of the lookup-preparation
        // optimization: the legacy path also caches bind groups by guest IDs.
        if texture_lifetime || self.bind_group_prepare.enabled {
            if texture_lifetime || binding_cache_needs_invalidation(self.objs[i].as_ref(), Some(&o)) {
                self.bind_groups.clear();
                self.bind_group_prepare.invalidated();
            } else if let Some(kind) = o.nonbinding_kind() {
                self.bind_group_prepare.skipped_set_obj(kind);
            }
        }
        self.invalidate_world_depth(id, serde_json::json!("set_obj"));
        if self.occlusion_active == Some(id) { self.end_active_occlusion(); }
        // A replaced buffer's lifetime ends here, and its op47 record with it.
        if let Some(Obj::Buf(old)) = self.objs[i].as_ref() { self.upload_dedup.forget(old.dedup); }
        self.objs[i] = Some(o);
    }
    fn tex(&self, id: u32) -> Option<&Tex> {
        match self.obj(id) { Some(Obj::Tex(t)) => Some(t), _ => None }
    }
    fn tex_mut(&mut self, id: u32) -> Option<&mut Tex> {
        match self.obj_mut(id) { Some(Obj::Tex(t)) => Some(t), _ => None }
    }
    fn buf(&self, id: u32) -> Option<&Buf> {
        match self.obj(id) { Some(Obj::Buf(b)) => Some(b), _ => None }
    }
    fn buf_mut(&mut self, id: u32) -> Option<&mut Buf> {
        match self.obj_mut(id) { Some(Obj::Buf(b)) => Some(b), _ => None }
    }
    fn view_obj(&self, id: u32) -> Option<&ViewObj> {
        match self.obj(id) { Some(Obj::Srv(v)) | Some(Obj::Uav(v)) => Some(v), _ => None }
    }
    fn view_obj_mut(&mut self, id: u32) -> Option<&mut ViewObj> {
        match self.obj_mut(id) { Some(Obj::Srv(v)) | Some(Obj::Uav(v)) => Some(v), _ => None }
    }
    fn state(&self, id: u32) -> Option<Rc<StateObj>> {
        match self.obj(id) { Some(Obj::State(s)) => Some(s.clone()), _ => None }
    }
    fn use_tex(&mut self, id: u32) {
        let e = self.enc_epoch;
        if let Some(t) = self.tex_mut(id) { t.enc = e; }
    }
    fn use_buf(&mut self, id: u32) {
        let e = self.enc_epoch;
        if let Some(b) = self.buf_mut(id) { b.enc = e; }
    }
    fn used(&self, id: u32) -> bool {
        match self.obj(id) {
            Some(Obj::Tex(t)) => t.enc == self.enc_epoch,
            Some(Obj::Buf(b)) => b.enc == self.enc_epoch,
            _ => false,
        }
    }
    // A GPU-side write is about to target this buffer. Uploads run in a queue
    // prelude ahead of every encoder command of their submission, so no later
    // op47 can be trusted to describe its bytes: the object leaves the op47
    // guard for good. Call this BEFORE the write itself is recorded.
    fn op47_dedup_gpu_write(&mut self, id: u32, kind: crate::upload_dedup::GpuWrite) {
        if !self.upload_dedup.enabled() { return; }
        if let Some(Some(Obj::Buf(b))) = self.objs.get_mut(id as usize) {
            self.upload_dedup.gpu_write(&mut b.dedup, kind);
        }
    }

    fn scoped<T>(&self, what: &str, f: impl FnOnce(&Device) -> T) -> Option<T> {
        let scope = self.device.push_error_scope(ErrorFilter::Validation);
        let v = f(&self.device);
        match pollster::block_on(scope.pop()) {
            None => Some(v),
            Some(e) => {
                let msg = format!("{e:?}");
                log_once(&format!("{what}:{}", &msg[..msg.len().min(160)]), &format!("{what} failed: {}", &msg[..msg.len().min(1500)]));
                None
            }
        }
    }

    fn encoder(&mut self) -> &mut CommandEncoder {
        if self.enc.is_none() {
            self.enc = Some(self.device.create_command_encoder(&CommandEncoderDescriptor::default()));
        }
        self.enc.as_mut().unwrap()
    }
    fn end_pass(&mut self) {
        if let Some(mut p) = self.pass.take() {
            if self.occlusion_pass_slot.take().is_some() {
                p.end_occlusion_query();
            }
            drop(p);
            self.pass_key = None;
            self.pass_info = None;
            self.pass_st = PassSt::default();
        }
    }
    fn flush(&mut self) {
        self.end_pass();
        self.flush_uniforms();
        let resolves = if self.pending_resolve.is_empty() { None } else { self.record_resolves() };
        let main = self.enc.take().map(CommandEncoder::finish);
        if let Some(idx) = self.uploads.submit(&self.queue, main) {
            self.last_submit = Some(idx);
            self.submits += 1;
            self.on_submitted();
            self.counter_slot = 0;
        }
        if let Some(r) = resolves {
            self.map_pending(r.buf, r.kind);
        }
        self.enc_epoch += 1;
    }

    fn run(&mut self) -> ! {
        let mut tail = RING_DATA as i32;
        self.hdr(1).store(tail, Ordering::SeqCst);
        self.hdr(2).store(1, Ordering::SeqCst);
        log("running");
        loop {
            if SUSPENDED.load(Ordering::Relaxed) {
                let started = Instant::now();
                let _ = self.device.poll(PollType::Wait { submission_index: None, timeout: None });
                self.thread_wall[THREAD_SUSPEND_WAIT].record(started.elapsed());
                let started = Instant::now();
                while SUSPENDED.load(Ordering::Relaxed) { std::thread::sleep(Duration::from_millis(50)); }
                self.thread_wall[THREAD_SUSPEND_SLEEP].record(started.elapsed());
            }
            self.process_maps(false);
            self.uploads.poll(&self.device);
            if completion_poll_needed(self.inflight.load(Ordering::SeqCst) > 0) {
                let started = Instant::now();
                let _ = self.device.poll(PollType::Poll);
                self.thread_wall[THREAD_QUERY_POLL].record(started.elapsed());
            }
            let head = self.hdr(0).load(Ordering::SeqCst);
            if head == tail {
                if !self.pending_events.is_empty() || !self.pending_resolve.is_empty() {
                    self.flush();
                }
                self.hdr(5).store(1, Ordering::SeqCst);
                if self.hdr(0).load(Ordering::SeqCst) != tail {
                    self.hdr(5).store(0, Ordering::SeqCst);
                    continue;
                }
                let timeout = producer_idle_timeout_millis(!self.pending_maps.is_empty(),
                    self.inflight.load(Ordering::SeqCst) > 0, self.uploads.has_pending_callbacks());
                let started = Instant::now();
                let _ = self.mem.atomic_wait32(self.ring, tail as u32, Some(Duration::from_millis(timeout)));
                self.thread_wall[THREAD_PRODUCER_IDLE].record(started.elapsed());
                self.hdr(5).store(0, Ordering::SeqCst);
                continue;
            }
            let started = Instant::now();
            tail = self.run_commands(tail, head);
            self.thread_wall[THREAD_REPLAY].record(started.elapsed());
            self.hdr(1).store(tail, Ordering::SeqCst);
            let _ = self.mem.atomic_notify(self.ring + 4, u32::MAX);
        }
    }

    fn run_commands(&mut self, mut tail: i32, head: i32) -> i32 {
        let ring_w = self.ring / 4;
        let mut since = 0u32;
        let mut executed = 0i32;
        while tail != head {
            let base = ring_w + (tail as u64 >> 2);
            let op = self.u(base);
            let n = self.u(base + 1) as u64;
            if let Some(view) = crate::xr_view::take_command_view(base * 4) { self.world_view = view; }
            if let Some(mark) = crate::xr_view::take_command_foreground(base * 4) { self.foreground_set(mark); }
            if self.command_trace.wants_command(op) { self.trace_ring_command(op, base, n); }
            if op == 1 {
                tail = RING_DATA as i32;
                continue;
            }
            let p = base + 2;
            self.exec(op, p, n);
            executed += 1;
            tail += (n * 4 + 8) as i32;
            since += 1;
            if since >= 512 {
                since = 0;
                self.hdr(3).fetch_add(executed, Ordering::Relaxed);
                executed = 0;
                self.hdr(1).store(tail, Ordering::SeqCst);
                let _ = self.mem.atomic_notify(self.ring + 4, u32::MAX);
                self.process_maps(false);
                self.uploads.poll(&self.device);
                if completion_poll_needed(self.inflight.load(Ordering::SeqCst) > 0) {
                    let started = Instant::now();
                    let _ = self.device.poll(PollType::Poll);
                    self.thread_wall[THREAD_QUERY_POLL].record(started.elapsed());
                }
            }
        }
        self.hdr(3).fetch_add(executed, Ordering::Relaxed);
        tail
    }

    fn exec(&mut self, op: u32, p: u64, n: u64) {
        let mb = self.base;
        let u = move |w: u64| unsafe { std::ptr::read_volatile(mb.add(((p + w) * 4) as usize) as *const u32) };
        self.hud_non_draw_command(op,p,n);
        match op {
            0 | 2 => {}
            3 => self.create_texture(u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7), u(8), u(9)),
            4 => self.destroy(u(0)),
            5 => self.clear_rt(u(0), u(1), u(2), [self.f(p + 3), self.f(p + 4), self.f(p + 5), self.f(p + 6)]),
            6 => self.clear_ds(u(0), u(1), u(2), u(3), self.f(p + 4), u(5)),
            7 => self.present(u(0)),
            8 => {
                self.flush();
                let started = Instant::now();
                let _ = self.device.poll(PollType::Wait { submission_index: None, timeout: None });
                self.thread_wall[THREAD_GUEST_WAIT].record(started.elapsed());
                let addr = u(0) as u64 + u(1) as u64 * 4294967296;
                atomic(&self.mem, addr).store(u(2) as i32, Ordering::SeqCst);
                let _ = self.mem.atomic_notify(addr, u32::MAX);
            }
            9 => {}
            10 => {
                let len = u(0) as u64;
                let text = String::from_utf8_lossy(self.bytes((p + 1) * 4, len)).into_owned();
                if text == "stats" { self.log_stats(); } else { log(&text); }
            }
            11 => self.create_buffer(u(0), u(1), u(2), u(3), u(4)),
            12 | 47 => self.upload_buffer(u(0), u(1) as u64, u(2) as u64, (p + 3) * 4, op == 47),
            13 => self.upload_texture(u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7), u(8), u(9), u(10), u(11) as u64, (p + 12) * 4),
            14 => self.create_shader(p),
            15 => self.create_layout(p),
            16 => self.create_state(p, n),
            17 => self.create_view(p, false),
            37 => self.create_view(p, true),
            18 => {
                let st = u(0) as usize;
                if st < 3 && self.s.shader[st] != u(1) { self.s.shader[st] = u(1); self.pdirty = true; }
            }
            19 => if self.s.layout != u(0) { self.s.layout = u(0); self.pdirty = true; },
            20 => if self.s.topo != u(0) { self.s.topo = u(0); self.pdirty = true; },
            21 => {
                let (s, c) = (u(0) as usize, u(1) as usize);
                for i in 0..c {
                    if s + i >= 16 { break; }
                    let (nb, ns, no) = (u(2 + i as u64 * 3), u(3 + i as u64 * 3), u(4 + i as u64 * 3));
                    let v = &mut self.s.vb[s + i];
                    if v.stride != ns || (v.buf == 0) != (nb == 0) { self.pdirty = true; }
                    *v = Vb { buf: nb, stride: ns, offset: no };
                }
            }
            22 => {
                if self.s.ib.1 != u(1) || (self.s.ib.0 == 0) != (u(0) == 0) { self.pdirty = true; }
                self.s.ib = (u(0), u(1), u(2));
            }
            23 => {
                let (st, s, c) = (u(0) as usize, u(1) as usize, u(2) as usize);
                for i in 0..c {
                    if s + i >= 16 || st >= 3 { break; }
                    self.s.cb[st][s + i] = Cb { buf: u(3 + i as u64 * 3), first: u(4 + i as u64 * 3) };
                }
            }
            24 => {
                let (st, s, c) = (u(0) as usize, u(1) as usize, u(2) as usize);
                for i in 0..c { if s + i < 128 && st < 3 { self.s.srv[st][s + i] = u(3 + i as u64); } }
            }
            25 => {
                let (st, s, c) = (u(0) as usize, u(1) as usize, u(2) as usize);
                for i in 0..c { if s + i < 16 && st < 3 { self.s.smp[st][s + i] = u(3 + i as u64); } }
            }
            26 => {
                for i in 0..8 {
                    let q = i as u64 * 4;
                    self.s.rt[i] = Rt { tex: u(q), mip: u(q + 1), slice: u(q + 2), fmt: u(q + 3) };
                }
                self.s.ds = Ds { tex: u(32), mip: u(33), slice: u(34), fmt: u(35), flags: u(36) };
                self.pdirty = true;
            }
            27 => {
                if self.s.blend.0 != u(0) { self.pdirty = true; }
                self.s.blend = (u(0), [self.f(p + 1) as f64, self.f(p + 2) as f64, self.f(p + 3) as f64, self.f(p + 4) as f64]);
            }
            28 => {
                if self.s.dss.0 != u(0) { self.pdirty = true; }
                self.s.dss = (u(0), u(1));
            }
            29 => if self.s.rs != u(0) { self.s.rs = u(0); self.pdirty = true; },
            30 => {
                let c = u(0) as usize;
                self.s.viewports = (0..c).map(|i| std::array::from_fn(|k| self.f(p + 1 + i as u64 * 6 + k as u64))).collect();
            }
            31 => {
                let c = u(0) as usize;
                self.s.scissors = (0..c).map(|i| std::array::from_fn(|k| self.u(p + 1 + i as u64 * 4 + k as u64))).collect();
            }
            32 => self.do_draw(p),
            33 => self.reset_state(),
            34 => self.copy_region(u(0), u(1), u(2), u(3), u(4), u(5), u(6), u(7) != 0, u(8), u(9), u(10), u(11), u(12), u(13)),
            35 => self.copy_resource(u(0), u(1)),
            36 => self.resolve(u(0), u(1), u(2), u(3)),
            38 => {
                let (st, s0, c) = (u(0) as usize, u(1) as usize, u(2) as usize);
                for i in 0..c {
                    if s0 + i >= 8 || st >= 3 { break; }
                    let id = u(3 + i as u64);
                    self.s.uav[st][s0 + i] = id;
                    let init = u(3 + c as u64 + i as u64);
                    if init != 0xffff_ffff { self.set_uav_counter(id, init); }
                }
            }
            39 => self.do_dispatch(u(0), u(1), u(2)),
            40 => self.clear_uav(u(0), [u(2), u(3), u(4), u(5)]),
            41 => self.generate_mips(u(0)),
            42 => self.readback(p),
            43 => self.copy_counter(u(0), u(1) as u64, u(2)),
            44 => self.create_query(u(0), u(1), u(2) as u64 + u(3) as u64 * 4294967296),
            45 => self.begin_query(u(0)),
            46 => self.end_query(u(0), u(1)),
            other => log_once(&format!("op{other}"), &format!("unknown opcode {other}")),
        }
    }

    /// Trace copies precede exec and every ring-tail acknowledgement. No guest
    /// slices, native resource handles or async mutable state escape this call.
    fn trace_ring_command(&mut self, op: u32, base: u64, n: u64) {
        use serde_json::json;
        if !self.command_trace.retaining_commands() {
            self.command_trace.command(op, json!({"opcode":op,"command_address":base*4,
                "declared_operand_words":n,"operands_complete":false,"record_omitted":"trace_capacity"}));
            return;
        }
        let p = base + 2;
        let memory_words = self.mem.data_size() as u64 / 4;
        // Wrap's n is not consumed by exec; retain its raw header without
        // interpreting a potentially stale length as a command payload.
        let wanted = if op == 1 { 0 } else { n };
        let valid = p.checked_add(wanted).is_some_and(|end|end<=memory_words);
        let copied = if valid { wanted.min((crate::world_command_trace::MAX_OPERAND_BYTES/4) as u64) } else { 0 };
        let words: Vec<u32> = (0..copied).map(|offset|self.u(p+offset)).collect();
        let mut accesses: Vec<(u32,&str)> = Vec::new();
        let mut add = |index:usize,role:&'static str| { if let Some(&id)=words.get(index) { if id!=0 { accesses.push((id,role)); } } };
        match op {
            3|11|14|15|16|44 => add(0,"create_id_preexisting_object"),
            17|37 => { add(0,"create_view_id_preexisting_object"); add(2,"view_resource"); },
            4 => add(0,"destroy"), 5|6|12|13|47 => add(0,"write"),
            7|42 => add(0,"read"),
            18 => add(1,"bind_shader"), 19|27|28|29 => add(0,"bind_state"),
            22 => add(0,"bind_index_buffer"),
            21|23|24|25|38 => {
                let (start,count,stride) = match op {
                    21 => (2,words.get(1).copied().unwrap_or(0),3),
                    23 => (3,words.get(2).copied().unwrap_or(0),3),
                    _ => (3,words.get(2).copied().unwrap_or(0),1),
                };
                for i in 0..(count as usize).min(128) { add(start+i*stride,"bind_possible_access"); }
            },
            26 => { for i in 0..8 { add(i*4,"bind_render_target"); } add(32,"bind_depth_target"); },
            34 => { add(0,"copy_destination"); add(5,"copy_source"); },
            35 => { add(0,"copy_destination"); add(1,"copy_source"); },
            36 => { add(0,"resolve_destination"); add(2,"resolve_source"); },
            40|41 => add(0,"view_read_write"),
            43 => { add(0,"counter_destination"); add(2,"counter_source_view"); },
            45|46 => add(0,"query"),
            32|39 => {
                if op==32 && words.first().is_some_and(|kind|*kind>=4) { add(1,"indirect_arguments_read"); }
                for &id in &self.s.shader { if id!=0 { accesses.push((id,"bound_shader")); } }
                for vb in &self.s.vb { if vb.buf!=0 { accesses.push((vb.buf,"bound_vertex_possible_read")); } }
                if self.s.ib.0!=0 { accesses.push((self.s.ib.0,"bound_index_possible_read")); }
                for stage in &self.s.cb { for cb in stage { if cb.buf!=0 { accesses.push((cb.buf,"bound_constant_possible_read")); } } }
                for stage in &self.s.srv { for &id in stage { if id!=0 { accesses.push((id,"bound_srv_possible_read")); } } }
                for stage in &self.s.uav { for &id in stage { if id!=0 { accesses.push((id,"bound_uav_possible_read_write")); } } }
                if op==32 {
                    for rt in &self.s.rt { if rt.tex!=0 { accesses.push((rt.tex,"bound_target_possible_write")); } }
                    if self.s.ds.tex!=0 { accesses.push((self.s.ds.tex,"bound_depth_possible_read_write")); }
                }
            },
            _ => {},
        }
        let resource_count=accesses.len();
        let resources: Vec<_> = accesses.into_iter().take(64).map(|(id,role)|json!({
            "id":id,"role":role,"pre_exec":self.command_trace_resource(id)})).collect();
        let mut payload = serde_json::Value::Null;
        let upload_layout = match op { 12|47=>Some((3,2)),13=>Some((12,11)),10=>Some((1,0)),_=>None };
        if let Some((header,length_word))=upload_layout {
            if let Some(&length)=words.get(length_word) {
                let length=length as usize;
                let declared_bytes=n.checked_mul(4).and_then(|bytes|bytes.checked_sub(header as u64*4));
                let inline_valid=valid && declared_bytes.is_some_and(|bytes|bytes>=length as u64);
                let hash_bytes=if inline_valid { self.command_trace.hash_allowance(length) } else { 0 };
                // Hash an OWNED copy before acknowledgement; truncated hashes
                // explicitly identify the prefix, never the full upload.
                let owned = if hash_bytes>0 { self.bytes((p+header as u64)*4,hash_bytes as u64).to_vec() } else { Vec::new() };
                let hash = (hash_bytes>0 || (inline_valid && length==0)).then(||crate::world_command_trace::upload_fingerprint(&owned));
                self.command_trace.note_hash_bytes(hash_bytes);
                payload=json!({"kind":if op==10 {"inline_text"} else {"inline_upload"},
                    "declared_bytes":length,"inline_range_valid":inline_valid,"hash_bytes":hash_bytes,
                    "hash_complete":inline_valid && hash_bytes==length,"hash":hash,
                    "hash_algorithm":"fnv1a64_noncryptographic_correlation_only",
                    "raw_payload_bytes_retained_in_operand_words":length.min(copied.saturating_sub(header as u64) as usize*4),
                    "raw_payload_complete_in_operand_words":inline_valid && copied.saturating_sub(header as u64)*4>=length as u64});
            } else { payload=json!({"kind":"inline_payload","metadata_missing":true}); }
        }
        let inherited = if op==7 { Some(self.command_trace_state()) } else { None };
        let record=json!({"opcode":op,"command_address":base*4,"declared_operand_words":n,
            "operands":words,"operands_complete":valid && copied==wanted,
            "operand_words_omitted":wanted.saturating_sub(copied),"operand_range_valid":valid,
            "wrap_declared_length_not_interpreted":op==1,
            "world":self.world_view.as_ref().map(crate::world_ui::marker),
            "resources":resources,"resource_access_rows_omitted":resource_count.saturating_sub(64),
            "resource_access_caveat":"pre-exec potential bound accesses; not shader-consumption or native graph proof",
            "payload":payload,"inherited_state":inherited,"native_context":self.command_trace_native_context()});
        self.command_trace.command(op,record);
    }

    fn command_trace_resource(&self, id:u32) -> serde_json::Value {
        use serde_json::json;
        match self.obj(id) {
            Some(Obj::Buf(b))=>json!({"kind":"buffer","identity":b.identity,"version":b.version,"size":b.size,"stride":b.stride,
                "native_allocation":b.gpu.as_ref().map(|_|json!({"kind":"buffer","tag":b.identity,
                    "basis":"returned wgpu Buffer handle associated with host Buf lifetime UID","scope":"this_gpu_instance",
                    "identity_kind":"wgpu_buffer_handle_lifetime","backend_allocation_validated":false,"backend_id":null})),
                "native_counter_present":b.counter.is_some(),"native_counter_allocation":null,
                "gpu_contents_captured":false,"version_caveat":"host buffer version is not a complete GPU-write generation"}),
            Some(Obj::Tex(t))=>json!({"kind":"texture","size":[t.desc.w,t.desc.h,t.desc.d],"mips":t.desc.mips,
                "samples":t.desc.samples,"format":format!("{:?}",t.format),"generation":null,"contents_captured":false,
                "native_allocation":trace_native_texture(t.trace_native_identity,"texture"),
                "proxies":([("depth",t.proxy_depth.as_ref()),("stencil",t.proxy_stencil.as_ref()),("float_depth",t.proxy_fdepth.as_ref())].into_iter()
                    .filter_map(|(kind,proxy)|proxy.map(|p|json!({"kind":kind,"dirty":p.dirty,"native_allocation":trace_native_texture(p.trace_native_identity,"texture_proxy")}))).collect::<Vec<_>>())}),
            Some(Obj::Shader(s))=>json!({"kind":"shader","stage":s.stage,"hash":s.hash,"native_variant_captured":false}),
            Some(Obj::Srv(v))|Some(Obj::Uav(v))=>{
                let mut cached=BTreeMap::new();
                for (key,view) in &v.views { cached.insert(key,view); if cached.len()>8 { cached.pop_last(); } }
                json!({"kind":"view","resource":v.res,"is_buffer":v.is_buf,
                    "desc":[v.fmt,v.vdim,v.a,v.b,v.c,v.d],"generation":null,
                    "current_resource_native_allocation":self.tex(v.res).map(|t|trace_native_texture(t.trace_native_identity,"texture")),
                    "cached_native_views":cached.iter().map(|(key,view)|json!({"key":key,
                        "retained_parent_allocation":self.command_trace_view_parent(v.res,view),"view_backend_identity":null})).collect::<Vec<_>>(),
                    "cached_native_views_omitted":v.views.len().saturating_sub(8),
                    "cache_caveat":"exact parent Eq where matched; cached entries are not proof that shader consumed any entry"})
            },
            Some(Obj::Layout(_))=>json!({"kind":"layout","generation":null}),
            Some(Obj::State(s))=>json!({"kind":"state","words":s.words.iter().take(128).copied().collect::<Vec<_>>(),
                "words_omitted":s.words.len().saturating_sub(128),"generation":null}),
            Some(Obj::Query(_))=>json!({"kind":"query","result_captured":false,"generation":null}),
            None=>json!({"kind":"missing"}),
        }
    }

    fn command_trace_state(&self) -> serde_json::Value {
        use serde_json::json;
        json!({"shader":self.s.shader,"layout":self.s.layout,"topology":self.s.topo,
            "vertex_buffers":self.s.vb.iter().map(|v|[v.buf,v.stride,v.offset]).collect::<Vec<_>>(),
            "index_buffer":self.s.ib,"constant_buffers":self.s.cb.iter().map(|stage|stage.iter().map(|b|[b.buf,b.first]).collect::<Vec<_>>()).collect::<Vec<_>>(),
            "srv":self.s.srv,"uav":self.s.uav,"samplers":self.s.smp,
            "render_targets":self.s.rt.iter().map(|rt|[rt.tex,rt.mip,rt.slice,rt.fmt]).collect::<Vec<_>>(),
            "depth_target":[self.s.ds.tex,self.s.ds.mip,self.s.ds.slice,self.s.ds.fmt,self.s.ds.flags],
            "blend_state":self.s.blend,"depth_state":self.s.dss,"raster_state":self.s.rs,
            "viewports":self.s.viewports.iter().take(128).copied().collect::<Vec<_>>(),
            "viewports_omitted":self.s.viewports.len().saturating_sub(128),
            "scissors":self.s.scissors.iter().take(128).copied().collect::<Vec<_>>(),
            "scissors_omitted":self.s.scissors.len().saturating_sub(128),
            "native_pass_state_and_pending_clears_evidence_present":true})
    }

    fn command_trace_view_parent(&self,id:u32,view:&TextureView)->serde_json::Value {
        let Some(t)=self.tex(id) else { return serde_json::json!({"resolved":false,"reason":"current_texture_missing_or_retired"}); };
        if view.texture()==&t.tex {
            return serde_json::json!({"resolved":t.trace_native_identity.is_some(),"parent_eq":true,
                "native_allocation":trace_native_texture(t.trace_native_identity,"texture")});
        }
        for proxy in [&t.proxy_depth,&t.proxy_stencil,&t.proxy_fdepth].into_iter().flatten() {
            if view.texture()==&proxy.tex {
                return serde_json::json!({"resolved":proxy.trace_native_identity.is_some(),"parent_eq":true,
                    "native_allocation":trace_native_texture(proxy.trace_native_identity,"texture_proxy")});
            }
        }
        serde_json::json!({"resolved":false,"reason":"cached_parent_not_current_texture_or_proxy","parent_eq":false})
    }

    fn command_trace_native_context(&self)->serde_json::Value {
        use serde_json::json;
        let key=self.pass_key.as_ref().map(PassKey::words);
        let mut attachments=Vec::new();
        if self.pass.is_some() {
            if let (Some(key),Some(info))=(key,self.pass_info.as_ref()) {
                for slot in 0..8 {
                    let Some(format)=info.rt_formats.get(slot).copied().flatten() else { continue; };
                    let q=slot*4; let id=key[q];
                    let view=self.tex(id).and_then(|t|t.views.get(&(format,key[q+1],key[q+2])));
                    attachments.push(json!({"aspect":"color","slot":slot,"guest_id":id,"mip":key[q+1],"slice":key[q+2],
                        "format":format!("{format:?}"),"retained_parent":view.map(|view|self.command_trace_view_parent(id,view)),
                        "attachment_basis":"retained pass key and cached native attachment; texture lifetime repair retires affected pass"}));
                }
                if let Some(format)=info.ds_format {
                    let id=key[32];
                    let view=self.tex(id).and_then(|t|t.views.get(&(format,key[33],key[34])));
                    attachments.push(json!({"aspect":"depth_stencil","guest_id":id,"mip":key[33],"slice":key[34],
                        "format":format!("{format:?}"),"retained_parent":view.map(|view|self.command_trace_view_parent(id,view)),
                        "attachment_basis":"retained pass key and cached native attachment; texture lifetime repair retires affected pass"}));
                }
            }
        }
        // Enumerate deterministically with bounded retention, even if the live
        // host map contains more subresources than the report can retain.
        let mut pending=BTreeMap::new(); let mut count=0usize;
        for (&id,subresources) in &self.pending_clears {
            for (index,&(mip,slice,clear)) in subresources.iter().enumerate() {
                count+=1; pending.insert((id,mip,slice,index),clear);
                if pending.len()>64 { pending.pop_last(); }
            }
        }
        json!({"capture_phase":"pre_exec","encoder_epoch":self.enc_epoch,"pass_active":self.pass.is_some(),
            "pass_key":key,"pass_info":self.pass_info.as_ref().map(|info|json!({"width":info.width,"height":info.height,
                "samples":info.samples,"read_only_depth":info.ro_depth,"read_only_stencil":info.ro_stencil})),
            "attachments":attachments,"pipeline_host_uid":self.pass_st.pl,"bind_group_host_uids":self.pass_st.bg,
            "applied_viewport":self.pass_st.vp,"applied_scissor":self.pass_st.sc,"applied_blend_constant":self.pass_st.bf,
            "applied_viewport_f32_bits":self.pass_st.vp.map(|values|values.map(f32::to_bits)),
            "applied_blend_constant_f64_bits":self.pass_st.bf.map(|values|values.map(f64::to_bits)),
            "applied_stencil_reference":self.pass_st.sref,
            "pending_clears":pending.into_iter().map(|((id,mip,slice,index),clear)|json!({"guest_id":id,"mip":mip,"slice":slice,"queue_index":index,
                "current_target_native_allocation":self.tex(id).map(|t|trace_native_texture(t.trace_native_identity,"texture")),
                "color_f64_bits":clear.color.map(|c|[c.r.to_bits(),c.g.to_bits(),c.b.to_bits(),c.a.to_bits()]),
                "depth_f32_bits":clear.depth.map(f32::to_bits),"stencil":clear.stencil,
                "association":"pending clear owns no native handle; current guest-ID target only"})).collect::<Vec<_>>(),
            "pending_clears_observed":count,"pending_clears_omitted":count.saturating_sub(64),
            "native_graph_complete":false,"actual_gpu_execution_certified":false})
    }

    fn reset_state(&mut self) {
        self.pdirty = true;
        self.s = State::new();
        self.end_pass();
    }

    fn destroy(&mut self, id: u32) {
        self.pending_clears.remove(&id);
        self.invalidate_world_depth(id, serde_json::json!("destroy"));
        if self.is_bound(id) { self.pdirty = true; }
        if self.obj(id).is_none() { return; }
        if self.used(id) { self.flush(); }
        let texture_lifetime = matches!(self.obj(id), Some(Obj::Tex(_)));
        if texture_lifetime { self.invalidate_texture_views(id); }
        if texture_lifetime || self.bind_group_prepare.enabled {
            if texture_lifetime || binding_cache_needs_invalidation(self.obj(id), None) {
                self.bind_groups.clear();
                self.bind_group_prepare.invalidated();
            } else if let Some(kind) = self.obj(id).and_then(Obj::nonbinding_kind) {
                self.bind_group_prepare.skipped_destroy(kind);
            }
        }
        if self.occlusion_active == Some(id) { self.end_active_occlusion(); }
        if let Some(o) = self.objs.get_mut(id as usize).and_then(|o| o.take()) {
            match o {
                Obj::Tex(t) => t.tex.destroy(),
                Obj::Buf(b) => {
                    self.upload_dedup.forget(b.dedup);
                    if let Some(g) = b.gpu { self.uploads.destroy(g); }
                }
                _ => {}
            }
        }
    }
    fn is_bound(&self, id: u32) -> bool {
        let s = &self.s;
        s.layout == id || s.blend.0 == id || s.dss.0 == id || s.rs == id || s.ib.0 == id || s.shader.contains(&id) || s.vb.iter().any(|v| v.buf == id)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_texture(&mut self, id: u32, dim: u32, w: u32, h: u32, d: u32, mips: u32, array: u32, dxgi: u32, bind: u32, samples: u32) {
        let mut m = map_format(dxgi, bind);
        let bc_source = if is_bc(m.format) && !self.device.features().contains(Features::TEXTURE_COMPRESSION_BC) {
            let source = m.format;
            m.format = crate::bc::decoded_format(source).expect("known BC format");
            m.views = m.views.iter().filter_map(|f| crate::bc::decoded_format(*f)).collect();
            log_once("bc_decode", "adapter has no BC compression; decoding texture uploads on CPU (including HDR and signed formats)");
            Some(source)
        } else { None };
        let mut usage = TextureUsages::COPY_SRC | TextureUsages::COPY_DST;
        if bind & BIND_SRV != 0 { usage |= TextureUsages::TEXTURE_BINDING; }
        if bind & (BIND_RT | BIND_DS) != 0 { usage |= TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING; }
        if bind & BIND_UAV != 0 { usage |= TextureUsages::STORAGE_BINDING; }
        if bind & (BIND_SRV | BIND_RT | BIND_DS | BIND_UAV) == 0 { usage |= TextureUsages::TEXTURE_BINDING; }
        let samples = samples.max(1);
        if samples > 1 { usage &= !(TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::STORAGE_BINDING); }

        let allowed = self.adapter.get_texture_format_features(m.format).allowed_usages;
        usage &= allowed | TextureUsages::COPY_SRC | TextureUsages::COPY_DST;
        let dimension = match dim { 3 => TextureDimension::D3, 1 => TextureDimension::D1, _ => TextureDimension::D2 };
        let size = Extent3d { width: w.max(1), height: if dim == 1 { 1 } else { h.max(1) }, depth_or_array_layers: if dim == 3 { d.max(1) } else { array.max(1) } };
        let views = m.views.clone();
        let desc = TextureDescriptor {
            label: None, size, mip_level_count: mips.max(1), sample_count: samples, dimension, format: m.format, usage, view_formats: &views,
        };
        let tex = match self.scoped(&format!("createTexture id={id} dim={dim} {}x{}x{} mips={mips} dxgi={dxgi} bind=0x{bind:x} samples={samples} -> {:?} usage {:?}", size.width, size.height, size.depth_or_array_layers, m.format, usage), |d| d.create_texture(&desc)) { Some(t) => t, None => return };
        let trace_native_identity=crate::world_command_trace::native_texture_tag(self.command_trace.captures_native_identities());
        self.set_obj(id, Obj::Tex(Tex {
            tex, trace_native_identity, desc: TexDesc { w: size.width, h: size.height, d: size.depth_or_array_layers, mips: mips.max(1), samples, dim: dimension },
            format: m.format, bc_source, views: HashMap::new(), a8: m.a8, enc: 0, in_bg: false, proxy_depth: None, proxy_stencil: None,
            proxy_fdepth: None, blit_bg: None, mip_passes: None,
        }));
    }

    fn create_buffer(&mut self, id: u32, size: u32, bind: u32, misc: u32, stride: u32) {
        let size = size as u64;
        let identity = self.uid();
        let mut b = Buf { identity, size, stride, gpu: None, bytes: 0, shadow: None, version: 0, enc: 0, in_bg: false, counter: None,
            dedup: crate::upload_dedup::Track::Untracked };
        if bind & 4 != 0 { b.shadow = Some(vec![0u8; size as usize]); }
        let mut usage = BufferUsages::COPY_DST | BufferUsages::COPY_SRC;
        if bind & 1 != 0 { usage |= BufferUsages::VERTEX; }
        if bind & 2 != 0 { usage |= BufferUsages::INDEX; }
        if bind & (8 | 0x80) != 0 { usage |= BufferUsages::STORAGE; }
        if misc & 0x10 != 0 && self.adapter.get_downlevel_capabilities().flags.contains(DownlevelFlags::INDIRECT_EXECUTION) {
            usage |= BufferUsages::INDIRECT;
        }
        if bind & (1 | 2 | 8 | 0x80) != 0 || misc & 0x10 != 0 {
            b.bytes = ((size + 3) & !3).max(4);
            b.gpu = Some(self.device.create_buffer(&BufferDescriptor { label: None, size: b.bytes, usage, mapped_at_creation: false }));
        }
        b.dedup = self.upload_dedup.track_new(bind, misc, b.gpu.is_some(), b.shadow.is_some());
        self.set_obj(id, Obj::Buf(b));
    }

    fn upload_buffer(&mut self, id: u32, offset: u64, size: u64, data_addr: u64, no_overwrite: bool) {
        let data = self.bytes(data_addr, size).to_vec();
        let used = self.used(id);
        let Some(b) = self.buf_mut(id) else { return };
        let mut version_wrapped = false;
        if let Some(sh) = b.shadow.as_mut() {
            let end = ((offset + size) as usize).min(sh.len());
            if (offset as usize) < end { sh[offset as usize..end].copy_from_slice(&data[..end - offset as usize]); }
            let (version, wrapped) = b.version.overflowing_add(1);
            b.version = version;
            version_wrapped = wrapped;
        }
        let (gpu, track, gpu_bytes) = (b.gpu.clone(), b.dedup, b.bytes);
        // End the buffer borrow before touching either feature's metadata.
        // CB-only wrap invalidation must happen even without GPU storage.
        if version_wrapped { self.uniform_snapshot.version_wrapped(); }
        let Some(g) = gpu else { return };
        if !no_overwrite { self.upload_dedup.op12(track); }
        if !no_overwrite && used { self.flush(); }
        let source = if no_overwrite { UPLOAD_OP47 } else { UPLOAD_OP12 };
        // An op47 repeating the recorded range and bytes of this very object
        // would rewrite what the GPU already holds: only that write is skipped.
        // Anything else takes the route below, and a compared miss is recorded
        // after its write.
        let record = if no_overwrite {
            match self.upload_dedup.op47(track, offset, &data, gpu_bytes) {
                crate::upload_dedup::Op47::Skip => return,
                crate::upload_dedup::Op47::Record(lifetime) => Some(lifetime),
                crate::upload_dedup::Op47::Legacy => None,
            }
        } else { None };
        let n = size & !3;
        if n > 0 { measured_write_buffer(&mut self.uploads, &self.device, &self.queue, &mut self.buffer_uploads[source], &g, offset, &data[..n as usize]); }
        if size & 3 != 0 {
            let mut tail = [0u8; 4];
            tail[..(size - n) as usize].copy_from_slice(&data[n as usize..]);
            measured_write_buffer(&mut self.uploads, &self.device, &self.queue, &mut self.buffer_uploads[source], &g, offset + n, &tail);
        }
        if let Some(lifetime) = record { self.upload_dedup.store(lifetime, offset, data); }
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_texture(&mut self, id: u32, mip: u32, slice: u32, x: u32, y: u32, z: u32, w: u32, h: u32, d: u32, bpr: u32, rows: u32, size: u64, data_addr: u64) {
        if self.tex(id).is_none() { return; }
        self.invalidate_world_depth(id, serde_json::json!("upload_texture"));
        if self.pending_clears.contains_key(&id) { self.materialize_clears(id); self.flush(); }
        if self.used(id) { self.flush(); }
        let t = self.tex_mut(id).unwrap();
        if let Some(p) = t.proxy_fdepth.as_mut() { p.dirty = true; }
        let three = t.desc.dim == TextureDimension::D3;
        let (tex, a8, bc_source, desc) = (t.tex.clone(), t.a8, t.bc_source, t.desc);
        let mut data = self.bytes(data_addr, size).to_vec();
        let mut bpr = bpr;
        let mut rows = rows;
        let (mut w, mut h) = (w, h);
        if let Some(format) = bc_source {
            // BC uploads describe complete 4x4 blocks even for 1x1/2x2 mips.
            // Decoded textures have logical texel bounds, so crop the padding.
            w = w.min((desc.w >> mip).max(1).saturating_sub(x));
            h = h.min((desc.h >> mip).max(1).saturating_sub(y));
            match crate::bc::decode(format, &data, w, h, d.max(1), bpr, rows) {
                Ok(decoded) => { data = decoded.bytes; bpr = decoded.bytes_per_row; rows = decoded.rows_per_image; }
                Err(error) => { log(&format!("BC texture {id} upload failed: {error:#}")); return; }
            }
        }
        if a8 {
            let mut out = vec![0u8; (w * h * 4) as usize];
            for r in 0..h as usize {
                for c in 0..w as usize {
                    out[(r * w as usize + c) * 4 + 3] = data.get(r * bpr as usize + c).copied().unwrap_or(0);
                }
            }
            data = out;
            bpr = w * 4;
        }
        self.queue.write_texture(
            TexelCopyTextureInfo { texture: &tex, mip_level: mip, origin: Origin3d { x, y, z: if three { z } else { slice } }, aspect: TextureAspect::All },
            &data,
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(rows.max(1)) },
            Extent3d { width: w, height: h, depth_or_array_layers: d.max(1) },
        );
    }

    fn create_shader(&mut self, p: u64) {
        let id = self.u(p);
        let stage = self.u(p + 1);
        let hash = format!("{:08x}{:08x}", self.u(p + 3), self.u(p + 2));
        let nin = self.u(p + 4) as u64;
        let sig = (0..nin).map(|i| SigEl { sem: self.u(p + 5 + i * 4), idx: self.u(p + 6 + i * 4), reg: self.u(p + 7 + i * 4) }).collect();
        self.set_obj(id, Obj::Shader(ShaderObj { stage, hash, sig, program: None }));
    }

    fn create_layout(&mut self, p: u64) {
        let id = self.u(p);
        let n = self.u(p + 1) as u64;
        let els: Vec<El> = (0..n)
            .map(|i| {
                let q = p + 2 + i * 7;
                El { sem: self.u(q), idx: self.u(q + 1), fmt: self.u(q + 2), slot: self.u(q + 3), offset: self.u(q + 4), cls: self.u(q + 5) }
            })
            .collect();
        let mut slots = Vec::new();
        let mut seen = 0u32;
        for e in &els {
            if e.slot < 32 && (seen >> e.slot) & 1 == 0 {
                seen |= 1 << e.slot;
                slots.push((e.slot, e.cls));
            }
        }
        self.set_obj(id, Obj::Layout(Rc::new(LayoutObj { els, slots })));
    }

    fn create_state(&mut self, p: u64, n: u64) {
        let id = self.u(p);
        let kind = self.u(p + 1);
        let words: Vec<u32> = (2..n).map(|i| self.u(p + i)).collect();
        let sampler = if kind == 3 { Some(self.make_sampler(&words)) } else { None };
        self.set_obj(id, Obj::State(Rc::new(StateObj { words, sampler })));
    }

    fn make_sampler(&self, w: &[u32]) -> Sampler {
        let g = |i: usize| w.get(i).copied().unwrap_or(0);
        let filter = g(0);
        let aniso = (filter & 0x7f) == 0x55;
        let lin = |b: bool| if b { FilterMode::Linear } else { FilterMode::Nearest };
        let lin_m = |b: bool| if b { MipmapFilterMode::Linear } else { MipmapFilterMode::Nearest };
        let lod_min = wf(g(11)).max(0.0);
        let mut lod_max = wf(g(12)).clamp(0.0, 32.0);
        if lod_max < lod_min { lod_max = lod_min; }
        let mut d = SamplerDescriptor {
            label: None,
            address_mode_u: address(g(1)), address_mode_v: address(g(2)), address_mode_w: address(g(3)),
            mag_filter: lin((filter >> 2) & 1 != 0), min_filter: lin((filter >> 4) & 1 != 0), mipmap_filter: lin_m(filter & 1 != 0),
            lod_min_clamp: lod_min, lod_max_clamp: lod_max,
            compare: if filter & 0x80 != 0 { Some(cmp(g(6)).unwrap_or(CompareFunction::Always)) } else { None },
            anisotropy_clamp: 1, border_color: None,
        };
        if aniso {
            d.mag_filter = FilterMode::Linear;
            d.min_filter = FilterMode::Linear;
            d.mipmap_filter = MipmapFilterMode::Linear;
            d.anisotropy_clamp = g(5).clamp(1, 16) as u16;
        }
        self.device.create_sampler(&d)
    }

    fn create_view(&mut self, p: u64, uav: bool) {
        let v = ViewObj {
            res: self.u(p + 2), is_buf: self.u(p + 1) == 1, fmt: self.u(p + 3), vdim: self.u(p + 4),
            a: self.u(p + 5), b: self.u(p + 6), c: self.u(p + 7), d: self.u(p + 8), views: HashMap::new(),
        };
        let id = self.u(p);
        self.set_obj(id, if uav { Obj::Uav(v) } else { Obj::Srv(v) });
    }

    fn shader_program(&mut self, id: u32) -> Option<Rc<Program>> {
        let (hash, stage) = match self.obj(id) {
            Some(Obj::Shader(s)) => {
                if let Some(p) = &s.program { return p.clone(); }
                (s.hash.clone(), s.stage)
            }
            _ => return None,
        };
        let prog = self.load_program(&hash, stage);
        if let Some(Obj::Shader(s)) = self.obj_mut(id) { s.program = Some(prog.clone()); }
        prog
    }

    fn load_program(&mut self, hash: &str, stage: u32) -> Option<Rc<Program>> {
        let rec = self.shader_index.get(hash).cloned();
        let ok = rec.as_ref().and_then(|r| r.get("ok")).and_then(|v| v.as_bool()).unwrap_or(false);
        let label = rec.as_ref().map(|r| format!("{}::{}", r["effect"].as_str().unwrap_or("?"), r["program"].as_str().unwrap_or("?"))).unwrap_or_else(|| hash.to_string());
        if !ok {
            log_once(&format!("noshader{hash}"), &format!("no WGSL for {} shader {hash} ({label})", ["vs", "ps", "cs"].get(stage as usize).unwrap_or(&"?")));
            return None;
        }
        let source = match std::fs::read_to_string(self.shader_dir.join(format!("{hash}.wgsl"))) {
            Ok(s) => s,
            Err(e) => { log_once(&format!("nosrc{hash}"), &format!("shader source {hash}: {e}")); return None; }
        };
        let (text, pack) = pack_uniforms(&source);
        let module = self.scoped(&format!("shader {label}"), |d| d.create_shader_module(ShaderModuleDescriptor { label: Some(&label), source: ShaderSource::Wgsl(text.as_str().into()) }))?;
        let info = parse_wgsl(&text);
        let mut consts = None;
        if rec.as_ref().and_then(|r| r.get("consts")).map(|c| !c.is_null() && c != &serde_json::Value::Bool(false)).unwrap_or(false) {
            if let Ok(t) = std::fs::read_to_string(self.shader_dir.join(format!("{hash}.consts.json"))) {
                let arr: Vec<serde_json::Value> = serde_json::from_str(&t).unwrap_or_default();
                let n = arr.len().max(1);
                let mut data = vec![0u32; n * 4];
                for (i, v) in arr.iter().enumerate() {
                    data[i * 4] = v.as_i64().map(|x| x as u32).or_else(|| v.as_f64().map(|x| x as i64 as u32)).unwrap_or(0);
                }
                let bytes: Vec<u8> = data.iter().flat_map(|x| x.to_le_bytes()).collect();
                let b = self.device.create_buffer(&BufferDescriptor { label: None, size: (bytes.len() as u64).max(16), usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST, mapped_at_creation: false });
                measured_write_buffer(&mut self.uploads, &self.device, &self.queue, &mut self.buffer_uploads[UPLOAD_SHADER_CONSTS], &b, 0, &bytes);
                consts = Some(b);
            }
        }
        let flat = RE_FLAT.captures_iter(&text).filter_map(|m| m[1].parse().ok()).collect();
        let mut group_bindings: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
        for b in &info.bindings { group_bindings.entry(b.group).or_default().push(b.clone()); }
        for v in group_bindings.values_mut() { v.sort_by_key(|b| b.binding); }
        let uid = self.uid();
        let vid = text.contains("vertex_index");
        let iid = text.contains("instance_index");
        let out_locs = if stage == 0 { locations_in(&text, true) } else { vec![] };
        let in_locs = if stage == 1 { locations_in(&text, false) } else { vec![] };
        let clip = text.contains("@builtin(clip_distances)");
        Some(Rc::new(Program { hud_kind:crate::xr_hud::certify_shader(hash,&source), module, info, consts, pack, text, flat, group_bindings, variants: RefCell::new(HashMap::new()), label, uid, vid, iid, out_locs, in_locs, clip }))
    }

    fn variant(&self, prog: &Program, key: &str, make: impl FnOnce(&str) -> String) -> Option<ShaderModule> {
        if let Some(m) = prog.variants.borrow().get(key) { return m.clone(); }
        let text = make(&prog.text);
        let label = format!("{} [{key}]", prog.label);
        let m = self.scoped(&format!("shader variant {label}"), |d| d.create_shader_module(ShaderModuleDescriptor { label: Some(&label), source: ShaderSource::Wgsl(text.as_str().into()) }));
        prog.variants.borrow_mut().insert(key.to_string(), m.clone());
        m
    }

    fn float_sampling_program(&mut self, prog: Rc<Program>, stage: usize) -> Option<Rc<Program>> {
        if self.device.features().contains(Features::FLOAT32_FILTERABLE) { return Some(prog); }
        let selected: Vec<(u32, u32)> = prog.info.bindings.iter().filter_map(|binding| {
            if binding.kind != Kind::Texture || binding.sample != Sample::Float
                || !matches!(binding.dim, TextureViewDimension::D2 | TextureViewDimension::D3) || binding.ms { return None; }
            let id = *self.s.srv[stage].get((binding.binding as usize).wrapping_sub(32))?;
            let view = self.view_obj(id)?;
            if view.is_buf { return None; }
            let texture = self.tex(view.res)?;
            let format = if is_depth(texture.format) { TextureFormat::R32Float } else { texture.format };
            matches!(format, TextureFormat::R32Float | TextureFormat::Rg32Float | TextureFormat::Rgba32Float)
                .then_some((binding.group, binding.binding))
        }).collect();
        if selected.is_empty() { return Some(prog); }

        let samplers: Vec<_> = prog.info.bindings.iter().filter(|binding| binding.kind == Kind::Sampler).map(|binding| {
            let sid = *self.s.smp[stage].get((binding.binding as usize).wrapping_sub(16)).unwrap_or(&0);
            let state = self.state(sid);
            // Match stage_bind_group's fallback sampler: clamp/nearest and LOD
            // range 0..32. A comparison sampler is rejected there for this slot.
            let words = state.filter(|state| state.words.first().copied().unwrap_or(0) & 0x80 == 0)
                .map(|state| state.words.clone()).unwrap_or_else(|| {
                    let mut words = vec![0; 13];
                    words[1] = 3; words[2] = 3; words[3] = 3; words[12] = 32.0f32.to_bits();
                    words
                });
            crate::float_sampling::SamplerState { group: binding.group, binding: binding.binding, words }
        }).collect();
        let mut state_key = Vec::new();
        for &(group, binding) in &selected { state_key.extend([group, binding]); }
        state_key.push(u32::MAX);
        for sampler in &samplers {
            state_key.extend([sampler.group, sampler.binding, sampler.words.len() as u32]);
            state_key.extend_from_slice(&sampler.words);
        }
        let key = (prog.uid, state_key);
        if let Some(cached) = self.float_programs.get(&key) { return cached.clone(); }
        let result = (|| {
            let rewritten = match crate::float_sampling::rewrite(&prog.text, &selected, &samplers) {
                Ok(rewritten) => rewritten,
                Err(error) => {
                    log_once(&format!("float32{}{:?}", prog.uid, key.1), &format!("{}: float32 sampling fallback failed: {error:#}", prog.label));
                    return None;
                }
            };
            let label = format!("{} [float32 textureLoad]", prog.label);
            let module = self.scoped(&format!("shader variant {label}"), |device| device.create_shader_module(ShaderModuleDescriptor {
                label: Some(&label), source: ShaderSource::Wgsl(rewritten.text.as_str().into()),
            }))?;
            let mut info = parse_wgsl(&rewritten.text);
            info.bindings.retain(|binding| !rewritten.removed_samplers.contains(&(binding.group, binding.binding)));
            for binding in &mut info.bindings {
                if selected.contains(&(binding.group, binding.binding)) { binding.sample = Sample::Unfilterable; }
            }
            let mut groups: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
            for binding in &info.bindings { groups.entry(binding.group).or_default().push(binding.clone()); }
            for bindings in groups.values_mut() { bindings.sort_by_key(|binding| binding.binding); }
            log_once(&format!("float32variant{}{:?}", prog.uid, key.1), &format!("{}: {} float32 sample call(s) use textureLoad; bindings {:?}", prog.label, rewritten.rewritten_calls, selected));
            Some(Rc::new(Program {
                hud_kind:prog.hud_kind, module, info, consts: prog.consts.clone(), pack: prog.pack.clone(), text: rewritten.text,
                flat: prog.flat.clone(), group_bindings: groups, variants: RefCell::new(HashMap::new()),
                label, uid: self.uid(), vid: prog.vid, iid: prog.iid,
                out_locs: prog.out_locs.clone(), in_locs: prog.in_locs.clone(), clip: prog.clip,
            }))
        })();
        self.float_programs.insert(key, result.clone());
        result
    }

    fn inter_stage_remap(&self, vs: &Program, ps: Option<&Program>) -> Option<(HashMap<u32, u32>, String, bool)> {
        let max = self.device.limits().max_inter_stage_shader_variables.max(16);
        let lim = if vs.clip { max - 1 } else { max };
        if !vs.out_locs.iter().any(|&l| l >= lim) { return None; }
        let ins = ps.map(|p| p.in_locs.clone()).unwrap_or_default();
        let mut used: std::collections::HashSet<u32> = vs.out_locs.iter().chain(ins.iter()).copied().filter(|&l| l < lim).collect();
        let mut map = HashMap::new();
        let mut free = 0;
        for &l in &vs.out_locs {
            if l < lim { continue; }
            while used.contains(&free) { free += 1; }
            if free >= lim { return None; }
            map.insert(l, free);
            used.insert(free);
        }
        let mut pairs: Vec<_> = map.iter().map(|(a, b)| format!("{a}>{b}")).collect();
        pairs.sort();
        let ps_hit = ins.iter().any(|l| map.contains_key(l));
        Some((map, pairs.join(","), ps_hit))
    }

    fn vertex_module(&self, vs: &Program, flat: &[u32], remap: Option<&(HashMap<u32, u32>, String, bool)>) -> Option<ShaderModule> {
        if flat.is_empty() && remap.is_none() { return Some(vs.module.clone()); }
        let key = format!("flat {}{}", flat.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","), remap.map(|r| format!("|{}", r.1)).unwrap_or_default());
        self.variant(vs, &key, |text| {
            let t = RE_VS_LOC_MEMBER.replace_all(text, |c: &regex::Captures| {
                let loc: u32 = c[1].parse().unwrap_or(0);
                if flat.contains(&loc) { format!("@location({}u) @interpolate(flat){}", &c[1], &c[2]) } else { c[0].to_string() }
            }).into_owned();
            match remap { Some(r) => remap_locations(&t, true, &r.0), None => t }
        })
    }

    fn pixel_module(&self, ps: &Program, a2c: bool, remap: Option<&(HashMap<u32, u32>, String, bool)>, hud:Option<crate::xr_hud::HudBlend>) -> Option<ShaderModule> {
        let remap = remap.filter(|r| r.2);
        if remap.is_none() && !a2c && hud.is_none() { return Some(ps.module.clone()); }
        let key = format!("{}{}", remap.map(|r| format!("remap|{}", r.1)).unwrap_or_default(), if a2c { "|a2c" } else { "" })+&format!("|hud{hud:?}");
        let m = self.variant(ps, &key, |text| {
            let mut t = text.to_string();
            if a2c { t = RE_MAIN_INNER.replace(&t, "$1\n  if (o0.w < 0.5f) { discard; }").into_owned(); }
            if let Some(mode)=hud { t=crate::xr_hud::augment_fragment(&t,mode).unwrap_or_else(||"invalid hud shader certificate".to_owned()); }
            match remap { Some(r) => remap_locations(&t, false, &r.0), None => t }
        });
        if m.is_none() && remap.is_none() && hud.is_none() { Some(ps.module.clone()) } else { m }
    }

    fn layout_entry(b: &Binding, stage: u32) -> BindGroupLayoutEntry {
        let visibility = match stage { 0 => ShaderStages::VERTEX, 1 => ShaderStages::FRAGMENT, _ => ShaderStages::COMPUTE };
        let ty = match b.kind {
            Kind::Uniform => BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: b.binding != 15, min_binding_size: None },
            Kind::Storage => BindingType::Buffer { ty: BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None },
            Kind::ReadOnlyStorage => BindingType::Buffer { ty: BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
            Kind::Sampler => BindingType::Sampler(SamplerBindingType::Filtering),
            Kind::SamplerCmp => BindingType::Sampler(SamplerBindingType::Comparison),
            Kind::Texture => BindingType::Texture {
                sample_type: match b.sample {
                    Sample::Float => TextureSampleType::Float { filterable: true },
                    Sample::Unfilterable => TextureSampleType::Float { filterable: false },
                    Sample::Uint => TextureSampleType::Uint,
                    Sample::Sint => TextureSampleType::Sint,
                    Sample::Depth => TextureSampleType::Depth,
                },
                view_dimension: b.dim,
                multisampled: b.ms,
            },
            Kind::StorageTexture => BindingType::StorageTexture { access: b.access, format: b.format.unwrap_or(TextureFormat::Rgba8Unorm), view_dimension: b.dim },
        };
        BindGroupLayoutEntry { binding: b.binding, visibility, ty, count: None }
    }

    fn bgl_for(&mut self, prog: &Program, stage: u32, group: u32) -> (BindGroupLayout, u64) {
        let numeric_key = (prog.uid, stage, group);
        let legacy_key = if self.render_prepare.enabled { None }
            else { Some((prog.label.clone() + "#" + &prog.uid.to_string(), stage, group)) };
        let cached = if self.render_prepare.enabled { self.numeric_bgl_cache.get(&numeric_key) }
            else { self.bgl_cache.get(legacy_key.as_ref().unwrap()) };
        self.render_prepare.bgl_lookup(cached.is_some());
        if let Some(b) = cached { return b.clone(); }
        let entries: Vec<_> = prog.info.bindings.iter().filter(|b| b.group == group).map(|b| Self::layout_entry(b, stage)).collect();
        let bgl = self.device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: None, entries: &entries });
        let uid = self.uid();
        if self.render_prepare.enabled { self.numeric_bgl_cache.insert(numeric_key, (bgl.clone(), uid)); }
        else { self.bgl_cache.insert(legacy_key.unwrap(), (bgl.clone(), uid)); }
        (bgl, uid)
    }
    fn empty_bgl(&mut self) -> BindGroupLayout {
        if self.empty_bgl.is_none() {
            let b = self.device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: None, entries: &[] });
            let uid = self.uid();
            self.empty_bgl = Some((b, uid));
        }
        self.empty_bgl.as_ref().unwrap().0.clone()
    }

    fn target_view(&mut self, id: u32, dxgi: u32, mip: u32, slice: u32) -> Option<(TextureView, TextureFormat)> {
        let t = self.tex(id)?;
        let fmt = if dxgi != 0 {
            let original = map_format(dxgi, 0).format;
            if t.bc_source.is_some() { crate::bc::decoded_format(original).unwrap_or(original) } else { original }
        } else { t.format };
        let fmt = if is_depth(t.format) { t.format } else { fmt };
        if let Some(v) = t.views.get(&(fmt, mip, slice)) { return Some((v.clone(), fmt)); }
        let three = t.desc.dim == TextureDimension::D3;
        let tex = t.tex.clone();
        let v = self.scoped("target view", |_| tex.create_view(&TextureViewDescriptor {
            format: Some(fmt), dimension: Some(if three { TextureViewDimension::D3 } else { TextureViewDimension::D2 }),
            base_mip_level: mip, mip_level_count: Some(1), base_array_layer: if three { 0 } else { slice }, array_layer_count: Some(1),
            ..Default::default()
        }))?;
        self.tex_mut(id).unwrap().views.insert((fmt, mip, slice), v.clone());
        Some((v, fmt))
    }

    fn mark_color_dirty(&mut self, id: u32) {
        if let Some(t) = self.tex_mut(id) { if let Some(p) = t.proxy_fdepth.as_mut() { p.dirty = true; } }
    }
    fn mark_depth_dirty(&mut self, id: u32) {
        if let Some(t) = self.tex_mut(id) {
            if let Some(p) = t.proxy_depth.as_mut() { p.dirty = true; }
            if let Some(p) = t.proxy_stencil.as_mut() { p.dirty = true; }
        }
    }

    fn ensure_pass(&mut self) -> bool {
        let key = self.render_prepare.pass_key(&self.s.rt, &self.s.ds);
        if self.pass.is_some() && self.pass_key.as_ref() == Some(&key) {
            self.render_prepare.pass_reused();
            return true;
        }
        self.end_pass();
        let mut colors: Vec<Option<(TextureView, TextureFormat)>> = Vec::new();
        let mut samples = 1;
        let mut any = false;
        let (mut pw, mut ph) = (1, 1);
        for i in 0..8 {
            let r = self.s.rt[i].clone();
            if r.tex == 0 || self.tex(r.tex).is_none() { colors.push(None); continue; }
            match self.target_view(r.tex, r.fmt, r.mip, r.slice) {
                Some(v) => {
                    self.mark_color_dirty(r.tex);
                    let t = self.tex(r.tex).unwrap();
                    samples = t.desc.samples;
                    pw = (t.desc.w >> r.mip).max(1);
                    ph = (t.desc.h >> r.mip).max(1);
                    colors.push(Some(v));
                    any = true;
                    self.use_tex(r.tex);
                }
                None => colors.push(None),
            }
        }
        while matches!(colors.last(), Some(None)) { colors.pop(); }
        if self.hud_draw.is_some() {
            let Some(texture)=self.hud_capture.as_ref().and_then(|h|h.images.as_ref()).map(|images|&images.contribution) else{return false;};
            colors.push(Some((texture.create_view(&TextureViewDescriptor::default()),TextureFormat::Rgba16Float)));
        }
        let ds = self.s.ds.clone();
        let mut ds_att = None;
        let mut ds_format = None;
        let (ro_d, ro_s) = (ds.flags & 1 != 0, ds.flags & 2 != 0);
        if ds.tex != 0 && self.tex(ds.tex).is_some() {
            if let Some((v, f)) = self.target_view(ds.tex, 0, ds.mip, ds.slice) {
                // Each aspect can change independently. A depth-writing pass
                // often has read-only stencil, and must invalidate its proxy.
                if let Some(t) = self.tex_mut(ds.tex) {
                    if !ro_d { if let Some(proxy) = &mut t.proxy_depth { proxy.dirty = true; } }
                    if !ro_s { if let Some(proxy) = &mut t.proxy_stencil { proxy.dirty = true; } }
                }
                let t = self.tex(ds.tex).unwrap();
                ds_format = Some(f);
                samples = t.desc.samples;
                if !self.s.rt.iter().any(|r| r.tex != 0) { pw = (t.desc.w >> ds.mip).max(1); ph = (t.desc.h >> ds.mip).max(1); }
                ds_att = Some((v, f));
                any = true;
                self.use_tex(ds.tex);
            }
        }
        if !any { return false; }

        if (ro_d || ro_s) && ds_att.is_some() && self.pending_clears.contains_key(&ds.tex) { self.materialize_clears(ds.tex); }

        let take = |me: &mut Self, tex: u32, mip: u32, slice: u32| -> Option<PendingClear> {
            let list = me.pending_clears.get_mut(&tex)?;
            let i = list.iter().position(|e| e.0 == mip && e.1 == slice)?;
            let c = list.swap_remove(i).2;
            if list.is_empty() { me.pending_clears.remove(&tex); }
            me.clears_folded += 1;
            Some(c)
        };
        let mut color_loads = Vec::with_capacity(colors.len());
        for (i, c) in colors.iter().enumerate() {
            let r = self.s.rt[i].clone();
            let load = match c {
                Some(_) => match take(self, r.tex, r.mip, r.slice) {
                    Some(pc) => {
                        pc.color.map(LoadOp::Clear).unwrap_or(LoadOp::Load)
                    }
                    None => LoadOp::Load,
                },
                None => LoadOp::Load,
            };
            color_loads.push(load);
        }
        let ds_clear = if ds_att.is_some() { take(self, ds.tex, ds.mip, ds.slice) } else { None };
        let color_atts: Vec<Option<RenderPassColorAttachment>> = colors.iter().zip(&color_loads).map(|(c, load)| c.as_ref().map(|(v, _)| RenderPassColorAttachment {
            view: v, depth_slice: None, resolve_target: None, ops: Operations { load: *load, store: StoreOp::Store },
        })).collect();
        let dsa = ds_att.as_ref().map(|(v, f)| RenderPassDepthStencilAttachment {
            view: v,
            depth_ops: if ro_d { None } else { Some(Operations { load: ds_clear.and_then(|c| c.depth).map(LoadOp::Clear).unwrap_or(LoadOp::Load), store: StoreOp::Store }) },
            stencil_ops: if has_stencil(*f) && !ro_s { Some(Operations { load: ds_clear.and_then(|c| c.stencil).map(LoadOp::Clear).unwrap_or(LoadOp::Load), store: StoreOp::Store }) } else { None },
        });
        let occ = self.occlusion_set.clone();
        let pass = self.encoder().begin_render_pass(&RenderPassDescriptor {
            label: None, color_attachments: &color_atts, depth_stencil_attachment: dsa, timestamp_writes: None,
            occlusion_query_set: occ.as_ref(), multiview_mask: None,
        }).forget_lifetime();
        self.pass = Some(pass);
        self.passes += 1;
        self.pass_key = Some(key);
        self.pdirty = true;
        self.pass_info = Some(PassInfo {
            rt_formats: colors.iter().map(|c| c.as_ref().map(|x| x.1)).collect(),
            samples, ds_format, ro_depth: ds_att.is_some() && ro_d, ro_stencil: ds_att.is_some() && ro_s, width: pw, height: ph,
        });
        self.pass_st = PassSt::default();
        self.begin_occlusion_in_pass();
        true
    }

    fn proxy_pipeline(&mut self, kind: &'static str) -> RenderPipeline {
        if let Some(p) = self.proxy_pipelines.get(kind) { return p.clone(); }
        let code = if kind == "depth" {
            "@group(0) @binding(0) var src: texture_depth_2d;
             @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3)); return vec4f(p[i], 0, 1); }
             @fragment fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f { return vec4f(textureLoad(src, vec2i(pos.xy), 0), 0, 0, 1); }"
        } else {
            "@group(0) @binding(0) var src: texture_2d<u32>;
             @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3)); return vec4f(p[i], 0, 1); }
             @fragment fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4<u32> { let s = textureLoad(src, vec2i(pos.xy), 0).x; return vec4<u32>(s, s, 0u, 1u); }"
        };
        let module = self.device.create_shader_module(ShaderModuleDescriptor { label: None, source: ShaderSource::Wgsl(code.into()) });
        let fmt = if kind == "depth" { TextureFormat::R32Float } else { STENCIL_PROXY };
        let pl = self.simple_pipeline(&module, Some(fmt), None);
        self.proxy_pipelines.insert(kind, pl.clone());
        pl
    }

    fn simple_pipeline(&self, module: &ShaderModule, color: Option<TextureFormat>, depth: Option<DepthStencilState>) -> RenderPipeline {
        let targets = [color.map(|f| ColorTargetState { format: f, blend: None, write_mask: ColorWrites::ALL })];
        self.device.create_render_pipeline(&RenderPipelineDescriptor {
            label: None, layout: None,
            vertex: VertexState { module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: PrimitiveState::default(),
            depth_stencil: depth,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState { module, entry_point: Some("fs"), compilation_options: Default::default(), targets: if color.is_some() { &targets } else { &[] } }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn depth_proxy(&mut self, id: u32, stencil: bool) -> Option<(Texture, TextureFormat)> {
        self.materialize_clears(id);
        let pl = self.proxy_pipeline(if stencil { "stencil" } else { "depth" });
        let (desc, tex) = { let t = self.tex(id)?; (t.desc, t.tex.clone()) };
        let dev = self.device.clone();
        let trace_identities=self.command_trace.captures_native_identities();
        let t = self.tex_mut(id).unwrap();
        let slot = if stencil { &mut t.proxy_stencil } else { &mut t.proxy_depth };
        if slot.is_none() {
            let fmt = if stencil { STENCIL_PROXY } else { TextureFormat::R32Float };
            let ptex = dev.create_texture(&TextureDescriptor {
                label: None, size: Extent3d { width: desc.w, height: desc.h, depth_or_array_layers: desc.d }, mip_level_count: desc.mips, sample_count: 1,
                dimension: TextureDimension::D2, format: fmt, usage: TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC, view_formats: &[],
            });
            let mut passes = Vec::new();
            for mip in 0..desc.mips {
                for layer in 0..desc.d {
                    let src = tex.create_view(&TextureViewDescriptor {
                        dimension: Some(TextureViewDimension::D2), base_mip_level: mip, mip_level_count: Some(1), base_array_layer: layer, array_layer_count: Some(1),
                        aspect: if stencil { TextureAspect::StencilOnly } else { TextureAspect::DepthOnly }, ..Default::default()
                    });
                    let dst = ptex.create_view(&TextureViewDescriptor { base_mip_level: mip, mip_level_count: Some(1), base_array_layer: layer, array_layer_count: Some(1), dimension: Some(TextureViewDimension::D2), ..Default::default() });
                    let bg = dev.create_bind_group(&BindGroupDescriptor { label: None, layout: &pl.get_bind_group_layout(0), entries: &[BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&src) }] });
                    passes.push((dst, bg));
                }
            }
            let trace_native_identity=crate::world_command_trace::native_texture_tag(trace_identities);
            *slot = Some(Proxy { tex: ptex, trace_native_identity, format: fmt, dirty: true, passes });
        }
        let px = slot.as_mut().unwrap();
        let out = (px.tex.clone(), px.format);
        if px.dirty {
            px.dirty = false;
            let passes: Vec<_> = px.passes.iter().map(|(v, b)| (v.clone(), b.clone())).collect();
            self.end_pass();
            let enc = self.encoder();
            for (dst, bg) in &passes {
                let mut p = enc.begin_render_pass(&RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(RenderPassColorAttachment { view: dst, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Clear(Color::BLACK), store: StoreOp::Store } })],
                    depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
                });
                p.set_pipeline(&pl);
                p.set_bind_group(0, bg, &[]);
                p.draw(0..3, 0..1);
            }
            self.use_tex(id);
        }
        Some(out)
    }

    fn float_to_depth_proxy(&mut self, id: u32) -> Option<(Texture, TextureFormat)> {
        self.materialize_clears(id);
        if self.f2d_pipeline.is_none() {
            let module = self.device.create_shader_module(ShaderModuleDescriptor { label: None, source: ShaderSource::Wgsl(
                "@group(0) @binding(0) var src: texture_2d<f32>;
                 @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3)); return vec4f(p[i], 0, 1); }
                 @fragment fn fs(@builtin(position) pos: vec4f) -> @builtin(frag_depth) f32 { return textureLoad(src, vec2i(pos.xy), 0).x; }".into()) });
            let ds = DepthStencilState { format: TextureFormat::Depth32Float, depth_write_enabled: Some(true), depth_compare: Some(CompareFunction::Always), stencil: Default::default(), bias: Default::default() };
            self.f2d_pipeline = Some(self.simple_pipeline(&module, None, Some(ds)));
        }
        let pl = self.f2d_pipeline.clone().unwrap();
        let (desc, tex) = { let t = self.tex(id)?; (t.desc, t.tex.clone()) };
        let dev = self.device.clone();
        let trace_identities=self.command_trace.captures_native_identities();
        let t = self.tex_mut(id).unwrap();
        if t.proxy_fdepth.is_none() {
            let ptex = dev.create_texture(&TextureDescriptor {
                label: None, size: Extent3d { width: desc.w, height: desc.h, depth_or_array_layers: desc.d }, mip_level_count: desc.mips, sample_count: 1,
                dimension: TextureDimension::D2, format: TextureFormat::Depth32Float, usage: TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT, view_formats: &[],
            });
            let mut passes = Vec::new();
            for mip in 0..desc.mips {
                for layer in 0..desc.d {
                    let src = tex.create_view(&TextureViewDescriptor { dimension: Some(TextureViewDimension::D2), base_mip_level: mip, mip_level_count: Some(1), base_array_layer: layer, array_layer_count: Some(1), ..Default::default() });
                    let dst = ptex.create_view(&TextureViewDescriptor { base_mip_level: mip, mip_level_count: Some(1), base_array_layer: layer, array_layer_count: Some(1), dimension: Some(TextureViewDimension::D2), ..Default::default() });
                    let bg = dev.create_bind_group(&BindGroupDescriptor { label: None, layout: &pl.get_bind_group_layout(0), entries: &[BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&src) }] });
                    passes.push((dst, bg));
                }
            }
            let trace_native_identity=crate::world_command_trace::native_texture_tag(trace_identities);
            t.proxy_fdepth = Some(Proxy { tex: ptex, trace_native_identity, format: TextureFormat::Depth32Float, dirty: true, passes });
        }
        let px = t.proxy_fdepth.as_mut().unwrap();
        let out = (px.tex.clone(), px.format);
        if px.dirty {
            px.dirty = false;
            let passes: Vec<_> = px.passes.iter().map(|(v, b)| (v.clone(), b.clone())).collect();
            self.end_pass();
            let enc = self.encoder();
            for (dst, bg) in &passes {
                let mut p = enc.begin_render_pass(&RenderPassDescriptor {
                    label: None, color_attachments: &[],
                    depth_stencil_attachment: Some(RenderPassDepthStencilAttachment { view: dst, depth_ops: Some(Operations { load: LoadOp::Clear(1.0), store: StoreOp::Store }), stencil_ops: None }),
                    timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
                });
                p.set_pipeline(&pl);
                p.set_bind_group(0, bg, &[]);
                p.draw(0..3, 0..1);
            }
            self.use_tex(id);
        }
        Some(out)
    }

    fn prepare_depth_proxies(&mut self, prog: &Program, stage: usize) {
        for b in prog.info.bindings.iter().filter(|b| b.kind == Kind::Texture) {
            let id = *self.s.srv[stage].get((b.binding as usize).wrapping_sub(32)).unwrap_or(&0);
            let Some(v) = self.view_obj(id) else { continue };
            if v.is_buf { continue; }
            let res = v.res;
            let Some(t) = self.tex(res) else { continue };
            if is_depth(t.format) && b.sample != Sample::Depth {
                self.depth_proxy(res, b.sample == Sample::Uint);
            } else if !is_depth(t.format) && b.sample == Sample::Depth {
                self.float_to_depth_proxy(res);
            }
        }
    }

    fn srv_view(&mut self, srv_id: u32, b: &Binding) -> Option<TextureView> {
        let (res, fmt, a, bb, c, d) = { let v = self.view_obj(srv_id)?; (v.res, v.fmt, v.a, v.b, v.c, v.d) };
        let t = self.tex(res)?;
        let (tformat, desc) = (t.format, t.desc);
        let mut proxied = false;
        let (tex, format) = if is_depth(tformat) && b.sample != Sample::Depth {
            proxied = true;
            self.depth_proxy(res, b.sample == Sample::Uint)?
        } else if !is_depth(tformat) && b.sample == Sample::Depth {
            proxied = true;
            self.float_to_depth_proxy(res)?
        } else {
            (self.tex(res)?.tex.clone(), tformat)
        };
        let key = format!("{:?}{}", b.dim, if proxied { ":p" } else { "" });
        if let Some(v) = self.view_obj(srv_id).and_then(|v| v.views.get(&key)) { return Some(v.clone()); }
        let total = desc.mips;
        let mip_count = if bb == 0xffff_ffff || bb == 0 { total.saturating_sub(a) } else { bb };
        let layers = desc.d;
        if matches!(b.dim, TextureViewDimension::Cube | TextureViewDimension::CubeArray) && layers.saturating_sub(c) < 6 {
            log_once(&format!("cubeview{res}"), &format!("cube SRV over a texture with {layers} layer(s): bound as an empty cube"));
            return None;
        }
        let mut vd = TextureViewDescriptor {
            dimension: Some(b.dim),
            base_mip_level: a.min(total - 1),
            mip_level_count: Some(mip_count.min(total.saturating_sub(a)).max(1)),
            ..Default::default()
        };
        if matches!(b.dim, TextureViewDimension::D2Array | TextureViewDimension::Cube | TextureViewDimension::CubeArray | TextureViewDimension::D2) {
            let cnt = match b.dim { TextureViewDimension::Cube => 6, TextureViewDimension::D2 => 1, _ => if d != 0 { d } else { layers - c } };
            vd.base_array_layer = c.min(layers - 1);
            vd.array_layer_count = Some(cnt.min(layers.saturating_sub(c)).max(1));
        }
        if fmt != 0 && !is_depth(format) && !proxied {
            let original = map_format(fmt, 0).format;
            let mf = if self.tex(res)?.bc_source.is_some() { crate::bc::decoded_format(original).unwrap_or(original) } else { original };
            if mf != format && mf.is_srgb() != format.is_srgb() && mf.remove_srgb_suffix() == format.remove_srgb_suffix() { vd.format = Some(mf); }
        }
        if is_depth(format) && !proxied {
            vd.aspect = if fmt == 47 || fmt == 22 { TextureAspect::StencilOnly } else { TextureAspect::DepthOnly };
        }
        let v = self.scoped("SRV view", |_| tex.create_view(&vd))?;
        if !proxied {
            if let Some(vo) = self.view_obj_mut(srv_id) { vo.views.insert(key, v.clone()); }
        }
        Some(v)
    }

    fn uniform_alloc(&mut self, size: u64) -> (usize, u64) {
        let size = (size + 255) & !255;
        let slot = self.ubo_slot;
        let need_new = match self.ubo_slots[slot].get(self.ubo_chunk) { None => true, Some(_) => self.ubo_off + size + UBO_WINDOW > RING_CHUNK };
        if need_new {
            if self.ubo_slots[slot].get(self.ubo_chunk).is_some() { self.ubo_chunk += 1; self.ubo_off = 0; }
            if self.ubo_slots[slot].get(self.ubo_chunk).is_none() {
                while self.ubo_shared.len() <= self.ubo_chunk {
                    let buf = self.device.create_buffer(&BufferDescriptor { label: Some("uniform ring"), size: RING_CHUNK * FRAME_SLOTS as u64, usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST, mapped_at_creation: false });
                    let id = self.uid();
                    self.ubo_shared.push((buf, id));
                }
                let (buf, id) = self.ubo_shared[self.ubo_chunk].clone();
                while self.ubo_slots[slot].len() <= self.ubo_chunk {
                    self.ubo_slots[slot].push(Chunk { buf: buf.clone(), id, base: slot as u64 * RING_CHUNK, stage: Vec::new(), lo: u64::MAX, hi: 0 });
                }
            }
        }
        let off = self.ubo_off;
        self.ubo_off += size;
        (self.ubo_chunk, off)
    }
    fn flush_uniforms(&mut self) {
        for slot in &mut self.ubo_slots {
            for c in slot.iter_mut() {
                if c.lo >= c.hi { continue; }
                let hi = ((c.hi + 3) & !3).min(c.stage.len() as u64);
                measured_write_buffer(&mut self.uploads, &self.device, &self.queue, &mut self.buffer_uploads[UPLOAD_UNIFORMS], &c.buf, c.base + c.lo, &c.stage[c.lo as usize..hi as usize]);
                c.lo = u64::MAX;
                c.hi = 0;
            }
        }
    }

    fn pack_copy(&mut self, prog: &Program, stage: usize) -> Option<(usize, u64)> {
        let pack = prog.pack.as_ref()?;
        if pack.size > UBO_WINDOW {
            self.uniform_pack.rejected_oversize();
            log_once(&format!("packbig{}", prog.label), &format!("{}: constant buffers total {} bytes (> 64 KB)", prog.label, pack.size));
            return None;
        }
        let bindings: &[Cb] = if pack.members.is_empty() { &[] } else { &self.s.cb[stage] };
        let (snapshot_key, location) = self.uniform_snapshot.probe(prog.uid, stage, pack, &self.objs, bindings);
        if let Some(location) = location { return Some(location); }
        let (ci, off) = self.uniform_alloc(pack.size);
        let (copied, grew) = if self.uniform_pack.enabled {
            // These fields are disjoint. No Gpu method, guest callback, wait or
            // submit can mutate the host shadows while this snapshot is taken.
            let objects = &self.objs;
            let bindings: &[Cb] = if pack.members.is_empty() { &[] } else { &self.s.cb[stage] };
            let chunk = &mut self.ubo_slots[self.ubo_slot][ci];
            uniform_pack::direct_copy(pack, objects, bindings, off, &mut chunk.stage)
        } else {
            // Reference path: keep the temporary block and member loop order.
            let mut block = vec![0u8; pack.size as usize];
            let mut copied = 0u64;
            for m in &pack.members {
                let cb = self.s.cb[stage].get(m.binding as usize).cloned().unwrap_or_default();
                if let Some(Buf { shadow: Some(sh), size, .. }) = self.buf(cb.buf) {
                    let from = cb.first as u64 * 16;
                    let len = m.size.min(size.saturating_sub(from));
                    if len > 0 {
                        block[m.offset as usize..(m.offset + len) as usize].copy_from_slice(&sh[from as usize..(from + len) as usize]);
                        copied = copied.saturating_add(len);
                    }
                }
            }
            let chunk = &mut self.ubo_slots[self.ubo_slot][ci];
            let end = (off + pack.size) as usize;
            let grew = chunk.stage.len() < end;
            if grew { chunk.stage.resize((end.max(262144)).next_power_of_two().min(RING_CHUNK as usize), 0); }
            chunk.stage[off as usize..end].copy_from_slice(&block);
            (copied, grew)
        };
        let chunk = &mut self.ubo_slots[self.ubo_slot][ci];
        let end = (off + pack.size) as usize;
        chunk.lo = chunk.lo.min(off);
        chunk.hi = chunk.hi.max(end as u64);
        self.uniform_pack.record(pack, copied, grew);
        self.uniform_snapshot.insert(snapshot_key, (ci, off));
        Some((ci, off))
    }

    fn dummy_texture(&mut self, b: &Binding) -> TextureView {
        let key = format!("{:?}|{:?}|{}", b.dim, b.sample, b.ms);
        if let Some(v) = self.dummy_tex.get(&key) { return v.clone(); }
        let format = match b.sample { Sample::Uint => TextureFormat::Rgba8Uint, Sample::Sint => TextureFormat::Rgba8Sint, Sample::Depth => TextureFormat::Depth32Float, _ => TextureFormat::Rgba8Unorm };
        let cube = matches!(b.dim, TextureViewDimension::Cube | TextureViewDimension::CubeArray);
        let dimension = match b.dim { TextureViewDimension::D3 => TextureDimension::D3, TextureViewDimension::D1 => TextureDimension::D1, _ => TextureDimension::D2 };
        let mut usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
        if b.ms { usage |= TextureUsages::RENDER_ATTACHMENT; usage.remove(TextureUsages::COPY_DST); }
        let tex = self.device.create_texture(&TextureDescriptor {
            label: Some("dummy"), size: Extent3d { width: 1, height: 1, depth_or_array_layers: if cube { 6 } else { 1 } }, mip_level_count: 1,
            sample_count: if b.ms { 4 } else { 1 }, dimension, format, usage, view_formats: &[],
        });
        let v = tex.create_view(&TextureViewDescriptor { dimension: Some(b.dim), ..Default::default() });
        self.dummy_tex.insert(key, v.clone());
        v
    }
    fn dummy_sampler(&mut self, cmp_: bool) -> Sampler {
        if cmp_ {
            self.dummy_cmp.get_or_insert_with(|| self.device.create_sampler(&SamplerDescriptor { compare: Some(CompareFunction::Always), ..Default::default() })).clone()
        } else {
            self.dummy_smp.get_or_insert_with(|| self.device.create_sampler(&SamplerDescriptor::default())).clone()
        }
    }
    fn dummy_buffer(&mut self, binding: u32) -> Buffer {
        let dev = self.device.clone();
        self.dummy_bufs.entry(binding).or_insert_with(|| dev.create_buffer(&BufferDescriptor { label: Some("dummy"), size: 65536, usage: BufferUsages::STORAGE | BufferUsages::COPY_DST, mapped_at_creation: false })).clone()
    }
    fn counter_buffer(&mut self, buf_id: u32) -> Option<Buffer> {
        let dev = self.device.clone();
        let b = self.buf_mut(buf_id)?;
        Some(b.counter.get_or_insert_with(|| dev.create_buffer(&BufferDescriptor { label: Some("uav counter"), size: 16, usage: BufferUsages::STORAGE | BufferUsages::COPY_SRC | BufferUsages::COPY_DST, mapped_at_creation: false })).clone())
    }

    fn stage_bind_group(&mut self, stage: usize, prog: &Program, group: u32, bgl: &(BindGroupLayout, u64)) -> Option<BgResult> {
        let sample = self.bind_group_prepare.begin_call();
        let started = sample.then(Instant::now);
        let result = self.stage_bind_group_inner(stage, prog, group, bgl);
        if let Some(started) = started {
            self.bind_group_prepare.record_wall(started.elapsed());
        }
        result
    }

    fn stage_bind_group_inner(&mut self, stage: usize, prog: &Program, group: u32, bgl: &(BindGroupLayout, u64)) -> Option<BgResult> {
        enum Res { Buf(Buffer, u64, Option<u64>), View(TextureView), Smp(Sampler) }
        let bindings = self.render_prepare.bindings(prog.group_bindings.get(&group).map(Vec::as_slice).unwrap_or(&[]));
        let fast = self.bind_group_prepare.enabled;
        let mut entries = EntryScratch::<(u32, Res)>::new(fast);
        let mut key = KeyScratch::new(fast, &[bgl.1, prog.uid, stage as u64]);
        let mut dyn_off = None;
        for b in bindings.as_ref() {
            match b.kind {
                Kind::Uniform => {
                    if b.binding == 15 {
                        match &prog.consts {
                            Some(c) => entries.push((15, Res::Buf(c.clone(), 0, None))),
                            None => { log_once(&format!("c15{}", prog.label), &format!("shader {} declares binding 15 but has no constant table", prog.label)); return None; }
                        }
                        continue;
                    }
                    let (ci, off) = self.pack_copy(prog, stage)?;
                    let c = &self.ubo_slots[self.ubo_slot][ci];
                    entries.push((0, Res::Buf(c.buf.clone(), 0, Some(prog.pack.as_ref().unwrap().size))));
                    dyn_off = Some((c.base + off) as u32);
                    key.push(c.id);
                }
                Kind::Sampler | Kind::SamplerCmp => {
                    let sid = *self.s.smp[stage].get((b.binding as usize).wrapping_sub(16)).unwrap_or(&0);
                    let want_cmp = b.kind == Kind::SamplerCmp;
                    let st = self.state(sid);
                    let s = match st.as_ref().and_then(|s| s.sampler.clone().filter(|_| ((s.words.first().copied().unwrap_or(0) & 0x80) != 0) == want_cmp)) {
                        Some(s) => { key.push(sid as u64); s }
                        None => { key.push(0); self.dummy_sampler(want_cmp) }
                    };
                    entries.push((b.binding, Res::Smp(s)));
                }
                Kind::Texture => {
                    let id = *self.s.srv[stage].get((b.binding as usize).wrapping_sub(32)).unwrap_or(&0);
                    let is_tex = self.view_obj(id).map(|v| !v.is_buf).unwrap_or(false);
                    if is_tex { if let Some(r) = self.view_obj(id).map(|v| v.res) { if self.pending_clears.contains_key(&r) { self.materialize_clears(r); } } }
                    let v = if is_tex { self.srv_view(id, b) } else { None };
                    match v {
                        Some(v) => {
                            let res = self.view_obj(id).unwrap().res;
                            self.use_tex(res);
                            if let Some(t) = self.tex_mut(res) { t.in_bg = true; }
                            key.push(id as u64);
                            key.push(0);
                            entries.push((b.binding, Res::View(v)));
                        }
                        None => { key.push(0); let d = self.dummy_texture(b); entries.push((b.binding, Res::View(d))); }
                    }
                }
                Kind::ReadOnlyStorage | Kind::Storage => {
                    let is_counter = b.binding >= 176;
                    let is_uav = b.binding >= 160;
                    let id = if is_uav { *self.s.uav[stage].get((b.binding - if is_counter { 176 } else { 160 }) as usize).unwrap_or(&0) } else { *self.s.srv[stage].get((b.binding as usize).wrapping_sub(32)).unwrap_or(&0) };
                    let buf_id = self.view_obj(id).filter(|v| v.is_buf).map(|v| (v.res, v.a, v.b));
                    let mut res_off = 0;
                    let mut res_size = None;
                    let gb = if is_counter {
                        match buf_id { Some((r, _, _)) => self.counter_buffer(r), None => None }.unwrap_or_else(|| self.dummy_buffer(b.binding))
                    } else {
                        match buf_id.and_then(|(r, a, n)| self.buf(r).and_then(|bo| bo.gpu.clone().map(|g| (g, bo.stride, bo.bytes, a, n)))) {
                            Some((g, stride, bytes, first, num)) => {
                                let stride = if stride == 0 { 4 } else { stride } as u64;
                                let off = first as u64 * stride;
                                if off != 0 && off % 256 == 0 {
                                    res_off = off;
                                    if num != 0 && num != 0xffff_ffff { res_size = Some((num as u64 * stride).min(bytes - off)); }
                                } else if off != 0 {
                                    log_once(&format!("bufoff{}", prog.label), &format!("{}: buffer view offset {off} is not 256-aligned; binding the whole buffer", prog.label));
                                }
                                g
                            }
                            None => self.dummy_buffer(b.binding),
                        }
                    };
                    if let Some((r, _, _)) = buf_id {
                        self.use_buf(r);
                        if let Some(bo) = self.buf_mut(r) { bo.in_bg = true; }
                        // Bound writable: a shader may change its bytes on the GPU.
                        if b.kind == Kind::Storage && !is_counter { self.op47_dedup_gpu_write(r, crate::upload_dedup::GpuWrite::StorageBinding); }
                        key.push(id as u64);
                    } else {
                        key.push(0);
                    }
                    key.push(res_off);
                    entries.push((b.binding, Res::Buf(gb, res_off, res_size)));
                }
                Kind::StorageTexture => {
                    let uid = *self.s.uav[stage].get((b.binding as usize).wrapping_sub(160)).unwrap_or(&0);
                    let (res, a, bb, c, vdim) = match self.view_obj(uid).filter(|v| !v.is_buf) { Some(v) => (v.res, v.a, v.b, v.c, v.vdim), None => {
                        log_once(&format!("nouavtex{}", prog.label), &format!("{}: storage texture u{} has no UAV bound", prog.label, b.binding - 160));
                        return None;
                    } };
                    if self.pending_clears.contains_key(&res) { self.materialize_clears(res); }
                    let (tex, tformat, layers) = { let t = self.tex(res)?; (t.tex.clone(), t.format, t.desc.d) };
                    let k2 = format!("{:?}:{:?}", b.dim, tformat);
                    let v = match self.view_obj(uid).and_then(|v| v.views.get(&k2).cloned()) {
                        Some(v) => v,
                        None => {
                            let mut d = TextureViewDescriptor { dimension: Some(b.dim), base_mip_level: a, mip_level_count: Some(1), ..Default::default() };
                            if b.dim == TextureViewDimension::D2Array {
                                d.base_array_layer = if vdim == 5 { bb } else { 0 };
                                d.array_layer_count = Some(if vdim == 5 { c } else { layers });
                            } else if b.dim == TextureViewDimension::D2 {
                                d.base_array_layer = if vdim == 5 { bb } else { 0 };
                                d.array_layer_count = Some(1);
                            }
                            let v = self.scoped("UAV view", |_| tex.create_view(&d))?;
                            self.view_obj_mut(uid).unwrap().views.insert(k2, v.clone());
                            v
                        }
                    };
                    self.use_tex(res);
                    self.mark_color_dirty(res);
                    if let Some(t) = self.tex_mut(res) { t.in_bg = true; }
                    key.push(uid as u64);
                    entries.push((b.binding, Res::View(v)));
                }
            }
        }
        let cached = self.bind_groups.get(key.as_slice());
        self.render_prepare.bind_group_lookup(cached.is_some());
        self.bind_group_prepare.lookup(cached.is_some(), key.spilled(), entries.spilled());
        if let Some((bg, uid)) = cached {
            return Some(BgResult { bg: bg.clone(), uid: *uid, dyn_off });
        }
        self.bind_group_prepare.materialized_miss(entries.len());
        let entries = entries.into_vec();
        let key = key.into_vec();
        let wentries: Vec<BindGroupEntry> = entries.iter().map(|(binding, r)| BindGroupEntry {
            binding: *binding,
            resource: match r {
                Res::Buf(b, off, size) => BindingResource::Buffer(BufferBinding { buffer: b, offset: *off, size: size.and_then(std::num::NonZeroU64::new) }),
                Res::View(v) => BindingResource::TextureView(v),
                Res::Smp(s) => BindingResource::Sampler(s),
            },
        }).collect();
        let bg = self.scoped(&format!("bind group {}", prog.label), |d| d.create_bind_group(&BindGroupDescriptor { label: None, layout: &bgl.0, entries: &wentries }))?;
        if self.bind_groups.len() > BG_LIMIT * 3 / 2 {
            self.bind_groups.clear();
            self.bind_group_prepare.invalidated();
        }
        let uid = self.uid();
        self.bind_groups.insert(key, (bg.clone(), uid));
        Some(BgResult { bg, uid, dyn_off })
    }

    fn get_pipeline(&mut self, vs_id: u32, ps_id: u32, vs: &Rc<Program>, ps: Option<&Rc<Program>>, layout: &Rc<LayoutObj>) -> Option<Rc<Pl>> {
        let pi = self.pass_info.clone()?;
        let (topo, strip) = topology(self.s.topo);
        let strip_fmt = if strip && self.s.ib.0 != 0 { Some(if self.s.ib.1 == 57 { IndexFormat::Uint16 } else { IndexFormat::Uint32 }) } else { None };
        let mut key: Vec<i64> = vec![vs.uid as i64, ps.map(|p| p.uid as i64).unwrap_or(0), self.s.layout as i64];
        let strides: Vec<u32> = (0..16).map(|i| { let v = &self.s.vb[i]; if v.buf != 0 && self.buf(v.buf).map(|b| b.gpu.is_some()).unwrap_or(false) { v.stride } else { 0 } }).collect();
        for &(slot, _) in &layout.slots { key.push(strides[slot as usize] as i64); }
        key.extend([self.s.topo as i64, match strip_fmt { Some(IndexFormat::Uint16) => 1, Some(_) => 2, None => 0 }, self.s.blend.0 as i64, self.s.dss.0 as i64, self.s.rs as i64]);
        for f in &pi.rt_formats { key.push(f.map(|f| format_id(f)).unwrap_or(-1)); }
        key.push(-100 - self.hud_draw.map(|mode|mode as i64+1).unwrap_or(0));
        key.extend([-7, pi.ds_format.map(format_id).unwrap_or(-1), pi.samples as i64, pi.ro_depth as i64, pi.ro_stencil as i64]);
        let _ = (vs_id, ps_id);
        match self.pipelines.get(&key) {
            Some(PipeEntry::Done(p)) => return p.clone(),
            Some(PipeEntry::Pending) => { self.pending_skips += 1; return None; }
            None => {}
        }
        self.pipe_creates += 1;
        let Some((job, vb_req)) = self.create_pipeline(vs, ps, layout, &strides, topo, strip_fmt, &pi) else {
            self.pipelines.insert(key, PipeEntry::Done(None));
            return None;
        };
        if let Some(tx) = self.pipe_jobs.clone() {
            let id = self.uid();
            self.pipe_pending.insert(id, (key.clone(), vb_req, job.label.clone()));
            self.pipelines.insert(key, PipeEntry::Pending);
            let _ = tx.send((id, job));
            self.pending_skips += 1;
            return None;
        }
        let pl = compile_pipeline(&self.device, &job).map(|pipeline| { let uid = self.uid(); Rc::new(Pl { pipeline, uid, vb_req, label: job.label }) });
        self.pipelines.insert(key, PipeEntry::Done(pl.clone()));
        pl
    }

    #[allow(clippy::too_many_arguments)]
    fn create_pipeline(&mut self, vs: &Rc<Program>, ps: Option<&Rc<Program>>, layout: &Rc<LayoutObj>, strides: &[u32], topo: PrimitiveTopology, strip_fmt: Option<IndexFormat>, pi: &PassInfo) -> Option<(PipeJob, Vec<VbReq>)> {
        let vs_obj_sig = self.vs_sig()?;
        let label = format!("{} + {}", vs.label, ps.map(|p| p.label.as_str()).unwrap_or("-"));
        let bgl0 = if vs.info.bindings.iter().any(|b| b.group == 0) { self.bgl_for(vs, 0, 0).0 } else { self.empty_bgl() };
        let bgl1 = match ps { Some(p) if p.info.bindings.iter().any(|b| b.group == 1) => self.bgl_for(p, 1, 1).0, _ => self.empty_bgl() };
        let pl_layout = self.device.create_pipeline_layout(&PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl0), Some(&bgl1)], immediate_size: 0 });
        let remap = self.inter_stage_remap(vs, ps.map(|p| p.as_ref()));
        let vmod = self.vertex_module(vs, ps.map(|p| p.flat.as_slice()).unwrap_or(&[]), remap.as_ref())?;

        let mut by_slot: BTreeMap<u32, (Vec<VertexAttribute>, VertexStepMode)> = BTreeMap::new();
        for e in &layout.els {
            let Some(s) = vs_obj_sig.iter().find(|s| s.sem == e.sem && s.idx == e.idx) else { continue };
            let Some(f) = vertex_format(e.fmt) else { log_once(&format!("vf{}", e.fmt), &format!("unsupported vertex format DXGI {}", e.fmt)); continue };
            let ent = by_slot.entry(e.slot).or_insert_with(|| (Vec::new(), if e.cls == 1 { VertexStepMode::Instance } else { VertexStepMode::Vertex }));
            ent.0.push(VertexAttribute { format: f, offset: e.offset as u64, shader_location: s.reg });
        }
        let max_slot = by_slot.keys().next_back().map(|s| *s as usize + 1).unwrap_or(0);
        let mut vb_layouts: Vec<Option<(u64, VertexStepMode, Vec<VertexAttribute>)>> = vec![None; max_slot];
        let mut vb_req = Vec::new();
        for (slot, (attrs, step)) in &by_slot {
            let stride = strides[*slot as usize] as u64;
            vb_layouts[*slot as usize] = Some((stride, *step, attrs.clone()));
            let last = attrs.iter().map(|a| a.offset + a.format.size()).max().unwrap_or(0);
            vb_req.push(VbReq { slot: *slot, instance: *step == VertexStepMode::Instance, stride, last });
        }

        let rs = if self.s.rs != 0 { self.state(self.s.rs) } else { None };
        let blend = if self.s.blend.0 != 0 { self.state(self.s.blend.0) } else { None };
        let dss = if self.s.dss.0 != 0 { self.state(self.s.dss.0) } else { None };
        let mut primitive = PrimitiveState { topology: topo, strip_index_format: strip_fmt, ..Default::default() };
        match &rs {
            Some(r) => {
                let w = &r.words;
                primitive.cull_mode = match w.get(1) { Some(2) => Some(Face::Front), Some(3) => Some(Face::Back), _ => None };
                primitive.front_face = if w.get(2).copied().unwrap_or(0) != 0 { FrontFace::Ccw } else { FrontFace::Cw };
                if w.get(6).copied() == Some(0) && self.device.features().contains(Features::DEPTH_CLIP_CONTROL) { primitive.unclipped_depth = true; }
            }
            None => { primitive.cull_mode = Some(Face::Back); primitive.front_face = FrontFace::Cw; }
        }
        let depth_stencil = pi.ds_format.map(|fmt| {
            let mut d = DepthStencilState { format: fmt, depth_write_enabled: Some(false), depth_compare: Some(CompareFunction::Always), stencil: StencilState::default(), bias: DepthBiasState::default() };
            if let Some(s) = &dss {
                let w = &s.words;
                let g = |i: usize| w.get(i).copied().unwrap_or(0);
                if g(0) != 0 { d.depth_write_enabled = Some(g(1) == 1); d.depth_compare = Some(cmp(g(2)).unwrap_or(CompareFunction::Less)); }
                if has_stencil(fmt) && g(3) != 0 {
                    d.stencil.read_mask = g(4) & 0xff;
                    d.stencil.write_mask = (g(4) >> 8) & 0xff;
                    d.stencil.front = StencilFaceState { fail_op: stencil_op(g(5)), depth_fail_op: stencil_op(g(6)), pass_op: stencil_op(g(7)), compare: cmp(g(8)).unwrap_or(CompareFunction::Always) };
                    d.stencil.back = StencilFaceState { fail_op: stencil_op(g(9)), depth_fail_op: stencil_op(g(10)), pass_op: stencil_op(g(11)), compare: cmp(g(12)).unwrap_or(CompareFunction::Always) };
                }
            }
            if pi.ro_depth { d.depth_write_enabled = Some(false); }
            if pi.ro_stencil { d.stencil.write_mask = 0; }
            if let Some(r) = &rs { d.bias = DepthBiasState { constant: r.words.get(3).copied().unwrap_or(0) as i32, slope_scale: wf(r.words.get(5).copied().unwrap_or(0)), clamp: wf(r.words.get(4).copied().unwrap_or(0)) }; }
            d
        });
        let a2c = blend.as_ref().map(|b| b.words.first().copied().unwrap_or(0) != 0).unwrap_or(false) && pi.samples <= 1 && ps.map(|p| p.info.outputs.contains(&0)).unwrap_or(false);
        let mut targets: Vec<Option<ColorTargetState>> = Vec::new();
        let fmod = match ps {
            Some(p) => {
                for (i, f) in pi.rt_formats.iter().enumerate() {
                    let Some(format) = f else { targets.push(None); continue };
                    if i==1 { if let Some(mode)=self.hud_draw { targets.push(Some(crate::xr_hud::target(mode))); continue; } }
                    if !p.info.outputs.contains(&(i as u32)) { targets.push(Some(ColorTargetState { format: *format, blend: None, write_mask: ColorWrites::empty() })); continue; }
                    let (bl, write) = blend_for(blend.as_deref(), i);

                    let blendable = self.adapter.get_texture_format_features(*format).flags.contains(TextureFormatFeatureFlags::BLENDABLE);
                    targets.push(Some(ColorTargetState { format: *format, blend: if blendable { bl } else { None }, write_mask: write }));
                }
                let missing: Vec<_> = p.info.outputs.iter().filter(|&&l| pi.rt_formats.get(l as usize).copied().flatten().is_none()).collect();
                if !missing.is_empty() { log_once(&format!("psout{}", p.label), &format!("{} writes colour output(s) {:?} with no render target bound", p.label, missing)); }
                Some(self.pixel_module(p, a2c, remap.as_ref(),self.hud_draw)?)
            }
            None => None,
        };
        let multisample = MultisampleState { count: pi.samples.max(1), mask: !0, alpha_to_coverage_enabled: pi.samples > 1 && blend.as_ref().map(|b| b.words.first().copied().unwrap_or(0) != 0).unwrap_or(false) };
        Some((PipeJob { label, layout: pl_layout, vmod, fmod, vbs: vb_layouts, primitive, depth_stencil, multisample, targets }, vb_req))
    }

    fn collect_pipelines(&mut self) {
        while let Ok((id, pipeline)) = self.pipe_results.try_recv() {
            let Some((key, vb_req, label)) = self.pipe_pending.remove(&id) else { continue };
            let entry = pipeline.map(|pipeline| { let uid = self.uid(); Rc::new(Pl { pipeline, uid, vb_req, label }) });
            self.pipelines.insert(key, PipeEntry::Done(entry));
            self.pdirty = true;
        }
    }

    fn vs_sig(&self) -> Option<Vec<SigEl>> {
        match self.obj(self.s.shader[0]) { Some(Obj::Shader(s)) => Some(s.sig.clone()), _ => None }
    }

    fn skip(&mut self) {
        if let Some(hud)=&mut self.hud_capture { hud.rejected.get_or_insert_with(||"draw skipped after final postFX".into()); }
        self.skipped += 1;
    }

    fn read_indirect_arguments(&mut self, buffer_id: u32, offset: u64, indexed: bool) -> Option<Vec<u32>> {
        self.indirect_reads.attempts = self.indirect_reads.attempts.saturating_add(1);
        let count = if indexed { 5 } else { 4 };
        let length = count * 4;
        let buffer = self.buf(buffer_id)?.gpu.clone()?;
        if offset & 3 != 0 || offset.checked_add(length)? > buffer.size() {
            log_once("indirect-arguments-range", "indirect argument buffer range is invalid");
            return None;
        }
        // The arguments may have been written by a compute shader. Read them
        // from the actual ordered GPU stream, never from an old CPU shadow.
        let readback = self.device.create_buffer(&BufferDescriptor {
            label: Some("indirect draw fallback arguments"), size: length,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false,
        });
        self.end_pass();
        self.encoder().copy_buffer_to_buffer(&buffer, offset, &readback, 0, length);
        self.flush();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        readback.slice(..).map_async(MapMode::Read, move |result| { let _ = send.send(result); });
        let wait_started = Instant::now();
        let poll = self.device.poll(PollType::Wait { submission_index: self.last_submit.clone(), timeout: Some(Duration::from_secs(5)) });
        let mapped = receive.try_recv();
        self.indirect_reads.wait_wall.record(wait_started.elapsed());
        if poll.is_err() { self.indirect_reads.poll_errors = self.indirect_reads.poll_errors.saturating_add(1); }
        if !matches!(mapped, Ok(Ok(()))) { self.indirect_reads.map_errors = self.indirect_reads.map_errors.saturating_add(1); }
        if poll.is_err() || !matches!(mapped, Ok(Ok(()))) {
            log_once("indirect-arguments-map", &format!("indirect GPU argument readback failed: poll {poll:?}; map {mapped:?}"));
            return None;
        }
        let arguments = {
            let Ok(view) = readback.slice(..).get_mapped_range() else { return None; };
            view.chunks_exact(4).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())).collect()
        };
        readback.unmap();
        self.indirect_reads.successes = self.indirect_reads.successes.saturating_add(1);
        log_once("indirect-fallback", "adapter lacks INDIRECT_EXECUTION: draws use synchronized GPU argument readback and direct draw calls");
        Some(arguments)
    }

    fn note_geometry_world_pair(&mut self, vs: &Program, ps: Option<&Program>) {
        let Some(ps) = ps else { return; };
        let vertex_hash = match self.obj(self.s.shader[0]) { Some(Obj::Shader(s)) => s.hash.clone(), _ => return };
        let pixel_hash = match self.obj(self.s.shader[1]) { Some(Obj::Shader(s)) => s.hash.clone(), _ => return };
        let diag = &mut self.geometry_capture_diagnostics;
        diag.qualified_world_depth_draws = diag.qualified_world_depth_draws.saturating_add(1);
        if let Some(pair) = diag.qualified_pairs.iter_mut().find(|pair|
            pair.vertex_hash == vertex_hash && pair.pixel_hash == pixel_hash) {
            pair.draws = pair.draws.saturating_add(1);
        } else if diag.qualified_pairs.len() < GEOMETRY_DIAG_PAIR_CAP {
            diag.qualified_pairs.push(GeometryPairCount { vertex_hash, pixel_hash,
                vertex_label: vs.label.clone(), pixel_label: ps.label.clone(), draws: 1 });
        } else {
            diag.other_qualified_pairs = diag.other_qualified_pairs.saturating_add(1);
        }
    }

    fn publish_geometry_capture_diagnostics(&self) {
        if !self.geometry_stereo_capture { return; }
        let d = &self.geometry_capture_diagnostics;
        let pairs: Vec<_> = d.qualified_pairs.iter().map(|pair| serde_json::json!({
            "vertex_hash":pair.vertex_hash,"pixel_hash":pair.pixel_hash,
            "vertex_label":pair.vertex_label,"pixel_label":pair.pixel_label,"draws":pair.draws
        })).collect();
        let value = serde_json::json!({"enabled":true,"schema":1,
            "capture_attempted":self.geometry_stereo_capture_done,
            "considered_unclamped_draws":d.considered,"vertex_match":d.vertex_match,
            "pair_match":d.pair_match,"view_match":d.view_match,
            "target_match":d.target_match,"depth_write_match":d.depth_write_match,
            "surface_match":d.surface_match,"draw_match":d.draw_match,
            "eligible_attempts":d.eligible_attempts,
            "first_pair_state":d.first_pair_state,
            "qualified_world_depth_draws":d.qualified_world_depth_draws,
            "qualified_pairs_first_32":pairs,"other_qualified_pair_draws":d.other_qualified_pairs});
        if let Ok(mut status) = GEOMETRY_DIAG_STATUS.lock() { *status = value; }
    }

    // One-shot, opt-in frozen draw evidence for the CPU geometry-stereo proof.
    // This never creates a second guest call or publishes an XR frame. The
    // selected opaque deferred shader pair is checked by the guest's own hashes.
    #[allow(clippy::too_many_arguments)]
    fn capture_geometry_stereo_draw(&mut self, kind: u32, count: u32, inst: u32, start: u32,
        base_vertex: i32, start_instance: u32, layout: &LayoutObj, vs: &Program, ps: Option<&Program>,
        viewport: [f32; 6], writes_depth: bool) {
        if !self.geometry_stereo_capture || self.geometry_stereo_capture_done { return; }
        let diag = &mut self.geometry_capture_diagnostics;
        diag.considered = diag.considered.saturating_add(1);
        let ps_id = self.s.shader[1];
        let vs_hash = match self.obj(self.s.shader[0]) { Some(Obj::Shader(s)) => s.hash.clone(), _ => return };
        if vs_hash != "6a6a0898a51cf403" { return; }
        self.geometry_capture_diagnostics.vertex_match = self.geometry_capture_diagnostics.vertex_match.saturating_add(1);
        let ps_hash = match self.obj(ps_id) { Some(Obj::Shader(s)) => s.hash.clone(), _ => return };
        if ps_hash != "75cf2d53bf9497c9" { return; }
        self.geometry_capture_diagnostics.pair_match = self.geometry_capture_diagnostics.pair_match.saturating_add(1);
        // Menu and offscreen uses of this same shader pair are not the proof target.
        let eligible_view = self.world_view.is_some_and(|v| v.headset_applied && v.depth_reverse_z);
        let eligible_target = self.tex(self.s.ds.tex).is_some_and(|depth|
            is_depth(depth.format) && depth.desc.samples == 1 && depth.desc.d == 1
            && viewport == [0., 0., depth.desc.w as f32, depth.desc.h as f32, 0., 1.]);
        let eligible_surface = self.s.ds.mip == 0 && self.s.ds.slice == 0;
        let eligible_draw = kind == 1 && inst == 1 && start_instance == 0
            && count >= 3 && count <= 65536 && self.s.topo == 4;
        if self.geometry_capture_diagnostics.first_pair_state.is_none() {
            let depth = self.tex(self.s.ds.tex).map(|t| serde_json::json!({"id":self.s.ds.tex,
                "width":t.desc.w,"height":t.desc.h,"layers":t.desc.d,"samples":t.desc.samples,
                "format":format!("{:?}",t.format)}));
            self.geometry_capture_diagnostics.first_pair_state = Some(serde_json::json!({
                "viewport":viewport,"depth_target":depth,"depth_mip":self.s.ds.mip,
                "depth_slice":self.s.ds.slice,
                "depth_state":self.state(self.s.dss.0).map(|s| s.words.clone()),
                "world_view":self.world_view.map(|v| serde_json::json!({
                    "camera_sequence":v.camera_sequence,"headset_applied":v.headset_applied,
                    "depth_reverse_z":v.depth_reverse_z})),
                "kind":kind,"instances":inst,"start_instance":start_instance,
                "count":count,"topology":self.s.topo,
                "eligible_view":eligible_view,"eligible_target":eligible_target,
                "writes_depth":writes_depth,"eligible_surface":eligible_surface,
                "eligible_draw":eligible_draw}));
        }
        if !eligible_view { return; }
        self.geometry_capture_diagnostics.view_match = self.geometry_capture_diagnostics.view_match.saturating_add(1);
        if !eligible_target { return; }
        self.geometry_capture_diagnostics.target_match = self.geometry_capture_diagnostics.target_match.saturating_add(1);
        if !writes_depth { return; }
        self.geometry_capture_diagnostics.depth_write_match = self.geometry_capture_diagnostics.depth_write_match.saturating_add(1);
        if !eligible_surface { return; }
        self.geometry_capture_diagnostics.surface_match = self.geometry_capture_diagnostics.surface_match.saturating_add(1);
        if !eligible_draw { return; }
        self.geometry_capture_diagnostics.draw_match = self.geometry_capture_diagnostics.draw_match.saturating_add(1);
        self.geometry_capture_diagnostics.eligible_attempts = self.geometry_capture_diagnostics.eligible_attempts.saturating_add(1);
        self.geometry_stereo_capture_done = true;
        let output = std::path::Path::new("target/verification/reprojection-implementation-v1/frozen-game-draw.json");
        let result = (|| -> Result<serde_json::Value, String> {
            let view = self.world_view.filter(|v| v.headset_applied && v.depth_reverse_z)
                .ok_or("missing qualified reverse-Z command view")?;
            let request_record = crate::xr_view::consumed_request_receipt(&view)
                .ok_or("exact consumed stereo request receipt unavailable")?;
            if kind != 1 || inst != 1 || count < 3 || count > 65536 || self.s.topo != 4 {
                return Err("expected one bounded indexed triangle-list instance".into());
            }
            if !writes_depth || self.s.ds.tex == 0 || self.s.ds.mip != 0 || self.s.ds.slice != 0 {
                return Err("draw does not write main world depth".into());
            }
            let depth = self.tex(self.s.ds.tex).ok_or("missing depth target")?;
            if depth.desc.samples != 1 || depth.desc.d != 1 || !is_depth(depth.format)
                || viewport != [0., 0., depth.desc.w as f32, depth.desc.h as f32, 0., 1.] {
                return Err("depth target or full viewport not qualified".into());
            }
            let ps = ps.ok_or("missing pixel shader")?;
            if ps.info.writes_fragment_depth { return Err("pixel shader overrides depth".into()); }
            let source_vs = std::fs::read_to_string(self.shader_dir.join(format!("{vs_hash}.wgsl")))
                .map_err(|e| format!("VS source read: {e}"))?;
            let source_ps = std::fs::read_to_string(self.shader_dir.join(format!("{ps_hash}.wgsl")))
                .map_err(|e| format!("PS source read: {e}"))?;
            if source_vs.len() > 256 * 1024 || source_ps.len() > 256 * 1024
                || vs.text.len() > 256 * 1024 || ps.text.len() > 256 * 1024 {
                return Err("shader text exceeds capture bound".into());
            }
            let mut constant_buffers = Vec::new();
            let mut constant_bytes = 0usize;
            for (stage, program) in [(0usize, vs), (1usize, ps)] {
                let pack = program.pack.as_ref().ok_or("shader has no bound CB pack")?;
                for member in &pack.members {
                    let cb = self.s.cb[stage].get(member.binding as usize).ok_or("CB binding out of range")?;
                    let buffer = self.buf(cb.buf).ok_or("bound CB missing")?;
                    let shadow = buffer.shadow.as_ref().ok_or("bound CB has no CPU shadow")?;
                    let from = cb.first as usize * 16;
                    let end = from.checked_add(member.size as usize).ok_or("CB range overflow")?;
                    let bytes = shadow.get(from..end).ok_or("CB range unavailable")?;
                    constant_bytes += bytes.len();
                    if constant_bytes > 256 * 1024 { return Err("CB capture exceeds 256 KiB".into()); }
                    constant_buffers.push(serde_json::json!({"stage":stage,"binding":member.binding,
                        "buffer_id":cb.buf,"first_16byte":cb.first,"version":buffer.version,
                        "bytes_hex":bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()}));
                }
            }
            let mut gpu_buffers: Vec<(u32, Buffer, u64)> = Vec::new();
            let mut vertex_buffers = Vec::new();
            let mut total = 0u64;
            // This proof decodes VS location 0 positions only. Other layout slots
            // may be unused by the active shader or bound to the renderer's
            // synthetic zero buffer; neither is evidence for CPU geometry.
            let position_sig = match self.obj(self.s.shader[0]) {
                Some(Obj::Shader(shader)) => {
                    let mut matches = shader.sig.iter().filter(|e| e.reg == 0);
                    let first = matches.next().ok_or("VS position register 0 absent")?;
                    if matches.next().is_some() { return Err("VS position register 0 ambiguous".into()); }
                    (first.sem, first.idx)
                }
                _ => return Err("vertex signature unavailable".into()),
            };
            let mut position_elements = layout.els.iter().filter(|e|
                e.sem == position_sig.0 && e.idx == position_sig.1);
            let position = position_elements.next().ok_or("position semantic absent from layout")?;
            if position_elements.next().is_some() { return Err("position semantic ambiguous in layout".into()); }
            let (slot, cls) = (position.slot, position.cls);
            if cls != 0 || position.fmt != 6 {
                return Err(format!("position slot {slot} is not per-vertex DXGI R32G32B32_FLOAT"));
            }
            if !layout.slots.iter().any(|&(s, c)| s == slot && c == 0) {
                return Err(format!("position slot {slot} missing from layout slots"));
            }
            let binding = self.s.vb.get(slot as usize)
                .ok_or_else(|| format!("position VB slot {slot} out of range"))?;
            let position_end = (binding.offset as u64)
                .checked_add(position.offset as u64)
                .and_then(|n| n.checked_add(12))
                .ok_or("position VB offset overflow")?;
            if (binding.stride as u64) < position.offset as u64 + 12 {
                return Err(format!("position VB slot {slot} stride too small"));
            }
            let buffer = self.buf(binding.buf)
                .ok_or_else(|| format!("position VB slot {slot} buffer {} missing", binding.buf))?;
            let gpu = buffer.gpu.clone()
                .ok_or_else(|| format!("position VB slot {slot} buffer {} lacks GPU storage", binding.buf))?;
            if buffer.bytes == 0 || buffer.bytes > 1024 * 1024 {
                return Err(format!("position VB slot {slot} exceeds 1 MiB bound"));
            }
            if position_end > buffer.size {
                return Err(format!("position VB slot {slot} offset exceeds logical buffer size"));
            }
            total += buffer.bytes;
            gpu_buffers.push((binding.buf, gpu, buffer.bytes));
            vertex_buffers.push(serde_json::json!({"slot":slot,"class":cls,"buffer_id":binding.buf,
                "stride":binding.stride,"offset":binding.offset,"size":buffer.size,
                "readback_size":buffer.bytes,"version":buffer.version}));
            let (ib_id, ib_format, ib_offset) = self.s.ib;
            let ib = self.buf(ib_id).ok_or("bound IB missing")?;
            let ib_gpu = ib.gpu.clone().ok_or("bound IB lacks GPU storage")?;
            if ib.bytes == 0 || ib.bytes > 1024 * 1024 { return Err("IB exceeds 1 MiB bound".into()); }
            total += ib.bytes;
            if total > 2 * 1024 * 1024 { return Err("geometry exceeds 2 MiB capture bound".into()); }
            let ib_meta = serde_json::json!({"buffer_id":ib_id,"format":ib_format,"offset":ib_offset,
                "size":ib.size,"readback_size":ib.bytes,"version":ib.version});
            gpu_buffers.push((ib_id, ib_gpu, ib.bytes));
            let mut staging = Vec::new();
            self.end_pass();
            for (_, source, size) in &gpu_buffers {
                let dst = self.device.create_buffer(&BufferDescriptor { label: Some("geometry stereo proof readback"),
                    size:*size, usage:BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation:false });
                self.encoder().copy_buffer_to_buffer(source, 0, &dst, 0, *size);
                staging.push(dst);
            }
            self.flush();
            let mut data = Vec::new();
            for dst in &staging {
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                dst.slice(..).map_async(MapMode::Read, move |r| { let _ = send.send(r); });
                self.device.poll(PollType::Wait { submission_index: self.last_submit.clone(), timeout: Some(Duration::from_secs(5)) })
                    .map_err(|e| format!("geometry readback poll: {e:?}"))?;
                receive.try_recv().map_err(|e| format!("geometry readback callback: {e}"))?
                    .map_err(|e| format!("geometry readback map: {e:?}"))?;
                let mapped = dst.slice(..).get_mapped_range().map_err(|e| format!("geometry mapped range: {e:?}"))?;
                data.push(mapped.iter().map(|b| format!("{b:02x}")).collect::<String>());
                drop(mapped);
                dst.unmap();
            }
            for (meta, bytes) in vertex_buffers.iter_mut().zip(data.iter()) { meta["bytes_hex"] = bytes.clone().into(); }
            let mut index_buffer = ib_meta;
            index_buffer["bytes_hex"] = data.last().cloned().unwrap_or_default().into();
            let elements: Vec<_> = layout.els.iter().map(|e| serde_json::json!({"semantic":e.sem,"semantic_index":e.idx,
                "format":e.fmt,"slot":e.slot,"offset":e.offset,"class":e.cls})).collect();
            let signature: Vec<_> = match self.obj(self.s.shader[0]) {
                Some(Obj::Shader(shader)) => shader.sig.iter().map(|e| serde_json::json!({
                    "semantic":e.sem,"semantic_index":e.idx,"register":e.reg})).collect(),
                _ => return Err("vertex signature unavailable".into()),
            };
            Ok(serde_json::json!({"schema":1,"status":"captured","source":"Gpu::do_draw",
                "capture_scope":"cpu_position_vertex_buffer_and_index_only",
                "shader":{"vertex_id":self.s.shader[0],"vertex_hash":vs_hash,"vertex_label":vs.label,
                    "pixel_id":ps_id,"pixel_hash":ps_hash,"pixel_label":ps.label,
                    "vertex_source_wgsl":source_vs,"pixel_source_wgsl":source_ps,
                    "vertex_translated_wgsl":vs.text,"pixel_translated_wgsl":ps.text},
                "world_view":{"provenance_epoch":view.provenance_epoch,"camera_sequence":view.camera_sequence,
                    "request_frame":view.request_frame,"head_pose_frame":view.head_pose_frame,
                    "presentation_time_seconds":view.presentation_time_seconds,
                    "eye_index":view.eye_index,"view_count":view.view_count,"game_from_eye":view.game_from_eye,
                    "projection_from_eye":view.projection_from_eye,"depth_reverse_z":view.depth_reverse_z,
                    "headset_applied":view.headset_applied},
                "draw":{"kind":kind,"count":count,"instances":inst,"start_index":start,
                    "base_vertex":base_vertex,"start_instance":start_instance,"topology":self.s.topo},
                "viewport":viewport,"depth_target":self.s.ds.tex,"render_targets":self.s.rt.iter().map(|r| r.tex).collect::<Vec<_>>(),
                "layout":elements,"vertex_signature":signature,"constant_buffers":constant_buffers,"vertex_buffers":vertex_buffers,
                "index_buffer":index_buffer,"request_record":request_record}))
        })();
        let value = match result { Ok(value) => value, Err(reason) => {
            let position_binding = match self.obj(self.s.shader[0]) {
                Some(Obj::Shader(shader)) => shader.sig.iter().find(|e| e.reg == 0)
                    .and_then(|sig| layout.els.iter().find(|e| e.sem == sig.sem && e.idx == sig.idx))
                    .map(|el| serde_json::json!({"slot":el.slot,"class":el.cls,"format":el.fmt,
                        "element_offset":el.offset,"bound":self.s.vb.get(el.slot as usize)
                            .map(|vb| serde_json::json!({"buffer_id":vb.buf,"stride":vb.stride,"offset":vb.offset}))})),
                _ => None,
            };
            serde_json::json!({"schema":1,"status":"rejected","source":"Gpu::do_draw",
                "reason":reason,"vertex_hash":vs_hash,"pixel_hash":ps_hash,
                "world_view_identity":self.world_view.map(|v| serde_json::json!({
                    "provenance_epoch":v.provenance_epoch,"camera_sequence":v.camera_sequence,
                    "request_frame":v.request_frame,"eye_index":v.eye_index,"depth_reverse_z":v.depth_reverse_z})),
                "position_binding":position_binding})
        } };
        if let Some(parent) = output.parent() {
            if parent.is_dir() {
                if let Ok(body) = serde_json::to_vec_pretty(&value) {
                    use std::io::Write;
                    match std::fs::OpenOptions::new().write(true).create_new(true).open(output) {
                        Ok(mut file) => if let Err(e) = file.write_all(&body) {
                            log(&format!("geometry proof capture write failed: {e}"));
                        },
                        Err(e) => log(&format!("geometry proof capture file unavailable (never overwritten): {e}")),
                    }
                }
            } else { log("geometry proof capture target directory missing"); }
        }
    }

    fn do_draw(&mut self, p: u64) {
        if self.dbg_idx != 0 {
            let lim = atomic(&self.mem, self.dbg_idx).load(Ordering::Relaxed);
            if lim >= 0 { }
        }
        let mb = self.base;
        let u = move |w: u64| unsafe { std::ptr::read_volatile(mb.add(((p + w) * 4) as usize) as *const u32) };
        let (mut kind, mut count, mut inst, mut start, mut base_vertex, mut start_instance) = (u(0), u(1), u(2), u(3), u(4) as i32, u(5));
        if kind >= 4 && !self.adapter.get_downlevel_capabilities().flags.contains(DownlevelFlags::INDIRECT_EXECUTION) {
            let indexed = kind == 4;
            let Some(arguments) = self.read_indirect_arguments(u(1), u(2) as u64, indexed) else { self.skip(); return };
            count = arguments[0]; inst = arguments[1]; start = arguments[2];
            base_vertex = if indexed { arguments[3] as i32 } else { 0 };
            start_instance = arguments[if indexed { 4 } else { 3 }];
            kind = if indexed { 1 } else { 0 };
        }
        self.collect_pipelines();
        let vs_id = self.s.shader[0];
        let ps_id = self.s.shader[1];
        let Some(vs) = self.shader_program(vs_id).and_then(|program| self.float_sampling_program(program, 0)) else { self.skip(); return };
        let ps = if ps_id != 0 { match self.shader_program(ps_id).and_then(|program| self.float_sampling_program(program, 1)) { Some(p) => Some(p), None => { self.skip(); return } } } else { None };
        // Before encoding: an attributed first-person depth write is accounted
        // for here; anything else closes an open overlay on its target first.
        let foreground_accepted = self.foreground_pre_draw(&vs, ps.as_deref());
        let ui_depth_clear = self.world_view.is_none()
            && vs.label == "billboard_nobump::VS_Blit"
            && ps.as_ref().is_some_and(|program| program.label == "im::PS_Clear"
                && !program.info.writes_fragment_depth)
            && self.s.ds.mip == 0 && self.s.ds.slice == 0 && self.s.ds.flags & 1 == 0
            && self.state(self.s.dss.0).is_some_and(|state|
                state.words.first() == Some(&1) && state.words.get(1) == Some(&1) && state.words.get(2) == Some(&8))
            && self.tex(self.s.ds.tex).is_some_and(|texture| {
                let expected = [0., 0., texture.desc.w as f32, texture.desc.h as f32, 0., 1.];
                let viewport = self.s.viewports.first().copied().unwrap_or(expected);
                viewport.iter().zip(expected).all(|(&actual, expected)|
                    actual.is_finite() && (actual - expected).abs() <= 0.00002)
            });
        if ui_depth_clear { self.save_world_depth_before_ui(self.s.ds.tex); }
        let hud_draw=self.prepare_hud_draw(&vs,ps.as_deref());
        if self.hud_draw!=hud_draw { self.end_pass(); self.hud_draw=hud_draw; self.pdirty=true; }
        let program_uids = (vs.uid, ps.as_ref().map(|program| program.uid).unwrap_or(0));
        if self.last_program_uids != Some(program_uids) {
            self.pdirty = true;
            self.last_program_uids = Some(program_uids);
        }
        let layout = match self.obj(self.s.layout) { Some(Obj::Layout(l)) => l.clone(), _ => { self.skip(); return } };
        self.prepare_depth_proxies(&vs, 0);
        if let Some(p) = &ps { self.prepare_depth_proxies(p, 1); }
        if !self.ensure_pass() {
            if self.hud_draw.is_some(){self.hud_reject("auxiliary HUD render pass unavailable");self.end_pass();self.hud_draw=None;self.pdirty=true;}
            if !self.ensure_pass(){self.skip();return;}
        }
        let mut pl = if !self.pdirty && self.last_pl.is_some() {
            self.last_pl.clone()
        } else {
            let pl = self.get_pipeline(vs_id, ps_id, &vs, ps.as_ref(), &layout);
            self.last_pl = pl.clone();
            self.pdirty = false;
            pl
        };
        if pl.is_none() && self.hud_draw.is_some(){
            // Optional pipeline compilation (including an asynchronous pending
            // result) must never suppress an original flat-screen game draw.
            self.hud_reject("auxiliary HUD pipeline unavailable; original draw retained");
            self.end_pass();self.hud_draw=None;self.pdirty=true;
            if !self.ensure_pass(){self.skip();return;}
            pl=self.get_pipeline(vs_id,ps_id,&vs,ps.as_ref(),&layout);
            self.last_pl=pl.clone();self.pdirty=false;
        }
        let Some(pl) = pl else { self.skip(); return };

        let g0 = if vs.group_bindings.contains_key(&0) {
            let bgl = self.bgl_for(&vs, 0, 0);
            match self.stage_bind_group(0, &vs, 0, &bgl) { Some(g) => Some(g), None => { self.skip(); return } }
        } else { None };
        let g1 = match &ps {
            Some(p) if p.group_bindings.contains_key(&1) => {
                let bgl = self.bgl_for(p, 1, 1);
                match self.stage_bind_group(1, p, 1, &bgl) { Some(g) => Some(g), None => { self.skip(); return } }
            }
            _ => None,
        };

        if self.pass.is_none() && !self.ensure_pass() { self.skip(); return; }

        let indexed = kind == 4 || (kind < 4 && (kind & 1) != 0);
        let (fold_v, fold_i) = (vs.vid, vs.iid);
        let v_first = if indexed || fold_v { 0 } else { start };
        let i_first = if fold_i { 0 } else { start_instance };

        let mut vb_binds: Vec<(u32, Buffer, u64, u64)> = Vec::new();
        for &(slot, cls) in &layout.slots {
            let vb = self.s.vb[slot as usize].clone();
            match self.buf(vb.buf).and_then(|b| b.gpu.clone().map(|g| (g, b.bytes))) {
                None => {
                    let dev = self.device.clone();
                    let z = self.zero_vb.get_or_insert_with(|| dev.create_buffer(&BufferDescriptor { label: Some("zero vb"), size: ZERO_VB_SIZE, usage: BufferUsages::VERTEX, mapped_at_creation: false })).clone();
                    self.vb_avail[slot as usize] = ZERO_VB_SIZE;
                    vb_binds.push((slot, z, 0, u64::MAX));
                }
                Some((g, bytes)) => {
                    let shift: i64 = if cls == 1 { if fold_i { start_instance as i64 * vb.stride as i64 } else { 0 } } else if fold_v { (if indexed { base_vertex as i64 } else { start as i64 }) * vb.stride as i64 } else { 0 };
                    let voff = (vb.offset as i64 + shift).max(0) as u64;
                    self.vb_avail[slot as usize] = bytes.saturating_sub(voff);
                    vb_binds.push((slot, g, voff, vb.buf as u64));
                    self.use_buf(vb.buf);
                }
            }
        }
        let mut ib_max = u64::MAX;
        let mut ib_bind = None;
        if indexed && kind < 4 {
            let (ibid, ifmt, ioff) = self.s.ib;
            let Some((g, bytes)) = self.buf(ibid).and_then(|b| b.gpu.clone().map(|g| (g, b.bytes))) else { self.skip(); return };
            let fmt = if ifmt == 57 { IndexFormat::Uint16 } else { IndexFormat::Uint32 };
            let isz = if fmt == IndexFormat::Uint16 { 2 } else { 4 };
            ib_max = (bytes.saturating_sub(ioff as u64) / isz).saturating_sub(start as u64);
            ib_bind = Some((g, fmt, ioff as u64, ibid));
            self.use_buf(ibid);
        } else if kind == 4 {
            let (ibid, ifmt, ioff) = self.s.ib;
            if let Some(g) = self.buf(ibid).and_then(|b| b.gpu.clone()) {
                ib_bind = Some((g, if ifmt == 57 { IndexFormat::Uint16 } else { IndexFormat::Uint32 }, ioff as u64, ibid));
                self.use_buf(ibid);
            }
        }
        let (pass_width, pass_height, pass_ro_depth) = self.render_prepare.dimensions(self.pass_info.as_ref().unwrap());
        let rs_scissor = self.s.rs != 0 && self.state(self.s.rs).map(|r| r.words.get(7).copied().unwrap_or(0) != 0).unwrap_or(false);
        let scissor = if rs_scissor && !self.s.scissors.is_empty() {
            let c = self.s.scissors[0];
            let x0 = pass_width.min(c[0]);
            let y0 = pass_height.min(c[1]);
            let x1 = pass_width.min(x0.max(c[2]));
            let y1 = pass_height.min(y0.max(c[3]));
            [x0, y0, x1 - x0, y1 - y0]
        } else {
            [0, 0, pass_width, pass_height]
        };
        let viewport = self.s.viewports.first().copied();
        let bf = self.s.blend.1;
        let sref = self.s.dss.1;

        let (mut d_count, mut d_inst) = (count as i64, inst as i64);
        if kind < 4 {
            for r in &pl.vb_req {
                let avail = self.vb_avail[r.slot as usize];
                let n: i64 = if avail < r.last { 0 } else if r.stride != 0 { ((avail - r.last) / r.stride + 1) as i64 } else { i64::MAX };
                if r.instance { d_inst = d_inst.min(n - i_first as i64); } else if !indexed { d_count = d_count.min(n - v_first as i64); }
            }
            if indexed { d_count = d_count.min(ib_max as i64); }
            if d_count != count as i64 || d_inst != inst as i64 {
                log_once(&format!("clamp{}", pl.label), &format!("draw clamped to its buffers ({}): {count} x {inst} -> {d_count} x {d_inst}", pl.label));
                if d_count <= 0 || d_inst <= 0 { self.skip(); return; }
            }
        }
        let args = if kind >= 4 {
            match self.buf(u(1)).and_then(|b| b.gpu.clone()) { Some(g) => { self.use_buf(u(1)); Some((g, u(2) as u64)) } None => { self.skip(); return } }
        } else { None };

        let st = &mut self.pass_st;
        let pass = self.pass.as_mut().unwrap();
        if st.pl != Some(pl.uid) { pass.set_pipeline(&pl.pipeline); st.pl = Some(pl.uid); }
        for (i, g) in [g0, g1].into_iter().enumerate() {
            if let Some(g) = g {
                let d = g.dyn_off.unwrap_or(u32::MAX);
                if st.bg[i] != Some((g.uid, d)) {
                    match g.dyn_off { Some(o) => pass.set_bind_group(i as u32, &g.bg, &[o]), None => pass.set_bind_group(i as u32, &g.bg, &[]) }
                    st.bg[i] = Some((g.uid, d));
                }
            }
        }
        if st.vb.len() < 16 { st.vb.resize(16, None); }
        for (slot, g, off, ident) in &vb_binds {
            let key = (*ident, *off);
            if st.vb[*slot as usize] != Some(key) {
                pass.set_vertex_buffer(*slot, g.slice(*off..));
                st.vb[*slot as usize] = Some(key);
            }
        }
        if let Some((g, fmt, off, ident)) = &ib_bind {
            if st.ib != Some((*ident as u64, *fmt, *off)) {
                pass.set_index_buffer(g.slice(*off..), *fmt);
                st.ib = Some((*ident as u64, *fmt, *off));
            }
        }
        if let Some(v) = viewport {
            if st.vp != Some(v) {
                pass.set_viewport(v[0], v[1], v[2].max(1.0), v[3].max(1.0), v[4].clamp(0.0, 1.0), v[5].clamp(v[4].clamp(0.0, 1.0), 1.0));
                st.vp = Some(v);
            }
        }
        if st.sc != Some(scissor) { pass.set_scissor_rect(scissor[0], scissor[1], scissor[2], scissor[3]); st.sc = Some(scissor); }
        if st.bf != Some(bf) { pass.set_blend_constant(Color { r: bf[0], g: bf[1], b: bf[2], a: bf[3] }); st.bf = Some(bf); }
        if st.sref != Some(sref) { pass.set_stencil_reference(sref); st.sref = Some(sref); }
        match kind {
            4 => { let (g, off) = args.unwrap(); pass.draw_indexed_indirect(&g, off); }
            5.. => { let (g, off) = args.unwrap(); pass.draw_indirect(&g, off); }
            _ => {
                if indexed {
                    pass.draw_indexed(start..start + d_count as u32, if fold_v { 0 } else { base_vertex }, i_first..i_first + d_inst as u32);
                } else {
                    pass.draw(v_first..v_first + d_count as u32, i_first..i_first + d_inst as u32);
                }
            }
        }
        self.draws += 1;
        self.finish_hud_draw(&vs,ps.as_deref());
        // Camera context arrives with this exact GPU command, not from a newer
        // game-thread camera. Only record the qualified world's real depth.
        let ds_tex = self.s.ds.tex;
        let actual_viewport = viewport.unwrap_or([0., 0., pass_width as f32, pass_height as f32, 0., 1.]);
        let full_world_viewport = self.tex(ds_tex).is_some_and(|t| {
            let expected = [0., 0., t.desc.w as f32, t.desc.h as f32, 0., 1.];
            actual_viewport.iter().zip(expected).all(|(&actual, expected)| actual.is_finite() && (actual - expected).abs() <= 0.00002)
        });
        let writes_depth = !pass_ro_depth && self.state(self.s.dss.0).is_some_and(|state|
            state.words.first().copied().unwrap_or(0) != 0 && state.words.get(1) == Some(&1));
        if self.geometry_stereo_capture && writes_depth && full_world_viewport
            && self.world_view.is_some_and(|view| view.headset_applied && view.depth_reverse_z)
            && self.tex(ds_tex).is_some_and(|texture| is_depth(texture.format)
                && texture.desc.samples == 1 && texture.desc.d == 1) {
            self.note_geometry_world_pair(&vs, ps.as_deref());
        }
        let ds = &self.s.ds;
        if writes_depth && !full_world_viewport && ds.tex != 0
            && self.world_view.is_some_and(|view| view.headset_applied) && self.world_depth_anomalies.len() < 8 {
            let depth_state = self.state(self.s.dss.0).map(|state| state.words.clone());
            self.world_depth_anomalies.push(serde_json::json!({"depth":ds.tex,"viewport":actual_viewport,
                "vertex":vs.label,"pixel":ps.as_ref().map(|program|program.label.as_str()),
                "depth_state":depth_state,"world":self.world_view.map(|view|serde_json::json!({
                    "sequence":view.camera_sequence,"reverse_z":view.depth_reverse_z,"headset_applied":view.headset_applied}))}));
        }
        if let Some(view) = self.world_view.filter(|view| view.headset_applied) {
            let key = (ds.tex, view.camera_sequence, writes_depth, ds.flags, full_world_viewport);
            if self.world_depth_trace.len() < 32 || self.world_depth_trace.contains_key(&key) {
                self.world_depth_trace.entry(key).or_insert((0, actual_viewport)).0 += 1;
            }
        }
        // GTA's far water and fog passes compress raster depth into two f32
        // epsilons at reverse-Z far. With clipped vertices, no depth bias, no
        // fragment depth override and Greater[Equal], those writes cannot touch
        // nearer scene depth. Preserve an already-qualified target only when
        // its camera/rays still match; the actual far values remain in the copy.
        let preserves_world_depth = self.world_view.is_some_and(|view|
            view.headset_applied && view.depth_reverse_z
            && self.world_depths.get(&ds.tex).is_some_and(|(previous, draws)|
                *draws > 0 && previous.depth_reverse_z
                && crate::world_frame::shares_screen_rays(&view, previous))
            && self.tex(ds.tex).is_some_and(|texture| {
                let expected = [0., 0., texture.desc.w as f32, texture.desc.h as f32];
                actual_viewport[..4].iter().zip(expected).all(|(&actual, expected)|
                    actual.is_finite() && (actual - expected).abs() <= 0.00002)
            })
            && actual_viewport[4].is_finite() && actual_viewport[5].is_finite()
            && actual_viewport[4] >= 0. && actual_viewport[4] <= actual_viewport[5]
            && actual_viewport[5] <= 2. * f32::EPSILON
            && self.state(self.s.dss.0).is_some_and(|state| matches!(state.words.get(2), Some(5 | 7)))
            && !ps.as_ref().is_some_and(|program| program.info.writes_fragment_depth)
            && self.state(self.s.rs).is_none_or(|state|
                state.words.get(3).copied().unwrap_or(0) == 0
                && wf(state.words.get(4).copied().unwrap_or(0)) == 0.
                && wf(state.words.get(5).copied().unwrap_or(0)) == 0.
                && (state.words.get(6).copied() != Some(0)
                    || !self.device.features().contains(Features::DEPTH_CLIP_CONTROL))));
        if writes_depth && preserves_world_depth {
            let pixel_label = ps.as_ref().map(|program| program.label.as_str()).unwrap_or("-");
            log_once(&format!("worldfar{}+{}", vs.label, pixel_label), &format!(
                "qualified far-background depth retained: {} + {}; viewport {:?}; depth state {:?}; rasterizer {:?}; reverse-Z same-camera depth already qualified",
                vs.label, pixel_label, actual_viewport,
                self.state(self.s.dss.0).map(|state| state.words.clone()),
                self.state(self.s.rs).map(|state| state.words.clone())));
        }
        if writes_depth && ds.mip == 0 && ds.slice == 0 && !foreground_accepted {
            if let Some(view) = self.world_view.filter(|view| view.headset_applied && full_world_viewport && self.tex(ds.tex).is_some_and(|t|
                is_depth(t.format) && t.desc.samples == 1 && t.desc.d == 1)) {
                self.world_depth_before_ui.remove(&ds.tex); // A newer qualified scene supersedes the saved image.
                let entry = self.world_depths.entry(ds.tex).or_insert((view, 0));
                if entry.0.camera_sequence != view.camera_sequence { *entry = (view, 0); }
                entry.1 += 1;
            } else if !preserves_world_depth {
                // A later weapon/reflection pass cannot inherit a prior world's
                // projection merely because it writes the same-sized target.
                if self.world_depths.contains_key(&ds.tex) || self.world_depth_before_ui.contains_key(&ds.tex) {
                    let reason = serde_json::json!({"operation":"unqualified depth draw", "vertex":vs.label,
                        "pixel":ps.as_ref().map(|program|program.label.as_str()), "viewport":actual_viewport,
                        "depth_state":self.state(self.s.dss.0).map(|state|state.words.clone()),
                        "rasterizer":self.state(self.s.rs).map(|state|state.words.clone()),
                        "world":self.world_view.map(|view|serde_json::json!({"sequence":view.camera_sequence,
                            "reverse_z":view.depth_reverse_z,"headset_applied":view.headset_applied}))});
                    self.invalidate_world_depth_inner(ds.tex, reason,
                        if ui_depth_clear { WorldDepthChange::ProvenUiClear } else { WorldDepthChange::Other });
                }
            }
        }
        self.trace_color_draw(&vs, ps.as_deref(), actual_viewport, scissor, writes_depth,
            full_world_viewport, ui_depth_clear, p, d_count as u32, d_inst as u32);
        if self.geometry_stereo_capture && !self.geometry_stereo_capture_done
            && d_count == count as i64 && d_inst == inst as i64 {
            self.capture_geometry_stereo_draw(kind, count, inst, start, base_vertex, start_instance, &layout, &vs,
                ps.as_deref(), actual_viewport, writes_depth);
        }
    }

    fn do_dispatch(&mut self, x: u32, y: u32, z: u32) {
        let cs_id = self.s.shader[2];
        let Some(prog) = self.shader_program(cs_id).and_then(|program| self.float_sampling_program(program, 2)) else { self.skipped += 1; return };
        self.prepare_depth_proxies(&prog, 2);
        self.end_pass();

        let mut subs = Vec::new();
        for b in prog.group_bindings.get(&0).cloned().unwrap_or_default() {
            if b.kind != Kind::StorageTexture { continue; }
            let uid = *self.s.uav[2].get((b.binding as usize).wrapping_sub(160)).unwrap_or(&0);
            let tf = self.view_obj(uid).filter(|v| !v.is_buf).and_then(|v| self.tex(v.res)).map(|t| t.format);
            if let (Some(tf), Some(bf)) = (tf, b.format) {
                if tf != bf { if let Some(name) = wgsl_format_name(tf) { subs.push((b.binding, b.format_name.clone(), name.to_string())); } }
            }
        }
        let key = format!("{}|{}", prog.uid, subs.iter().map(|s| format!("{}:{}", s.0, s.2)).collect::<Vec<_>>().join(";"));
        if !self.compute.contains_key(&key) {
            let cprog = if subs.is_empty() { prog.clone() } else {
                let mut text = prog.text.clone();
                for (binding, from, to) in &subs {
                    let re = Regex::new(&format!(r"(@binding\({binding}u?\)\s*var\s+\w+\s*:\s*texture_storage_\w+<){}", regex::escape(from))).unwrap();
                    text = re.replace(&text, format!("${{1}}{to}").as_str()).into_owned();
                }
                let label = format!("{} [uav formats]", prog.label);
                match self.scoped("compute variant", |d| d.create_shader_module(ShaderModuleDescriptor { label: Some(&label), source: ShaderSource::Wgsl(text.as_str().into()) })) {
                    Some(module) => {
                        let mut info = parse_wgsl(&text);
                        for binding in &mut info.bindings {
                            if prog.info.bindings.iter().any(|original| original.group == binding.group
                                && original.binding == binding.binding && original.sample == Sample::Unfilterable) {
                                binding.sample = Sample::Unfilterable;
                            }
                        }
                        let mut gb: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
                        for b in &info.bindings { gb.entry(b.group).or_default().push(b.clone()); }
                        for v in gb.values_mut() { v.sort_by_key(|b| b.binding); }
                        let uid = self.uid();
                        Rc::new(Program { hud_kind:prog.hud_kind, module, info, consts: prog.consts.clone(), pack: prog.pack.as_ref().map(|p| Pack { members: p.members.iter().map(|m| PackMember { binding: m.binding, size: m.size, offset: m.offset }).collect(), size: p.size }), text, flat: vec![], group_bindings: gb, variants: RefCell::new(HashMap::new()), label: prog.label.clone(), uid, vid: false, iid: false, out_locs: vec![], in_locs: vec![], clip: false })
                    }
                    None => { self.compute.insert(key.clone(), None); self.skipped += 1; return; }
                }
            };
            let entries: Vec<_> = cprog.info.bindings.iter().filter(|b| b.group == 0).map(|b| Self::layout_entry(b, 2)).collect();
            let bgl = self.device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: None, entries: &entries });
            let layout = self.device.create_pipeline_layout(&PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
            let label = cprog.label.clone();
            let pl = self.scoped(&format!("compute pipeline {label}"), |d| d.create_compute_pipeline(&ComputePipelineDescriptor {
                label: Some(&label), layout: Some(&layout), module: &cprog.module, entry_point: Some("main"), compilation_options: Default::default(), cache: None,
            }));
            let uid = self.uid();
            self.compute.insert(key.clone(), pl.map(|p| (p, bgl, uid, cprog)));
        }
        let Some((pl, bgl, bgl_uid, cprog)) = self.compute.get(&key).cloned().flatten() else { self.skipped += 1; return };
        let Some(g0) = self.stage_bind_group(2, &cprog, 0, &(bgl, bgl_uid)) else { self.skipped += 1; return };
        let enc = self.encoder();
        let mut cp = enc.begin_compute_pass(&ComputePassDescriptor::default());
        cp.set_pipeline(&pl);
        match g0.dyn_off { Some(o) => cp.set_bind_group(0, &g0.bg, &[o]), None => cp.set_bind_group(0, &g0.bg, &[]) }
        cp.dispatch_workgroups(x, y, z);
    }

    fn clear_view(&mut self, id: u32, mip: u32, slice: u32) -> Option<TextureView> {
        let t = self.tex(id)?;
        if t.desc.dim == TextureDimension::D3 {
            return Some(t.tex.create_view(&TextureViewDescriptor { base_mip_level: mip, mip_level_count: Some(1), base_array_layer: slice, array_layer_count: Some(1), dimension: Some(TextureViewDimension::D2), ..Default::default() }));
        }
        self.target_view(id, 0, mip, slice).map(|v| v.0)
    }

    fn in_open_pass(&self, id: u32) -> bool {
        self.pass.is_some() && self.pass_key.as_ref().map(|k| (0..9).any(|i| k.words()[i * 4] == id)).unwrap_or(false)
    }
    fn clear_rt(&mut self, id: u32, mip: u32, slice: u32, c: [f32; 4]) {
        let Some(three) = self.tex(id).map(|t| t.desc.dim == TextureDimension::D3) else { return };
        self.mark_color_dirty(id);
        if three { self.materialize_clears(id); self.clear_rt_now(id, mip, slice, Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: c[3] as f64 }); return; }
        if self.in_open_pass(id) { self.end_pass(); }
        let list = self.pending_clears.entry(id).or_default();
        let color = Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: c[3] as f64 };
        match list.iter_mut().find(|e| e.0 == mip && e.1 == slice) {
            Some(e) => e.2.color = Some(color),
            None => list.push((mip, slice, PendingClear { color: Some(color), ..Default::default() })),
        }
    }
    fn clear_ds(&mut self, id: u32, mip: u32, slice: u32, flags: u32, depth: f32, stencil: u32) {
        let Some((fmt, three)) = self.tex(id).map(|t| (t.format, t.desc.dim == TextureDimension::D3)) else { return };
        self.mark_depth_dirty(id);
        let d = (flags & 1 != 0).then(|| depth.clamp(0.0, 1.0));
        if d.is_some() && mip == 0 && slice == 0 { self.invalidate_world_depth_inner(id, serde_json::json!("clear_ds"), WorldDepthChange::LiveClear); }
        let st = (flags & 2 != 0 && has_stencil(fmt)).then_some(stencil);
        if d.is_none() && st.is_none() { return; }
        if three { self.materialize_clears(id); self.clear_ds_now(id, mip, slice, d, st); return; }
        if self.in_open_pass(id) { self.end_pass(); }
        let list = self.pending_clears.entry(id).or_default();
        match list.iter_mut().find(|e| e.0 == mip && e.1 == slice) {
            Some(e) => { if d.is_some() { e.2.depth = d; } if st.is_some() { e.2.stencil = st; } }
            None => list.push((mip, slice, PendingClear { depth: d, stencil: st, ..Default::default() })),
        }
    }

    fn materialize_clears(&mut self, id: u32) {
        let Some(list) = self.pending_clears.remove(&id) else { return };
        self.end_pass();
        for (mip, slice, c) in list {
            if let Some(color) = c.color { self.clear_rt_now(id, mip, slice, color); }
            if c.depth.is_some() || c.stencil.is_some() { self.clear_ds_now(id, mip, slice, c.depth, c.stencil); }
        }
    }
    fn clear_rt_now(&mut self, id: u32, mip: u32, slice: u32, color: Color) {
        if self.tex(id).is_none() { return; }
        self.end_pass();
        let Some(view) = self.clear_view(id, mip, slice) else { return };
        self.use_tex(id);
        self.encoder().begin_render_pass(&RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Clear(color), store: StoreOp::Store } })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
    }
    fn clear_ds_now(&mut self, id: u32, mip: u32, slice: u32, depth: Option<f32>, stencil: Option<u32>) {
        let Some(fmt) = self.tex(id).map(|t| t.format) else { return };
        if depth.is_some() && mip == 0 && slice == 0 { self.invalidate_world_depth_inner(id, serde_json::json!("clear_ds_now"), WorldDepthChange::LiveClear); }
        self.end_pass();
        let Some(view) = self.clear_view(id, mip, slice) else { return };
        self.use_tex(id);
        self.encoder().begin_render_pass(&RenderPassDescriptor {
            label: None, color_attachments: &[],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: &view,
                depth_ops: Some(Operations { load: depth.map(LoadOp::Clear).unwrap_or(LoadOp::Load), store: StoreOp::Store }),
                stencil_ops: if has_stencil(fmt) { Some(Operations { load: stencil.map(LoadOp::Clear).unwrap_or(LoadOp::Load), store: StoreOp::Store }) } else { None },
            }),
            timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
    }
    fn clear_uav(&mut self, id: u32, values: [u32; 4]) {
        let Some((res, is_buf, a)) = self.view_obj(id).map(|v| (v.res, v.is_buf, v.a)) else { return };
        let zero = values.iter().all(|&v| v == 0);
        if is_buf {
            let Some((g, bytes)) = self.buf(res).and_then(|b| b.gpu.clone().map(|g| (g, b.bytes))) else { return };
            self.op47_dedup_gpu_write(res, crate::upload_dedup::GpuWrite::ClearUav);
            self.end_pass();
            if zero {
                self.encoder().clear_buffer(&g, 0, None);
            } else {
                self.flush();
                let data: Vec<u8> = (0..bytes / 4).flat_map(|i| values[(i & 3) as usize].to_le_bytes()).collect();
                measured_write_buffer(&mut self.uploads, &self.device, &self.queue, &mut self.buffer_uploads[UPLOAD_UAV_CLEAR], &g, 0, &data);
            }
            self.use_buf(res);
            return;
        }
        self.materialize_clears(res);
        let Some((tex, fmt, desc)) = self.tex(res).map(|t| (t.tex.clone(), t.format, t.desc)) else { return };
        let bpt = texel_bytes(fmt);
        if bpt == 0 { log_once(&format!("clearuav{fmt:?}"), &format!("ClearUnorderedAccessView: texture format {fmt:?} not supported")); return; }
        let (w, h) = ((desc.w >> a).max(1), (desc.h >> a).max(1));
        let layers = if desc.dim == TextureDimension::D3 { (desc.d >> a).max(1) } else { desc.d };
        if zero {
            let bpr = (w * bpt).div_ceil(256) * 256;
            let need = (bpr * h) as u64;
            if self.zero_src.as_ref().map(|z| z.size() < need).unwrap_or(true) {
                self.zero_src = Some(self.device.create_buffer(&BufferDescriptor { label: Some("zero"), size: need.max(1 << 20), usage: BufferUsages::COPY_SRC, mapped_at_creation: false }));
            }
            let z = self.zero_src.clone().unwrap();
            self.end_pass();
            let enc = self.encoder();
            for l in 0..layers {
                enc.copy_buffer_to_texture(
                    TexelCopyBufferInfo { buffer: &z, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) } },
                    TexelCopyTextureInfo { texture: &tex, mip_level: a, origin: Origin3d { x: 0, y: 0, z: l }, aspect: TextureAspect::All },
                    Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
            }
            self.use_tex(res);
            self.mark_color_dirty(res);
            return;
        }
        self.flush();
        let mut data = vec![0u8; (w * h * bpt) as usize];
        for i in 0..(w * h) as usize {
            for c in 0..(bpt / 4) as usize {
                data[i * bpt as usize + c * 4..i * bpt as usize + c * 4 + 4].copy_from_slice(&values[c & 3].to_le_bytes());
            }
        }
        for l in 0..layers {
            self.queue.write_texture(
                TexelCopyTextureInfo { texture: &tex, mip_level: a, origin: Origin3d { x: 0, y: 0, z: l }, aspect: TextureAspect::All },
                &data, TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bpt), rows_per_image: Some(h) },
                Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
    }

    fn sub_of(desc: &TexDesc, sub: u32) -> (u32, u32) {
        (sub % desc.mips, sub / desc.mips)
    }
    #[allow(clippy::too_many_arguments)]
    fn copy_region(&mut self, dst: u32, dst_sub: u32, dx: u32, dy: u32, dz: u32, src: u32, src_sub: u32, has_box: bool, l: u32, t: u32, f: u32, r: u32, b: u32, bk: u32) {
        self.materialize_clears(src);
        self.materialize_clears(dst);
        if self.obj(src).is_none() || self.obj(dst).is_none() { return; }
        self.end_pass();
        if let (Some(sb), Some(db)) = (self.buf(src), self.buf(dst)) {
            let (Some(sg), Some(dg)) = (sb.gpu.clone(), db.gpu.clone()) else { return };
            let size = if has_box { (r - l) as u64 } else { sb.size.min(db.size) };
            let size = size & !3;
            self.op47_dedup_gpu_write(dst, crate::upload_dedup::GpuWrite::CopyRegion);
            if size > 0 { self.encoder().copy_buffer_to_buffer(&sg, if has_box { l as u64 } else { 0 }, &dg, dx as u64, size); }
            self.use_buf(src);
            self.use_buf(dst);
            return;
        }
        let (Some(s), Some(d)) = (self.tex(src), self.tex(dst)) else { return };
        let (ss, ds) = (Self::sub_of(&s.desc, src_sub), Self::sub_of(&d.desc, dst_sub));
        let (mw, mh, md) = ((s.desc.w >> ss.0).max(1), (s.desc.h >> ss.0).max(1), if s.desc.dim == TextureDimension::D3 { (s.desc.d >> ss.0).max(1) } else { 1 });
        let (w, h, dep) = if has_box { (r - l, b - t, bk - f) } else { (mw, mh, md) };
        let three = s.desc.dim == TextureDimension::D3;
        let d3 = d.desc.dim == TextureDimension::D3;
        let (stex, dtex, sf, df) = (s.tex.clone(), d.tex.clone(), s.format, d.format);
        self.invalidate_world_depth(dst, serde_json::json!("copy_region"));
        if sf != df && !three && !d3 && (block_bytes(sf) != 0) != (block_bytes(df) != 0) && block_bytes(sf).max(block_bytes(df)) == texel_bytes(sf).max(texel_bytes(df)) {
            let bytes = block_bytes(sf).max(block_bytes(df));
            let s_bc = block_bytes(sf) != 0;
            let (cw, ch) = if s_bc { (w.div_ceil(4), h.div_ceil(4)) } else { (w, h) };
            let bpr = (cw * bytes).div_ceil(256) * 256;
            let need = (bpr * ch) as u64;
            if self.scratch.as_ref().map(|x| x.size() < need).unwrap_or(true) {
                self.scratch = Some(self.device.create_buffer(&BufferDescriptor { label: Some("scratch"), size: need.max(1 << 16), usage: BufferUsages::COPY_SRC | BufferUsages::COPY_DST, mapped_at_creation: false }));
            }
            let sc = self.scratch.clone().unwrap();
            let enc = self.encoder();
            enc.copy_texture_to_buffer(
                TexelCopyTextureInfo { texture: &stex, mip_level: ss.0, origin: Origin3d { x: if has_box { l } else { 0 }, y: if has_box { t } else { 0 }, z: ss.1 }, aspect: TextureAspect::All },
                TexelCopyBufferInfo { buffer: &sc, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(ch) } },
                Extent3d { width: if s_bc { cw * 4 } else { cw }, height: if s_bc { ch * 4 } else { ch }, depth_or_array_layers: 1 },
            );
            enc.copy_buffer_to_texture(
                TexelCopyBufferInfo { buffer: &sc, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(ch) } },
                TexelCopyTextureInfo { texture: &dtex, mip_level: ds.0, origin: Origin3d { x: dx, y: dy, z: ds.1 }, aspect: TextureAspect::All },
                Extent3d { width: if !s_bc { cw * 4 } else { cw }, height: if !s_bc { ch * 4 } else { ch }, depth_or_array_layers: 1 },
            );
        } else {
            let enc = self.encoder();
            enc.copy_texture_to_texture(
                TexelCopyTextureInfo { texture: &stex, mip_level: ss.0, origin: Origin3d { x: if has_box { l } else { 0 }, y: if has_box { t } else { 0 }, z: if three { if has_box { f } else { 0 } } else { ss.1 } }, aspect: TextureAspect::All },
                TexelCopyTextureInfo { texture: &dtex, mip_level: ds.0, origin: Origin3d { x: dx, y: dy, z: if d3 { dz } else { ds.1 } }, aspect: TextureAspect::All },
                Extent3d { width: w, height: h, depth_or_array_layers: if three { dep } else { 1 } },
            );
        }
        self.mark_depth_dirty(dst);
        self.mark_color_dirty(dst);
        self.use_tex(src);
        self.use_tex(dst);
    }
    fn copy_resource(&mut self, dst: u32, src: u32) {
        self.materialize_clears(src);
        self.materialize_clears(dst);
        if self.buf(src).is_some() { self.copy_region(dst, 0, 0, 0, 0, src, 0, false, 0, 0, 0, 0, 0, 0); return; }
        let (Some(s), Some(d)) = (self.tex(src), self.tex(dst)) else { return };
        let (stex, dtex, sd, dd) = (s.tex.clone(), d.tex.clone(), s.desc, d.desc);
        self.invalidate_world_depth(dst, serde_json::json!("copy_resource"));
        self.end_pass();
        let enc = self.encoder();
        for mip in 0..sd.mips.min(dd.mips) {
            let (w, h) = ((sd.w >> mip).max(1), (sd.h >> mip).max(1));
            let layers = if sd.dim == TextureDimension::D3 { (sd.d >> mip).max(1) } else { sd.d };
            enc.copy_texture_to_texture(
                TexelCopyTextureInfo { texture: &stex, mip_level: mip, origin: Origin3d::ZERO, aspect: TextureAspect::All },
                TexelCopyTextureInfo { texture: &dtex, mip_level: mip, origin: Origin3d::ZERO, aspect: TextureAspect::All },
                Extent3d { width: w, height: h, depth_or_array_layers: layers },
            );
        }
        self.mark_depth_dirty(dst);
        self.mark_color_dirty(dst);
        self.use_tex(src);
        self.use_tex(dst);
    }
    fn resolve(&mut self, dst: u32, dst_sub: u32, src: u32, src_sub: u32) {
        self.foreground_foreign_write(dst, "resolve_destination");
        self.materialize_clears(src);
        self.materialize_clears(dst);
        let (Some(s), Some(d)) = (self.tex(src), self.tex(dst)) else { return };
        let (ss, ds) = (Self::sub_of(&s.desc, src_sub), Self::sub_of(&d.desc, dst_sub));
        let sv = s.tex.create_view(&TextureViewDescriptor { base_mip_level: ss.0, mip_level_count: Some(1), base_array_layer: ss.1, array_layer_count: Some(1), dimension: Some(TextureViewDimension::D2), ..Default::default() });
        let dv = d.tex.create_view(&TextureViewDescriptor { base_mip_level: ds.0, mip_level_count: Some(1), base_array_layer: ds.1, array_layer_count: Some(1), dimension: Some(TextureViewDimension::D2), ..Default::default() });
        self.end_pass();
        self.encoder().begin_render_pass(&RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(RenderPassColorAttachment { view: &sv, depth_slice: None, resolve_target: Some(&dv), ops: Operations { load: LoadOp::Load, store: StoreOp::Discard } })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
        self.mark_color_dirty(dst);
        self.use_tex(src);
        self.use_tex(dst);
    }

    fn generate_mips(&mut self, srv_id: u32) {
        let Some(res) = self.view_obj(srv_id).map(|v| v.res) else { return };
        self.materialize_clears(res);
        let Some((fmt, desc, tex)) = self.tex(res).map(|t| (t.format, t.desc, t.tex.clone())) else { return };
        if desc.mips < 2 { return; }
        if is_depth(fmt) || is_bc(fmt) || desc.dim == TextureDimension::D3 { log_once(&format!("mips{fmt:?}"), &format!("GenerateMips: unsupported format/dimension {fmt:?}")); return; }
        self.end_pass();
        if !self.mip_pipelines.contains_key(&fmt) {
            let module = self.device.create_shader_module(ShaderModuleDescriptor { label: None, source: ShaderSource::Wgsl(
                "@group(0) @binding(0) var src: texture_2d<f32>;
                 @group(0) @binding(1) var smp: sampler;
                 @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3)); return vec4f(p[i], 0, 1); }
                 @fragment fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
                   let dst = max(vec2f(1.0), floor(vec2f(textureDimensions(src, 0)) * 0.5));
                   return textureSampleLevel(src, smp, pos.xy / dst, 0.0);
                 }".into()) });
            let pl = self.simple_pipeline(&module, Some(fmt), None);
            self.mip_pipelines.insert(fmt, pl);
        }
        let pl = self.mip_pipelines[&fmt].clone();
        let dev = self.device.clone();
        let smp = self.mip_sampler.get_or_insert_with(|| dev.create_sampler(&SamplerDescriptor { mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..Default::default() })).clone();
        let t = self.tex_mut(res).unwrap();
        if t.mip_passes.is_none() {
            let mut passes = Vec::new();
            for layer in 0..desc.d {
                for mip in 1..desc.mips {
                    let src = tex.create_view(&TextureViewDescriptor { dimension: Some(TextureViewDimension::D2), base_mip_level: mip - 1, mip_level_count: Some(1), base_array_layer: layer, array_layer_count: Some(1), ..Default::default() });
                    let dst = tex.create_view(&TextureViewDescriptor { dimension: Some(TextureViewDimension::D2), base_mip_level: mip, mip_level_count: Some(1), base_array_layer: layer, array_layer_count: Some(1), ..Default::default() });
                    let bg = dev.create_bind_group(&BindGroupDescriptor { label: None, layout: &pl.get_bind_group_layout(0), entries: &[
                        BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&src) }, BindGroupEntry { binding: 1, resource: BindingResource::Sampler(&smp) }] });
                    passes.push((dst, bg));
                }
            }
            t.mip_passes = Some(passes);
        }
        let passes = t.mip_passes.clone().unwrap();
        let enc = self.encoder();
        for (dst, bg) in &passes {
            let mut p = enc.begin_render_pass(&RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(RenderPassColorAttachment { view: dst, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Clear(Color::BLACK), store: StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            p.set_pipeline(&pl);
            p.set_bind_group(0, bg, &[]);
            p.draw(0..3, 0..1);
        }
        self.use_tex(res);
        self.mark_color_dirty(res);
    }

    fn readback(&mut self, p: u64) {
        let mb = self.base;
        let u = move |w: u64| unsafe { std::ptr::read_volatile(mb.add(((p + w) * 4) as usize) as *const u32) };
        let (src, sub, has_box) = (u(0), u(1), u(2) != 0);
        let (l, t, f, r, b, bk) = (u(3), u(4), u(5), u(6), u(7), u(8));
        let addr = u(9) as u64 + u(10) as u64 * 4294967296;
        let (row_pitch, slice_pitch, is_buf, size) = (u(11) as u64, u(12) as u64, u(13) != 0, u(14) as u64);
        let flag = u(15) as u64 + u(16) as u64 * 4294967296;
        self.materialize_clears(src);
        let done = |me: &Self| { if flag != 0 { atomic(&me.mem, flag).store(0, Ordering::SeqCst); let _ = me.mem.atomic_notify(flag, u32::MAX); } };
        if self.obj(src).is_none() { done(self); return; }
        if is_buf {
            let Some((g, bsize)) = self.buf(src).and_then(|bo| bo.gpu.clone().map(|g| (g, bo.size))) else { done(self); return };
            let n = if has_box { (r - l) as u64 } else { size.min(bsize) };
            let padded = ((n + 3) & !3).max(4);
            let rb = self.device.create_buffer(&BufferDescriptor { label: Some("readback"), size: padded, usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
            self.end_pass();
            let from = if has_box { l as u64 } else { 0 };
            let copy = padded.min(g.size().saturating_sub(from)) & !3;
            if copy > 0 { self.encoder().copy_buffer_to_buffer(&g, from, &rb, 0, copy); }
            self.flush();
            let rows = self.bound_readback(vec![(0, addr, n)], addr, size, "a buffer");
            self.map_pending(rb, MapKind::Readback { flag, rows });
            return;
        }
        let Some((tex, fmt, desc, decoded)) = self.tex(src).map(|x| (x.tex.clone(), x.format, x.desc, x.bc_source)) else { done(self); return };
        if let Some(source) = decoded {
            // The guest destination is laid out in BC blocks, but this device
            // holds the texture as decoded RGBA: different row count and pitch.
            log_once(&format!("rbbc{source:?}"), &format!("readback of software-decoded {source:?} texture skipped; the guest destination is not written"));
            done(self);
            return;
        }
        if desc.samples > 1 || is_depth(fmt) { log_once(&format!("rbfmt{fmt:?}"), &format!("readback of {fmt:?} not supported")); done(self); return; }
        let (mip, layer) = (sub % desc.mips, sub / desc.mips);
        let (mw, mh) = ((desc.w >> mip).max(1), (desc.h >> mip).max(1));
        let three = desc.dim == TextureDimension::D3;
        let md = if three { (desc.d >> mip).max(1) } else { 1 };
        let (bw, bh) = fmt.block_dimensions();
        let bytes = fmt.block_copy_size(None).unwrap_or(4);
        let (x0, y0, z0) = if has_box { (l, t, f) } else { (0, 0, 0) };
        let (w, h, d) = if has_box { (r - l, b - t, bk - f) } else { (mw, mh, md) };
        let (bx, by) = (w.div_ceil(bw), h.div_ceil(bh));
        let row_bytes = (bx * bytes) as u64;
        let bpr = row_bytes.div_ceil(256) * 256;
        let rb_size = bpr * by as u64 * d as u64;
        let rb = self.device.create_buffer(&BufferDescriptor { label: Some("readback"), size: rb_size.max(4), usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        self.end_pass();
        self.encoder().copy_texture_to_buffer(
            TexelCopyTextureInfo { texture: &tex, mip_level: mip, origin: Origin3d { x: x0, y: y0, z: if three { z0 } else { layer } }, aspect: TextureAspect::All },
            TexelCopyBufferInfo { buffer: &rb, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr as u32), rows_per_image: Some(by) } },
            Extent3d { width: bx * bw, height: by * bh, depth_or_array_layers: if three { d } else { 1 } },
        );
        self.flush();
        let mut rows = Vec::new();
        for z in 0..d as u64 {
            for row in 0..by as u64 {
                rows.push(((z * by as u64 + row) * bpr, addr + z * slice_pitch + row * row_pitch, row_bytes));
            }
        }
        let rows = self.bound_readback(rows, addr, size, &format!("{fmt:?} {bx}x{by}"));
        self.map_pending(rb, MapKind::Readback { flag, rows });
    }

    /// A readback never writes outside the guest's destination subresource.
    fn bound_readback(&self, rows: Vec<(u64, u64, u64)>, addr: u64, size: u64, what: &str) -> Vec<(u64, u64, u64)> {
        let (rows, withheld) = bounded_readback_rows(rows, addr, size, self.mem.data_size() as u64);
        if withheld != 0 {
            log_once(&format!("rbbound{what}"), &format!("readback of {what} exceeds the guest's {size}-byte destination at 0x{addr:x}; {withheld} bytes not written"));
        }
        rows
    }

    fn map_pending(&mut self, buf: Buffer, kind: MapKind) {
        let done = Arc::new(AtomicBool::new(false));
        let d2 = done.clone();
        buf.slice(..).map_async(MapMode::Read, move |r| { if r.is_err() { log("map failed"); } d2.store(true, Ordering::SeqCst); });
        self.pending_maps.push(PendingMap { buf, done, kind });
    }

    fn process_maps(&mut self, wait: bool) {
        if self.pending_maps.is_empty() { return; }
        let started = Instant::now();
        let _ = self.device.poll(if wait { PollType::Wait { submission_index: None, timeout: None } } else { PollType::Poll });
        let mut i = 0;
        while i < self.pending_maps.len() {
            if !self.pending_maps[i].done.load(Ordering::SeqCst) { i += 1; continue; }
            let pm = self.pending_maps.swap_remove(i);
            {
                let range = pm.buf.slice(..).get_mapped_range().expect("mapped range");
                match &pm.kind {
                    MapKind::Readback { rows, .. } => {
                        for &(from, to, n) in rows {
                            let n = n.min(range.len() as u64 - from.min(range.len() as u64));
                            unsafe { std::ptr::copy_nonoverlapping(range.as_ptr().add(from as usize), self.base.add(to as usize), n as usize); }
                        }
                    }
                    MapKind::Occlusion { list } => {
                        for (byte, cnt, life, consecutive, seq) in list {
                            let mut sum = 0u64;
                            for k in 0..*cnt as u64 {
                                let at = (byte + k * if *consecutive { 8 } else { 256 }) as usize;
                                if at + 8 <= range.len() { sum += u64::from_le_bytes(range[at..at + 8].try_into().unwrap()); }
                            }
                            life.complete(&self.mem, if sum != 0 { OCCLUSION_VISIBLE_COUNT } else { 0 }, *seq as u32);
                        }
                    }
                }
            }
            pm.buf.unmap();
            if let MapKind::Readback { flag, .. } = pm.kind {
                if flag != 0 { atomic(&self.mem, flag).store(0, Ordering::SeqCst); let _ = self.mem.atomic_notify(flag, u32::MAX); }
            }
        }
        self.thread_wall[THREAD_MAP_PROCESS].record(started.elapsed());
    }

    /// Closes the occlusion query open in the current pass, if any, and
    /// forgets which query is active.
    fn end_active_occlusion(&mut self) {
        if self.occlusion_pass_slot.take().is_some() { if let Some(p) = self.pass.as_mut() { p.end_occlusion_query(); } }
        self.occlusion_active = None;
    }
    fn create_query(&mut self, id: u32, qtype: u32, addr: u64) {
        self.set_obj(id, Obj::Query(QueryObj { qtype, life: QueryLife::new(addr), slots: vec![] }));
        if self.occlusion_set.is_none() {
            self.occlusion_set = Some(self.device.create_query_set(&QuerySetDescriptor { label: None, ty: QueryType::Occlusion, count: OCCLUSION_CAPACITY }));
        }
    }
    fn is_occlusion(t: u32) -> bool { t == 1 || t == 5 }
    fn begin_occlusion_in_pass(&mut self) {
        let Some(q) = self.occlusion_active else { return };
        if !matches!(self.obj(q), Some(Obj::Query(_))) { self.end_active_occlusion(); return; }
        if self.pass.is_none() || self.occlusion_pass_slot.is_some() { return; }
        if self.occlusion_next >= OCCLUSION_CAPACITY { log_once("qfull", "occlusion query set exhausted"); return; }
        let slot = self.occlusion_next;
        self.occlusion_next += 1;
        if let Some(Obj::Query(qo)) = self.obj_mut(q) { qo.slots.push(slot); }
        self.pass.as_mut().unwrap().begin_occlusion_query(slot);
        self.occlusion_pass_slot = Some(slot);
    }
    fn begin_query(&mut self, id: u32) {
        let Some(Obj::Query(q)) = self.obj_mut(id) else { return };
        if !Self::is_occlusion(q.qtype) { return; }
        q.slots.clear();
        self.occlusion_active = Some(id);
        self.begin_occlusion_in_pass();
    }
    fn end_query(&mut self, id: u32, seq: u32) {
        // The result belongs to this query instance, whatever later holds its id.
        let Some((qtype, life)) = (match self.obj(id) { Some(Obj::Query(q)) => Some((q.qtype, q.life.clone())), _ => None }) else { return };
        if Self::is_occlusion(qtype) {
            if self.occlusion_active == Some(id) { self.end_active_occlusion(); }
            let slots = match self.obj_mut(id) { Some(Obj::Query(q)) => std::mem::take(&mut q.slots), _ => vec![] };
            self.pending_resolve.push((life, seq, slots));
        } else {
            let t = Instant::now().duration_since(*START).as_nanos() as u64;
            self.pending_events.push((life, seq, if qtype == 2 { t } else { 1 }));
        }
    }
    fn record_resolves(&mut self) -> Option<PendingMap> {
        let list = std::mem::take(&mut self.pending_resolve);
        let mut layout = Vec::new();
        let mut bytes = 0u64;
        for (life, seq, slots) in list {
            if !life.is_live() { continue; }
            if slots.is_empty() { life.complete(&self.mem, 0, seq); continue; }
            let consecutive = slots.windows(2).all(|w| w[1] == w[0] + 1);
            layout.push((slots, bytes, life, consecutive, seq));
            bytes += if consecutive { (layout.last().unwrap().0.len() as u64 * 8).div_ceil(256) * 256 } else { layout.last().unwrap().0.len() as u64 * 256 };
        }
        if self.occlusion_next > OCCLUSION_CAPACITY - 256 && self.occlusion_active.is_none() { self.occlusion_next = 0; }
        if bytes == 0 { return None; }
        let size = bytes.next_power_of_two().max(256);
        let rb = self.device.create_buffer(&BufferDescriptor { label: Some("occlusion resolve"), size, usage: BufferUsages::QUERY_RESOLVE | BufferUsages::COPY_SRC, mapped_at_creation: false });
        let map = self.device.create_buffer(&BufferDescriptor { label: Some("occlusion map"), size, usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        let set = self.occlusion_set.clone()?;
        let enc = self.encoder();
        let mut out = Vec::new();
        for (slots, at, life, consecutive, seq) in &layout {
            if *consecutive {
                enc.resolve_query_set(&set, slots[0]..slots[0] + slots.len() as u32, &rb, *at);
            } else {
                for (i, s) in slots.iter().enumerate() { enc.resolve_query_set(&set, *s..*s + 1, &rb, at + i as u64 * 256); }
            }
            out.push((*at, slots.len() as u32, life.clone(), *consecutive, *seq as usize));
        }
        enc.copy_buffer_to_buffer(&rb, 0, &map, 0, bytes);
        let done = Arc::new(AtomicBool::new(false));
        Some(PendingMap { buf: map, done, kind: MapKind::Occlusion { list: out } })
    }
    fn on_submitted(&mut self) {
        let unmapped: Vec<usize> = self.pending_maps.iter().enumerate().filter(|(_, m)| matches!(m.kind, MapKind::Occlusion { .. }) && !m.done.load(Ordering::SeqCst)).map(|(i, _)| i).collect();
        let _ = unmapped;
        if !self.pending_events.is_empty() {
            let evs = std::mem::take(&mut self.pending_events);
            let mem = self.mem.clone();
            let inflight = self.inflight.clone();
            inflight.fetch_add(1, Ordering::SeqCst);
            self.queue.on_submitted_work_done(move || {
                for (life, seq, v) in evs { life.complete(&mem, v, seq); }
                inflight.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }

    fn set_uav_counter(&mut self, uav: u32, value: u32) {
        let Some(res) = self.view_obj(uav).filter(|v| v.is_buf).map(|v| v.res) else { return };
        if self.buf(res).is_none() { return; }
        if self.counter_src.is_none() {
            self.counter_src = Some(self.device.create_buffer(&BufferDescriptor { label: Some("counter src"), size: 1024 * 4, usage: BufferUsages::COPY_SRC | BufferUsages::COPY_DST, mapped_at_creation: false }));
        }
        if self.counter_slot >= 1024 { self.flush(); }
        let slot = self.counter_slot as u64;
        self.counter_slot += 1;
        let src = self.counter_src.clone().unwrap();
        measured_write_buffer(&mut self.uploads, &self.device, &self.queue, &mut self.buffer_uploads[UPLOAD_UAV_COUNTER], &src, slot * 4, &value.to_le_bytes());
        self.end_pass();
        let Some(cb) = self.counter_buffer(res) else { return };
        self.encoder().copy_buffer_to_buffer(&src, slot * 4, &cb, 0, 4);
        self.use_buf(res);
    }
    fn copy_counter(&mut self, dst: u32, offset: u64, uav: u32) {
        let Some(res) = self.view_obj(uav).filter(|v| v.is_buf).map(|v| v.res) else { return };
        let Some(dg) = self.buf(dst).and_then(|b| b.gpu.clone()) else { return };
        let Some(cb) = self.counter_buffer(res) else { return };
        self.op47_dedup_gpu_write(dst, crate::upload_dedup::GpuWrite::CopyCounter);
        self.end_pass();
        self.encoder().copy_buffer_to_buffer(&cb, 0, &dg, offset, 4);
        self.use_buf(dst);
        self.use_buf(res);
    }

    fn present(&mut self, id: u32) {
        let present_started = Instant::now();
        self.materialize_clears(id);
        // The compositor owns presentation during immersive play. Acquiring a
        // drawable from a hidden window can block world-frame publication.
        let visible = !OCCLUDED.load(Ordering::Relaxed) && !crate::xr_view::world_camera_enabled();
        #[cfg(target_os = "visionos")]
        let visible = visible && crate::native::window_can_present();
        if let (Some(t), Some(_), true) = (self.tex(id), self.surface.as_ref(), visible) {
            let (w, h) = (t.desc.w, t.desc.h);
            let tex = t.tex.clone();
            self.end_pass();
            if self.surface_size != (w, h) {
                let (surface, window) = self.surface.as_ref().unwrap();
                let caps = surface.get_capabilities(&self.adapter);
                surface.configure(&self.device, &SurfaceConfiguration {
                    usage: TextureUsages::RENDER_ATTACHMENT,
                    format: self.surface_format,
                    width: w,
                    height: h,
                    present_mode: if caps.present_modes.contains(&PresentMode::Mailbox) { PresentMode::Mailbox } else { PresentMode::Fifo },
                    desired_maximum_frame_latency: 2,
                    alpha_mode: caps.alpha_modes[0],
                    view_formats: vec![],
                    color_space: SurfaceColorSpace::Auto,
                });
                self.surface_size = (w, h);
                log(&format!("surface configured {w}x{h} {:?}", self.surface_format));
                #[cfg(not(target_os = "visionos"))]
                window.request_redraw();
                #[cfg(target_os = "visionos")]
                let _ = window;
            }
            if self.blit.is_none() {
                let module = self.device.create_shader_module(ShaderModuleDescriptor { label: Some("blit"), source: ShaderSource::Wgsl(
                    "@group(0) @binding(0) var src: texture_2d<f32>;
                     @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3)); return vec4f(p[i], 0, 1); }
                     @fragment fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f { return textureLoad(src, vec2i(pos.xy), 0); }".into()) });
                self.blit = Some(self.simple_pipeline(&module, Some(self.surface_format), None));
            }
            let blit = self.blit.clone().unwrap();
            let dev = self.device.clone();
            let bg = {
                let t = self.tex_mut(id).unwrap();
                t.blit_bg.get_or_insert_with(|| {
                    let v = tex.create_view(&TextureViewDescriptor { dimension: Some(TextureViewDimension::D2), base_mip_level: 0, mip_level_count: Some(1), base_array_layer: 0, array_layer_count: Some(1), ..Default::default() });
                    dev.create_bind_group(&BindGroupDescriptor { label: None, layout: &blit.get_bind_group_layout(0), entries: &[BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&v) }] })
                }).clone()
            };
            let frame = match self.surface.as_ref().unwrap().0.get_current_texture() {
                CurrentSurfaceTexture::Success(f) | CurrentSurfaceTexture::Suboptimal(f) => Some(f),
                other => { log_once("surface", &format!("surface texture unavailable: {other:?}")); self.surface_size = (0, 0); None }
            };
            if let Some(frame) = frame {
                let view = frame.texture.create_view(&TextureViewDescriptor::default());
                self.use_tex(id);
                {
                    let enc = self.encoder();
                    let mut p = enc.begin_render_pass(&RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Clear(Color::BLACK), store: StoreOp::Store } })],
                        depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
                    });
                    p.set_pipeline(&blit);
                    p.set_bind_group(0, &bg, &[]);
                    p.draw(0..3, 0..1);
                }
                self.flush();
                self.queue.present(frame);
                WINDOW_PRESENTS.fetch_add(1, Ordering::Relaxed);
            } else {
                self.flush();
            }
        } else {
            self.flush();
        }
        if let Some(texture) = self.tex(id) {
            crate::capture::on_present(&self.device, &self.queue, &texture.tex, self.presents + 1);
        }
        let started = Instant::now();
        self.publish_world_frame(id);
        self.foreground_capture_present(id);
        self.thread_wall[THREAD_WORLD_PUBLICATION].record(started.elapsed());
        let started = Instant::now();
        self.collect_pipelines();
        self.thread_wall[THREAD_PIPELINE_COLLECTION].record(started.elapsed());
        self.thread_wall[THREAD_PRESENT_BEFORE_WAIT].record(present_started.elapsed());
        self.presents += 1;
        PRESENTS.store(self.presents, Ordering::Relaxed);
        self.publish_geometry_capture_diagnostics();
        self.log_fps();

        // Metadata is only valid until this present retires. Ordinary flush
        // deliberately retains it because ring bytes are still immutable.
        self.uniform_snapshot.present();
        self.frame_slot_done[self.ubo_slot] = self.last_submit.clone();
        self.ubo_slot = (self.ubo_slot + 1) % FRAME_SLOTS;
        self.ubo_chunk = 0;
        self.ubo_off = 0;
        if let Some(idx) = self.frame_slot_done[self.ubo_slot].take() {
            let started = Instant::now();
            let _ = self.device.poll(PollType::Wait { submission_index: Some(idx), timeout: None });
            self.thread_wall[THREAD_UNIFORM_WAIT].record(started.elapsed());
        }
        self.process_maps(false);
    }

    fn publish_world_frame(&mut self, color_id: u32) {
        // Consume every presentation, including loading, to preserve queue order.
        let present_view = crate::xr_view::take_present_view();
        self.end_pass();
        self.hud_draw=None;
        let hud_capture=self.hud_capture.take();
        if self.command_trace.wants_command(7) {
            self.command_trace.present(self.presents+1, present_view.as_ref(),
                crate::xr_view::world_camera_enabled() && present_view.as_ref().is_some_and(|view|
                    crate::xr_view::view_epoch_is_active(view.provenance_epoch)));
        }
        if self.color_phase_trace.wants_draw(false) {
            let target = self.color_phase_target(color_id);
            self.color_phase_trace.present(self.presents+1, present_view.as_ref(), target);
        }
        let depths = std::mem::take(&mut self.world_depths);
        let mut saved = std::mem::take(&mut self.world_depth_before_ui);
        let snapshot_overflow = std::mem::replace(&mut self.world_depth_snapshot_overflow, false);
        let snapshots_this_present = std::mem::replace(&mut self.world_depth_snapshots_this_present, 0);
        let trace = std::mem::take(&mut self.world_depth_trace);
        let anomalies = std::mem::take(&mut self.world_depth_anomalies);
        // Foreground bookkeeping is per present, like the world records. An
        // accepted overlay bypassed the generic unqualified-writer invalidation;
        // unless it merges below, withdraw its frozen world exactly as that
        // rule would have, so FP colour never pairs world-only depth there.
        let fp_sessions = std::mem::take(&mut self.fp_sessions);
        let mut fp_images = std::mem::take(&mut self.fp_images);
        for overlay in fp_sessions.overlays.iter().filter(|overlay| overlay.draws > 0) {
            if let Err(reason) = fp_sessions.publishable(overlay.target) {
                if saved.remove(&overlay.target).is_some() { fg::note_withdrawal(reason); }
            }
        }
        let Some(view) = present_view.filter(|view| view.headset_applied) else { return; };
        let Some(original_color) = self.tex(color_id).map(|t| t.tex.clone()) else { return; };
        let hud=match hud_capture {
            Some(capture) if capture.final_id==color_id && capture.final_texture==original_color
                && capture.rejected.is_none() && capture.draws>0
                && crate::world_frame::shares_screen_rays(&view,&capture.view)
                && capture.images.is_some()=>{
                    capture.images
                },
            Some(capture)=>{crate::xr_hud::record_result(false,capture.rejected.as_deref().or(Some("final HUD provenance/draw qualification failed")));None},
            None=>None,
        };
        let color=original_color;
        // Main-world camera provenance plus full-size, single-sample depth must
        // select exactly one target. Ambiguous weapon/reflection targets fail closed.
        let mut sources: BTreeMap<u32, (crate::xr_view::WorldView, u64, Option<crate::world_frame::FrozenPreUiDepth>)> = saved.into_iter()
            .map(|(id, (view, texture, draws))| (id, (view, draws, Some(texture)))).collect();
        for (id, (view, draws)) in depths { sources.insert(id, (view, draws, None)); }
        let candidates: Vec<_> = sources.into_iter().filter(|(id, (candidate, draws, saved))|
            *draws > 0 && crate::world_frame::shares_screen_rays(&view, candidate)
            && saved.as_ref().map_or_else(|| self.tex(*id).is_some_and(|t|
                t.desc.w == color.width() && t.desc.h == color.height()),
                |depth| depth.texture().width() == color.width()
                    && depth.texture().height() == color.height())).collect();
        if snapshot_overflow || candidates.len() != 1 {
            let bindings: Vec<_> = trace.into_iter().map(|((id, sequence, writes, flags, full_viewport), (draws, viewport))| {
                let texture = self.tex(id).map(|t| format!("{}x{} samples{} layers{} {:?}", t.desc.w, t.desc.h, t.desc.samples, t.desc.d, t.format));
                (id, sequence, writes, flags, full_viewport, viewport, draws, texture)
            }).collect();
            crate::world_frame::reject(format!("qualified world depth selection needs one target, found {:?}; pre-UI snapshots {} overflow {}; present_sequence {}; world depth bindings {:?}; nonstandard depth writes {}",
                candidates.iter().map(|(id, (_, draws, _))| (*id, *draws)).collect::<Vec<_>>(), snapshots_this_present, snapshot_overflow, view.camera_sequence, bindings, serde_json::to_string(&anomalies).unwrap_or_default()));
            return;
        }
        let (depth_id, (view, _, frozen_depth)) = candidates.into_iter().next().unwrap();
        // The selected frozen world carries accepted attributed foreground
        // writes: publish the actual merge or nothing.
        let mut merged_overlay = None;
        let frozen_depth = match frozen_depth {
            Some(world) if fp_sessions.overlay(depth_id).is_some_and(|overlay| overlay.draws > 0) => {
                match self.merge_foreground_depth(&fp_sessions, &mut fp_images, depth_id, world.texture()) {
                    Ok((texture, overlay)) => {
                        merged_overlay = Some(overlay);
                        Some(crate::world_frame::FrozenPreUiDepth::new_merged_foreground(texture))
                    }
                    Err(reason) => {
                        fg::note_withdrawal(reason);
                        self.foreground_recycle(fp_images);
                        crate::world_frame::reject(format!("attributed first-person depth on the selected frozen world could not be merged: {reason}"));
                        return;
                    }
                }
            }
            other => other,
        };
        self.foreground_recycle(fp_images);
        // Preserve the opaque pass's actual depth convention. The final
        // lighting pass can have identical screen rays with a different Z row.
        let depth = match &frozen_depth {
            Some(saved) => saved.texture().clone(),
            None => match self.depth_proxy(depth_id, false) {
                Some((depth, _)) => depth,
                None => { crate::world_frame::reject("qualified world depth conversion failed".into()); return; }
            },
        };
        if let Some(overlay) = &merged_overlay { fg::note_merge(self.presents + 1, overlay); }
        if let Some(capture) = self.foreground_capture.as_mut().filter(|capture| capture.state == ForegroundCaptureState::Triggered) {
            capture.published = Some(serde_json::json!({"target":depth_id,"frozen":frozen_depth.is_some(),
                "merged_overlay":merged_overlay.map(|o| serde_json::json!({"draws":o.draws,"families":o.families,
                    "first_generation":o.first_generation,"last_generation":o.last_generation})),
                "view":crate::world_ui::marker(&view)}));
            capture.published_depth = Some(depth.clone());
        }
        self.flush();
        let p = view.projection_from_eye;
        let (near_denominator, far_denominator) = if view.depth_reverse_z { (p[10] + 1., p[10]) }
            else { (p[10], p[10] + 1.) };
        let near = p[14] / near_denominator;
        let far = if far_denominator == 0. { f32::INFINITY } else { p[14] / far_denominator };
        let frame = crate::world_frame::WorldFrame {
            game_frame: self.presents + 1, request_frame: view.request_frame, head_frame: view.head_pose_frame,
            presentation_time: view.presentation_time_seconds, view_index: view.eye_index, view_count: view.view_count,
            projection_from_eye: view.projection_from_eye, game_from_eye: view.game_from_eye,
            game_from_stage: view.game_from_stage, depth_near: near, depth_far: far,
            ..Default::default()
        };
        crate::world_frame::publish_with_hud_and_optics(&self.device, &self.queue, &color,
            &depth, frozen_depth, frame, view.provenance_epoch,hud,view.camera_sequence,view.optics);
    }

    fn log_fps(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.fps_t0).as_secs_f64();
        if dt >= 10.0 {
            let frames = self.presents - self.fps_n0;
            log(&format!(
                "fps: {:.1} ({} MB memory, game heap {} MB, {} frames, {} clears folded, {} draws/frame, {} draws waiting for a compiling pipeline, {:.1} submits/frame, {:.1} passes/frame, {} skipped total, pipelines {}, bind groups cached {}, validation errors {})",
                frames as f64 / dt, crate::footprint_mb(), crate::RT.get().map(|r| r.mem.data_size() >> 20).unwrap_or(0), frames, self.clears_folded, (self.draws - self.fps_d0) / frames.max(1), self.pending_skips - self.pending_skips0, (self.submits - self.submits0) as f64 / frames.max(1) as f64,
                (self.passes - self.passes0) as f64 / frames.max(1) as f64, self.skipped, self.pipe_creates, self.bind_groups.len(), self.errors.load(Ordering::Relaxed)
            ));
            self.log_upload_timing(dt);
            log(&format!("render prepare: {}", self.render_prepare.metrics(self.presents, self.draws, self.passes, self.submits, self.errors.load(Ordering::Relaxed))));
            log(&format!("uniform stage pack: {}", self.uniform_pack.metrics(self.presents, self.draws, self.submits, self.errors.load(Ordering::Relaxed))));
            log(&format!("uniform snapshot cache: {}", self.uniform_snapshot.metrics()));
            log(&format!("bind group prepare: {}", self.bind_group_prepare.metrics()));
            if let Some(r) = self.instance.generate_report() {
                let h = &r.hub;
                log(&format!("live wgpu objects: buffers {} textures {} views {} bind groups {} samplers {} pipelines {} | engine objects {}",
                    h.buffers.num_kept_from_user, h.textures.num_kept_from_user, h.texture_views.num_kept_from_user,
                    h.bind_groups.num_kept_from_user, h.samplers.num_kept_from_user, h.render_pipelines.num_kept_from_user,
                    self.objs.iter().filter(|o| o.is_some()).count()));
            }
            self.submits0 = self.submits;
            self.pending_skips0 = self.pending_skips;
            self.passes0 = self.passes;
            self.fps_t0 = now;
            self.fps_n0 = self.presents;
            self.fps_d0 = self.draws;
            self.buffer_uploads = [BufferUploadStats::default(); UPLOAD_SOURCE_COUNT];
            self.indirect_reads = IndirectReadStats::default();
            self.uploads.reset_interval();
            self.upload_dedup.reset_interval();
            self.thread_wall = [ApiWallStats::default(); THREAD_TIMING_COUNT];
        }
    }
    fn log_upload_timing(&self, seconds: f64) {
        let mut other = BufferUploadStats::default();
        for stats in &self.buffer_uploads[UPLOAD_UNIFORMS..] { other.accumulate(stats); }
        for (source, stats) in [("op12", self.buffer_uploads[UPLOAD_OP12]), ("op47", self.buffer_uploads[UPLOAD_OP47]), ("other", other)] {
            log(&format!("buffer upload timing: source {source}, interval_s {seconds:.3}, calls {}, bytes {}, API_wall_ms {:.3}, API_wall_max_ms {:.3}, wall_ns_bins_le[10000,100000,1000000,10000000,inf] {:?}, byte_bins_le[256,4096,65536,1048576,inf] {:?} (CPU-thread route wall time including blocking; not GPU time; bytes include padded tails; mode {})",
                stats.api_wall.calls, stats.bytes, stats.api_wall.total_ns as f64 / 1e6, stats.api_wall.max_ns as f64 / 1e6, stats.api_wall.buckets, stats.byte_buckets, self.uploads.mode()));
        }
        let sources = &self.buffer_uploads;
        log(&format!("buffer upload other calls/bytes: uniforms {}/{}, shader_consts {}/{}, uav_clear {}/{}, uav_counter {}/{}",
            sources[UPLOAD_UNIFORMS].api_wall.calls, sources[UPLOAD_UNIFORMS].bytes,
            sources[UPLOAD_SHADER_CONSTS].api_wall.calls, sources[UPLOAD_SHADER_CONSTS].bytes,
            sources[UPLOAD_UAV_CLEAR].api_wall.calls, sources[UPLOAD_UAV_CLEAR].bytes,
            sources[UPLOAD_UAV_COUNTER].api_wall.calls, sources[UPLOAD_UAV_COUNTER].bytes));
        let indirect = &self.indirect_reads;
        log(&format!("indirect fallback timing: attempts {}, poll_calls {}, successful_reads {}, poll_errors {}, map_errors {}, wait_wall_ms {:.3}, wait_wall_max_ms {:.3}, wall_ns_bins_le[10000,100000,1000000,10000000,inf] {:?} (CPU-thread poll plus callback-result check wall time; not GPU time)",
            indirect.attempts, indirect.wait_wall.calls, indirect.successes, indirect.poll_errors, indirect.map_errors,
            indirect.wait_wall.total_ns as f64 / 1e6, indirect.wait_wall.max_ns as f64 / 1e6, indirect.wait_wall.buckets));
        log(&format!("buffer upload pool: {}", self.uploads.metrics(seconds)));
        if self.upload_dedup.enabled() { log(&format!("op47 upload dedup: {}", self.upload_dedup.metrics(seconds))); }
        let t = &self.thread_wall;
        log(&format!("gpu thread timing: {}", serde_json::json!({"schema": 1, "mode": self.uploads.mode(), "interval_s": seconds,
            "segments": {"uniform_slot_wait": t[THREAD_UNIFORM_WAIT].metrics(), "guest_completion_wait": t[THREAD_GUEST_WAIT].metrics(),
                "readback_processing": t[THREAD_MAP_PROCESS].metrics(), "query_event_poll": t[THREAD_QUERY_POLL].metrics(),
                "producer_idle_wait": t[THREAD_PRODUCER_IDLE].metrics(), "suspended_gpu_wait": t[THREAD_SUSPEND_WAIT].metrics(),
                "suspended_sleep": t[THREAD_SUSPEND_SLEEP].metrics(), "world_publication": t[THREAD_WORLD_PUBLICATION].metrics(),
                "pipeline_collection": t[THREAD_PIPELINE_COLLECTION].metrics()},
            "inclusive": {"command_replay": t[THREAD_REPLAY].metrics(), "present_before_slot_wait": t[THREAD_PRESENT_BEFORE_WAIT].metrics()},
            "timing_scope": "CPU-thread wall including waits/callbacks; not GPU execution time. Inclusive parent spans contain child/manager segments and must not be summed. Replay/slot spans crossing a ten-second report finish in the following interval."})));
    }
    fn log_stats(&self) {
        log(&format!("stats: draws {} (skipped {}), presents {}, objects {}, pipelines {}", self.draws, self.skipped, self.presents, self.objs.iter().filter(|o| o.is_some()).count(), self.pipelines.len()));
    }
}

/// Actual depth of one overlay target immediately before its first and after
/// its last producer-attributed first-person write (R32Float proxy copies).
struct ForegroundImages { target: Texture, before: Texture, after: Option<Texture> }

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForegroundCaptureState { Armed, Triggered, Done }

/// One-shot capture at the first attributed first-person draw of a present.
/// Registered raw images plus every depth writer/mutation of that present.
struct ForegroundCapture {
    state: ForegroundCaptureState,
    events: Vec<serde_json::Value>,
    events_omitted: u64,
    budget: fg::logic::CaptureBudget,
    images: Vec<fg::CaptureImage>,
    images_omitted: Vec<serde_json::Value>,
    trigger: Option<serde_json::Value>,
    depth_target: Option<(u32, Texture)>,
    color_target: Option<(u32, u32, u32, Texture)>,
    post: (Option<usize>, Option<usize>),
    segments: u64,
    published: Option<serde_json::Value>,
    published_depth: Option<Texture>,
}

impl ForegroundCapture {
    fn from_env() -> Option<Self> {
        if !fg::attribution_requested() { return None; }
        fg::capture_dir()?;
        fg::capture_state("armed");
        Some(Self { state: ForegroundCaptureState::Armed, events: Vec::new(), events_omitted: 0, budget: Default::default(),
            images: Vec::new(), images_omitted: Vec::new(), trigger: None, depth_target: None, color_target: None,
            post: (None, None), segments: 0, published: None, published_depth: None })
    }
}

impl Gpu {
    fn foreground_set(&mut self, mark: Option<fg::ForegroundMark>) {
        let mark = mark.filter(|mark| mark.view.headset_applied && crate::xr_view::view_epoch_is_active(mark.view.provenance_epoch));
        let previous = std::mem::replace(&mut self.foreground, mark);
        if previous.is_some_and(|previous| mark.map(|mark| mark.generation) != Some(previous.generation)) {
            self.foreground_capture_segment_end();
        }
        if self.foreground_capture_live() {
            let event = serde_json::json!({"event":"foreground_marker","draw":self.draws,"generation":mark.map(|m| m.generation),
                "family":mark.map(|m| m.family),"camera_sequence":mark.map(|m| m.view.camera_sequence),"eye":mark.map(|m| m.view.eye_index)});
            self.foreground_capture_event(event);
        }
    }

    fn foreground_capture_live(&self) -> bool {
        self.foreground_capture.as_ref().is_some_and(|capture| capture.state != ForegroundCaptureState::Done)
    }

    fn foreground_capture_event(&mut self, event: serde_json::Value) {
        let Some(capture) = self.foreground_capture.as_mut().filter(|capture| capture.state != ForegroundCaptureState::Done) else { return; };
        if capture.events.len() < fg::logic::MAX_CAPTURE_EVENTS { capture.events.push(event); } else { capture.events_omitted += 1; }
    }

    /// Before the draw is encoded. True only when an attributed first-person
    /// depth write is accounted for here and bypasses the generic rule.
    fn foreground_pre_draw(&mut self, vs: &Program, ps: Option<&Program>) -> bool {
        if self.foreground.is_none() && self.fp_sessions.is_empty() && !self.foreground_capture_live() { return false; }
        let ds = self.s.ds.clone();
        let may_write = ds.tex != 0 && ds.flags & 1 == 0 && self.state(self.s.dss.0).is_some_and(|state|
            state.words.first().copied().unwrap_or(0) != 0 && state.words.get(1) == Some(&1));
        let mark = self.foreground;
        if self.foreground_capture_live() && (may_write || mark.is_some()) { self.foreground_capture_draw(vs, ps, mark, may_write); }
        let Some(mark) = mark else {
            if may_write { self.foreground_foreign_write(ds.tex, "unattributed_depth_draw"); }
            return false;
        };
        // Attribution alone (capture) keeps every existing rule unchanged.
        if !fg::merge_requested() { return false; }
        fg::gpu_status().foreground_draws += 1;
        if !may_write { return false; }
        fg::gpu_status().foreground_depth_draws += 1;
        if let Err(reason) = self.foreground_draw_qualifies(&mark, ps) { return self.foreground_generic(ds.tex, reason); }
        // A clear issued before this draw belongs to the image before it.
        self.materialize_clears(ds.tex);
        let identity = match (self.fp_images.get(&ds.tex), self.tex(ds.tex)) {
            (Some(images), Some(texture)) if images.target != texture.tex => 2,
            _ => 1,
        };
        let relation = self.foreground_relation(ds.tex, &mark.view);
        match self.fp_sessions.foreground_depth_draw(ds.tex, identity, relation, mark.generation, mark.family) {
            fg::logic::Decision::Combined => { fg::gpu_status().combined_live_writes += 1; true }
            fg::logic::Decision::ContinueOverlay => { fg::gpu_status().overlay_writes += 1; true }
            fg::logic::Decision::OpenOverlay => {
                if !self.foreground_snapshot(ds.tex, true) { return self.foreground_generic(ds.tex, "pre_foreground_snapshot_failed"); }
                self.fp_sessions.opened(ds.tex);
                let mut status = fg::gpu_status();
                status.overlays_opened += 1;
                status.overlay_writes += 1;
                true
            }
            fg::logic::Decision::Generic(reason) => { fg::note_generic(reason); false }
        }
    }

    fn foreground_generic(&mut self, target: u32, reason: &'static str) -> bool {
        self.fp_sessions.reject(target, reason);
        fg::note_generic(reason);
        false
    }

    /// Opaque, unbiased, projection-exact actual depth on a full single-sample
    /// target, in the attributed view's own depth convention.
    fn foreground_draw_qualifies(&self, mark: &fg::ForegroundMark, ps: Option<&Program>) -> Result<(), &'static str> {
        if !mark.view.headset_applied || !crate::xr_view::view_epoch_is_active(mark.view.provenance_epoch) { return Err("foreground_view_epoch_inactive"); }
        let ds = &self.s.ds;
        if ds.mip != 0 || ds.slice != 0 { return Err("foreground_depth_subresource"); }
        let Some(texture) = self.tex(ds.tex) else { return Err("foreground_depth_target_missing"); };
        if !is_depth(texture.format) || texture.desc.samples != 1 || texture.desc.d != 1 { return Err("foreground_depth_target_layout"); }
        let expected = [0., 0., texture.desc.w as f32, texture.desc.h as f32, 0., 1.];
        let viewport = self.s.viewports.first().copied().unwrap_or(expected);
        if !viewport.iter().zip(expected).all(|(&actual, expected)| actual.is_finite() && (actual - expected).abs() <= 0.00002) {
            return Err("foreground_viewport_or_depth_range_not_full");
        }
        if ps.is_some_and(|program| program.info.writes_fragment_depth) { return Err("foreground_fragment_depth"); }
        let compare = self.state(self.s.dss.0).and_then(|state| state.words.get(2).copied()).unwrap_or(0);
        let nearer = if mark.view.depth_reverse_z { matches!(compare, 5 | 7 | 8) } else { matches!(compare, 2 | 4 | 8) };
        if !nearer { return Err("foreground_depth_compare_convention"); }
        let exact = self.state(self.s.rs).is_none_or(|state|
            state.words.get(3).copied().unwrap_or(0) == 0
            && wf(state.words.get(4).copied().unwrap_or(0)) == 0.
            && wf(state.words.get(5).copied().unwrap_or(0)) == 0.
            && (state.words.get(6).copied() != Some(0) || !self.device.features().contains(Features::DEPTH_CLIP_CONTROL)));
        if !exact { return Err("foreground_depth_bias_or_clamp"); }
        let blend = self.state(self.s.blend.0);
        if self.s.rt.iter().enumerate().any(|(slot, rt)| rt.tex != 0
            && ps.is_some_and(|program| program.info.outputs.contains(&(slot as u32)))
            && blend_for(blend.as_deref(), slot).0.is_some()) { return Err("foreground_blended_depth_writer"); }
        Ok(())
    }

    fn foreground_relation(&self, target: u32, view: &crate::xr_view::WorldView) -> fg::logic::Relation {
        use fg::logic::Relation;
        if let Some((live, draws)) = self.world_depths.get(&target) {
            return if *draws > 0 && fg::same_raster(live, view) { Relation::LiveSame } else { Relation::LiveDifferent };
        }
        if let Some((frozen, _, draws)) = self.world_depth_before_ui.get(&target) {
            return if *draws > 0 && fg::same_raster(frozen, view) { Relation::FrozenSame } else { Relation::FrozenDifferent };
        }
        Relation::Absent
    }

    fn foreground_pooled(&mut self, width: u32, height: u32) -> Texture {
        if let Some(index) = self.fp_pool.iter().position(|texture| texture.width() == width && texture.height() == height) {
            return self.fp_pool.swap_remove(index);
        }
        self.device.create_texture(&TextureDescriptor { label: Some("Actual depth bracketing attributed first-person writes"),
            size: Extent3d { width, height, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: TextureDimension::D2, format: TextureFormat::R32Float,
            usage: TextureUsages::COPY_DST | TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING, view_formats: &[] })
    }

    /// Queue order makes reuse safe: later copies into a pooled image are
    /// encoded after the merge pass that reads it.
    fn foreground_recycle(&mut self, images: HashMap<u32, ForegroundImages>) {
        for images in images.into_values() {
            for texture in std::iter::once(images.before).chain(images.after) {
                if self.fp_pool.len() < 4 { self.fp_pool.push(texture); }
            }
        }
    }

    /// Exact copy of the target's actual depth at this point of the stream.
    /// `before` opens the overlay images; otherwise records its post image.
    fn foreground_snapshot(&mut self, target: u32, before: bool) -> bool {
        self.end_pass();
        let Some(current) = self.tex(target).map(|texture| texture.tex.clone()) else { return false; };
        if !before && !self.fp_images.get(&target).is_some_and(|images| images.target == current && images.after.is_none()) { return false; }
        let Some((proxy, _)) = self.depth_proxy(target, false) else { return false; };
        let (width, height) = (proxy.width(), proxy.height());
        let copy = self.foreground_pooled(width, height);
        self.end_pass();
        self.encoder().copy_texture_to_texture(
            TexelCopyTextureInfo { texture: &proxy, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
            TexelCopyTextureInfo { texture: &copy, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
            Extent3d { width, height, depth_or_array_layers: 1 });
        if before { self.fp_images.insert(target, ForegroundImages { target: current, before: copy, after: None }); }
        else if let Some(images) = self.fp_images.get_mut(&target) { images.after = Some(copy); }
        true
    }

    /// Any non-attributed writer of an open overlay target: record the actual
    /// post-foreground depth before that write is encoded.
    fn foreground_foreign_write(&mut self, target: u32, reason: &'static str) {
        if self.fp_sessions.is_empty() || !self.fp_sessions.foreign_write(target) { return; }
        if !self.foreground_snapshot(target, false) { self.fp_sessions.reject(target, "post_foreground_snapshot_failed"); }
        self.foreground_capture_event(serde_json::json!({"event":"overlay_closed","target":target,"reason":reason,"draw":self.draws}));
    }

    fn foreground_merge_pipeline(&mut self) -> (RenderPipeline, BindGroupLayout) {
        if let Some(pipeline) = &self.fp_merge { return pipeline.clone(); }
        let entry = |binding| BindGroupLayoutEntry { binding, visibility: ShaderStages::FRAGMENT, count: None,
            ty: BindingType::Texture { sample_type: TextureSampleType::Float { filterable: false }, view_dimension: TextureViewDimension::D2, multisampled: false } };
        let layout = self.device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("first-person depth merge"), entries: &[entry(0), entry(1), entry(2)] });
        let pipeline_layout = self.device.create_pipeline_layout(&PipelineLayoutDescriptor { label: Some("first-person depth merge"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let module = self.device.create_shader_module(ShaderModuleDescriptor { label: Some("first-person depth merge"), source: ShaderSource::Wgsl(fg::logic::MERGE_WGSL.into()) });
        let targets = [Some(ColorTargetState { format: TextureFormat::R32Float, blend: None, write_mask: ColorWrites::ALL })];
        let pipeline = self.device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("first-person depth merge"), layout: Some(&pipeline_layout),
            vertex: VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: PrimitiveState::default(), depth_stencil: None, multisample: MultisampleState::default(),
            fragment: Some(FragmentState { module: &module, entry_point: Some("fs"), compilation_options: Default::default(), targets: &targets }),
            multiview_mask: None, cache: None });
        self.fp_merge = Some((pipeline.clone(), layout.clone()));
        (pipeline, layout)
    }

    /// New R32Float = frozen world except where the attributed foreground
    /// actually wrote (exact before/after bit difference), which takes its
    /// actual depth. Same extent and convention as the frozen world.
    fn merge_foreground_depth(&mut self, sessions: &fg::logic::Sessions, images: &mut HashMap<u32, ForegroundImages>,
        target: u32, world: &Texture) -> Result<(Texture, fg::logic::Overlay), &'static str> {
        let overlay = sessions.publishable(target)?;
        let entry = images.get_mut(&target).ok_or("missing_pre_foreground_snapshot")?;
        if entry.after.is_none() {
            // Still open at presentation: the live target holds the last write.
            if !self.tex(target).is_some_and(|texture| texture.tex == entry.target) { return Err("depth_target_replaced_before_present"); }
            self.end_pass();
            let Some((proxy, _)) = self.depth_proxy(target, false) else { return Err("post_foreground_snapshot_failed"); };
            let copy = self.foreground_pooled(proxy.width(), proxy.height());
            self.end_pass();
            self.encoder().copy_texture_to_texture(
                TexelCopyTextureInfo { texture: &proxy, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
                TexelCopyTextureInfo { texture: &copy, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
                Extent3d { width: proxy.width(), height: proxy.height(), depth_or_array_layers: 1 });
            entry.after = Some(copy);
        }
        let Some(after) = entry.after.as_ref() else { return Err("post_foreground_snapshot_failed"); };
        let before = &entry.before;
        if world.format() != TextureFormat::R32Float
            || [before, after].iter().any(|texture| texture.width() != world.width() || texture.height() != world.height()) {
            return Err("foreground_world_extent_or_format_differs");
        }
        let (pipeline, layout) = self.foreground_merge_pipeline();
        let output = self.device.create_texture(&TextureDescriptor { label: Some("Frozen world with actual first-person depth writes"),
            size: Extent3d { width: world.width(), height: world.height(), depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: TextureDimension::D2, format: TextureFormat::R32Float,
            usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[] });
        let views = [world, before, after].map(|texture| texture.create_view(&TextureViewDescriptor::default()));
        let bind_group = self.device.create_bind_group(&BindGroupDescriptor { label: Some("first-person depth merge"), layout: &layout,
            entries: &[BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&views[0]) },
                BindGroupEntry { binding: 1, resource: BindingResource::TextureView(&views[1]) },
                BindGroupEntry { binding: 2, resource: BindingResource::TextureView(&views[2]) }] });
        let output_view = output.create_view(&TextureViewDescriptor::default());
        self.end_pass();
        {
            let mut pass = self.encoder().begin_render_pass(&RenderPassDescriptor { label: Some("Merge actual first-person depth over frozen world"),
                color_attachments: &[Some(RenderPassColorAttachment { view: &output_view, depth_slice: None, resolve_target: None,
                    ops: Operations { load: LoadOp::Clear(Color::BLACK), store: StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        Ok((output, overlay))
    }

    /// Registered raw copy of one subresource into a bounded MAP_READ buffer.
    fn foreground_capture_copy(&mut self, name: &'static str, texture: &Texture, mip: u32, layer: u32, source: serde_json::Value) -> Option<usize> {
        let bytes_per_texel = texel_bytes(texture.format());
        let (width, height) = ((texture.width() >> mip).max(1), (texture.height() >> mip).max(1));
        let eligible = bytes_per_texel != 0 && texture.sample_count() == 1 && texture.dimension() == TextureDimension::D2
            && texture.usage().contains(TextureUsages::COPY_SRC) && mip < texture.mip_level_count() && layer < texture.depth_or_array_layers();
        let padded = fg::logic::padded_row_bytes(width, bytes_per_texel).filter(|_| eligible);
        let size = padded.map(|padded| padded as u64 * height as u64);
        let capture = self.foreground_capture.as_mut()?;
        if !size.is_some_and(|size| capture.budget.admit(size)) {
            capture.images_omitted.push(serde_json::json!({"name":name,"format":format!("{:?}",texture.format()),
                "width":width,"height":height,"eligible":eligible,"source":source}));
            return None;
        }
        let (padded, size) = (padded?, size?);
        let buffer = self.device.create_buffer(&BufferDescriptor { label: Some("First-person foreground capture"), size,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        self.end_pass();
        self.encoder().copy_texture_to_buffer(
            TexelCopyTextureInfo { texture, mip_level: mip, origin: Origin3d { x: 0, y: 0, z: layer }, aspect: TextureAspect::All },
            TexelCopyBufferInfo { buffer: &buffer, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(height) } },
            Extent3d { width, height, depth_or_array_layers: 1 });
        let capture = self.foreground_capture.as_mut()?;
        capture.images.push(fg::CaptureImage { name, buffer, width, height, row_bytes: width * bytes_per_texel, padded_row_bytes: padded,
            format: format!("{:?}", texture.format()), source });
        Some(capture.images.len() - 1)
    }

    /// Overwrite an already registered image in stream order (last wins).
    fn foreground_capture_recopy(&mut self, index: usize, texture: &Texture, mip: u32, layer: u32) -> bool {
        let Some(image) = self.foreground_capture.as_ref().and_then(|capture| capture.images.get(index)) else { return false; };
        let (width, height) = ((texture.width() >> mip).max(1), (texture.height() >> mip).max(1));
        if image.width != width || image.height != height || image.format != format!("{:?}", texture.format()) { return false; }
        let (buffer, padded) = (image.buffer.clone(), image.padded_row_bytes);
        self.end_pass();
        self.encoder().copy_texture_to_buffer(
            TexelCopyTextureInfo { texture, mip_level: mip, origin: Origin3d { x: 0, y: 0, z: layer }, aspect: TextureAspect::All },
            TexelCopyBufferInfo { buffer: &buffer, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(height) } },
            Extent3d { width, height, depth_or_array_layers: 1 });
        true
    }

    fn foreground_native(texture: Option<&Texture>) -> Option<String> {
        texture.and_then(crate::world_frame::native_texture_handle).map(|handle| format!("{handle:#x}"))
    }

    /// Logs every depth-writing or attributed draw while armed; the first
    /// attributed draw triggers the before-images of its actual targets.
    fn foreground_capture_draw(&mut self, vs: &Program, ps: Option<&Program>, mark: Option<fg::ForegroundMark>, may_write: bool) {
        let ds = self.s.ds.clone();
        let rt0 = self.s.rt[0].clone();
        let event = serde_json::json!({"event":"draw","draw":self.draws,"depth":ds.tex,"depth_subresource":[ds.mip,ds.slice,ds.flags],
            "color0":[rt0.tex,rt0.mip,rt0.slice],"writes_depth":may_write,
            "depth_state":self.state(self.s.dss.0).map(|state| state.words.iter().take(3).copied().collect::<Vec<_>>()),
            "vertex":vs.label.chars().take(96).collect::<String>(),"pixel":ps.map(|program| program.label.chars().take(96).collect::<String>()),
            "viewport":self.s.viewports.first().copied(),
            "world":self.world_view.map(|view| serde_json::json!([view.camera_sequence,view.eye_index,view.depth_reverse_z])),
            "foreground":mark.map(|mark| serde_json::json!([mark.generation,mark.family,mark.view.camera_sequence,mark.view.eye_index])),
            "live_world_depth":self.world_depths.get(&ds.tex).map(|(view,draws)| serde_json::json!([view.camera_sequence,draws])),
            "frozen_world_depth":self.world_depth_before_ui.get(&ds.tex).map(|(view,_,draws)| serde_json::json!([view.camera_sequence,draws]))});
        self.foreground_capture_event(event);
        let Some(mark) = mark else { return; };
        if self.foreground_capture.as_ref().is_none_or(|capture| capture.state != ForegroundCaptureState::Armed) { return; }
        if let Some(capture) = self.foreground_capture.as_mut() { capture.state = ForegroundCaptureState::Triggered; }
        fg::capture_state("triggered");
        let depth = self.tex(ds.tex).filter(|texture| is_depth(texture.format) && texture.desc.samples == 1).map(|texture| texture.tex.clone());
        if depth.is_some() {
            self.end_pass();
            if let Some((proxy, _)) = self.depth_proxy(ds.tex, false) {
                let index = self.foreground_capture_copy("depth_before_first_foreground", &proxy, 0, 0,
                    serde_json::json!({"target":ds.tex,"native":Self::foreground_native(depth.as_ref()),"conversion":"exact R32Float depth proxy"}));
                if index.is_none() { self.foreground_capture_event(serde_json::json!({"event":"capture_image_omitted","name":"depth_before_first_foreground"})); }
            }
        }
        let color = self.tex(rt0.tex).map(|texture| texture.tex.clone());
        if let Some(texture) = &color {
            self.foreground_capture_copy("color0_before_first_foreground", texture, rt0.mip, rt0.slice,
                serde_json::json!({"target":rt0.tex,"mip":rt0.mip,"slice":rt0.slice,"native":Self::foreground_native(Some(texture))}));
        }
        let frozen = self.world_depth_before_ui.get(&ds.tex).map(|(view, frozen, draws)| (*view, frozen.texture().clone(), *draws));
        if let Some((view, texture, draws)) = &frozen {
            self.foreground_capture_copy("frozen_world_depth_at_trigger", texture, 0, 0, serde_json::json!({"target":ds.tex,
                "camera_sequence":view.camera_sequence,"world_draws":draws,"native":Self::foreground_native(Some(texture))}));
        }
        let trigger = serde_json::json!({"present":self.presents + 1,"draw":self.draws,"depth_target":ds.tex,
            "depth_texture":self.color_phase_target(ds.tex),"color0_texture":self.color_phase_target(rt0.tex),
            "native":{"depth":Self::foreground_native(depth.as_ref()),"color0":Self::foreground_native(color.as_ref()),
                "frozen_world":Self::foreground_native(frozen.as_ref().map(|(_, texture, _)| texture))},
            "mark":{"generation":mark.generation,"family":mark.family,"view":crate::world_ui::marker(&mark.view)},
            "merge_requested":fg::merge_requested()});
        if let Some(capture) = self.foreground_capture.as_mut() {
            capture.trigger = Some(trigger);
            capture.depth_target = depth.map(|texture| (ds.tex, texture));
            capture.color_target = color.map(|texture| (rt0.tex, rt0.mip, rt0.slice, texture));
        }
    }

    /// End of an attributed segment: the actual targets after it (last wins).
    fn foreground_capture_segment_end(&mut self) {
        let Some(capture) = self.foreground_capture.as_mut().filter(|capture| capture.state == ForegroundCaptureState::Triggered) else { return; };
        capture.segments += 1;
        let (depth, color, (depth_slot, color_slot)) = (capture.depth_target.clone(), capture.color_target.clone(), capture.post);
        let mut post = (depth_slot, color_slot);
        if let Some((id, texture)) = depth {
            if self.tex(id).is_some_and(|current| current.tex == texture) {
                self.end_pass();
                if let Some((proxy, _)) = self.depth_proxy(id, false) {
                    post.0 = match depth_slot {
                        Some(index) => self.foreground_capture_recopy(index, &proxy, 0, 0).then_some(index),
                        None => self.foreground_capture_copy("depth_after_last_foreground", &proxy, 0, 0,
                            serde_json::json!({"target":id,"native":Self::foreground_native(Some(&texture)),"conversion":"exact R32Float depth proxy"})),
                    };
                }
            } else { self.foreground_capture_event(serde_json::json!({"event":"capture_depth_target_replaced","target":id,"draw":self.draws})); }
        }
        if let Some((id, mip, slice, texture)) = color {
            if self.tex(id).is_some_and(|current| current.tex == texture) {
                self.materialize_clears(id);
                post.1 = match color_slot {
                    Some(index) => self.foreground_capture_recopy(index, &texture, mip, slice).then_some(index),
                    None => self.foreground_capture_copy("color0_after_last_foreground", &texture, mip, slice,
                        serde_json::json!({"target":id,"mip":mip,"slice":slice,"native":Self::foreground_native(Some(&texture))})),
                };
            } else { self.foreground_capture_event(serde_json::json!({"event":"capture_color_target_replaced","target":id,"draw":self.draws})); }
        }
        if let Some(capture) = self.foreground_capture.as_mut() { capture.post = post; }
        self.foreground_capture_event(serde_json::json!({"event":"foreground_segment_end","draw":self.draws}));
    }

    /// Present of the triggered capture: final colour and published depth, then
    /// one submission and create-new background persistence. Untriggered
    /// presents only reset the bounded log.
    fn foreground_capture_present(&mut self, color_id: u32) {
        let Some(state) = self.foreground_capture.as_ref().map(|capture| capture.state) else { return; };
        match state {
            ForegroundCaptureState::Done => return,
            ForegroundCaptureState::Armed => {
                if let Some(capture) = self.foreground_capture.as_mut() { capture.events.clear(); capture.events_omitted = 0; }
                return;
            }
            ForegroundCaptureState::Triggered => {}
        }
        self.materialize_clears(color_id);
        if let Some(texture) = self.tex(color_id).map(|texture| texture.tex.clone()) {
            self.foreground_capture_copy("present_final_color", &texture, 0, 0,
                serde_json::json!({"target":color_id,"native":Self::foreground_native(Some(&texture))}));
        }
        if let Some(depth) = self.foreground_capture.as_mut().and_then(|capture| capture.published_depth.take()) {
            self.foreground_capture_copy("published_depth", &depth, 0, 0, serde_json::json!({"native":Self::foreground_native(Some(&depth))}));
        }
        self.flush();
        let submission = self.last_submit.clone();
        let presents = self.presents + 1;
        let Some(capture) = self.foreground_capture.as_mut() else { return; };
        capture.state = ForegroundCaptureState::Done;
        fg::note_capture_events(capture.events.len());
        let manifest = serde_json::json!({"schema":"muguet.first-person-foreground-depth-capture.v1","present":presents,
            "trigger":capture.trigger.take(),"published":capture.published.take(),"segments":capture.segments,
            "events":std::mem::take(&mut capture.events),"events_omitted":capture.events_omitted,
            "images_omitted":std::mem::take(&mut capture.images_omitted),
            "merge_requested":fg::merge_requested(),
            "interpretation":"Raw images are GPU copies in command-stream order. depth_before_first_foreground and depth_after_last_foreground bracket every producer-attributed first-person draw of this present on the trigger depth target; events list every depth-writing draw, attribution marker, freeze and depth-target mutation observed in it, in order.",
            "replay_complete":false,"command_snapshot_complete":false,"physical_visibility_certified":false});
        let images = std::mem::take(&mut capture.images);
        fg::persist_capture(self.device.clone(), submission, images, manifest);
    }
}

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

fn format_id(f: TextureFormat) -> i64 {
    let s = format!("{f:?}");
    let mut h: i64 = 1469598103934665603u64 as i64;
    for b in s.bytes() { h = (h ^ b as i64).wrapping_mul(1099511628211); }
    h
}

fn blend_for(state: Option<&StateObj>, rt: usize) -> (Option<BlendState>, ColorWrites) {
    let Some(s) = state else { return (None, ColorWrites::ALL) };
    let w = &s.words;
    let g = |i: usize| w.get(i).copied().unwrap_or(0);
    let r = if g(1) != 0 { rt } else { 0 };
    let q = 2 + r * 8;
    let write = ColorWrites::from_bits_truncate(g(q + 7) & 0xf);
    if g(q) == 0 { return (None, write); }
    (Some(BlendState {
        color: BlendComponent { src_factor: blend_factor(g(q + 1), BlendFactor::One), dst_factor: blend_factor(g(q + 2), BlendFactor::Zero), operation: blend_op(g(q + 3)) },
        alpha: BlendComponent { src_factor: blend_factor(g(q + 4), BlendFactor::One), dst_factor: blend_factor(g(q + 5), BlendFactor::Zero), operation: blend_op(g(q + 6)) },
    }), write)
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

#[cfg(test)]
mod readback_bound_tests {
    use super::bounded_readback_rows;

    const ADDR: u64 = 0x1_0000;
    const MEMORY: u64 = 0x10_0000;
    fn rows(count: u64, pitch: u64, bytes: u64) -> Vec<(u64, u64, u64)> {
        (0..count).map(|row| (row * 256, ADDR + row * pitch, bytes)).collect()
    }
    fn end(rows: &[(u64, u64, u64)]) -> u64 { rows.iter().map(|(_, to, n)| to + n).max().unwrap_or(ADDR) }

    #[test]
    fn matching_layout_is_untouched() {
        // 4x4 RGBA8: four 16-byte rows into a 64-byte destination.
        let wanted = rows(4, 16, 16);
        assert_eq!(bounded_readback_rows(wanted.clone(), ADDR, 64, MEMORY), (wanted, 0));
        assert_eq!(bounded_readback_rows(vec![(0, ADDR, 100)], ADDR, 100, MEMORY), (vec![(0, ADDR, 100)], 0));
    }

    #[test]
    fn decoded_rgba_rows_never_pass_a_bc_sized_destination() {
        // 64x64 BC1 in the guest: 16 block rows of 128 bytes = 2048 bytes.
        // The same texture decoded on the host: 64 rows of 256 bytes.
        let (kept, withheld) = bounded_readback_rows(rows(64, 128, 256), ADDR, 2048, MEMORY);
        assert_eq!(kept.len(), 16);
        assert_eq!(end(&kept), ADDR + 2048);
        assert_eq!(kept[15], (15 * 256, ADDR + 15 * 128, 128));
        assert_eq!(kept.iter().map(|row| row.2).sum::<u64>() + withheld, 64 * 256);
    }

    #[test]
    fn oversized_box_zero_size_and_memory_end_are_bounded() {
        assert_eq!(bounded_readback_rows(vec![(0, ADDR, 4096)], ADDR, 1024, MEMORY), (vec![(0, ADDR, 1024)], 3072));
        assert_eq!(bounded_readback_rows(rows(4, 16, 16), ADDR, 0, MEMORY), (vec![], 64));
        let (kept, withheld) = bounded_readback_rows(vec![(0, MEMORY - 8, 64)], MEMORY - 8, 64, MEMORY);
        assert_eq!((kept, withheld), (vec![(0, MEMORY - 8, 8)], 56));
        assert_eq!(bounded_readback_rows(vec![(0, MEMORY + 8, 64)], MEMORY + 8, 64, MEMORY), (vec![], 64));
        assert_eq!(bounded_readback_rows(vec![(0, u64::MAX - 4, 64)], u64::MAX - 4, u64::MAX, MEMORY), (vec![], 64));
        assert_eq!(bounded_readback_rows(vec![(0, ADDR - 16, 64)], ADDR, 64, MEMORY), (vec![], 64));
    }
}

#[cfg(test)]
mod query_life_tests {
    use super::*;

    const RECORD: u64 = 64;
    fn memory() -> SharedMemory {
        let mut config = wasmtime::Config::new(); config.wasm_threads(true); config.shared_memory(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let memory = SharedMemory::new(&engine, wasmtime::MemoryType::shared(1, 1)).unwrap();
        assert!(crate::write_bytes(&memory, RECORD - 16, &[0xa5; 48]));
        memory
    }
    fn record(memory: &SharedMemory) -> Vec<u8> { crate::read_bytes(memory, RECORD - 16, 48) }

    #[test]
    fn completion_writes_sequence_and_result_only() {
        let memory = memory();
        let life = QueryLife::new(RECORD);
        assert!(life.is_live() && life.complete(&memory, 0x1122_3344_5566_7788, 9));
        let mut wanted = vec![0xa5u8; 48];
        wanted[16..20].copy_from_slice(&9u32.to_le_bytes());
        wanted[24..32].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(record(&memory), wanted, "bytes 4..8 of the record and both neighbours stay untouched");
    }

    #[test]
    fn retired_life_does_not_write_and_dropping_the_query_retires_it() {
        let memory = memory();
        let query = QueryObj { qtype: 1, life: QueryLife::new(RECORD), slots: vec![] };
        let life = query.life.clone();
        drop(query);
        assert!(!life.is_live() && !life.complete(&memory, 1, 1));
        assert_eq!(record(&memory), vec![0xa5u8; 48]);
        let other = QueryLife::new(RECORD);
        other.retire();
        other.retire();
        assert!(!other.complete(&memory, 1, 1));
        assert_eq!(record(&memory), vec![0xa5u8; 48]);
    }

    #[test]
    fn retire_excludes_a_completion_running_on_another_thread() {
        let memory = memory();
        let life = QueryLife::new(RECORD);
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (memory, life, stop) = (memory.clone(), life.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 1u64;
                // The same n goes into the result and the sequence word.
                while !stop.load(Ordering::Relaxed) { life.complete(&memory, n, n as u32); n += 1; }
            })
        };
        while crate::read_u32(&memory, RECORD).is_none_or(|sequence| sequence < 1000 || sequence == 0xa5a5_a5a5) { std::thread::yield_now(); }
        life.retire();
        // From here the writer keeps calling complete(); nothing may change again.
        let frozen = record(&memory);
        for _ in 0..2000 {
            std::thread::yield_now();
            assert_eq!(record(&memory), frozen, "a completion wrote after retire() returned");
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert_eq!(record(&memory), frozen);
        // The last completion finished as a whole: both words come from one n.
        let sequence = u32::from_le_bytes(frozen[16..20].try_into().unwrap());
        let result = u64::from_le_bytes(frozen[24..32].try_into().unwrap());
        assert_eq!(result as u32, sequence);
        assert!(result >= 1000);
    }
}

#[cfg(test)]
mod frozen_world_depth_policy_tests {
    use super::*;

    fn world() -> crate::xr_view::WorldView {
        let identity = [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.];
        crate::xr_view::WorldView { provenance_epoch: 7, camera_sequence: 2256,
            request_frame: 338, head_pose_frame: 353, presentation_time_seconds: 42.,
            eye_index: 0, view_count: 2, game_from_eye: identity, game_from_stage: identity,
            projection_from_eye: [0.732051,0.,0.,0., 0.,0.9125127,0.,0.,
                -0.267949,-0.087487,0.00010001659,-1., 0.,0.,0.10001,0.],
            depth_reverse_z: true, headset_applied: true, optics:crate::xr_view::ScopeOptics::default() }
    }

    #[test]
    fn physical_ui_then_live_clears_retain_same_immutable_snapshot_until_present() {
        // Physical build3: depth1221, camera2256,1580 qualified draws. The known
        // UI draw preserved its copy, then clear_ds incorrectly removed it.
        let view = world();
        let samples = vec![0x3f000001u32, 0x3e800002, 0x00000000];
        let mut live = HashMap::from([(1221, (view, 1580))]);
        let mut frozen = HashMap::from([(1221, (view, samples.clone(), 1580))]);
        for event in [WorldDepthChange::ProvenUiClear, WorldDepthChange::LiveClear,
            WorldDepthChange::LiveClear] {
            assert_eq!(invalidate_world_depth_records(&mut live, &mut frozen, 1221, event), Some((view, 1580)));
            assert!(live.is_empty());
            assert_eq!(frozen.get(&1221), Some(&(view, samples.clone(), 1580)));
        }
        assert!(crate::world_frame::shares_screen_rays(&view, &frozen[&1221].0));
        // publish_world_frame takes all frozen entries on EVERY present, even
        // a loading/unqualified present, preventing cross-present resurrection.
        let retired = std::mem::take(&mut frozen);
        assert_eq!(retired[&1221].1, samples);
        assert!(invalidate_world_depth_records(&mut live, &mut frozen, 1221, WorldDepthChange::LiveClear).is_none());
        assert!(frozen.is_empty());
    }

    #[test]
    fn other_depth_mutations_revoke_snapshot_and_clear_never_creates_one() {
        let view = world();
        let mut live = HashMap::from([(1221, (view, 1580))]);
        let mut frozen: HashMap<u32, (_, Vec<u32>, _)> = HashMap::new();
        assert_eq!(invalidate_world_depth_records(&mut live, &mut frozen, 1221, WorldDepthChange::LiveClear), Some((view, 1580)));
        assert!(live.is_empty() && frozen.is_empty());
        frozen.insert(1221, (view, vec![0x3f000001], 1580));
        // Resource replacement/destruction, copy/upload and unqualified non-UI
        // depth draws all use Other, even after a successful UI snapshot.
        assert_eq!(invalidate_world_depth_records(&mut live, &mut frozen, 1221, WorldDepthChange::Other), Some((view, 1580)));
        assert!(live.is_empty() && frozen.is_empty());
    }

    #[test]
    fn retained_snapshot_still_rejects_foreign_or_stale_present_identity() {
        let view = world();
        let mut live = HashMap::new();
        let mut frozen = HashMap::from([(1221, (view, vec![0x3f000001u32], 1580))]);
        invalidate_world_depth_records(&mut live, &mut frozen, 1221, WorldDepthChange::LiveClear);
        let depth = frozen[&1221].0;
        let mut foreign = Vec::new();
        foreign.push(crate::xr_view::WorldView { provenance_epoch: view.provenance_epoch + 1, ..view });
        foreign.push(crate::xr_view::WorldView { camera_sequence: view.camera_sequence + 1, ..view });
        foreign.push(crate::xr_view::WorldView { request_frame: view.request_frame + 1, ..view });
        foreign.push(crate::xr_view::WorldView { head_pose_frame: view.head_pose_frame + 1, ..view });
        foreign.push(crate::xr_view::WorldView { eye_index: 1, ..view });
        let mut moved = view; moved.game_from_eye[12] = 0.01; foreign.push(moved);
        let mut different_rays = view; different_rays.projection_from_eye[8] += 0.001; foreign.push(different_rays);
        for color in foreign { assert!(!crate::world_frame::shares_screen_rays(&color, &depth)); }
    }
}
