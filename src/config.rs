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
    let native_graphics = prepare_native_graphics(save_root)?;
    let start_mode = native_start_mode(save_root)?;
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
}

impl Default for NativeGraphics {
    fn default() -> Self { Self { width: 960, height: 540, fps_limit: 60 } }
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
        let config = Self { width: integer("width")?, height: integer("height")?, fps_limit: integer("fps_limit")? };
        ensure!(matches!((config.width, config.height),
            (640, 360) | (960, 540) | (1280, 720) | (1920, 1080) | (2560, 1440) | (3840, 2160)),
            "native resolution must be 640x360, 960x540, 1280x720, 1920x1080, 2560x1440 or 3840x2160");
        ensure!(matches!(config.fps_limit, 30 | 60), "native fps_limit must be 30 or 60");
        ensure!(object["graphics_preset"].as_str() == Some("low"), "native graphics_preset must be low");
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
}

impl NativeGraphicsLaunch {
    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!({"width":self.config.width, "height":self.config.height,
            "fps_limit":self.config.fps_limit,"graphics_preset":"low", "config_source":self.config_source,
            "config_path":self.config_path,"settings_path":self.settings_path,
            "settings_action":self.settings_action,"changed_tags":self.changed_tags,"backup_path":self.backup_path,
            "marker_path":self.marker_path,"marker_action":self.marker_action,
            "settings_scope":"Low defaults on missing XML; existing GTA choices preserved except MSAA, which is disabled for the current immersive depth path; launcher resolution and FPS changes apply separately"})
    }
}

/// Story is the normal launch. Developer mode deliberately opts into the
/// original guest's Sandbox path; this does not alter the compiled game AOT.
fn native_start_mode(save_root: &Path) -> anyhow::Result<u32> {
    use anyhow::{ensure, Context};
    use std::io::Read;
    let path = save_root.join("native-gameplay.json");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(1),
        Err(error) => return Err(error).context("open native-gameplay.json"),
    };
    let mut text = String::new();
    file.take(65_537).read_to_string(&mut text).context("read native-gameplay.json")?;
    ensure!(text.len() <= 65_536, "native-gameplay.json is too large");
    let value: serde_json::Value = serde_json::from_str(&text).context("parse native-gameplay.json")?;
    let object = value.as_object().ok_or_else(|| anyhow::anyhow!("native gameplay config must be an object"))?;
    let developer = match object.get("developer_mode") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| anyhow::anyhow!("developer_mode must be a boolean"))?,
    };
    Ok(if developer { 2 } else { 1 })
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
                if let Some(value) = wanted.get(&(stack[1], name)) {
                    ensure!(self_closing && seen.insert((stack[1], name)), "duplicate or non-value graphics tag {name}");
                    let captures = value_attribute.captures(&xml[start..end + 1])
                        .with_context(|| format!("settings tag {name} has no value attribute"))?;
                    let field = captures.get(1).or_else(|| captures.get(2)).unwrap();
                    if field.as_str() != value {
                        edits.push((start + field.start(), start + field.end(), value.clone()));
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
            eprintln!("[config] {} missing; using native low 960x540 at 60 fps", config_path.display());
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
    let (xml, mut changed_tags) = if !existed {
        low_settings_xml(&original, &config).context("create native low graphics defaults")?
    } else if applied_config.as_ref().is_some_and(|previous| previous != &config) {
        video_settings_xml(&original, &config).context("apply changed native resolution and frame cap")?
    } else {
        // A missing marker means this may be an upgrade with GTA-owned XML.
        // Validate its structure, then adopt it without rewriting user values.
        settings_xml_with_values(&original, std::collections::BTreeMap::new())
            .context("validate existing GTA settings")?
    };
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
        if existed { "updated_with_backup" } else { "created_low_defaults" }
    };
    let marker_action = if applied_config.as_ref() == Some(&config) && existed { "unchanged" } else {
        write_native_graphics_marker(&marker_path, &config)?;
        if applied_config.is_some() { "updated" } else if existed { "adopted_existing" } else { "created" }
    };
    let launch = NativeGraphicsLaunch { config, config_path, config_source, settings_path, settings_action,
        changed_tags, backup_path, marker_path, marker_action };
    eprintln!("[config] native graphics {}", launch.status_json());
    Ok(launch)
}

fn write_native_graphics_marker(path: &Path, config: &NativeGraphics) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;
    let data = serde_json::to_vec(&serde_json::json!({"width": config.width, "height": config.height,
        "fps_limit": config.fps_limit, "graphics_preset": "low"}))?;
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
            std::fs::write(self.0.join("native-graphics.json"), format!(r#"{{"width":{width},"height":{height},"fps_limit":{fps},"graphics_preset":"low"}}"#)).unwrap();
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
                assert_eq!(NativeGraphics::parse(&value).unwrap(), NativeGraphics { width, height, fps_limit: fps });
            }
        }
        assert_eq!(NativeGraphics::default(), NativeGraphics { width: 960, height: 540, fps_limit: 60 });
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

    #[test]
    fn native_low_xml_preserves_unrelated_sections_attributes_and_comments() {
        let xml = r#"<?xml version="1.0"?><Settings><version value="27"/><configSource>SMC_AUTO</configSource>
<graphics><!-- <TextureQuality value="2" /> --><TextureQuality custom="keep" value='2' /><ShaderQuality value="0" />
<CustomQuality value="7" /><custom><TextureQuality value="2" /></custom></graphics>
<system><maxFileStoreSize value="65536" /></system><audio><Audio3d value="true" /></audio>
<video><ScreenWidth value="1280"/><ScreenHeight value="720"/><RefreshRate value="60"/><VSync value="2"/>
<AdapterIndex value="9" /></video><VideoCardDescription>KEEP</VideoCardDescription></Settings>"#;
        let config = NativeGraphics { width: 640, height: 360, fps_limit: 30 };
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
        assert_eq!(launch.config, NativeGraphics { width: 3840, height: 2160, fps_limit: 60 });
        let xml = std::fs::read_to_string(&path).unwrap();
        assert!(xml.contains("<ScreenWidth value=\"3840\" />"));
        assert!(xml.contains("<ScreenHeight value=\"2160\" />"));
        assert!(xml.contains("<RefreshRate value=\"60\" />"));
        assert_eq!(std::fs::read(save).unwrap(), [9, 0, 255, 32]);
        assert_eq!(std::fs::read_to_string(launch.backup_path.unwrap()).unwrap(), NATIVE_DEFAULT_SETTINGS);
        assert_eq!(prepare_native_graphics(&fixture.0).unwrap().settings_action, "unchanged");
        assert_eq!(NativeGraphics::default(), NativeGraphics { width: 960, height: 540, fps_limit: 60 });
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
}
