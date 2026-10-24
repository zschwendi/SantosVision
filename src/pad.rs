// muguet - pad.rs
// Constantin Clerc (c22dev) - 0.1

use crate::input;
use objc2::runtime::NSObjectProtocol;
use objc2_game_controller::{GCController, GCControllerButtonInput, GCExtendedGamepad};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[path = "pad_guest_context.rs"]
mod guest_context;
#[path = "pad_spatial.rs"]
mod spatial;
use spatial::Sample;

pub static ACTIVE: AtomicBool = AtomicBool::new(false);
static POLL_SAMPLES: AtomicU64 = AtomicU64::new(0);
static CONNECTED_SAMPLES: AtomicU64 = AtomicU64::new(0);
static ACTIVE_INPUT_SAMPLES: AtomicU64 = AtomicU64::new(0);
static KEY_EVENTS: AtomicU64 = AtomicU64::new(0);
static BUTTON_EVENTS: AtomicU64 = AtomicU64::new(0);
static WHEEL_EVENTS: AtomicU64 = AtomicU64::new(0);
static LOOK_EVENTS: AtomicU64 = AtomicU64::new(0);
static MENU_PRESS_COUNT: AtomicU64 = AtomicU64::new(0);
/// Lightweight native UI poll; avoids serializing the full runtime report just
/// to notice Start. The same cumulative counter remains in status_json.
#[no_mangle]
pub extern "C" fn sv_native_menu_press_count_v1() -> u64 {
    MENU_PRESS_COUNT.load(Ordering::Relaxed)
}

// 0 absent, 1 established extended gamepad, 2 spatial controls. Hand bits are
// available validated controls, not Bluetooth pairing or tracked pose claims.
static INPUT_PROFILE: AtomicU64 = AtomicU64::new(0);
static SPATIAL_HANDS: AtomicU64 = AtomicU64::new(0);
static LOOK_SENSITIVITY: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
// Exact supported builds follow the guest pause state; Escape parity is only a
// compatibility fallback for other executables.
static MENU_NAVIGATION: AtomicBool = AtomicBool::new(false);
static GUEST_CONTEXT_AVAILABLE: AtomicBool = AtomicBool::new(false);
static PHONE_INSTANTIATED: AtomicBool = AtomicBool::new(false);

/// Share the exact original-game certificate with scoped guest-native helpers.
pub(crate) fn exact_guest_abi_verified() -> bool {
    guest_context::abi_verified() == Some(true)
}

pub(crate) fn verified_guest_gameplay_context() -> bool {
    guest_context::read().is_some_and(|context| !context.paused)
}
pub(crate) fn spatial_controls_selected()->bool {INPUT_PROFILE.load(Ordering::Relaxed)==2}

/// Cumulative sampling/mapping counters, independent of engine block readiness.
/// Compare with input::status_json delivery counters to locate a broken path.
pub(crate) fn status_json() -> serde_json::Value {
    serde_json::json!({
        "connected": ACTIVE.load(Ordering::Relaxed),
        "poll_samples": POLL_SAMPLES.load(Ordering::Relaxed),
        "connected_samples": CONNECTED_SAMPLES.load(Ordering::Relaxed),
        "active_input_samples": ACTIVE_INPUT_SAMPLES.load(Ordering::Relaxed),
        "mapped_key_events": KEY_EVENTS.load(Ordering::Relaxed),
        "mapped_button_events": BUTTON_EVENTS.load(Ordering::Relaxed),
        "mapped_look_events": LOOK_EVENTS.load(Ordering::Relaxed),
        "mapped_wheel_events": WHEEL_EVENTS.load(Ordering::Relaxed),
        "menu_press_count": MENU_PRESS_COUNT.load(Ordering::Relaxed),
        "input_profile": match INPUT_PROFILE.load(Ordering::Relaxed) { 1 => "extended_gamepad", 2 => "spatial_gamepad_buttons_v1", _ => "none" },
        "spatial_left_controls_available": SPATIAL_HANDS.load(Ordering::Relaxed) & 1 != 0,
        "spatial_right_controls_available": SPATIAL_HANDS.load(Ordering::Relaxed) & 2 != 0,
        "motion_aim_diagnostic_enabled": crate::xr_motion_aim::diagnostic_enabled(),
        "motion_aim_gameplay_enabled": crate::xr_motion_aim::gameplay_enabled(),
        "menu_navigation_inferred": MENU_NAVIGATION.load(Ordering::Relaxed),
        "menu_navigation": MENU_NAVIGATION.load(Ordering::Relaxed),
        "menu_context_source": if GUEST_CONTEXT_AVAILABLE.load(Ordering::Relaxed) { "verified_guest_pause_state" } else { "escape_fallback" },
        "guest_context_abi_verified": guest_context::abi_verified(),
        "phone_instantiated": if GUEST_CONTEXT_AVAILABLE.load(Ordering::Relaxed) { Some(PHONE_INSTANTIATED.load(Ordering::Relaxed)) } else { None },
        "look_sensitivity_percent": LOOK_SENSITIVITY.get().copied(),
        "look_speed_units_per_second": LOOK_SENSITIVITY.get().map(|percent| LOOK_SPEED * percent / 100.0),
    })
}

fn emit_key(key: &str, pressed: bool) {
    KEY_EVENTS.fetch_add(1, Ordering::Relaxed);
    let text = if pressed { match key { "KeyY" => Some("y"), "KeyN" => Some("n"), _ => None } } else { None };
    input::key(key, pressed, false, text, false);
}

fn emit_button(button: usize, pressed: bool) {
    BUTTON_EVENTS.fetch_add(1, Ordering::Relaxed);
    input::button(button, pressed);
}

pub static IN_VEHICLE: AtomicBool = AtomicBool::new(false);

pub static HORN_PAD: AtomicBool = AtomicBool::new(false);
pub static HORN_TOUCH: AtomicBool = AtomicBool::new(false);
pub fn horn_held() -> bool {
    HORN_PAD.load(Ordering::Relaxed) || HORN_TOUCH.load(Ordering::Relaxed)
}

struct State {
    held: HashSet<&'static str>,
    buttons: u8,
    last: Instant,
    down_since: Option<Instant>,
    menu_navigation: bool,
    menu_button_down: bool,
    blocked_context_buttons: u8,
    spatial_router: spatial::Router,
    native_controls_require_neutral: bool,
    utility_source: Option<Option<spatial::SpatialIdentity>>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq)]
enum AppAction { Key(&'static str), Gameplay(&'static str), Resume, Wheel(i32) }
struct MenuActions {
    queued: VecDeque<AppAction>,
    active: Option<(&'static str, Instant)>,
    gap_until: Option<Instant>,
    waiting_for_gameplay: Option<(&'static str, Instant)>,
    wheel_pending: i32,
}
static MENU_ACTIONS: Mutex<MenuActions> = Mutex::new(MenuActions {
    queued: VecDeque::new(), active: None, gap_until: None, waiting_for_gameplay: None, wheel_pending: 0,
});
const MENU_ACTION_HOLD: Duration = Duration::from_millis(120);
const MENU_ACTION_GAP: Duration = Duration::from_millis(32);
const MENU_ACTION_QUEUE_CAPACITY: usize = 8;
const GAMEPLAY_ACTION_TIMEOUT: Duration = Duration::from_secs(2);

/// Native actions merge into the actual input mapper, including while native UI
/// owns physical input. Gameplay shortcuts wait for verified unpause, rather
/// than applying a camera/phone key to GTA's paused settings screen.
pub fn queue_menu_action(action: u32) -> bool {
    let action = match action {
        1 => AppAction::Key("Escape"), 2 => AppAction::Key("Enter"),
        3 => AppAction::Key("Space"), 4 => AppAction::Key("KeyY"), 5 => AppAction::Key("KeyN"),
        6 => AppAction::Gameplay("KeyV"), 7 => AppAction::Gameplay("ArrowUp"),
        8 => AppAction::Key("ArrowUp"), 9 => AppAction::Key("ArrowDown"),
        10 => AppAction::Key("ArrowLeft"), 11 => AppAction::Key("ArrowRight"),
        12 => AppAction::Key("Backspace"), 13 => AppAction::Resume,
        14 => AppAction::Key("Tab"),
        15 => AppAction::Wheel(1), 16 => AppAction::Wheel(-1),
        _ => return false,
    };
    let mut actions = MENU_ACTIONS.lock().unwrap();
    if actions.queued.len() >= MENU_ACTION_QUEUE_CAPACITY { return false; }
    actions.queued.push_back(action);
    true
}

fn menu_action_key(actions: &mut MenuActions, now: Instant, paused: Option<bool>) -> Option<&'static str> {
    if actions.waiting_for_gameplay.is_some_and(|(_, deadline)| now >= deadline) {
        actions.waiting_for_gameplay = None;
    }
    if let Some((_, until)) = actions.active {
        if now >= until {
            actions.active = None;
            actions.gap_until = Some(now + MENU_ACTION_GAP);
        }
    }
    if actions.active.is_some() { return actions.active.map(|(key, _)| key); }
    if actions.gap_until.is_some_and(|until| now < until) { return None; }
    actions.gap_until = None;
    if let Some((key, deadline)) = actions.waiting_for_gameplay {
        if now >= deadline { actions.waiting_for_gameplay = None; }
        else if paused == Some(false) {
            actions.waiting_for_gameplay = None;
            actions.active = Some((key, now + MENU_ACTION_HOLD));
            return Some(key);
        } else { return None; }
    }
    if let Some(action) = actions.queued.pop_front() {
        let key = match action {
            AppAction::Key(key) => Some(key),
            AppAction::Resume => (paused == Some(true)).then_some("Escape"),
            AppAction::Wheel(notches) => { actions.wheel_pending += notches; None },
            AppAction::Gameplay(key) => match paused {
                Some(false) => Some(key),
                Some(true) => {
                    actions.waiting_for_gameplay = Some((key, now + GAMEPLAY_ACTION_TIMEOUT));
                    Some("Escape")
                }
                None => None, // Unknown guest state must not invoke an accidental menu command.
            },
        };
        if let Some(key) = key { actions.active = Some((key, now + MENU_ACTION_HOLD)); }
    }
    actions.active.map(|(key, _)| key)
}

#[cfg(all(target_os = "visionos", not(test)))]
fn native_controls_own_input() -> bool {
    extern "C" { fn sv_native_controls_owns_input_v1() -> u32; }
    unsafe { sv_native_controls_owns_input_v1() != 0 }
}
#[cfg(test)]
static TEST_NATIVE_CONTROLS_OWN_INPUT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
fn native_controls_own_input() -> bool { TEST_NATIVE_CONTROLS_OWN_INPUT.load(Ordering::Relaxed) }
#[cfg(all(not(target_os = "visionos"), not(test)))]
fn native_controls_own_input() -> bool { false }

fn physical_sample_for_owner(sample: Option<Sample>, owns: bool, require_neutral: &mut bool) -> Option<Sample> {
    if owns { *require_neutral = true; return None; }
    if *require_neutral {
        if sample.is_some_and(|sample| sample.neutral()) { *require_neutral = false; }
        return None;
    }
    sample
}

const UTILITY_DPAD_WATCHDOG: Duration = Duration::from_millis(250);
static UTILITY_DPAD: Mutex<[Option<Instant>; 4]> = Mutex::new([None; 4]);

/// Directional UI holds are refreshed by the UI's 50ms heartbeat. They share
/// the real D-pad mapping below, including Down's held character-wheel action.
#[no_mangle]
pub extern "C" fn sv_native_utility_dpad_v1(action: u32, pressed: u32) -> i32 {
    if !(8..=11).contains(&action) || pressed > 1 { return -1; }
    UTILITY_DPAD.lock().unwrap()[action as usize - 8] = (pressed == 1).then(Instant::now);
    0
}

fn utility_dpad(now: Instant) -> u8 {
    let mut held = UTILITY_DPAD.lock().unwrap();
    let mut directions = 0;
    for (index, stamp) in held.iter_mut().enumerate() {
        if stamp.is_some_and(|at| now.saturating_duration_since(at) >= UTILITY_DPAD_WATCHDOG) { *stamp = None; }
        if stamp.is_some() { directions |= 1 << index; }
    }
    directions
}
fn clear_utility_dpad() { *UTILITY_DPAD.lock().unwrap() = [None; 4]; }

const SWITCH_HOLD: Duration = Duration::from_millis(300);
const STICK_DEADZONE: f32 = 0.3;
const LOOK_DEADZONE: f32 = 0.12;

const LOOK_SPEED: f64 = 700.0;
const DEFAULT_LOOK_SENSITIVITY_PERCENT: f64 = 75.0;
const CROSS: u8 = 1;
const CIRCLE: u8 = 2;
const SQUARE: u8 = 4;
const TRIANGLE: u8 = 8;
const LEFT_SHOULDER: u8 = 16;
const RIGHT_SHOULDER: u8 = 32;

fn context_buttons(blocked: &mut u8, context_changed: bool, pressed: u8) -> u8 {
    *blocked &= pressed;
    if context_changed { *blocked |= pressed; }
    pressed & !*blocked
}

fn resolve_look_sensitivity_percent(env: Option<&str>, setting: Option<&str>) -> f64 {
    let valid = |v: f64| (v.is_finite() && (0.0..=100.0).contains(&v)).then_some(v);
    env.and_then(|v| v.trim().parse::<f64>().ok()).and_then(valid)
        .or_else(|| setting.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|v| v.get("look_sensitivity_percent").and_then(|v| v.as_f64())).and_then(valid))
        .unwrap_or(DEFAULT_LOOK_SENSITIVITY_PERCENT)
}

/// Scales the controller's existing right-stick curve before the shared mouse
/// input gain and GTA's own mouse setting. 100% is the previous 700-unit speed;
/// 75% is 525 units/sec at full deflection. This does not rewrite GTA settings.
fn look_sensitivity_percent() -> f64 {
    *LOOK_SENSITIVITY.get_or_init(|| {
        let env = std::env::var("MUGUET_CONTROLLER_LOOK_PERCENT").ok();
        let setting = std::env::var_os("MUGUET_FSROOT")
            .and_then(|root| std::fs::read_to_string(std::path::Path::new(&root).join("native-controller.json")).ok());
        resolve_look_sensitivity_percent(env.as_deref(), setting.as_deref())
    })
}

fn gamepad() -> Option<objc2::rc::Retained<GCExtendedGamepad>> {
    unsafe { GCController::controllers().iter().find_map(|c| c.extendedGamepad()) }
}

fn down(b: &GCControllerButtonInput) -> bool {
    unsafe { b.isPressed() }
}

fn touchpad_pressed(gamepad: &GCExtendedGamepad) -> bool {
    // Both Apple's DualSense and DualShock profiles expose this button.
    // Other profiles may not; avoid subclass feature/dependency changes and
    // never send an unsupported selector. DualShock's binding also permits nil.
    if !gamepad.respondsToSelector(objc2::sel!(touchpadButton)) { return false; }
    let button: Option<objc2::rc::Retained<GCControllerButtonInput>> =
        unsafe { objc2::msg_send![gamepad, touchpadButton] };
    button.is_some_and(|button| down(&button))
}

fn controller_sample() -> Option<Sample> {
    #[cfg(target_os = "visionos")]
    {
        extern "C" {
            fn sv_spatial_gamepad_read_v1(values: *mut f32, count: u32, buttons: *mut u32, left_device: *mut u64) -> u32;
        }
        let mut values = [0.0; 6];
        let mut buttons = 0;
        let mut left_device = 0;
        let hands = unsafe { sv_spatial_gamepad_read_v1(values.as_mut_ptr(), 6, &mut buttons, &mut left_device) };
        SPATIAL_HANDS.store((hands & 3) as u64, Ordering::Relaxed);
        if let Some(sample) = spatial::spatial_sample(hands, buttons, values, left_device) {
            INPUT_PROFILE.store(2, Ordering::Relaxed);
            return Some(sample);
        }
    }
    let Some(gp) = gamepad() else {
        INPUT_PROFILE.store(0, Ordering::Relaxed);
        return None;
    };
    INPUT_PROFILE.store(1, Ordering::Relaxed);
    // Preserve the established single extended-gamepad selection and all of
    // its existing button meanings on iOS, desktop and visionOS.
    unsafe {
        let mut face_and_shoulders = 0;
        for (bit, button) in [(CROSS, gp.buttonA()), (CIRCLE, gp.buttonB()), (SQUARE, gp.buttonX()),
            (TRIANGLE, gp.buttonY()), (LEFT_SHOULDER, gp.leftShoulder()), (RIGHT_SHOULDER, gp.rightShoulder())] {
            if down(&button) { face_and_shoulders |= bit; }
        }
        let left = gp.leftThumbstick();
        let right = gp.rightThumbstick();
        let dpad = gp.dpad();
        Some(Sample {
            left_stick: [left.xAxis().value(), left.yAxis().value()],
            right_stick: [right.xAxis().value(), right.yAxis().value()],
            left_trigger: gp.leftTrigger().value(), right_trigger: gp.rightTrigger().value(),
            face_and_shoulders,
            left_click: gp.leftThumbstickButton().is_some_and(|b| down(&b)),
            right_click: gp.rightThumbstickButton().is_some_and(|b| down(&b)),
            menu: down(&gp.buttonMenu()),
            camera: gp.buttonOptions().is_some_and(|b| down(&b)) || touchpad_pressed(&gp),
            dpad: u8::from(down(&dpad.up())) | (u8::from(down(&dpad.down())) << 1)
                | (u8::from(down(&dpad.left())) << 2) | (u8::from(down(&dpad.right())) << 3),
            spatial: None,
        })
    }
}

fn face_keys(keys: &mut HashSet<&'static str>, menu: bool, in_car: bool,
             cross: bool, circle: bool, square: bool) {
    if menu {
        // Rockstar's PC front end uses Enter/Backspace and the cursor keys.
        // Shift+Enter and R+Backspace can change a highlighted setting while
        // the player is trying to accept or dismiss its confirmation dialog.
        if cross { keys.insert("Enter"); }
        if circle { keys.insert("Backspace"); }
        if square { keys.insert("Space"); }
    } else {
        // Preserve the established in-game bindings, including the native
        // start-mode Enter key before the game reaches its world.
        if cross {
            keys.insert("Enter");
            if !in_car { keys.insert("ShiftLeft"); }
        }
        if circle { keys.insert("KeyR"); keys.insert("Backspace"); }
        if square { keys.insert("Space"); }
    }
}

fn navigation_context(previous: bool, escape_released: bool,
                      guest: Option<guest_context::Context>) -> (bool, bool) {
    if let Some(context) = guest {
        // Phone Back/Escape and Circle leaving the root pause menu do not
        // reliably toggle the pause state. Follow the game's actual state.
        (context.paused, context.phone)
    } else {
        (previous ^ escape_released, false)
    }
}

pub fn poll() -> bool {
    POLL_SAMPLES.fetch_add(1, Ordering::Relaxed);
    let gp = controller_sample();
    poll_sample(gp)
}

fn poll_sample(gp: Option<Sample>) -> bool {
    let connected = gp.is_some();
    if ACTIVE.swap(connected, Ordering::Relaxed) != connected {
        eprintln!("[pad] controller {}", if connected { "connected" } else { "disconnected" });
    }
    if connected { CONNECTED_SAMPLES.fetch_add(1, Ordering::Relaxed); }
    let mut guard = STATE.lock().unwrap();
    let st = guard.get_or_insert_with(|| State { held: HashSet::new(), buttons: 0, last: Instant::now(), down_since: None,
        menu_navigation: MENU_NAVIGATION.load(Ordering::Relaxed), menu_button_down: false, blocked_context_buttons: 0,
        spatial_router: spatial::Router::default(), native_controls_require_neutral: false, utility_source: None });
    let source = gp.map(|sample| sample.spatial);
    if !connected || source != st.utility_source { clear_utility_dpad(); }
    st.utility_source = source;
    let gp = physical_sample_for_owner(gp, native_controls_own_input(), &mut st.native_controls_require_neutral);
    let gp = gp.map(|sample| st.spatial_router.apply(sample, Instant::now()));
    if gp.is_none() { st.spatial_router = spatial::Router::default(); }
    let dt = st.last.elapsed().as_secs_f64().min(0.05);
    st.last = Instant::now();

    let in_car = IN_VEHICLE.load(Ordering::Relaxed);
    let mut keys: HashSet<&'static str> = HashSet::new();
    let mut buttons = 0u8;
    let mut looking = false;
    let guest = guest_context::read();
    let (menu_key, wheel) = {
        let mut actions = MENU_ACTIONS.lock().unwrap();
        let key = menu_action_key(&mut actions, Instant::now(), guest.map(|context| context.paused));
        (key, std::mem::take(&mut actions.wheel_pending))
    };
    if wheel != 0 {
        WHEEL_EVENTS.fetch_add(1, Ordering::Relaxed);
        input::wheel(wheel);
    }
    let menu_down = gp.as_ref().is_some_and(|gp| gp.menu);
    if menu_down && !st.menu_button_down {
        MENU_PRESS_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    st.menu_button_down = menu_down;
    let escape_released = st.held.contains("Escape") && !menu_down && menu_key != Some("Escape");
    let (menu, phone_instantiated) = navigation_context(st.menu_navigation, escape_released, guest);
    let context_changed = menu != st.menu_navigation;
    st.menu_navigation = menu;
    MENU_NAVIGATION.store(menu, Ordering::Relaxed);
    GUEST_CONTEXT_AVAILABLE.store(guest.is_some(), Ordering::Relaxed);
    PHONE_INSTANTIATED.store(phone_instantiated, Ordering::Relaxed);
    if menu_down { keys.insert("Escape"); }
    if let Some(gp) = gp.as_ref() {
        let menu = st.menu_navigation;
        let pressed = gp.face_and_shoulders;
        // Closing a menu with Circle must release Backspace, not turn the same
        // still-held press into Reload. Require a fresh press after a transition.
        let allowed = context_buttons(&mut st.blocked_context_buttons, context_changed, pressed);
        let pressed = |bit| allowed & bit != 0;
        let [lx, ly] = gp.left_stick;
        if lx < -STICK_DEADZONE { keys.insert(if menu { "ArrowLeft" } else { "KeyA" }); }
        if lx > STICK_DEADZONE { keys.insert(if menu { "ArrowRight" } else { "KeyD" }); }
        if menu {
            if ly > STICK_DEADZONE { keys.insert("ArrowUp"); }
            if ly < -STICK_DEADZONE { keys.insert("ArrowDown"); }
            HORN_PAD.store(false, Ordering::Relaxed);
        } else if in_car {
            if ly > STICK_DEADZONE { keys.insert("Numpad8"); }
            if ly < -STICK_DEADZONE { keys.insert("Numpad2"); }
            if gp.right_trigger > 0.2 { keys.insert("KeyW"); }
            if gp.left_trigger > 0.2 { keys.insert("KeyS"); }
            if pressed(RIGHT_SHOULDER) { keys.insert("Space"); }
            HORN_PAD.store(gp.left_click, Ordering::Relaxed);
        } else {
            if ly > STICK_DEADZONE { keys.insert("KeyW"); }
            if ly < -STICK_DEADZONE { keys.insert("KeyS"); }
            let right_trigger=if gp.spatial.is_some() {crate::xr_motion_aim::filter_spatial_trigger(gp.right_trigger)}else{gp.right_trigger};
            if right_trigger > 0.2 { buttons |= 1; }
            if gp.left_trigger > 0.2 { buttons |= 2; }
            if pressed(RIGHT_SHOULDER) { keys.insert("KeyQ"); }
            if gp.left_click { keys.insert("ControlLeft"); }
        }
        face_keys(&mut keys, menu, in_car, pressed(CROSS), pressed(CIRCLE), pressed(SQUARE));
        if menu {
            if pressed(TRIANGLE) { keys.insert("KeyY"); }
            if pressed(RIGHT_SHOULDER) { keys.insert("KeyN"); }
            if pressed(LEFT_SHOULDER) { keys.insert("Tab"); }
        } else {
            if pressed(TRIANGLE) { keys.insert("KeyF"); }
            if pressed(LEFT_SHOULDER) { keys.insert("Tab"); }
            if gp.right_click { keys.insert("KeyC"); }
        }
        if !menu {
            if gp.camera { keys.insert("KeyV"); }
        }

        let curve = |v: f32| -> f64 {
            let a = v.abs();
            if a < LOOK_DEADZONE { 0.0 } else { (((a - LOOK_DEADZONE) / (1.0 - LOOK_DEADZONE)) as f64).powf(1.8).copysign(v as f64) }
        };
        let (dx, dy) = (curve(gp.right_stick[0]), -curve(gp.right_stick[1]));
        if !menu && (dx != 0.0 || dy != 0.0) {
            looking = true;
            LOOK_EVENTS.fetch_add(1, Ordering::Relaxed);
            let speed = LOOK_SPEED * look_sensitivity_percent() / 100.0;
            input::mouse_delta_f(dx * speed * dt, dy * speed * dt);
        }
    } else {
        st.menu_button_down = false;
        st.blocked_context_buttons = 0;
        HORN_PAD.store(false, Ordering::Relaxed);
    }
    // UI holds bypass physical modal filtering intentionally, but still use
    // the identical direction/character-wheel semantics and normal key diff.
    let directions = gp.as_ref().map_or(0, |sample| sample.dpad) | utility_dpad(Instant::now());
    if directions & 1 != 0 { keys.insert("ArrowUp"); }
    if directions & 2 != 0 {
        let since = *st.down_since.get_or_insert_with(Instant::now);
        if !menu && !phone_instantiated && since.elapsed() >= SWITCH_HOLD { keys.insert("AltLeft"); }
        else { keys.insert("ArrowDown"); }
    } else { st.down_since = None; }
    if directions & 4 != 0 { keys.insert("ArrowLeft"); }
    if directions & 8 != 0 { keys.insert("ArrowRight"); }
    if !keys.is_empty() || buttons != 0 || looking || (in_car && HORN_PAD.load(Ordering::Relaxed)) {
        ACTIVE_INPUT_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(key) = menu_key { keys.insert(key); }

    if cfg!(target_os = "ios") { input::set_locked(true); }

    for k in st.held.difference(&keys) { emit_key(k, false); }
    for k in keys.difference(&st.held) {
        crate::mp::key_pressed(k);
        emit_key(k, true);
    }
    st.held = keys;
    for (bit, dom) in [(1u8, 0usize), (2, 2)] {
        if (buttons ^ st.buttons) & bit != 0 { emit_button(dom, buttons & bit != 0); }
    }
    st.buttons = buttons;
    connected
}

fn release(st: State) {
    for k in st.held { emit_key(k, false); }
    if st.buttons & 1 != 0 { emit_button(0, false); }
    if st.buttons & 2 != 0 { emit_button(2, false); }
}

/// Release held controller input when the native application's scene deactivates.
pub fn suspend() {
    clear_utility_dpad();
    if let Some(state) = STATE.lock().unwrap().take() {
        MENU_NAVIGATION.store(state.menu_navigation, Ordering::Relaxed);
        release(state);
    }
    let mut actions = MENU_ACTIONS.lock().unwrap();
    actions.queued.clear();
    actions.active = None;
    actions.gap_until = None;
    actions.waiting_for_gameplay = None;
    actions.wheel_pending = 0;
    ACTIVE.store(false, Ordering::Relaxed);
    INPUT_PROFILE.store(0, Ordering::Relaxed);
    SPATIAL_HANDS.store(0, Ordering::Relaxed);
    HORN_PAD.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_guest_pause_state_recovers_sticks_after_back_and_phone_escape() {
        let gameplay = guest_context::Context { paused: false, phone: false };
        let phone = guest_context::Context { paused: false, phone: true };
        let paused = guest_context::Context { paused: true, phone: false };
        // Circle/Backspace can leave the root pause menu without an Escape.
        assert_eq!(navigation_context(true, false, Some(gameplay)), (false, false));
        // Escape can close a phone panel without opening the pause menu.
        assert_eq!(navigation_context(false, true, Some(phone)), (false, true));
        assert_eq!(navigation_context(true, true, Some(gameplay)), (false, false));
        // Keyboard/menu transitions and confirmations use the actual menu state.
        assert_eq!(navigation_context(false, false, Some(paused)), (true, false));
        assert_eq!(navigation_context(true, true, Some(paused)), (true, false));
        let (menu, _) = navigation_context(true, false, Some(gameplay));
        let mut keys = HashSet::new();
        face_keys(&mut keys, menu, false, true, false, false);
        assert!(keys.contains("ShiftLeft"));
        assert_eq!(navigation_context(false, true, None), (true, false));
    }

    #[test]
    fn held_circle_releases_backspace_without_reloading_after_menu_close() {
        let mut blocked = 0;
        let allowed = context_buttons(&mut blocked, false, CIRCLE);
        let mut menu_keys = HashSet::new();
        face_keys(&mut menu_keys, true, false, false, allowed & CIRCLE != 0, false);
        assert_eq!(menu_keys, HashSet::from(["Backspace"]));
        let allowed = context_buttons(&mut blocked, true, CIRCLE);
        let mut game_keys = HashSet::new();
        face_keys(&mut game_keys, false, false, false, allowed & CIRCLE != 0, false);
        assert!(game_keys.is_empty());
        assert_eq!(menu_keys.difference(&game_keys).copied().collect::<HashSet<_>>(), HashSet::from(["Backspace"]));
        assert_eq!(context_buttons(&mut blocked, false, CIRCLE), 0);
        assert_eq!(context_buttons(&mut blocked, false, 0), 0);
        let allowed = context_buttons(&mut blocked, false, CIRCLE);
        face_keys(&mut game_keys, false, false, false, allowed & CIRCLE != 0, false);
        assert_eq!(game_keys, HashSet::from(["KeyR", "Backspace"]));
        // Holding Yes/No/Apply during another context change is equally gated.
        assert_eq!(context_buttons(&mut blocked, true, TRIANGLE | RIGHT_SHOULDER | SQUARE), 0);
    }

    #[test]
    fn controller_look_percentage_respects_persistence_and_explicit_override() {
        let saved = r#"{"look_sensitivity_percent":40}"#;
        assert_eq!(resolve_look_sensitivity_percent(None, None), 75.0);
        assert_eq!(resolve_look_sensitivity_percent(None, Some(saved)), 40.0);
        assert_eq!(resolve_look_sensitivity_percent(Some("100"), Some(saved)), 100.0);
        assert_eq!(resolve_look_sensitivity_percent(Some("0"), Some(saved)), 0.0);
        for invalid in ["NaN", "inf", "-1", "101", "nonsense"] {
            assert_eq!(resolve_look_sensitivity_percent(Some(invalid), Some(saved)), 40.0);
        }
        assert_eq!(resolve_look_sensitivity_percent(None, Some(r#"{"look_sensitivity_percent":101}"#)), 75.0);
    }

    #[test]
    fn menu_confirmation_keys_do_not_send_gameplay_actions() {
        let mut keys = HashSet::new();
        face_keys(&mut keys, true, false, true, true, true);
        assert_eq!(keys, HashSet::from(["Enter", "Backspace", "Space"]));
        assert!(!keys.contains("ShiftLeft"));
        assert!(!keys.contains("KeyR"));
    }

    #[test]
    fn existing_gameplay_face_bindings_remain() {
        let mut keys = HashSet::new();
        face_keys(&mut keys, false, false, true, true, true);
        assert_eq!(keys, HashSet::from(["Enter", "ShiftLeft", "KeyR", "Backspace", "Space"]));
    }

    #[test]
    fn menu_actions_hold_then_release_before_repeating() {
        let now = Instant::now();
        let mut actions = MenuActions {
            queued: VecDeque::from([AppAction::Key("Escape"), AppAction::Key("Escape")]), active: None, gap_until: None, waiting_for_gameplay: None, wheel_pending: 0,
        };
        assert_eq!(menu_action_key(&mut actions, now, None), Some("Escape"));
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD - Duration::from_millis(1), None), Some("Escape"));
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD, None), None);
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD + MENU_ACTION_GAP - Duration::from_millis(1), None), None);
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD + MENU_ACTION_GAP, None), Some("Escape"));
    }

    #[test]
    fn gameplay_shortcuts_wait_for_real_unpause_and_fail_closed_when_unknown() {
        let now = Instant::now();
        let make = || MenuActions { queued: VecDeque::from([AppAction::Gameplay("KeyV")]),
            active: None, gap_until: None, waiting_for_gameplay: None, wheel_pending: 0 };
        let mut actions = make();
        assert_eq!(menu_action_key(&mut actions, now, Some(true)), Some("Escape"));
        let after = now + MENU_ACTION_HOLD + MENU_ACTION_GAP;
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD, Some(true)), None);
        assert_eq!(menu_action_key(&mut actions, after, Some(true)), None);
        assert_eq!(menu_action_key(&mut actions, after, Some(false)), Some("KeyV"));
        let mut unknown = make();
        assert_eq!(menu_action_key(&mut unknown, now, None), None);
        assert!(unknown.queued.is_empty());
        let mut timeout = make();
        assert_eq!(menu_action_key(&mut timeout, now, Some(true)), Some("Escape"));
        assert_eq!(menu_action_key(&mut timeout, now + GAMEPLAY_ACTION_TIMEOUT, Some(false)), None);
        assert!(timeout.waiting_for_gameplay.is_none());
        let mut resume = MenuActions { queued: VecDeque::from([AppAction::Resume]),
            active: None, gap_until: None, waiting_for_gameplay: None, wheel_pending: 0 };
        assert_eq!(menu_action_key(&mut resume, now, Some(false)), None,
            "Resume must not pause gameplay when only the native UI is open");
    }

    #[test]
    fn modal_ownership_discards_inputs_then_requires_a_neutral_sample() {
        let held = Sample { right_trigger: 1.0, left_stick: [0., 1.], ..Sample::default() };
        let mut blocked = false;
        assert_eq!(physical_sample_for_owner(Some(held), true, &mut blocked), None);
        assert!(blocked);
        assert_eq!(physical_sample_for_owner(Some(held), false, &mut blocked), None);
        assert_eq!(physical_sample_for_owner(None, false, &mut blocked), None);
        assert!(blocked);
        assert_eq!(physical_sample_for_owner(Some(Sample::default()), false, &mut blocked), None);
        assert!(!blocked);
        assert_eq!(physical_sample_for_owner(Some(held), false, &mut blocked), Some(held));
    }

    #[test]
    fn menu_action_api_rejects_unknown_and_excess_actions() {
        assert!(!queue_menu_action(0));
        assert!(!queue_menu_action(17));
        for action in [1, 2, 3, 4, 5, 1, 2, 3] { assert!(queue_menu_action(action)); }
        assert!(!queue_menu_action(4));
        MENU_ACTIONS.lock().unwrap().queued.clear();
    }
}
