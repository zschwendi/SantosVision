// muguet - pad.rs
// Constantin Clerc (c22dev) - 0.1

use crate::input;
use objc2::runtime::NSObjectProtocol;
use objc2_game_controller::{GCController, GCControllerButtonInput, GCExtendedGamepad};
use std::collections::HashSet;
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
    })
}

fn emit_key(key: &str, pressed: bool) {
    KEY_EVENTS.fetch_add(1, Ordering::Relaxed);
    input::key(key, pressed, false, None, false);
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
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

const SWITCH_HOLD: Duration = Duration::from_millis(300);
const STICK_DEADZONE: f32 = 0.3;
const LOOK_DEADZONE: f32 = 0.12;

const LOOK_SPEED: f64 = 700.0;

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

pub fn poll() -> bool {
    POLL_SAMPLES.fetch_add(1, Ordering::Relaxed);
    let Some(gp) = gamepad() else {
        if ACTIVE.swap(false, Ordering::Relaxed) {
            if let Some(st) = STATE.lock().unwrap().take() { release(st); }
            eprintln!("[pad] controller disconnected");
        }
        return false;
    };
    if !ACTIVE.swap(true, Ordering::Relaxed) {
        eprintln!("[pad] controller connected");
    }
    CONNECTED_SAMPLES.fetch_add(1, Ordering::Relaxed);
    let mut guard = STATE.lock().unwrap();
    let st = guard.get_or_insert_with(|| State { held: HashSet::new(), buttons: 0, last: Instant::now(), down_since: None });
    let dt = st.last.elapsed().as_secs_f64().min(0.05);
    st.last = Instant::now();

    let in_car = IN_VEHICLE.load(Ordering::Relaxed);
    let mut keys: HashSet<&'static str> = HashSet::new();
    let mut buttons = 0u8;
    let mut looking = false;
    unsafe {
        let ls = gp.leftThumbstick();
        let (lx, ly) = (ls.xAxis().value(), ls.yAxis().value());
        if lx < -STICK_DEADZONE { keys.insert("KeyA"); }
        if lx > STICK_DEADZONE { keys.insert("KeyD"); }
        if in_car {
            if ly > STICK_DEADZONE { keys.insert("Numpad8"); }
            if ly < -STICK_DEADZONE { keys.insert("Numpad2"); }
            if gp.rightTrigger().value() > 0.2 { keys.insert("KeyW"); }
            if gp.leftTrigger().value() > 0.2 { keys.insert("KeyS"); }
            if down(&gp.rightShoulder()) { keys.insert("Space"); }
            HORN_PAD.store(gp.leftThumbstickButton().is_some_and(|b| down(&b)), Ordering::Relaxed);
            if down(&gp.buttonX()) { keys.insert("Space"); }
        } else {
            if ly > STICK_DEADZONE { keys.insert("KeyW"); }
            if ly < -STICK_DEADZONE { keys.insert("KeyS"); }
            if gp.rightTrigger().value() > 0.2 { buttons |= 1; }
            if gp.leftTrigger().value() > 0.2 { buttons |= 2; }
            if down(&gp.buttonA()) { keys.insert("ShiftLeft"); }
            if down(&gp.buttonX()) { keys.insert("Space"); }
            if down(&gp.rightShoulder()) { keys.insert("KeyQ"); }
            if gp.leftThumbstickButton().is_some_and(|b| down(&b)) { keys.insert("ControlLeft"); }
        }
        if down(&gp.buttonA()) { keys.insert("Enter"); }
        if down(&gp.buttonB()) { keys.insert("KeyR"); keys.insert("Backspace"); }
        if down(&gp.buttonY()) { keys.insert("KeyF"); }
        if down(&gp.leftShoulder()) { keys.insert("Tab"); }
        if gp.rightThumbstickButton().is_some_and(|b| down(&b)) { keys.insert("KeyC"); }
        let dp = gp.dpad();
        if down(&dp.up()) { keys.insert("ArrowUp"); }
        if down(&dp.down()) {
            let since = *st.down_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= SWITCH_HOLD { keys.insert("AltLeft"); } else { keys.insert("ArrowDown"); }
        } else {
            st.down_since = None;
        }
        if down(&dp.left()) { keys.insert("ArrowLeft"); }
        if down(&dp.right()) { keys.insert("ArrowRight"); }
        if down(&gp.buttonMenu()) { keys.insert("Escape"); }
        if gp.buttonOptions().is_some_and(|b| down(&b)) { keys.insert("KeyV"); }
        if touchpad_pressed(&gp) { keys.insert("KeyV"); }

        let rs = gp.rightThumbstick();
        let curve = |v: f32| -> f64 {
            let a = v.abs();
            if a < LOOK_DEADZONE { 0.0 } else { (((a - LOOK_DEADZONE) / (1.0 - LOOK_DEADZONE)) as f64).powf(1.8).copysign(v as f64) }
        };
        let (dx, dy) = (curve(rs.xAxis().value()), -curve(rs.yAxis().value()));
        if dx != 0.0 || dy != 0.0 {
            looking = true;
            LOOK_EVENTS.fetch_add(1, Ordering::Relaxed);
            input::mouse_delta_f(dx * LOOK_SPEED * dt, dy * LOOK_SPEED * dt);
        }
    }
    if !keys.is_empty() || buttons != 0 || looking || (in_car && HORN_PAD.load(Ordering::Relaxed)) {
        ACTIVE_INPUT_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }

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
    true
}

fn release(st: State) {
    for k in st.held { emit_key(k, false); }
    if st.buttons & 1 != 0 { emit_button(0, false); }
    if st.buttons & 2 != 0 { emit_button(2, false); }
}

/// Release held controller input when the native application's scene deactivates.
pub fn suspend() {
    if let Some(state) = STATE.lock().unwrap().take() { release(state); }
    ACTIVE.store(false, Ordering::Relaxed);
    HORN_PAD.store(false, Ordering::Relaxed);
}
