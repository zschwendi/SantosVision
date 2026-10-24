#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/pad-routing}
mkdir -p "$out/harness/src"
python3 - "$out" <<'PY'
import hashlib, json, pathlib, sys
root = pathlib.Path.cwd()
out = pathlib.Path(sys.argv[1]).resolve()
(out/'harness/Cargo.toml').write_text('''[package]
name = "santosvision-pad-routing-check"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
objc2 = "0.6"
objc2-game-controller = { version = "0.3", default-features = false, features = ["std", "GCController", "GCExtendedGamepad", "GCPhysicalInputProfile", "GCControllerElement", "GCControllerAxisInput", "GCControllerButtonInput", "GCControllerDirectionPad"] }
objc2-foundation = { version = "0.3", default-features = false, features = ["std", "NSArray", "NSString", "NSEnumerator"] }
serde_json = "1.0.151"
[features]
jit = []
''')
source = r'''#![allow(dead_code, unexpected_cfgs)]
struct Runtime { mem: () }
static RT: std::sync::OnceLock<Runtime> = std::sync::OnceLock::new();
fn mem_base_len(_: &()) -> (*mut u8, usize) { (std::ptr::null_mut(), 0) }
mod input {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Event { Key(String, bool), Button(usize, bool), Motion(f64, f64), Wheel(i32) }
    static EVENTS: std::sync::Mutex<Vec<Event>> = std::sync::Mutex::new(Vec::new());
    pub fn take() -> Vec<Event> { std::mem::take(&mut *EVENTS.lock().unwrap()) }
    pub fn key(k: &str, down: bool, _: bool, _: Option<&str>, _: bool) { EVENTS.lock().unwrap().push(Event::Key(k.into(), down)); }
    pub fn button(b: usize, down: bool) { EVENTS.lock().unwrap().push(Event::Button(b, down)); }
    pub fn mouse_delta_f(x: f64, y: f64) { EVENTS.lock().unwrap().push(Event::Motion(x,y)); }
    pub fn wheel(notches: i32) { EVENTS.lock().unwrap().push(Event::Wheel(notches)); }
    pub fn set_locked(_: bool) {}
}
mod mp { pub fn key_pressed(_: &str) {} }
mod xr_motion_aim {
    pub fn diagnostic_enabled() -> bool { false }
    pub fn gameplay_enabled() -> bool { false }
    pub fn filter_spatial_trigger(value: f32) -> f32 { value }
}
mod pad {
    include!(PAD_SOURCE_PATH);
    #[cfg(test)]
    mod emitted_lifetime_tests {
        use super::*;
        use crate::input::Event;
        #[test]
        fn phone_back_has_no_reload_or_held_face_leak_for_both_controller_profiles() {
            for spatial in [false, true] {
                suspend();
                TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
                TEST_NATIVE_CONTROLS_POLICY.store(0, Ordering::Relaxed);
                let neutral = if spatial { spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap() }
                    else { Sample::default() };
                let phone = || Some(guest_context::Context { paused: false, phone: true });
                let gameplay = || Some(guest_context::Context { paused: false, phone: false });
                poll_sample_in_context(Some(neutral), phone);
                crate::input::take();
                let circle = Sample { face_and_shoulders: CIRCLE, ..neutral };
                poll_sample_in_context(Some(circle), phone);
                let events = crate::input::take();
                assert!(events.contains(&Event::Key("Backspace".into(), true)));
                assert!(!events.contains(&Event::Key("KeyR".into(), true)));
                poll_sample_in_context(Some(circle), gameplay);
                let events = crate::input::take();
                assert!(events.contains(&Event::Key("Backspace".into(), false)));
                assert!(!events.contains(&Event::Key("KeyR".into(), true)));
                poll_sample_in_context(Some(neutral), gameplay);
                crate::input::take();
                poll_sample_in_context(Some(circle), gameplay);
                assert!(crate::input::take().contains(&Event::Key("KeyR".into(), true)));
                suspend();
            }
        }
        #[test]
        fn verified_phone_close_queue_reaches_guest_under_transition_modal_ownership() {
            suspend();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(true, Ordering::Relaxed);
            let held = Sample { right_trigger: 1.0, face_and_shoulders: CROSS, ..Sample::default() };
            assert!(queue_menu_action(12));
            crate::input::take();
            poll_sample_in_context(Some(held), || Some(guest_context::Context { paused: false, phone: true }));
            let events = crate::input::take();
            assert!(events.contains(&Event::Key("Backspace".into(), true)));
            assert!(!events.iter().any(|event| matches!(event, Event::Button(_, true))));
            assert!(!events.contains(&Event::Key("ShiftLeft".into(), true)));
            MENU_ACTIONS.lock().unwrap().active = Some(("Backspace", Instant::now() - Duration::from_millis(1)));
            poll_sample_in_context(Some(held), || Some(guest_context::Context { paused: false, phone: false }));
            assert!(crate::input::take().contains(&Event::Key("Backspace".into(), false)));
            suspend();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
        }
        #[test]
        fn partial_disconnect_and_suspend_emit_actual_releases_and_stop_look() {
            suspend();
            MENU_NAVIGATION.store(false, Ordering::Relaxed);
            IN_VEHICLE.store(false, Ordering::Relaxed);
            let neutral = spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap();
            poll_sample(Some(neutral));
            crate::input::take();
            let held = spatial::spatial_sample(3, 1, [0.0, 1.0, 0.7, 0.0, 1.0, 1.0], 42).unwrap();
            poll_sample(Some(held));
            let events = crate::input::take();
            assert!(events.contains(&Event::Key("KeyW".into(), true)));
            assert!(events.contains(&Event::Button(2, true)));
            assert!(events.contains(&Event::Button(0, true)));
            assert!(events.iter().any(|event| matches!(event, Event::Motion(x, _) if *x > 0.0)));
            // Left disconnects while aim/movement are held; right continues firing.
            let right = spatial::spatial_sample(2, 1, [1.0; 6], 0).unwrap();
            poll_sample(Some(right));
            let events = crate::input::take();
            assert!(events.contains(&Event::Key("KeyW".into(), false)));
            assert!(events.contains(&Event::Button(2, false)));
            assert!(!events.contains(&Event::Button(0, false)));
            // Right then disconnects: fire and sprint release; no further look.
            poll_sample(None);
            let events = crate::input::take();
            assert!(events.contains(&Event::Button(0, false)));
            assert!(events.contains(&Event::Key("ShiftLeft".into(), false)));
            assert!(!events.iter().any(|event| matches!(event, Event::Motion(..))));
            assert!(STATE.lock().unwrap().as_ref().unwrap().held.is_empty());
            // Focus/runtime suspension uses the same actual release path.
            poll_sample(Some(held));
            crate::input::take();
            suspend();
            let events = crate::input::take();
            assert!(events.contains(&Event::Key("KeyW".into(), false)));
            assert!(events.contains(&Event::Button(0, false)));
            assert!(events.contains(&Event::Button(2, false)));
            assert!(STATE.lock().unwrap().is_none());
            // A pending L3 tap is discarded on suspension and cannot reappear.
            poll_sample(Some(neutral));
            let click = spatial::spatial_sample(3, 1 << 6, [0.0; 6], 42).unwrap();
            poll_sample(Some(click));
            suspend();
            crate::input::take();
            poll_sample(Some(neutral));
            assert!(!crate::input::take().contains(&Event::Key("ControlLeft".into(), true)));
            suspend();
        }
        #[test]
        fn held_ui_down_runs_real_character_wheel_mapping_under_physical_ownership() {
            suspend();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
            MENU_NAVIGATION.store(false, Ordering::Relaxed);
            let neutral = spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap();
            poll_sample(Some(neutral));
            crate::input::take();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(true, Ordering::Relaxed);
            assert_eq!(sv_native_utility_dpad_v1(9, 1), 0);
            poll_sample(Some(neutral));
            assert!(crate::input::take().contains(&Event::Key("ArrowDown".into(), true)));
            STATE.lock().unwrap().as_mut().unwrap().down_since = Some(Instant::now() - SWITCH_HOLD);
            assert_eq!(sv_native_utility_dpad_v1(9, 1), 0); // UI heartbeat refresh.
            poll_sample(Some(neutral));
            let events = crate::input::take();
            assert!(events.contains(&Event::Key("ArrowDown".into(), false)));
            assert!(events.contains(&Event::Key("AltLeft".into(), true)), "Held UI Down must open the actual GTA character wheel, not repeat a120ms tap");
            assert_eq!(sv_native_utility_dpad_v1(9, 0), 0);
            poll_sample(Some(neutral));
            assert!(crate::input::take().contains(&Event::Key("AltLeft".into(), false)));
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
            suspend();
            crate::input::take();
        }
        #[test]
        fn utility_character_wheel_allows_actual_right_stick_mouse_only_after_open_and_rearms_actions() {
            suspend();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
            TEST_NATIVE_CONTROLS_POLICY.store(0, Ordering::Relaxed);
            MENU_NAVIGATION.store(false, Ordering::Relaxed);
            IN_VEHICLE.store(false, Ordering::Relaxed);
            let neutral = spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap();
            let held = spatial::spatial_sample(3, 63, [1.0, 1.0, 0.8, 0.6, 1.0, 1.0], 42).unwrap();
            poll_sample(Some(neutral)); crate::input::take();
            TEST_NATIVE_CONTROLS_POLICY.store(2, Ordering::Relaxed);
            assert_eq!(sv_native_utility_dpad_v1(9, 1), 0);
            poll_sample(Some(held));
            let initial = crate::input::take();
            assert!(initial.contains(&Event::Key("ArrowDown".into(), true)));
            assert!(!initial.iter().any(|event| matches!(event, Event::Motion(..) | Event::Button(_,true))));
            assert!(!initial.iter().any(|event| matches!(event, Event::Key(k,true) if ["KeyW","KeyA","KeyS","KeyD","ShiftLeft","Space","KeyF","Tab","KeyQ"].contains(&k.as_str()))));
            STATE.lock().unwrap().as_mut().unwrap().down_since = Some(Instant::now() - SWITCH_HOLD);
            assert_eq!(sv_native_utility_dpad_v1(9, 1), 0);
            poll_sample(Some(held));
            let opened = crate::input::take();
            assert!(opened.contains(&Event::Key("AltLeft".into(), true)));
            assert!(!opened.iter().any(|event| matches!(event, Event::Motion(..))), "Threshold-crossing poll cannot rotate the view before emitting wheel-open");
            poll_sample(Some(held));
            let navigation = crate::input::take();
            assert!(navigation.iter().any(|event| matches!(event, Event::Motion(x,y) if *x > 0.0 && *y < 0.0)), "Held UI Down must allow real character-wheel right-stick mouse selection");
            assert!(!navigation.iter().any(|event| matches!(event, Event::Button(_,true))));
            let state = STATE.lock().unwrap();
            assert_eq!(state.as_ref().unwrap().held, std::collections::HashSet::from(["AltLeft"]));
            assert_eq!(state.as_ref().unwrap().buttons, 0); drop(state);
            // Full modal policy overrides even a held character-wheel gesture.
            TEST_NATIVE_CONTROLS_POLICY.store(1, Ordering::Relaxed);
            poll_sample(Some(held));
            assert!(!crate::input::take().iter().any(|event| matches!(event, Event::Motion(..) | Event::Button(_,true))));
            assert_eq!(sv_native_utility_dpad_v1(9, 0), 0);
            TEST_NATIVE_CONTROLS_POLICY.store(2, Ordering::Relaxed);
            poll_sample(Some(held));
            let released = crate::input::take();
            assert!(released.contains(&Event::Key("AltLeft".into(), false)));
            assert!(!released.iter().any(|event| matches!(event, Event::Motion(..))));
            TEST_NATIVE_CONTROLS_POLICY.store(0, Ordering::Relaxed);
            poll_sample(Some(held));
            assert!(crate::input::take().is_empty(), "Held actions cannot leak on utility dismissal");
            poll_sample(Some(neutral)); crate::input::take();
            poll_sample(Some(held));
            let rearmed = crate::input::take();
            assert!(rearmed.contains(&Event::Button(0,true)) && rearmed.contains(&Event::Button(2,true)));
            assert!(rearmed.contains(&Event::Key("KeyW".into(), true)), "A real neutral sample must rearm normal controls");
            suspend(); crate::input::take();
        }
        #[test]
        fn ui_direction_watchdog_disconnect_replacement_and_suspend_emit_releases() {
            suspend();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
            MENU_NAVIGATION.store(false, Ordering::Relaxed);
            let neutral = spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap();
            poll_sample(Some(neutral));
            crate::input::take();
            assert_eq!(sv_native_utility_dpad_v1(8, 1), 0);
            poll_sample(Some(neutral));
            assert!(crate::input::take().contains(&Event::Key("ArrowUp".into(), true)));
            UTILITY_DPAD.lock().unwrap()[0] = Some(Instant::now() - UTILITY_DPAD_WATCHDOG);
            poll_sample(Some(neutral));
            assert!(crate::input::take().contains(&Event::Key("ArrowUp".into(), false)), "A stalled UI heartbeat must release actual held input within250ms");
            assert_eq!(sv_native_utility_dpad_v1(8, 1), 0);
            poll_sample(Some(neutral));
            crate::input::take();
            poll_sample(None);
            assert!(crate::input::take().contains(&Event::Key("ArrowUp".into(), false)));
            assert_eq!(utility_dpad(Instant::now()), 0);
            poll_sample(Some(neutral));
            assert_eq!(sv_native_utility_dpad_v1(10, 1), 0);
            poll_sample(Some(neutral));
            crate::input::take();
            let replacement = spatial::spatial_sample(3, 0, [0.0; 6], 99).unwrap();
            poll_sample(Some(replacement));
            assert!(crate::input::take().contains(&Event::Key("ArrowLeft".into(), false)));
            assert_eq!(utility_dpad(Instant::now()), 0);
            assert_eq!(sv_native_utility_dpad_v1(11, 1), 0);
            poll_sample(Some(replacement));
            crate::input::take();
            suspend();
            assert!(crate::input::take().contains(&Event::Key("ArrowRight".into(), false)));
            assert_eq!(utility_dpad(Instant::now()), 0);
        }
        #[test]
        fn utility_hold_api_rejects_invalid_controls_and_preserves_heartbeat_age_bound() {
            clear_utility_dpad();
            for (action, pressed) in [(7,1), (12,1), (8,2), (u32::MAX,0)] {
                assert_eq!(sv_native_utility_dpad_v1(action, pressed), -1);
            }
            assert_eq!(utility_dpad(Instant::now()), 0);
            assert_eq!(sv_native_utility_dpad_v1(10,1), 0);
            let at = UTILITY_DPAD.lock().unwrap()[2].unwrap();
            assert_eq!(utility_dpad(at + Duration::from_millis(249)), 4);
            assert_eq!(utility_dpad(at + Duration::from_millis(250)), 0);
            clear_utility_dpad();
        }
        #[test]
        fn fast_native_start_counter_matches_actual_physical_poll_edges() {
            suspend();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
            let neutral = spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap();
            poll_sample(Some(neutral));
            let before = sv_native_menu_press_count_v1();
            let menu = spatial::spatial_sample(3, 1 << 9, [0.0; 6], 42).unwrap();
            poll_sample(Some(menu));
            assert_eq!(sv_native_menu_press_count_v1(), before + 1);
            poll_sample(Some(menu));
            assert_eq!(sv_native_menu_press_count_v1(), before + 1, "Holding Start cannot generate repeated native menu activations");
            poll_sample(Some(neutral));
            poll_sample(Some(menu));
            assert_eq!(sv_native_menu_press_count_v1(), before + 2);
            suspend();
            MENU_NAVIGATION.store(false, Ordering::Relaxed);
            crate::input::take();
        }
        #[test]
        fn native_modal_releases_actual_inputs_keeps_app_actions_and_requires_rearm() {
            suspend();
            MENU_NAVIGATION.store(false, Ordering::Relaxed);
            IN_VEHICLE.store(false, Ordering::Relaxed);
            let neutral = spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap();
            let held = spatial::spatial_sample(3, 1, [0.0, 1.0, 0.0, 0.0, 1.0, 1.0], 42).unwrap();
            poll_sample(Some(neutral));
            poll_sample(Some(held));
            crate::input::take();
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(true, Ordering::Relaxed);
            assert!(queue_menu_action(3));
            poll_sample(Some(held));
            let events = crate::input::take();
            assert!(events.contains(&Event::Key("KeyW".into(), false)));
            assert!(events.contains(&Event::Button(0, false)));
            assert!(events.contains(&Event::Button(2, false)));
            assert!(events.contains(&Event::Key("Space".into(), true)), "Apply still reaches GTA while the native UI owns physical input");
            // Expire the Apply pulse using the queue's real clock, then prove
            // the same production poller emits actual mouse-wheel map zoom.
            { let mut actions = MENU_ACTIONS.lock().unwrap(); actions.active = None; actions.gap_until = None; }
            assert!(queue_menu_action(15));
            poll_sample(Some(held));
            assert!(crate::input::take().contains(&Event::Wheel(1)));
            assert!(queue_menu_action(16));
            poll_sample(Some(held));
            assert!(crate::input::take().contains(&Event::Wheel(-1)));
            TEST_NATIVE_CONTROLS_OWN_INPUT.store(false, Ordering::Relaxed);
            poll_sample(Some(held));
            let events = crate::input::take();
            assert!(!events.contains(&Event::Button(0, true)));
            assert!(!events.contains(&Event::Key("KeyW".into(), true)));
            poll_sample(Some(neutral));
            poll_sample(Some(held));
            let events = crate::input::take();
            assert!(events.contains(&Event::Button(0, true)));
            assert!(events.contains(&Event::Key("KeyW".into(), true)));
            suspend();
            crate::input::take();
        }
        #[test]
        fn moving_with_l3_uses_actual_movement_and_never_opens_phone() {
            suspend();
            MENU_NAVIGATION.store(false, Ordering::Relaxed);
            IN_VEHICLE.store(false, Ordering::Relaxed);
            let neutral = spatial::spatial_sample(3, 0, [0.0; 6], 42).unwrap();
            poll_sample(Some(neutral));
            crate::input::take();
            let chord = spatial::spatial_sample(3, 1 << 6, [0.8, 0.9, 0.0, 0.0, 0.0, 0.0], 42).unwrap();
            poll_sample(Some(chord));
            let events = crate::input::take();
            assert!(!events.iter().any(|event| matches!(event, Event::Key(k, true) if k.starts_with("Arrow"))));
            assert!(events.contains(&Event::Key("KeyW".into(), true)));
            assert!(events.contains(&Event::Key("KeyD".into(), true)));
            poll_sample(Some(neutral));
            let events = crate::input::take();
            assert!(events.contains(&Event::Key("KeyW".into(), false)));
            assert!(events.contains(&Event::Key("KeyD".into(), false)));
            assert!(events.contains(&Event::Key("ControlLeft".into(), true)));
            suspend();
        }
    }
}
'''.replace('PAD_SOURCE_PATH', json.dumps(str(root/'src/pad.rs')))
(out/'harness/src/lib.rs').write_text(source)
files = ['src/pad.rs', 'src/pad_spatial.rs', 'src/pad_guest_context.rs', 'scripts/test-pad-routing.sh']
(out/'source-hashes.json').write_text(json.dumps({f: hashlib.sha256((root/f).read_bytes()).hexdigest() for f in files}, indent=2)+'\n')
PY
export CARGO_HOME=${CARGO_HOME:-$PWD/.tools/cargo}
export RUSTUP_HOME=${RUSTUP_HOME:-$PWD/.tools/rustup}
export PATH="$CARGO_HOME/bin:$PATH"
cargo +1.98.1 test --offline --manifest-path "$out/harness/Cargo.toml" -j2 -- --test-threads=1
