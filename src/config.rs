// muguet - config.rs
// Constantin Clerc (c22dev) - 0.1

use std::path::{Path, PathBuf};

#[cfg_attr(target_os = "ios", allow(dead_code))]
pub struct Startup {
    pub wasm_path: PathBuf,
    pub cache_dir: PathBuf,

    pub engine_args: Vec<String>,
    pub native_graphics: Option<NativeGraphicsLaunch>,
}

const DEFAULT_CONFIG: &str = r#"{
  "scale": 0.5,
  "mouse_sensitivity": 2.0,
  "fullscreen": true,
  "fps_limit": 60,
  "start_mode": 2,
  "mute": false,
  "multiplayer": false,
  "host": false,
  "server": "",
  "player_name": "",
  "extra_args": []
}
"#;

const DEFAULT_START_MODE: i64 = if cfg!(target_os = "ios") { 1 } else { 2 };

fn default_config() -> serde_json::Value {
    let mut cfg: serde_json::Value = serde_json::from_str(DEFAULT_CONFIG).unwrap();
    cfg["start_mode"] = DEFAULT_START_MODE.into();
    cfg
}

fn support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    if cfg!(any(target_os = "ios", target_os = "visionos")) {
        return PathBuf::from(home).join("Documents");
    }
    PathBuf::from(home).join("Library/Application Support/Muguet")
}

pub fn game_dir() -> PathBuf {
    support_dir().join("game")
}

pub fn game_ready() -> bool {
    let dir = game_dir();
    dir.join("data/manifest.json").exists() && build_dir(&dir).is_some_and(|b| !cfg!(feature = "jit") || crate::code_embedded() || b.join("game.wasm").exists())
}

pub fn bundled_resources() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let res = exe.parent()?.parent()?.join("Resources");
    (exe.parent()?.ends_with("Contents/MacOS") && res.is_dir()).then_some(res)
}

fn build_dir(root: &Path) -> Option<PathBuf> {
    std::fs::read_dir(root.join("b")).ok()?.flatten().map(|e| e.path()).find(|p| p.join("shaders/index.json").exists())
}

fn set_default(key: &str, value: impl AsRef<std::ffi::OsStr>) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

fn redirect_logs_if_detached() {
    extern "C" {
        fn isatty(fd: i32) -> i32;
        fn dup2(old: i32, new: i32) -> i32;
    }

    if cfg!(any(target_os = "ios", target_os = "visionos")) {
        return;
    }
    let in_bundle = std::env::current_exe().map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/")).unwrap_or(false);
    if !in_bundle || unsafe { isatty(2) } != 0 {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let path = PathBuf::from(home).join("Library/Logs/Muguet.log");
    if let Ok(f) = std::fs::File::create(&path) {
        use std::os::fd::IntoRawFd;
        let fd = f.into_raw_fd();
        unsafe {
            dup2(fd, 2);
            dup2(fd, 1);
        }
    }
}

pub fn setup() -> anyhow::Result<Startup> {
    redirect_logs_if_detached();
    let support = support_dir();
    std::fs::create_dir_all(support.join("saves"))?;

    let cfg_path = support.join("config.json");
    if !cfg_path.exists() {
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&default_config())?)?;
    }
    let engine_args = settings(&cfg_path);

    set_default("MUGUET_FSROOT", support.join("saves"));
    let build = locate();
    #[cfg(not(target_os = "ios"))]
    let build = build?;
    #[cfg(target_os = "ios")]
    let build = build.unwrap_or_default();
    Ok(Startup { wasm_path: build.join("game.wasm"), cache_dir: support, engine_args, native_graphics: None })
}

fn locate() -> anyhow::Result<PathBuf> {
    let root = game_dir();
    let build = build_dir(&root).filter(|_| game_ready()).ok_or_else(|| anyhow::anyhow!("no game data: import your game .zip from the launcher"))?;
    std::env::set_var("MUGUET_DATA", root.join("data"));
    std::env::set_var("MUGUET_SHADERS", build.join("shaders"));
    eprintln!("[config] game {}", root.display());
    Ok(build)
}

const SETTING_VARS: &[&str] = &["MUGUET_SCALE", "MUGUET_MOUSE_SENS", "MUGUET_TOUCH_SENS", "MUGUET_RES", "MUGUET_FULLSCREEN", "MUGUET_MUTE", "MUGUET_HOST", "MUGUET_SERVER", "MUGUET_NAME"];

#[cfg(target_os = "ios")]
pub fn reload() -> Vec<String> {
    for v in SETTING_VARS { std::env::remove_var(v); }
    let args = settings(&support_dir().join("config.json"));
    if let Err(e) = locate() {
        eprintln!("[config] {e:#}");
    }
    eprintln!("[config] launcher settings applied: {}", args.join(" "));
    args
}

fn settings(cfg_path: &Path) -> Vec<String> {
    let _ = SETTING_VARS;
    let cfg: serde_json::Value = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).map_err(|e| eprintln!("[config] {}: {e}", cfg_path.display())).ok())
        .unwrap_or_else(default_config);
    let num = |k: &str| cfg.get(k).and_then(|v| v.as_f64());
    let flag = |k: &str| cfg.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    if let Some(v) = num("scale") { set_default("MUGUET_SCALE", v.to_string()); }
    if let Some(v) = num("mouse_sensitivity") { set_default("MUGUET_MOUSE_SENS", v.to_string()); }
    if let Some(v) = num("touch_sensitivity") { set_default("MUGUET_TOUCH_SENS", v.to_string()); }
    if let Some(r) = cfg.get("resolution").and_then(|v| v.as_str()) { set_default("MUGUET_RES", r); }
    if flag("fullscreen") { set_default("MUGUET_FULLSCREEN", "1"); }
    if flag("mute") { set_default("MUGUET_MUTE", "1"); }

    let text = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty());
    if flag("multiplayer") {
        if flag("host") { set_default("MUGUET_HOST", "1"); }
        else if let Some(v) = text("server") { set_default("MUGUET_SERVER", v); }
    }
    if let Some(v) = text("player_name") { set_default("MUGUET_NAME", v); }

    let mut engine_args = Vec::new();

    let fps = std::env::var("MUGUET_FPS").ok().and_then(|v| v.parse::<f64>().ok()).or_else(|| num("fps_limit")).unwrap_or(60.0);
    engine_args.push(format!("-frameLimit={}", if fps <= 0.0 { 0 } else if fps <= 30.0 { 2 } else { 1 }));

    let start_mode = std::env::var("MUGUET_START").ok().and_then(|v| v.parse::<f64>().ok()).or_else(|| num("start_mode")).unwrap_or(DEFAULT_START_MODE as f64) as i64;
    let start_mode = if (1..=2).contains(&start_mode) { start_mode } else { DEFAULT_START_MODE };
    if !std::env::args().any(|a| a.starts_with("-wasmstartchoice")) {
        engine_args.push(format!("-wasmstartchoice={start_mode}"));
    }
    if let Some(extra) = cfg.get("extra_args").and_then(|v| v.as_array()) {
        engine_args.extend(extra.iter().filter_map(|a| a.as_str().map(String::from)));
    }
    fix_settings(num("texture_quality").map(|q| q as i64));
    engine_args
}

fn fix_settings(texture_quality: Option<i64>) {
    let path = support_dir().join("saves/userdata/Documents/Rockstar Games/GTA V/settings.xml");
    let Ok(xml) = std::fs::read_to_string(&path) else { return };
    let mut new = xml.clone();
    let mut set = |tag: &str, fix: &dyn Fn(i64) -> i64| {
        let re = regex::Regex::new(&format!(r#"<{tag} value="(\d+)" />"#)).unwrap();
        let Some(v) = re.captures(&new).and_then(|c| c[1].parse::<i64>().ok()) else { return };
        let to = fix(v);
        if to != v {
            new = re.replace(&new, format!(r#"<{tag} value="{to}" />"#).as_str()).into_owned();
            eprintln!("[config] {tag} {v} -> {to}");
        }
    };
    if let Some(q) = texture_quality {
        set("TextureQuality", &|_| q.clamp(0, 2));
    }
    set("ShaderQuality", &|v| v.max(1));
    set("MSAA", &|v| if v > 4 { v.min(crate::gpu::max_msaa() as i64) } else { v });
    if new != xml {
        let _ = std::fs::write(&path, new.as_bytes());
    }
}

/// Configure a native shell using explicit sandbox paths, without the desktop launcher.
pub fn setup_native(game_root: &Path, save_root: &Path) -> anyhow::Result<Startup> {
    anyhow::ensure!(game_root.join("data/manifest.json").is_file(), "game root has no data/manifest.json: {}", game_root.display());
    let build = build_dir(game_root).ok_or_else(|| anyhow::anyhow!("game root has no b/<build>/shaders/index.json"))?;
    std::fs::create_dir_all(save_root)?;
    // Validate every launcher choice before graphics may rewrite settings.xml.
    let start_mode = native_start_mode(save_root)?;
    let session = NativeSession::load(save_root)?;
    let native_graphics = prepare_native_graphics(save_root)?;
    for (key, value) in session.environment() {
        match value { Some(value) => std::env::set_var(key, value), None => std::env::remove_var(key) }
    }
    if let Ok(mut status) = NATIVE_SESSION_STATUS.lock() { *status = Some(session.status_json()); }
    eprintln!("[config] native session {}", session.status_json());
    let cache_dir = save_root.join("cache");
    std::fs::create_dir_all(&cache_dir)?;
    std::env::set_var("MUGUET_FSROOT", save_root);
    std::env::set_var("MUGUET_DATA", game_root.join("data"));
    std::env::set_var("MUGUET_SHADERS", build.join("shaders"));
    let frame_limit = if native_graphics.config.fps_limit == 30 { 2 } else { 1 };
    Ok(Startup { wasm_path: build.join("game.wasm"), cache_dir,
        engine_args: vec![format!("-frameLimit={frame_limit}"), format!("-wasmstartchoice={start_mode}")],
        native_graphics: Some(native_graphics) })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeGraphics {
    pub width: u32,
    pub height: u32,
    pub fps_limit: u32,
    pub preset: NativeGraphicsPreset,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeGraphicsPreset { Existing, Performance, Clarity }
impl NativeGraphicsPreset {
    fn parse(value: &str) -> Option<Self> { match value {
        "existing" | "low" => Some(Self::Existing), // Legacy low never reapplied quality to existing XML.
        "performance" => Some(Self::Performance), "clarity" => Some(Self::Clarity), _ => None,
    }}
    fn as_str(self) -> &'static str { match self { Self::Existing => "existing", Self::Performance => "performance", Self::Clarity => "clarity" } }
}

impl Default for NativeGraphics {
    fn default() -> Self { Self { width: 960, height: 540, fps_limit: 60, preset: NativeGraphicsPreset::Existing } }
}

impl NativeGraphics {
    fn parse(text: &str) -> anyhow::Result<Self> {
        use anyhow::{ensure, Context};
        let value: serde_json::Value = serde_json::from_str(text).context("parse native-graphics.json")?;
        let object = value.as_object().ok_or_else(|| anyhow::anyhow!("native graphics config must be an object"))?;
        ensure!(object.len() == 4 && ["width", "height", "fps_limit", "graphics_preset"].iter().all(|key| object.contains_key(*key)),
            "native graphics config requires exactly width, height, fps_limit and graphics_preset");
        let integer = |key: &str| -> anyhow::Result<u32> {
            let number = object[key].as_u64().ok_or_else(|| anyhow::anyhow!("{key} must be an unsigned integer"))?;
            u32::try_from(number).with_context(|| format!("{key} is out of range"))
        };
        let preset = object["graphics_preset"].as_str().and_then(NativeGraphicsPreset::parse)
            .context("native graphics_preset must be existing, performance or clarity")?;
        let config = Self { width: integer("width")?, height: integer("height")?, fps_limit: integer("fps_limit")?, preset };
        ensure!(matches!((config.width, config.height),
            (640, 360) | (960, 540) | (1280, 720) | (1920, 1080) | (2560, 1440) | (3840, 2160) | (2560, 2304)),
            "native resolution must be a supported 16:9 size or the explicit 2560x2304 experiment");
        ensure!(matches!(config.fps_limit, 30 | 60), "native fps_limit must be 30 or 60");
        Ok(config)
    }
}

pub struct NativeGraphicsLaunch {
    pub config: NativeGraphics,
    config_path: PathBuf,
    config_source: &'static str,
    settings_path: PathBuf,
    settings_action: &'static str,
    changed_tags: Vec<String>,
    backup_path: Option<PathBuf>,
    marker_path: PathBuf,
    marker_action: &'static str,
    quality_action: &'static str,
    texture_override: Option<u8>,
    actual_graphics_values: std::collections::BTreeMap<String, String>,
}

impl NativeGraphicsLaunch {
    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!({"width":self.config.width, "height":self.config.height,
            "fps_limit":self.config.fps_limit,"graphics_preset":self.config.preset.as_str(), "config_source":self.config_source,
            "config_path":self.config_path,"settings_path":self.settings_path,
            "settings_action":self.settings_action,"changed_tags":self.changed_tags,"backup_path":self.backup_path,
            "marker_path":self.marker_path,"marker_action":self.marker_action,
            "quality_action":self.quality_action,"texture_override":self.texture_override,
            "actual_graphics_values":self.actual_graphics_values,
            "settings_scope":"Preset is a requested one-time change, not a claim about current GTA quality. Existing choices are preserved on unchanged launch; Clarity changes textures/filtering/blur only; a launcher Textures choice is applied after any preset; MSAA remains disabled for immersive depth. Resolution and frame cap apply separately."})
    }
}

/// Read one small optional launcher JSON object. A missing file is `None`;
/// unknown keys are left to their owners.
fn read_native_object(save_root: &Path, name: &str) -> anyhow::Result<Option<serde_json::Map<String, serde_json::Value>>> {
    use anyhow::{ensure, Context};
    use std::io::Read;
    let file = match std::fs::File::open(save_root.join(name)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {name}")),
    };
    let mut text = String::new();
    file.take(65_537).read_to_string(&mut text).with_context(|| format!("read {name}"))?;
    ensure!(text.len() <= 65_536, "{name} is too large");
    let value: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parse {name}"))?;
    match value {
        serde_json::Value::Object(object) => Ok(Some(object)),
        _ => anyhow::bail!("{name} must contain a JSON object"),
    }
}

fn optional_bool(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> anyhow::Result<Option<bool>> {
    object.get(key).map(|value| value.as_bool().ok_or_else(|| anyhow::anyhow!("{key} must be a boolean"))).transpose()
}

/// Story is the normal launch. Developer mode deliberately opts into the
/// original guest's Sandbox path; this does not alter the compiled game AOT.
fn native_start_mode(save_root: &Path) -> anyhow::Result<u32> {
    let Some(object) = read_native_object(save_root, "native-gameplay.json")? else { return Ok(1) };
    Ok(if optional_bool(&object, "developer_mode")?.unwrap_or(false) { 2 } else { 1 })
}

/// The launcher's Textures choice. Absent or null keeps GTA's own value; a
/// choice is applied after any quality preset, so the two stay independent.
fn native_texture_quality(save_root: &Path) -> anyhow::Result<Option<u8>> {
    let Some(object) = read_native_object(save_root, "native-textures.json")? else { return Ok(None) };
    match object.get("texture_quality") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value.as_u64().filter(|quality| *quality <= 2).map(|quality| Some(quality as u8))
            .ok_or_else(|| anyhow::anyhow!("texture_quality must be 0, 1, 2 or null")),
    }
}

/// Prelaunch audio and multiplayer choices. Mute is separate from the saved
/// master volume, so unmuting restores the chosen level.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeSession {
    pub mute: bool,
    pub multiplayer: Option<NativeMultiplayer>,
}

/// Muguet's existing UDP session, selected before the engine starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeMultiplayer {
    pub host: bool,
    /// Normalized `host:port` when joining.
    pub server: Option<String>,
    pub player_name: String,
}

const NATIVE_DEFAULT_PLAYER: &str = "Vision Pro";

impl NativeSession {
    fn load(save_root: &Path) -> anyhow::Result<Self> {
        let mute = match read_native_object(save_root, "native-audio.json")? {
            Some(object) => optional_bool(&object, "mute")?.unwrap_or(false),
            None => false,
        };
        let multiplayer = match read_native_object(save_root, "native-multiplayer.json")? {
            Some(object) if optional_bool(&object, "multiplayer")?.unwrap_or(false) => Some(NativeMultiplayer::parse(&object)?),
            _ => None,
        };
        Ok(Self { mute, multiplayer })
    }

    /// Environment consumed by audio::start and mp::net. Absent choices leave
    /// any explicit diagnostic environment untouched.
    fn environment(&self) -> Vec<(&'static str, Option<String>)> {
        let mut values = Vec::new();
        if self.mute { values.push(("MUGUET_MUTE", Some("1".into()))); }
        if let Some(multiplayer) = &self.multiplayer {
            values.push(("MUGUET_HOST", multiplayer.host.then(|| "1".into())));
            values.push(("MUGUET_SERVER", multiplayer.server.clone()));
            values.push(("MUGUET_NAME", Some(multiplayer.player_name.clone())));
        }
        values
    }

    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!({"mute": self.mute, "multiplayer": self.multiplayer.as_ref().map(|multiplayer| serde_json::json!({
            "role": if multiplayer.host { "host" } else { "join" }, "server": multiplayer.server,
            "player_name": multiplayer.player_name, "port": crate::proto::DEFAULT_PORT,
            "scope": "Muguet LAN UDP session between V players; not Rockstar GTA Online"}))})
    }
}

impl NativeMultiplayer {
    fn parse(object: &serde_json::Map<String, serde_json::Value>) -> anyhow::Result<Self> {
        let text = |key: &str| -> anyhow::Result<String> {
            match object.get(key) {
                None => Ok(String::new()),
                Some(value) => value.as_str().map(|text| text.trim().to_string()).ok_or_else(|| anyhow::anyhow!("{key} must be text")),
            }
        };
        let host = optional_bool(object, "host")?.unwrap_or(false);
        let server = if host { None } else { Some(multiplayer_address(&text("server")?)?) };
        let name: String = text("player_name")?.chars().filter(|c| !c.is_control()).collect();
        anyhow::ensure!(name.chars().count() <= 24, "Use a player name of at most 24 characters.");
        let player_name = if name.trim().is_empty() { NATIVE_DEFAULT_PLAYER.into() } else { name.trim().into() };
        Ok(Self { host, server, player_name })
    }
}

/// Validate a host address for Muguet's IPv4 UDP client: IPv4 or a host name,
/// with an optional port. Returns `host:port`.
pub fn multiplayer_address(raw: &str) -> anyhow::Result<String> {
    use anyhow::{ensure, Context};
    let text = raw.trim();
    ensure!(!text.is_empty(), "Enter the host's address to join.");
    ensure!(text.matches(':').count() <= 1 && !text.contains('['), "Use the host's IPv4 address or name, for example 192.168.1.20.");
    let (host, port) = match text.split_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok().filter(|port| *port != 0).context("The port must be a number from 1 to 65535.")?),
        None => (text, crate::proto::DEFAULT_PORT),
    };
    let labels: Vec<&str> = host.split('.').collect();
    let valid_name = host.len() <= 253 && labels.iter().all(|label| !label.is_empty() && label.len() <= 63
        && !label.starts_with('-') && !label.ends_with('-') && label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'));
    let numeric = labels.iter().all(|label| label.bytes().all(|c| c.is_ascii_digit()));
    ensure!(valid_name && (!numeric || host.parse::<std::net::Ipv4Addr>().is_ok()),
        "Use the host's IPv4 address or name, for example 192.168.1.20.");
    Ok(format!("{host}:{port}"))
}

static NATIVE_SESSION_STATUS: std::sync::Mutex<Option<serde_json::Value>> = std::sync::Mutex::new(None);

/// The launcher choices applied by the latest native startup.
pub fn native_session_status() -> Option<serde_json::Value> {
    NATIVE_SESSION_STATUS.lock().ok().and_then(|status| status.clone())
}

const LOW_GRAPHICS: &[(&str, &str)] = &[
    ("TextureQuality", "0"), ("ShaderQuality", "1"), ("ShadowQuality", "0"),
    ("ReflectionQuality", "0"), ("GrassQuality", "0"), ("WaterQuality", "0"),
    ("ParticleQuality", "0"), ("PostFX", "0"), ("Tessellation", "0"),
    ("ReflectionMSAA", "0"), ("SSAO", "0"), ("AnisotropicFiltering", "0"),
    ("MSAA", "0"), ("MSAAFragments", "0"), ("MSAAQuality", "0"), ("SamplingMode", "0"),
    ("FXAA_Enabled", "false"), ("TXAA_Enabled", "false"), ("DoF", "false"),
    ("MotionBlurStrength", "0.000000"), ("Lighting_FogVolumes", "false"), ("Shader_SSA", "false"),
    ("UltraShadows_Enabled", "false"), ("Shadow_ParticleShadows", "false"),
    ("Shadow_LongShadows", "false"), ("Shadow_SoftShadows", "0"), ("Reflection_MipBlur", "false"),
    ("LodScale", "0.000000"), ("MaxLodScale", "0.000000"), ("PedLodBias", "0.000000"),
    ("VehicleLodBias", "0.000000"), ("CityDensity", "0.000000"),
];
// Exact original65672 forwards graphics+40 unchanged to MaxAnisotropy;
// the host sampler accepts/clamps the raw multiplier to16, not an enum index.
const CLARITY_GRAPHICS: &[(&str, &str)] = &[
    ("TextureQuality", "2"), ("AnisotropicFiltering", "16"),
    ("DoF", "false"), ("MotionBlurStrength", "0.000000"),
];

// Same root/version/system/audio/video shape as the actual game-generated file.
// The graphics values and video dimensions are inserted by the scoped updater.
const NATIVE_DEFAULT_SETTINGS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Settings>
  <version value="27" />
  <configSource>SMC_AUTO</configSource>
  <graphics>
    <MSAA value="0" />
    <Shadow_Distance value="1.000000" />
    <Shadow_SplitZStart value="0.930000" />
    <Shadow_SplitZEnd value="0.890000" />
    <Shadow_aircraftExpWeight value="0.990000" />
    <Shadow_DisableScreenSizeCheck value="false" />
    <DX_Version value="2" />
    <PedVarietyMultiplier value="1.000000" />
    <VehicleVarietyMultiplier value="1.000000" />
    <HdStreamingInFlight value="false" />
  </graphics>
  <system>
    <numBytesPerReplayBlock value="9000000" />
    <numReplayBlocks value="36" />
    <maxSizeOfStreamingReplay value="1024" />
    <maxFileStoreSize value="65536" />
  </system>
  <audio><Audio3d value="false" /></audio>
  <video>
    <AdapterIndex value="0" />
    <OutputIndex value="0" />
    <Windowed value="1" />
    <Stereo value="0" />
    <Convergence value="0.100000" />
    <Separation value="1.000000" />
    <PauseOnFocusLoss value="1" />
    <AspectRatio value="0" />
  </video>
  <VideoCardDescription>N</VideoCardDescription>
</Settings>
"#;

/// Edit only value attributes in the graphics/video children of Settings.
/// Comments, CDATA, audio/system settings and unknown tags remain byte-for-byte.
fn low_settings_xml(xml: &str, config: &NativeGraphics) -> anyhow::Result<(String, Vec<String>)> {
    use std::collections::BTreeMap;
    let mut wanted: BTreeMap<(&str, &str), String> = LOW_GRAPHICS.iter()
        .map(|(tag, value)| (("graphics", *tag), (*value).into())).collect();
    for (tag, value) in [("ScreenWidth", config.width), ("ScreenHeight", config.height),
                         ("RefreshRate", config.fps_limit), ("VSync", 1)] {
        wanted.insert(("video", tag), value.to_string());
    }
    settings_xml_with_values(xml, wanted)
}

fn video_settings_xml(xml: &str, config: &NativeGraphics) -> anyhow::Result<(String, Vec<String>)> {
    let wanted = [("ScreenWidth", config.width), ("ScreenHeight", config.height),
                  ("RefreshRate", config.fps_limit)].into_iter()
        .map(|(tag, value)| (("video", tag), value.to_string())).collect();
    settings_xml_with_values(xml, wanted)
}

fn settings_xml_with_values(xml: &str, wanted: std::collections::BTreeMap<(&str, &str), String>)
    -> anyhow::Result<(String, Vec<String>)> {
    settings_xml_with_observed_values(xml, wanted, &mut std::collections::BTreeMap::new())
}
fn settings_xml_with_observed_values(xml: &str, wanted: std::collections::BTreeMap<(&str, &str), String>,
    observed: &mut std::collections::BTreeMap<String, String>) -> anyhow::Result<(String, Vec<String>)> {
    settings_xml_with_value_policy(xml, wanted, observed, false)
}

/// Exact attribute qualification is confined to the opt-in taller source.
/// Leave the existing wide-profile XML parser behavior unchanged.
fn tall_aspect_xml(xml: &str) -> anyhow::Result<(String, Vec<String>)> {
    settings_xml_with_value_policy(xml,
        [(("video", "AspectRatio"), "0".to_string())].into_iter().collect(),
        &mut std::collections::BTreeMap::new(), true)
}

fn exact_single_value_attribute(tag: &str) -> anyhow::Result<(usize, usize)> {
    use anyhow::{ensure, Context};
    let bytes = tag.as_bytes();
    let name_character = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-' | b'.');
    let whitespace = |byte: u8| matches!(byte, b' ' | b'\t' | b'\r' | b'\n');
    ensure!(bytes.first() == Some(&b'<'), "invalid tall-source aspect tag");
    let mut cursor = 1;
    while bytes.get(cursor).is_some_and(|byte| name_character(*byte)) { cursor += 1; }
    let mut names = std::collections::BTreeSet::new();
    let mut value = None;
    loop {
        let before_space = cursor;
        while bytes.get(cursor).is_some_and(|byte| whitespace(*byte)) { cursor += 1; }
        if bytes.get(cursor) == Some(&b'/') {
            ensure!(bytes.get(cursor + 1) == Some(&b'>') && cursor + 2 == bytes.len(), "invalid tall-source aspect tag ending");
            break;
        }
        ensure!(cursor > before_space, "tall-source aspect attributes must be separated by whitespace");
        let start = cursor;
        ensure!(bytes.get(cursor).is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':')),
            "invalid tall-source aspect attribute name");
        while bytes.get(cursor).is_some_and(|byte| name_character(*byte)) { cursor += 1; }
        let name = &tag[start..cursor];
        ensure!(names.insert(name), "duplicate tall-source aspect attribute {name}");
        while bytes.get(cursor).is_some_and(|byte| whitespace(*byte)) { cursor += 1; }
        ensure!(bytes.get(cursor) == Some(&b'='), "tall-source aspect attribute has no equals sign");
        cursor += 1;
        while bytes.get(cursor).is_some_and(|byte| whitespace(*byte)) { cursor += 1; }
        let quote = *bytes.get(cursor).context("missing tall-source aspect attribute value")?;
        ensure!(matches!(quote, b'\'' | b'"'), "tall-source aspect attribute value must be quoted");
        cursor += 1;
        let value_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| *byte != quote) { cursor += 1; }
        ensure!(bytes.get(cursor) == Some(&quote), "unterminated tall-source aspect attribute value");
        if name == "value" { value = Some((value_start, cursor)); }
        cursor += 1;
    }
    value.context("tall-source aspect requires one exact value attribute")
}

fn settings_xml_with_value_policy(xml: &str, wanted: std::collections::BTreeMap<(&str, &str), String>,
    observed: &mut std::collections::BTreeMap<String, String>, strict_tall_aspect: bool) -> anyhow::Result<(String, Vec<String>)> {
    use anyhow::{ensure, Context};
    use std::collections::{BTreeMap, BTreeSet};
    let value_attribute = regex::Regex::new(r#"\bvalue\s*=\s*(?:"([^"]*)"|'([^']*)')"#)?;
    let mut stack: Vec<&str> = Vec::new();
    let mut sections = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut changed = Vec::new();
    let mut root_end = None;
    let mut roots = 0;
    let mut cursor = 0;
    while let Some(relative) = xml[cursor..].find('<') {
        let start = cursor + relative;
        let special = if xml[start..].starts_with("<!--") { Some(("<!--", "-->")) }
            else if xml[start..].starts_with("<![CDATA[") { Some(("<![CDATA[", "]]>")) }
            else if xml[start..].starts_with("<?") { Some(("<?", "?>")) } else { None };
        if let Some((prefix, suffix)) = special {
            cursor = start + prefix.len() + xml[start + prefix.len()..].find(suffix)
                .with_context(|| format!("unterminated XML {prefix}"))? + suffix.len();
            continue;
        }
        ensure!(!xml[start..].starts_with("<!"), "unsupported settings XML declaration");
        let mut quote = None;
        let mut end = None;
        for (index, byte) in xml.as_bytes()[start + 1..].iter().copied().enumerate() {
            if let Some(q) = quote { if byte == q { quote = None; } }
            else if matches!(byte, b'\'' | b'"') { quote = Some(byte); }
            else if byte == b'>' { end = Some(start + 1 + index); break; }
            else { ensure!(byte != b'<', "malformed settings XML tag"); }
        }
        let end = end.context("unterminated settings XML tag")?;
        let body = xml[start + 1..end].trim();
        let closing = body.starts_with('/');
        let named = if closing { body[1..].trim() } else { body };
        let name_end = named.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(named.len());
        let name = &named[..name_end];
        ensure!(!name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'-')), "invalid settings XML tag");
        if closing {
            ensure!(named[name_end..].trim().is_empty() && stack.pop() == Some(name), "unbalanced settings XML at {name}");
            if stack == ["Settings"] && matches!(name, "graphics" | "video") { sections.insert(name, start); }
            if stack.is_empty() { root_end = Some(start); }
        } else {
            let self_closing = body.ends_with('/');
            if stack.is_empty() {
                ensure!(name == "Settings" && !self_closing && roots == 0, "settings XML must have one Settings root");
                roots += 1;
            }
            if stack.len() == 2 && stack[0] == "Settings" {
                if stack[1] == "graphics" && ["TextureQuality", "ShaderQuality", "AnisotropicFiltering", "ShadowQuality", "ReflectionQuality", "SSAO", "Tessellation", "FXAA_Enabled", "MSAA", "DoF", "MotionBlurStrength"].contains(&name) {
                    if let Some(captures) = value_attribute.captures(&xml[start..end + 1]) {
                        observed.insert(name.into(), captures.get(1).or_else(|| captures.get(2)).unwrap().as_str().into());
                    }
                }
                if let Some(value) = wanted.get(&(stack[1], name)) {
                    ensure!(self_closing && seen.insert((stack[1], name)), "duplicate or non-value graphics tag {name}");
                    let (field_start, field_end) = if strict_tall_aspect && stack[1] == "video" && name == "AspectRatio" {
                        exact_single_value_attribute(&xml[start..end + 1])?
                    } else {
                        let captures = value_attribute.captures(&xml[start..end + 1])
                            .with_context(|| format!("settings tag {name} has no value attribute"))?;
                        let field = captures.get(1).or_else(|| captures.get(2)).unwrap();
                        (field.start(), field.end())
                    };
                    if &xml[start + field_start..start + field_end] != value {
                        edits.push((start + field_start, start + field_end, value.clone()));
                        changed.push(name.into());
                    }
                }
            }
            if !self_closing { stack.push(name); }
        }
        cursor = end + 1;
    }
    ensure!(stack.is_empty() && roots == 1, "unbalanced settings XML");
    let root_end = root_end.context("settings XML has no closing Settings tag")?;
    for section in ["graphics", "video"] {
        let mut missing = String::new();
        for ((parent, tag), value) in &wanted {
            if *parent == section && !seen.contains(&(*parent, *tag)) {
                missing.push_str(&format!("    <{tag} value=\"{value}\" />\n"));
                changed.push((*tag).into());
            }
        }
        if !missing.is_empty() {
            if let Some(end) = sections.get(section) { edits.push((*end, *end, missing)); }
            else { edits.push((root_end, root_end, format!("  <{section}>\n{missing}  </{section}>\n"))); }
        }
    }
    edits.sort_by_key(|edit| edit.0);
    let mut result = xml.to_owned();
    for (start, end, value) in edits.into_iter().rev() { result.replace_range(start..end, &value); }
    changed.sort();
    Ok((result, changed))
}

fn prepare_native_graphics(save_root: &Path) -> anyhow::Result<NativeGraphicsLaunch> {
    use anyhow::{ensure, Context};
    use std::io::Write;
    let config_path = save_root.join("native-graphics.json");
    let (config, config_source) = match std::fs::read_to_string(&config_path) {
        Ok(text) => { ensure!(text.len() <= 4096, "native graphics config exceeds 4096 bytes"); (NativeGraphics::parse(&text)?, "file") }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("[config] {} missing; preserving GTA quality with native 960x540 at 60 fps", config_path.display());
            (NativeGraphics::default(), "built_in_default")
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", config_path.display())),
    };
    let settings_path = save_root.join("userdata/Documents/Rockstar Games/GTA V/settings.xml");
    let marker_path = save_root.join("native-graphics-applied.json");
    let applied_config = match std::fs::read_to_string(&marker_path) {
        Ok(text) => {
            ensure!(text.len() <= 4096, "native graphics marker exceeds 4096 bytes");
            Some(NativeGraphics::parse(&text).context("parse native graphics marker")?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("read {}", marker_path.display())),
    };
    let (original, existed) = match std::fs::read_to_string(&settings_path) {
        Ok(xml) => { ensure!(xml.len() <= 512 << 10, "settings.xml exceeds 512 KiB"); (xml, true) }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (NATIVE_DEFAULT_SETTINGS.into(), false),
        Err(error) => return Err(error).with_context(|| format!("read {}", settings_path.display())),
    };
    let tall_source_experiment = (config.width, config.height) == (2560, 2304);
    if tall_source_experiment {
        // Reuse the validated XML walker only as a read-only qualification.
        // A forced or unknown game aspect must not distort the taller source;
        // neither that setting nor the player's FOV is silently rewritten.
        let (automatic, _) = tall_aspect_xml(&original)
            .context("validate Automatic aspect ratio for the taller source")?;
        ensure!(automatic == original,
            "Choose Automatic aspect ratio in GTA before using the 2560x2304 experimental resolution.");
    }
    let preset_changed = applied_config.as_ref().map(|previous| previous.preset != config.preset)
        .unwrap_or(config.preset != NativeGraphicsPreset::Existing);
    let video_changed = applied_config.as_ref().map(|previous|
        (previous.width, previous.height, previous.fps_limit) != (config.width, config.height, config.fps_limit))
        .unwrap_or(config.preset != NativeGraphicsPreset::Existing || tall_source_experiment);
    let (mut xml, mut changed_tags) = if !existed {
        low_settings_xml(&original, &config).context("create native low graphics defaults")?
    } else {
        // A missing marker means this may be an upgrade with GTA-owned XML.
        // Validate its structure, then adopt it without rewriting user values.
        settings_xml_with_values(&original, std::collections::BTreeMap::new())
            .context("validate existing GTA settings")?
    };
    let quality_action = match config.preset {
        NativeGraphicsPreset::Clarity if !existed || preset_changed => {
            let wanted = CLARITY_GRAPHICS.iter().map(|(tag, value)| (("graphics", *tag), (*value).into())).collect();
            let (updated, tags) = settings_xml_with_values(&xml, wanted).context("apply explicit Clarity graphics")?;
            xml = updated; changed_tags.extend(tags); "applied_clarity"
        }
        NativeGraphicsPreset::Performance if existed && preset_changed => {
            let wanted = LOW_GRAPHICS.iter().map(|(tag, value)| (("graphics", *tag), (*value).into())).collect();
            let (updated, tags) = settings_xml_with_values(&xml, wanted).context("apply explicit Performance graphics")?;
            xml = updated; changed_tags.extend(tags); "applied_performance"
        }
        _ if !existed => "created_performance_baseline",
        NativeGraphicsPreset::Existing => "kept_existing",
        _ => "preserved_game_choices",
    };
    let texture_override = native_texture_quality(save_root)?;
    if let Some(quality) = texture_override {
        let wanted = [(("graphics", "TextureQuality"), quality.to_string())].into_iter().collect();
        let (updated, tags) = settings_xml_with_values(&xml, wanted).context("apply launcher texture quality")?;
        xml = updated; changed_tags.extend(tags);
    }
    if existed && video_changed {
        let (updated, tags) = video_settings_xml(&xml, &config).context("apply changed native resolution and frame cap")?;
        xml = updated; changed_tags.extend(tags);
    }
    // The current world-frame handoff requires actual single-sample depth.
    // Do not accept MSAA targets as if they were qualified scene depth. Keep
    // all other game choices, including FXAA, and use the existing exact backup.
    let (xml, compatibility_tags) = settings_xml_with_values(&xml,
        [( ("graphics", "MSAA"), "0".to_string() )].into_iter().collect())
        .context("apply immersive single-sample depth compatibility")?;
    changed_tags.extend(compatibility_tags);
    changed_tags.sort();
    changed_tags.dedup();
    let mut backup_path = None;
    let settings_action = if existed && xml == original { "unchanged" } else {
        let parent = settings_path.parent().unwrap();
        std::fs::create_dir_all(parent).context("create native graphics settings directory")?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        if existed {
            let backup = parent.join(format!("settings.xml.native-graphics-backup.{stamp}.{}", std::process::id()));
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&backup).context("create settings.xml backup")?;
            file.write_all(original.as_bytes()).context("write settings.xml backup")?;
            file.sync_all().context("flush settings.xml backup")?;
            backup_path = Some(backup);
        }
        let temporary = parent.join(format!(".settings.xml.native-graphics.{stamp}.{}.tmp", std::process::id()));
        let result = (|| -> anyhow::Result<()> {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).context("create temporary settings.xml")?;
            if existed { file.set_permissions(std::fs::metadata(&settings_path)?.permissions())?; }
            file.write_all(xml.as_bytes()).context("write native graphics settings.xml")?;
            file.sync_all().context("flush native graphics settings.xml")?;
            std::fs::rename(&temporary, &settings_path).context("replace native graphics settings.xml")?;
            Ok(())
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temporary); }
        result?;
        if existed { "updated_with_backup" } else if config.preset == NativeGraphicsPreset::Clarity { "created_clarity_defaults" } else { "created_low_defaults" }
    };
    let marker_action = if applied_config.as_ref() == Some(&config) && existed { "unchanged" } else {
        write_native_graphics_marker(&marker_path, &config)?;
        if applied_config.is_some() { "updated" } else if existed { "adopted_existing" } else { "created" }
    };
    let mut actual_graphics_values = std::collections::BTreeMap::new();
    settings_xml_with_observed_values(&xml, std::collections::BTreeMap::new(), &mut actual_graphics_values)?;
    let launch = NativeGraphicsLaunch { config, config_path, config_source, settings_path, settings_action,
        changed_tags, backup_path, marker_path, marker_action, quality_action, texture_override, actual_graphics_values };
    eprintln!("[config] native graphics {}", launch.status_json());
    Ok(launch)
}

fn write_native_graphics_marker(path: &Path, config: &NativeGraphics) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;
    let data = serde_json::to_vec(&serde_json::json!({"width": config.width, "height": config.height,
        "fps_limit": config.fps_limit, "graphics_preset": config.preset.as_str()}))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let temporary = path.with_file_name(format!(".native-graphics-applied.{stamp}.{}.tmp", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)
            .context("create temporary native graphics marker")?;
        file.write_all(&data).context("write native graphics marker")?;
        file.sync_all().context("flush native graphics marker")?;
        std::fs::rename(&temporary, path).context("replace native graphics marker")?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}

#[cfg(test)]
mod native_graphics_tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("muguet-native-graphics-test.{}.{stamp}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn config(&self, width: u32, height: u32, fps: u32) {
            self.preset(width, height, fps, "low");
        }
        fn preset(&self, width: u32, height: u32, fps: u32, preset: &str) {
            std::fs::write(self.0.join("native-graphics.json"), format!(r#"{{"width":{width},"height":{height},"fps_limit":{fps},"graphics_preset":"{preset}"}}"#)).unwrap();
        }
        fn settings(&self) -> PathBuf { self.0.join("userdata/Documents/Rockstar Games/GTA V/settings.xml") }
        fn marker(&self) -> PathBuf { self.0.join("native-graphics-applied.json") }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[test]
    fn native_start_mode_defaults_to_story_and_requires_explicit_sandbox() {
        let fixture = Fixture::new();
        assert_eq!(native_start_mode(&fixture.0).unwrap(), 1);
        let path = fixture.0.join("native-gameplay.json");
        for (text, expected) in [("{}", 1), (r#"{"developer_mode":false}"#, 1),
            (r#"{"developer_mode":true,"other_setting":7}"#, 2)] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(native_start_mode(&fixture.0).unwrap(), expected);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        for text in [r#"{"developer_mode":1}"#, r#"{"developer_mode":"true"}"#, "[]", "broken"] {
            std::fs::write(&path, text).unwrap();
            assert!(native_start_mode(&fixture.0).is_err());
        }
        std::fs::write(&path, " ".repeat(65_537)).unwrap();
        assert!(native_start_mode(&fixture.0).is_err());
    }

    #[test]
    fn native_graphics_accepts_only_safe_resolution_and_frame_caps() {
        for (width, height) in [(640, 360), (960, 540), (1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)] {
            for fps in [30, 60] {
                let value = format!(r#"{{"width":{width},"height":{height},"fps_limit":{fps},"graphics_preset":"low"}}"#);
                assert_eq!(NativeGraphics::parse(&value).unwrap(), NativeGraphics { width, height, fps_limit: fps, ..NativeGraphics::default() });
            }
        }
        assert_eq!(NativeGraphics::default(), NativeGraphics { width: 960, height: 540, fps_limit: 60, preset: NativeGraphicsPreset::Existing });
    }

    #[test]
    fn opt_in_tall_source_requires_explicit_automatic_aspect_before_any_file_write() {
        let fixture = Fixture::new();
        fixture.preset(2560, 2304, 60, "existing");
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = NATIVE_DEFAULT_SETTINGS.replace("<AspectRatio value=\"0\" />", "<AspectRatio value=\"2\" />");
        std::fs::write(&path, &original).unwrap();
        write_native_graphics_marker(&fixture.marker(), &NativeGraphics::default()).unwrap();
        let marker = std::fs::read(fixture.marker()).unwrap();
        let error = prepare_native_graphics(&fixture.0).err().expect("forced aspect must reject the tall source");
        assert!(error.to_string().contains("Automatic aspect ratio"), "{error:#}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(std::fs::read(fixture.marker()).unwrap(), marker);
        assert!(std::fs::read_dir(path.parent().unwrap()).unwrap().all(|entry|
            !entry.unwrap().file_name().to_string_lossy().contains("backup")));

        std::fs::write(&path, NATIVE_DEFAULT_SETTINGS).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!((launch.config.width, launch.config.height), (2560, 2304));
        let xml = std::fs::read_to_string(&path).unwrap();
        assert!(xml.contains("<ScreenWidth value=\"2560\""));
        assert!(xml.contains("<ScreenHeight value=\"2304\""));
        assert!(xml.contains("<AspectRatio value=\"0\" />"));
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), NATIVE_DEFAULT_SETTINGS);
        assert!(!launch.changed_tags.iter().any(|tag| tag == "AspectRatio"));
    }

    #[test]
    fn tall_source_rejects_missing_and_duplicate_aspect_but_keeps_wide_behavior() {
        for aspect in ["", "<AspectRatio value=\"0\" /><AspectRatio value=\"0\" />", "<AspectRatio value=\"Auto\" />"] {
            let fixture = Fixture::new();
            fixture.preset(2560, 2304, 30, "existing");
            let path = fixture.settings();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let xml = NATIVE_DEFAULT_SETTINGS.replace("<AspectRatio value=\"0\" />", aspect);
            std::fs::write(&path, &xml).unwrap();
            assert!(prepare_native_graphics(&fixture.0).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), xml);
            assert!(!fixture.marker().exists());
        }
        let fixture = Fixture::new();
        fixture.preset(2560, 1440, 60, "existing");
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let xml = NATIVE_DEFAULT_SETTINGS.replace("<AspectRatio value=\"0\" />", "<AspectRatio value=\"2\" />");
        std::fs::write(&path, &xml).unwrap();
        prepare_native_graphics(&fixture.0).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("<AspectRatio value=\"2\" />"));
    }

    #[test]
    fn explicit_tall_source_without_marker_updates_only_video_with_exact_backup() {
        let fixture = Fixture::new();
        fixture.preset(2560, 2304, 60, "existing");
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = NATIVE_DEFAULT_SETTINGS.replace("<video>",
            "<video><ScreenWidth value=\"1920\"/><ScreenHeight value=\"1080\"/><RefreshRate value=\"60\"/>");
        std::fs::write(&path, &original).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.changed_tags, ["ScreenHeight", "ScreenWidth"]);
        assert_eq!(launch.quality_action, "kept_existing");
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), original);
        let wanted = original.replace("ScreenWidth value=\"1920\"", "ScreenWidth value=\"2560\"")
            .replace("ScreenHeight value=\"1080\"", "ScreenHeight value=\"2304\"");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), wanted);
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
    }

    fn rejected_tall_aspect_keeps_files(tag: &str) {
        let fixture = Fixture::new();
        fixture.preset(2560, 2304, 60, "existing");
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = NATIVE_DEFAULT_SETTINGS.replace("<AspectRatio value=\"0\" />", tag);
        std::fs::write(&path, &original).unwrap();
        write_native_graphics_marker(&fixture.marker(), &NativeGraphics::default()).unwrap();
        let marker = std::fs::read(fixture.marker()).unwrap();
        assert!(prepare_native_graphics(&fixture.0).is_err(), "ambiguous or forced aspect was admitted: {tag}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(std::fs::read(fixture.marker()).unwrap(), marker);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1,
                   "rejection must precede even backup creation");
    }

    #[test]
    fn tall_source_rejects_suffix_attribute_alias_before_writes() {
        rejected_tall_aspect_keeps_files("<AspectRatio note-value=\"0\" value=\"2\" />");
    }

    #[test]
    fn tall_source_rejects_duplicate_value_attribute_before_writes() {
        rejected_tall_aspect_keeps_files("<AspectRatio value=\"0\" value=\"2\" />");
        rejected_tall_aspect_keeps_files("<AspectRatio value=\"0\" value='0' />");
    }

    #[test]
    fn tall_source_accepts_exact_single_auto_attribute_in_ordinary_formats() {
        for tag in ["<AspectRatio value='0' />", "<AspectRatio\n value = '0'/>",
            "<AspectRatio note-value=\"2\" value=\"0\" />",
            "<AspectRatio value=\"0\" note-value=\"2\" />",
            "<AspectRatio note=\"1 &gt; 0\" value=\"0\" />"] {
            let fixture = Fixture::new();
            fixture.preset(2560, 2304, 60, "existing");
            let path = fixture.settings();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = NATIVE_DEFAULT_SETTINGS.replace("<AspectRatio value=\"0\" />", tag);
            std::fs::write(&path, &original).unwrap();
            let launch = prepare_native_graphics(&fixture.0).unwrap();
            assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), original);
            assert!(std::fs::read_to_string(&path).unwrap().contains(tag), "valid aspect syntax was rewritten");
            assert!(!launch.changed_tags.iter().any(|name| name == "AspectRatio"));
        }
    }

    #[test]
    fn native_graphics_rejects_malformed_or_unsupported_configuration() {
        for value in ["{", "[]", "{}",
            r#"{"width":-1,"height":540,"fps_limit":60,"graphics_preset":"low"}"#,
            r#"{"width":960.5,"height":540,"fps_limit":60,"graphics_preset":"low"}"#,
            r#"{"width":960,"height":720,"fps_limit":60,"graphics_preset":"low"}"#,
            r#"{"width":3840,"height":1440,"fps_limit":60,"graphics_preset":"low"}"#,
            r#"{"width":7680,"height":4320,"fps_limit":60,"graphics_preset":"low"}"#,
            r#"{"width":960,"height":540,"fps_limit":90,"graphics_preset":"low"}"#,
            r#"{"width":960,"height":540,"fps_limit":60,"graphics_preset":"high"}"#,
            r#"{"width":960,"height":540,"fps_limit":60,"graphics_preset":"low","extra_args":[]}"#] {
            assert!(NativeGraphics::parse(value).is_err(), "accepted {value}");
        }
    }

    fn current_quality_xml() -> String {
        let graphics = r#"<TextureQuality value="1"/><AnisotropicFiltering value="2"/><ShaderQuality value="1"/><ShadowQuality value="2"/><ReflectionQuality value="1"/><SSAO value="1"/><Tessellation value="1"/><FXAA_Enabled value="true"/><DoF value="true"/><MotionBlurStrength value="1.000000"/>"#;
        NATIVE_DEFAULT_SETTINGS.replace("<graphics>", &format!("<graphics>{graphics}"))
            .replace("<video>", "<video><ScreenWidth value=\"1920\"/><ScreenHeight value=\"1080\"/><RefreshRate value=\"60\"/>")
    }

    #[test]
    fn clarity_is_surgical_once_then_preserves_game_edits_and_atomic_backup() {
        let fixture = Fixture::new();
        fixture.preset(1920, 1080, 60, "clarity");
        let original = current_quality_xml();
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &original).unwrap();
        write_native_graphics_marker(&fixture.marker(), &NativeGraphics { width:1920, height:1080, ..NativeGraphics::default() }).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.quality_action, "applied_clarity");
        assert_eq!(launch.changed_tags, ["AnisotropicFiltering", "DoF", "MotionBlurStrength", "TextureQuality"]);
        assert_eq!(std::fs::read_to_string(launch.backup_path.as_ref().unwrap()).unwrap(), original);
        let expected = original.replace("<TextureQuality value=\"1\"/>", "<TextureQuality value=\"2\"/>")
            .replace("<AnisotropicFiltering value=\"2\"/>", "<AnisotropicFiltering value=\"16\"/>")
            .replace("<DoF value=\"true\"/>", "<DoF value=\"false\"/>")
            .replace("<MotionBlurStrength value=\"1.000000\"/>", "<MotionBlurStrength value=\"0.000000\"/>");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        let status = launch.status_json();
        assert_eq!(status["graphics_preset"], "clarity");
        assert_eq!(status["actual_graphics_values"]["ShadowQuality"], "2");
        assert_eq!(status["actual_graphics_values"]["ShaderQuality"], "1");
        assert_eq!(status["actual_graphics_values"]["FXAA_Enabled"], "true");
        assert_eq!(NativeGraphics::parse(&std::fs::read_to_string(fixture.marker()).unwrap()).unwrap().preset, NativeGraphicsPreset::Clarity);
        for directory in [&fixture.0, path.parent().unwrap()] {
            assert!(std::fs::read_dir(directory).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
        }
        let user_edit = expected.replace("<TextureQuality value=\"2\"/>", "<TextureQuality value=\"1\"/>")
            .replace("<AnisotropicFiltering value=\"16\"/>", "<AnisotropicFiltering value=\"4\"/>");
        std::fs::write(&path, &user_edit).unwrap();
        let next = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(next.settings_action, "unchanged");
        assert_eq!(next.quality_action, "preserved_game_choices");
        assert!(next.backup_path.is_none());
        assert_eq!(next.actual_graphics_values["AnisotropicFiltering"], "4");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), user_edit);
        fixture.preset(1920, 1080, 60, "existing");
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), user_edit);
    }

    #[test]
    fn explicit_quality_without_marker_applies_requested_video_and_keeps_other_choices() {
        let fixture = Fixture::new();
        fixture.preset(2560, 1440, 60, "clarity");
        let original = current_quality_xml();
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &original).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), original);
        let xml = std::fs::read_to_string(&path).unwrap();
        for exact in ["<ScreenWidth value=\"2560\"/>", "<ScreenHeight value=\"1440\"/>", "<ShadowQuality value=\"2\"/>"] { assert!(xml.contains(exact)); }
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), xml);
    }

    #[test]
    fn performance_requires_explicit_transition_and_does_not_change_video() {
        let fixture = Fixture::new();
        fixture.preset(2560, 1440, 60, "performance");
        let original = current_quality_xml();
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &original).unwrap();
        write_native_graphics_marker(&fixture.marker(), &NativeGraphics { width:2560, height:1440, preset:NativeGraphicsPreset::Clarity, ..NativeGraphics::default() }).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.quality_action, "applied_performance");
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), original);
        let xml = std::fs::read_to_string(&path).unwrap();
        for exact in ["<TextureQuality value=\"0\"/>", "<ShadowQuality value=\"0\"/>", "<ScreenWidth value=\"1920\"/>", "<ScreenHeight value=\"1080\"/>"] { assert!(xml.contains(exact)); }
        assert!(!launch.changed_tags.iter().any(|tag| tag == "ScreenWidth" || tag == "ScreenHeight"));
    }

    #[test]
    fn missing_xml_clarity_has_complete_defaults_and_invalid_profile_never_writes() {
        let fixture = Fixture::new();
        fixture.preset(2560, 1440, 60, "clarity");
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.settings_action, "created_clarity_defaults");
        let xml = std::fs::read_to_string(fixture.settings()).unwrap();
        for exact in ["<TextureQuality value=\"2\" />", "<AnisotropicFiltering value=\"16\" />", "<ShaderQuality value=\"1\" />", "<MSAA value=\"0\" />", "<ScreenWidth value=\"2560\" />"] { assert!(xml.contains(exact)); }
        let marker = std::fs::read(fixture.marker()).unwrap();
        fixture.preset(2560, 1440, 60, "ultra");
        assert!(prepare_native_graphics(&fixture.0).is_err());
        assert_eq!(std::fs::read_to_string(fixture.settings()).unwrap(), xml);
        assert_eq!(std::fs::read(fixture.marker()).unwrap(), marker);
        fixture.preset(2560, 1440, 60, "performance");
        let malformed = "<Settings><graphics><TextureQuality value=\"1\"/>";
        std::fs::write(fixture.settings(), malformed).unwrap();
        assert!(prepare_native_graphics(&fixture.0).is_err());
        assert_eq!(std::fs::read_to_string(fixture.settings()).unwrap(), malformed);
        assert_eq!(std::fs::read(fixture.marker()).unwrap(), marker);
    }

    #[test]
    fn native_low_xml_preserves_unrelated_sections_attributes_and_comments() {
        let xml = r#"<?xml version="1.0"?><Settings><version value="27"/><configSource>SMC_AUTO</configSource>
<graphics><!-- <TextureQuality value="2" /> --><TextureQuality custom="keep" value='2' /><ShaderQuality value="0" />
<CustomQuality value="7" /><custom><TextureQuality value="2" /></custom></graphics>
<system><maxFileStoreSize value="65536" /></system><audio><Audio3d value="true" /></audio>
<video><ScreenWidth value="1280"/><ScreenHeight value="720"/><RefreshRate value="60"/><VSync value="2"/>
<AdapterIndex value="9" /></video><VideoCardDescription>KEEP</VideoCardDescription></Settings>"#;
        let config = NativeGraphics { width: 640, height: 360, fps_limit: 30, ..NativeGraphics::default() };
        let (new, tags) = low_settings_xml(xml, &config).unwrap();
        for exact in ["<!-- <TextureQuality value=\"2\" /> -->", "<CustomQuality value=\"7\" />",
            "<custom><TextureQuality value=\"2\" /></custom>", "<system><maxFileStoreSize value=\"65536\" /></system>",
            "<audio><Audio3d value=\"true\" /></audio>", "<AdapterIndex value=\"9\" />", "<VideoCardDescription>KEEP</VideoCardDescription>"] {
            assert!(new.contains(exact), "changed unrelated XML {exact}");
        }
        assert!(new.contains("<TextureQuality custom=\"keep\" value='0' />"));
        assert!(new.contains("<ShaderQuality value=\"1\" />"));
        assert!(new.contains("<ScreenWidth value=\"640\"/>"));
        assert!(new.contains("<ScreenHeight value=\"360\"/>"));
        assert!(new.contains("<RefreshRate value=\"30\"/>"));
        assert!(tags.contains(&"TextureQuality".into()));
        let (again, changed) = low_settings_xml(&new, &config).unwrap();
        assert_eq!(again, new);
        assert!(changed.is_empty());
    }

    #[test]
    fn native_graphics_first_launch_creates_low_file_and_preserves_saves() {
        let fixture = Fixture::new();
        let save = fixture.0.join("SGTA50000");
        std::fs::write(&save, [0, 255, 22, 31]).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.config_source, "built_in_default");
        assert_eq!(launch.settings_action, "created_low_defaults");
        assert_eq!(launch.marker_action, "created");
        assert!(launch.backup_path.is_none());
        let xml = std::fs::read_to_string(fixture.settings()).unwrap();
        assert!(xml.contains("<version value=\"27\" />"));
        assert!(xml.contains("<TextureQuality value=\"0\" />"));
        assert!(xml.contains("<ShaderQuality value=\"1\" />"));
        assert!(xml.contains("<ScreenWidth value=\"960\" />"));
        assert_eq!(std::fs::read(save).unwrap(), [0, 255, 22, 31]);
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
    }

    #[test]
    fn native_graphics_backs_up_existing_xml_before_dimension_change() {
        let fixture = Fixture::new();
        fixture.config(1280, 720, 30);
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, NATIVE_DEFAULT_SETTINGS).unwrap();
        write_native_graphics_marker(&fixture.marker(), &NativeGraphics::default()).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.config_source, "file");
        assert_eq!(launch.settings_action, "updated_with_backup");
        assert_eq!(launch.changed_tags, vec!["RefreshRate".to_string(), "ScreenHeight".to_string(), "ScreenWidth".to_string()]);
        assert_eq!(launch.marker_action, "updated");
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), NATIVE_DEFAULT_SETTINGS);
        let xml = std::fs::read_to_string(path).unwrap();
        assert!(xml.contains("<RefreshRate value=\"30\" />"));
        assert!(!xml.contains("<TextureQuality"));
        assert!(!xml.contains("<ShadowQuality"));
    }

    #[test]
    fn native_4k_settings_reach_game_xml_without_changing_save_or_default() {
        let fixture = Fixture::new();
        fixture.config(3840, 2160, 60);
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, NATIVE_DEFAULT_SETTINGS).unwrap();
        write_native_graphics_marker(&fixture.marker(), &NativeGraphics::default()).unwrap();
        let save = fixture.0.join("SGTA50000");
        std::fs::write(&save, [9, 0, 255, 32]).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.config, NativeGraphics { width: 3840, height: 2160, fps_limit: 60, ..NativeGraphics::default() });
        let xml = std::fs::read_to_string(&path).unwrap();
        assert!(xml.contains("<ScreenWidth value=\"3840\" />"));
        assert!(xml.contains("<ScreenHeight value=\"2160\" />"));
        assert!(xml.contains("<RefreshRate value=\"60\" />"));
        assert_eq!(std::fs::read(save).unwrap(), [9, 0, 255, 32]);
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), NATIVE_DEFAULT_SETTINGS);
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
        assert_eq!(NativeGraphics::default(), NativeGraphics { width: 960, height: 540, fps_limit: 60, preset: NativeGraphicsPreset::Existing });
    }

    #[test]
    fn native_existing_xml_without_marker_is_adopted_byte_for_byte() {
        let fixture = Fixture::new();
        fixture.config(1920, 1080, 60);
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let xml = NATIVE_DEFAULT_SETTINGS.replace("<Shadow_Distance value=\"1.000000\" />",
            "<Shadow_Distance value=\"1.000000\" /><TextureQuality value=\"1\" /><ShadowQuality value=\"2\" /><Shader_SSA value=\"true\" />");
        std::fs::write(&path, &xml).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.settings_action, "unchanged");
        assert_eq!(launch.marker_action, "adopted_existing");
        assert!(launch.backup_path.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), xml);
        let again = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(again.settings_action, "unchanged");
        assert_eq!(again.marker_action, "unchanged");
        assert_eq!(std::fs::read_to_string(path).unwrap(), xml);
    }

    #[test]
    fn native_low_defaults_do_not_overwrite_game_normalization_on_restart() {
        let fixture = Fixture::new();
        let first = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(first.settings_action, "created_low_defaults");
        let path = fixture.settings();
        let normalized = std::fs::read_to_string(&path).unwrap()
            .replace("<TextureQuality value=\"0\" />", "<TextureQuality value=\"1\" />")
            .replace("<ShadowQuality value=\"0\" />", "<ShadowQuality value=\"2\" />")
            .replace("<Shader_SSA value=\"false\" />", "<Shader_SSA value=\"true\" />");
        std::fs::write(&path, &normalized).unwrap();
        let next = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(next.settings_action, "unchanged");
        assert!(next.changed_tags.is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), normalized);
    }

    #[test]
    fn native_msaa_compatibility_preserves_other_choices_and_exact_backup() {
        let fixture = Fixture::new();
        prepare_native_graphics(&fixture.0).unwrap();
        let path = fixture.settings();
        let changed = std::fs::read_to_string(&path).unwrap()
            .replace("<MSAA value=\"0\" />", "<MSAA value=\"2\" />")
            .replace("<TextureQuality value=\"0\" />", "<TextureQuality value=\"1\" />")
            .replace("<FXAA_Enabled value=\"false\" />", "<FXAA_Enabled value=\"true\" />");
        std::fs::write(&path, &changed).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.changed_tags, ["MSAA"]);
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), changed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(),
            changed.replace("<MSAA value=\"2\" />", "<MSAA value=\"0\" />"));
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
    }

    #[test]
    fn native_graphics_errors_do_not_overwrite_existing_xml() {
        let fixture = Fixture::new();
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, NATIVE_DEFAULT_SETTINGS).unwrap();
        std::fs::write(fixture.0.join("native-graphics.json"), "bad JSON").unwrap();
        assert!(prepare_native_graphics(&fixture.0).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), NATIVE_DEFAULT_SETTINGS);
        fixture.config(960, 540, 60);
        std::fs::write(&path, "<Settings><graphics>").unwrap();
        assert!(prepare_native_graphics(&fixture.0).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<Settings><graphics>");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir_all(fixture.0.join("userdata")).unwrap();
        std::fs::write(fixture.0.join("userdata"), "directory blocked").unwrap();
        assert!(prepare_native_graphics(&fixture.0).is_err());
    }

    #[test]
    fn launcher_session_absent_files_keep_previous_launch_exactly() {
        let fixture = Fixture::new();
        let session = NativeSession::load(&fixture.0).unwrap();
        assert_eq!(session, NativeSession::default());
        assert!(session.environment().is_empty(), "solo launch must not touch diagnostic environment");
        assert_eq!(native_texture_quality(&fixture.0).unwrap(), None);
        // The current Vision Pro files: volume and look stay; no mute or session keys yet.
        std::fs::write(fixture.0.join("native-audio.json"), r#"{"master_volume":1}"#).unwrap();
        std::fs::write(fixture.0.join("native-multiplayer.json"), r#"{"multiplayer":false,"host":false,"server":"not an address"}"#).unwrap();
        assert_eq!(NativeSession::load(&fixture.0).unwrap(), NativeSession::default());
    }

    #[test]
    fn mute_is_separate_from_the_saved_master_volume() {
        let fixture = Fixture::new();
        let text = r#"{"master_volume":0.8,"mute":true,"other":1}"#;
        std::fs::write(fixture.0.join("native-audio.json"), text).unwrap();
        let session = NativeSession::load(&fixture.0).unwrap();
        assert!(session.mute);
        assert_eq!(session.environment(), [("MUGUET_MUTE", Some("1".to_string()))]);
        assert_eq!(std::fs::read_to_string(fixture.0.join("native-audio.json")).unwrap(), text);
        assert_eq!(crate::audio::resolve_master_volume(None, Some(text)), 0.8, "unmuting restores the chosen volume");
        for bad in [r#"{"mute":1}"#, r#"{"mute":"yes"}"#, "[]"] {
            std::fs::write(fixture.0.join("native-audio.json"), bad).unwrap();
            assert!(NativeSession::load(&fixture.0).is_err(), "{bad}");
        }
    }

    #[test]
    fn multiplayer_join_and_host_select_muguet_transport_environment() {
        let fixture = Fixture::new();
        let path = fixture.0.join("native-multiplayer.json");
        std::fs::write(&path, r#"{"multiplayer":true,"host":false,"server":" 192.168.1.20 ","player_name":" Zach "}"#).unwrap();
        let join = NativeSession::load(&fixture.0).unwrap();
        assert_eq!(join.environment(), [("MUGUET_HOST", None), ("MUGUET_SERVER", Some("192.168.1.20:47520".into())),
            ("MUGUET_NAME", Some("Zach".into()))]);
        std::fs::write(&path, r#"{"multiplayer":true,"host":true,"server":"stale text","player_name":""}"#).unwrap();
        let host = NativeSession::load(&fixture.0).unwrap();
        assert_eq!(host.environment(), [("MUGUET_HOST", Some("1".into())), ("MUGUET_SERVER", None),
            ("MUGUET_NAME", Some(NATIVE_DEFAULT_PLAYER.into()))]);
        assert_eq!(host.status_json()["multiplayer"]["role"], "host");
        for bad in [r#"{"multiplayer":true,"host":false,"server":""}"#, r#"{"multiplayer":true,"server":"10.0.0.300"}"#,
            r#"{"multiplayer":"yes"}"#, r#"{"multiplayer":true,"host":true,"player_name":"abcdefghijklmnopqrstuvwxyz"}"#,
            r#"{"multiplayer":true,"host":1}"#] {
            std::fs::write(&path, bad).unwrap();
            assert!(NativeSession::load(&fixture.0).is_err(), "{bad}");
        }
    }

    #[test]
    fn multiplayer_addresses_are_ipv4_or_names_with_valid_ports() {
        for (raw, expected) in [("192.168.1.20", "192.168.1.20:47520"), ("10.0.0.5:5000", "10.0.0.5:5000"),
            ("zachs-mac.local", "zachs-mac.local:47520"), ("host:65535", "host:65535")] {
            assert_eq!(multiplayer_address(raw).unwrap(), expected);
        }
        for raw in ["", "  ", "1.2.3", "300.1.1.1", "a b", "host:0", "host:70000", "host:", "fe80::1", "[::1]:47520",
            "-bad.local", "under_score", "http://host"] {
            assert!(multiplayer_address(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn launcher_texture_choice_follows_any_preset_and_null_keeps_gta_value() {
        let fixture = Fixture::new();
        let path = fixture.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = current_quality_xml();
        std::fs::write(&path, &original).unwrap();
        // Clarity's one-time TextureQuality 2 is still followed by the explicit launcher choice.
        fixture.preset(2560, 1440, 60, "clarity");
        std::fs::write(fixture.0.join("native-textures.json"), r#"{"texture_quality":0}"#).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.texture_override, Some(0));
        assert_eq!(launch.actual_graphics_values["TextureQuality"], "0");
        assert_eq!(launch.actual_graphics_values["AnisotropicFiltering"], "16", "preset still applies its other choices");
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), original);
        // Unchanged preset, new texture choice: only TextureQuality moves.
        std::fs::write(fixture.0.join("native-textures.json"), r#"{"texture_quality":2}"#).unwrap();
        let launch = prepare_native_graphics(&fixture.0).unwrap();
        assert_eq!(launch.changed_tags, ["TextureQuality"]);
        let current = std::fs::read_to_string(&path).unwrap();
        // Null and absent keep whatever GTA now has.
        std::fs::write(fixture.0.join("native-textures.json"), r#"{"texture_quality":null}"#).unwrap();
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), current);
        for bad in [r#"{"texture_quality":3}"#, r#"{"texture_quality":"high"}"#, r#"{"texture_quality":-1}"#, "nope"] {
            std::fs::write(fixture.0.join("native-textures.json"), bad).unwrap();
            assert!(prepare_native_graphics(&fixture.0).is_err(), "{bad}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), current, "invalid choice must not write");
        }
    }

    #[test]
    fn setup_native_applies_launcher_choices_before_engine_start() {
        let fixture = Fixture::new();
        let game = fixture.0.join("game");
        std::fs::create_dir_all(game.join("data")).unwrap();
        std::fs::write(game.join("data/manifest.json"), "{}").unwrap();
        std::fs::create_dir_all(game.join("b/test/shaders")).unwrap();
        std::fs::write(game.join("b/test/shaders/index.json"), "{}").unwrap();
        let saves = fixture.0.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("native-gameplay.json"), r#"{"developer_mode":false}"#).unwrap();
        std::fs::write(saves.join("native-graphics.json"), r#"{"width":2560,"height":1440,"fps_limit":60,"graphics_preset":"clarity"}"#).unwrap();
        std::fs::write(saves.join("native-multiplayer.json"), r#"{"multiplayer":true,"host":false,"server":"192.168.4.7","player_name":"Vision"}"#).unwrap();
        // A rejected choice fails before settings.xml exists and before any environment is chosen.
        std::fs::write(saves.join("native-textures.json"), r#"{"texture_quality":9}"#).unwrap();
        assert!(setup_native(&game, &saves).is_err());
        assert!(!saves.join("userdata").exists());
        std::fs::write(saves.join("native-textures.json"), r#"{"texture_quality":1}"#).unwrap();
        let startup = setup_native(&game, &saves).unwrap();
        assert_eq!(startup.engine_args, ["-frameLimit=1", "-wasmstartchoice=1"]);
        assert_eq!(std::env::var("MUGUET_SERVER").unwrap(), "192.168.4.7:47520");
        assert_eq!(std::env::var("MUGUET_NAME").unwrap(), "Vision");
        assert!(std::env::var_os("MUGUET_HOST").is_none());
        assert_eq!(native_session_status().unwrap()["multiplayer"]["role"], "join");
        let xml = std::fs::read_to_string(saves.join("userdata/Documents/Rockstar Games/GTA V/settings.xml")).unwrap();
        assert!(xml.contains(r#"<TextureQuality value="1" />"#));
        for key in ["MUGUET_SERVER", "MUGUET_NAME", "MUGUET_FSROOT", "MUGUET_DATA", "MUGUET_SHADERS"] { std::env::remove_var(key); }
    }
}
