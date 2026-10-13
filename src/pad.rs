// muguet - pad.rs
// Constantin Clerc (c22dev) - 0.1

use crate::input;
use objc2::runtime::NSObjectProtocol;
use objc2_game_controller::{GCController, GCControllerButtonInput, GCExtendedGamepad};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub static ACTIVE: AtomicBool = AtomicBool::new(false);
static POLL_SAMPLES: AtomicU64 = AtomicU64::new(0);
static CONNECTED_SAMPLES: AtomicU64 = AtomicU64::new(0);
static ACTIVE_INPUT_SAMPLES: AtomicU64 = AtomicU64::new(0);
static KEY_EVENTS: AtomicU64 = AtomicU64::new(0);
static BUTTON_EVENTS: AtomicU64 = AtomicU64::new(0);
static LOOK_EVENTS: AtomicU64 = AtomicU64::new(0);
static MENU_PRESS_COUNT: AtomicU64 = AtomicU64::new(0);
static LOOK_SENSITIVITY: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
// Navigation context follows controller Menu presses. It is an input-routing
// hint, not an engine-confirmed paused state.
static MENU_NAVIGATION: AtomicBool = AtomicBool::new(false);

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
        "menu_press_count": MENU_PRESS_COUNT.load(Ordering::Relaxed),
        "menu_navigation_inferred": MENU_NAVIGATION.load(Ordering::Relaxed),
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
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

struct MenuActions {
    queued: VecDeque<&'static str>,
    active: Option<(&'static str, Instant)>,
    gap_until: Option<Instant>,
}
static MENU_ACTIONS: Mutex<MenuActions> = Mutex::new(MenuActions {
    queued: VecDeque::new(), active: None, gap_until: None,
});
const MENU_ACTION_HOLD: Duration = Duration::from_millis(120);
const MENU_ACTION_GAP: Duration = Duration::from_millis(32);
const MENU_ACTION_QUEUE_CAPACITY: usize = 8;

/// Queue one bounded keyboard action from the native pause controls.
/// IDs: 1 Escape, 2 Enter, 3 Space, 4 Y, 5 N. Invalid/full queues reject.
/// The gamepad poller merges these pulses with physical keys, so releasing a
/// controller button cannot clear a simultaneous app action.
pub fn queue_menu_action(action: u32) -> bool {
    let key = match action {
        1 => "Escape", 2 => "Enter", 3 => "Space", 4 => "KeyY", 5 => "KeyN",
        _ => return false,
    };
    let mut actions = MENU_ACTIONS.lock().unwrap();
    if actions.queued.len() >= MENU_ACTION_QUEUE_CAPACITY { return false; }
    actions.queued.push_back(key);
    true
}

fn menu_action_key(actions: &mut MenuActions, now: Instant) -> Option<&'static str> {
    if let Some((_, until)) = actions.active {
        if now >= until {
            actions.active = None;
            actions.gap_until = Some(now + MENU_ACTION_GAP);
        }
    }
    if actions.active.is_none() && actions.gap_until.is_none_or(|until| now >= until) {
        actions.gap_until = None;
        if let Some(key) = actions.queued.pop_front() {
            actions.active = Some((key, now + MENU_ACTION_HOLD));
        }
    }
    actions.active.map(|(key, _)| key)
}

const SWITCH_HOLD: Duration = Duration::from_millis(300);
const STICK_DEADZONE: f32 = 0.3;
const LOOK_DEADZONE: f32 = 0.12;

const LOOK_SPEED: f64 = 700.0;
const DEFAULT_LOOK_SENSITIVITY_PERCENT: f64 = 75.0;

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

pub fn poll() -> bool {
    POLL_SAMPLES.fetch_add(1, Ordering::Relaxed);
    let gp = gamepad();
    let connected = gp.is_some();
    if ACTIVE.swap(connected, Ordering::Relaxed) != connected {
        eprintln!("[pad] controller {}", if connected { "connected" } else { "disconnected" });
    }
    if connected { CONNECTED_SAMPLES.fetch_add(1, Ordering::Relaxed); }
    let mut guard = STATE.lock().unwrap();
    let st = guard.get_or_insert_with(|| State { held: HashSet::new(), buttons: 0, last: Instant::now(), down_since: None,
        menu_navigation: MENU_NAVIGATION.load(Ordering::Relaxed), menu_button_down: false });
    let dt = st.last.elapsed().as_secs_f64().min(0.05);
    st.last = Instant::now();

    let in_car = IN_VEHICLE.load(Ordering::Relaxed);
    let mut keys: HashSet<&'static str> = HashSet::new();
    let mut buttons = 0u8;
    let mut looking = false;
    let menu_key = menu_action_key(&mut MENU_ACTIONS.lock().unwrap(), Instant::now());
    let menu_down = gp.as_ref().is_some_and(|gp| unsafe { down(&gp.buttonMenu()) });
    if menu_down && !st.menu_button_down {
        MENU_PRESS_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    st.menu_button_down = menu_down;
    // Escape's merged release changes the inferred navigation context before
    // sampling face buttons, so a fast Cross after Menu never leaks Shift.
    if st.held.contains("Escape") && !menu_down && menu_key != Some("Escape") {
        st.menu_navigation = !st.menu_navigation;
        MENU_NAVIGATION.store(st.menu_navigation, Ordering::Relaxed);
    }
    if menu_down { keys.insert("Escape"); }
    if let Some(gp) = gp.as_ref() { unsafe {
        let menu = st.menu_navigation;
        let ls = gp.leftThumbstick();
        let (lx, ly) = (ls.xAxis().value(), ls.yAxis().value());
        if lx < -STICK_DEADZONE { keys.insert(if menu { "ArrowLeft" } else { "KeyA" }); }
        if lx > STICK_DEADZONE { keys.insert(if menu { "ArrowRight" } else { "KeyD" }); }
        if menu {
            if ly > STICK_DEADZONE { keys.insert("ArrowUp"); }
            if ly < -STICK_DEADZONE { keys.insert("ArrowDown"); }
            HORN_PAD.store(false, Ordering::Relaxed);
        } else if in_car {
            if ly > STICK_DEADZONE { keys.insert("Numpad8"); }
            if ly < -STICK_DEADZONE { keys.insert("Numpad2"); }
            if gp.rightTrigger().value() > 0.2 { keys.insert("KeyW"); }
            if gp.leftTrigger().value() > 0.2 { keys.insert("KeyS"); }
            if down(&gp.rightShoulder()) { keys.insert("Space"); }
            HORN_PAD.store(gp.leftThumbstickButton().is_some_and(|b| down(&b)), Ordering::Relaxed);
        } else {
            if ly > STICK_DEADZONE { keys.insert("KeyW"); }
            if ly < -STICK_DEADZONE { keys.insert("KeyS"); }
            if gp.rightTrigger().value() > 0.2 { buttons |= 1; }
            if gp.leftTrigger().value() > 0.2 { buttons |= 2; }
            if down(&gp.rightShoulder()) { keys.insert("KeyQ"); }
            if gp.leftThumbstickButton().is_some_and(|b| down(&b)) { keys.insert("ControlLeft"); }
        }
        face_keys(&mut keys, menu, in_car, down(&gp.buttonA()), down(&gp.buttonB()), down(&gp.buttonX()));
        if menu {
            if down(&gp.buttonY()) { keys.insert("KeyY"); }
            if down(&gp.rightShoulder()) { keys.insert("KeyN"); }
            if down(&gp.leftShoulder()) { keys.insert("Tab"); }
        } else {
            if down(&gp.buttonY()) { keys.insert("KeyF"); }
            if down(&gp.leftShoulder()) { keys.insert("Tab"); }
            if gp.rightThumbstickButton().is_some_and(|b| down(&b)) { keys.insert("KeyC"); }
        }
        let dp = gp.dpad();
        if down(&dp.up()) { keys.insert("ArrowUp"); }
        if down(&dp.down()) {
            let since = *st.down_since.get_or_insert_with(Instant::now);
            if !menu && since.elapsed() >= SWITCH_HOLD { keys.insert("AltLeft"); } else { keys.insert("ArrowDown"); }
        } else {
            st.down_since = None;
        }
        if down(&dp.left()) { keys.insert("ArrowLeft"); }
        if down(&dp.right()) { keys.insert("ArrowRight"); }
        if !menu {
            if gp.buttonOptions().is_some_and(|b| down(&b)) { keys.insert("KeyV"); }
            if touchpad_pressed(&gp) { keys.insert("KeyV"); }
        }

        let rs = gp.rightThumbstick();
        let curve = |v: f32| -> f64 {
            let a = v.abs();
            if a < LOOK_DEADZONE { 0.0 } else { (((a - LOOK_DEADZONE) / (1.0 - LOOK_DEADZONE)) as f64).powf(1.8).copysign(v as f64) }
        };
        let (dx, dy) = (curve(rs.xAxis().value()), -curve(rs.yAxis().value()));
        if !menu && (dx != 0.0 || dy != 0.0) {
            looking = true;
            LOOK_EVENTS.fetch_add(1, Ordering::Relaxed);
            let speed = LOOK_SPEED * look_sensitivity_percent() / 100.0;
            input::mouse_delta_f(dx * speed * dt, dy * speed * dt);
        }
    } } else {
        st.menu_button_down = false;
        st.down_since = None;
        HORN_PAD.store(false, Ordering::Relaxed);
    }
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
    if let Some(state) = STATE.lock().unwrap().take() {
        MENU_NAVIGATION.store(state.menu_navigation, Ordering::Relaxed);
        release(state);
    }
    let mut actions = MENU_ACTIONS.lock().unwrap();
    actions.queued.clear();
    actions.active = None;
    actions.gap_until = None;
    ACTIVE.store(false, Ordering::Relaxed);
    HORN_PAD.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

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
            queued: VecDeque::from(["Escape", "Escape"]), active: None, gap_until: None,
        };
        assert_eq!(menu_action_key(&mut actions, now), Some("Escape"));
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD - Duration::from_millis(1)), Some("Escape"));
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD), None);
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD + MENU_ACTION_GAP - Duration::from_millis(1)), None);
        assert_eq!(menu_action_key(&mut actions, now + MENU_ACTION_HOLD + MENU_ACTION_GAP), Some("Escape"));
    }

    #[test]
    fn menu_action_api_rejects_unknown_and_excess_actions() {
        assert!(!queue_menu_action(0));
        assert!(!queue_menu_action(6));
        for action in [1, 2, 3, 4, 5, 1, 2, 3] { assert!(queue_menu_action(action)); }
        assert!(!queue_menu_action(4));
        MENU_ACTIONS.lock().unwrap().queued.clear();
    }
}
