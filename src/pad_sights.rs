//! First-person sights for Sense. GTA keeps ordinary aim (INPUT_AIM, L2 as
//! the right mouse button) separate from first-person sights. Original
//! CPlayerInfo::ProcessFPSState41680 changes CPlayerInfo+4664 value 2 only from
//! INPUT_ACCURATE_AIM while aiming: a pad R3 press toggles it, while a
//! keyboard/mouse source (device -4) is a signed wheel axis: negative enters
//! sights and positive leaves them. GTA's own pad layout puts this "zoom aim"
//! on R3 beside LOOK_BEHIND. The guest only receives keyboard/mouse input, so a
//! Sense R3 press while aiming in first person sends one wheel notch instead
//! of C. Third person, vehicles, menus, scoped cameras and DualSense keep C.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Guest CControlMgr::Update65444 swaps our wheel word and copies its sign to
/// rage's mouse dZ; +1 is wheel up on every host path. Which sign GTA's KB/M
/// binding reports as negative is binding data, not code, so it is learned:
/// an unchanged native flag after a full window tries the other sign next
/// press, and the first observed native change locks the sign.
const INITIAL_SIGHTS_WHEEL: i32 = 1;
const CONFIRM_WINDOW: Duration = Duration::from_millis(300);
/// A notch read without INPUT_AIM would select another weapon. Keep the
/// existing aim button down while the guest consumes it.
const AIM_HOLD_AFTER_NOTCH: Duration = Duration::from_millis(120);
const VIEW_FRESHNESS: Duration = Duration::from_millis(500);

static SIGHTS_WHEEL: AtomicI32 = AtomicI32::new(INITIAL_SIGHTS_WHEEL);
static SIGN_LOCKED: AtomicU64 = AtomicU64::new(0);
static ROUTED_PRESSES: AtomicU64 = AtomicU64::new(0);
static NOTCHES_UP: AtomicU64 = AtomicU64::new(0);
static NOTCHES_DOWN: AtomicU64 = AtomicU64::new(0);
static CONFIRMED: AtomicU64 = AtomicU64::new(0);
static UNCHANGED: AtomicU64 = AtomicU64::new(0);
static SIGN_FLIPS: AtomicU64 = AtomicU64::new(0);
static AIM_PRESSES: AtomicU64 = AtomicU64::new(0);
static AIM_RELEASES: AtomicU64 = AtomicU64::new(0);
static LAST_NATIVE: Mutex<Option<(Option<bool>, Instant)>> = Mutex::new(None);
static ON_FOOT_VIEW: Mutex<Option<(bool, Instant)>> = Mutex::new(None);

/// Script-tick sample of GetFollowPedCamViewMode; 4 is first person.
pub(super) fn record_follow_ped_view_mode(mode: i32) {
    *ON_FOOT_VIEW.lock().unwrap_or_else(|p| p.into_inner()) = Some((mode == 4, Instant::now()));
}

pub(super) fn first_person_on_foot(now: Instant) -> Option<bool> {
    let (first, at) = (*ON_FOOT_VIEW.lock().unwrap_or_else(|p| p.into_inner()))?;
    (now.saturating_duration_since(at) <= VIEW_FRESHNESS).then_some(first)
}

pub(super) fn record_aim_edge(pressed: bool) {
    (if pressed { &AIM_PRESSES } else { &AIM_RELEASES }).fetch_add(1, Ordering::Relaxed);
}

fn sights_wheel(on: bool) -> i32 {
    let sign = SIGHTS_WHEEL.load(Ordering::Relaxed);
    if on { sign } else { -sign }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Route {
    pub look_behind: bool,
    pub wheel: i32,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    want_on: bool,
    before: bool,
    sign: i32,
    at: Instant,
    interrupted: bool,
}

/// Per input lifecycle. The learned sign and counters are process-wide.
#[derive(Default)]
pub(super) struct Toggle {
    r3_held: bool,
    r3_consumed: bool,
    model_on: bool,
    pending: Option<Pending>,
    aim_hold_until: Option<Instant>,
}

impl Toggle {
    /// `aiming` means L2 is held now and the aim button already reached the
    /// guest on an earlier poll. `native` is read only when needed.
    pub fn route(&mut self, spatial: bool, r3: bool, aiming: bool, first_person: Option<bool>,
                 native: impl FnOnce() -> Option<super::guest_context::Sights>, now: Instant) -> Route {
        let rising = r3 && !self.r3_held;
        self.r3_held = r3;
        if !r3 { self.r3_consumed = false; }
        let wants = rising && spatial && aiming && first_person == Some(true);
        let sample = (wants || self.pending.is_some()).then(native).flatten();
        if self.pending.is_some() || wants {
            *LAST_NATIVE.lock().unwrap_or_else(|p| p.into_inner()) = Some((sample.map(|s| s.on), now));
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.interrupted |= !aiming || first_person != Some(true);
        }
        self.observe(sample.map(|s| s.on), now);
        // The scoped camera's wheel is sniper zoom, owned by the scope path.
        if !wants || sample.is_some_and(|s| s.scoped_camera) {
            return Route { look_behind: r3 && !self.r3_consumed, wheel: 0 };
        }
        let before = sample.map(|s| s.on);
        let want_on = !before.unwrap_or(self.model_on);
        let wheel = sights_wheel(want_on);
        self.model_on = want_on;
        self.pending = before.map(|before| Pending { want_on, before, sign: SIGHTS_WHEEL.load(Ordering::Relaxed), at: now, interrupted: false });
        self.r3_consumed = true;
        self.aim_hold_until = Some(now + AIM_HOLD_AFTER_NOTCH);
        ROUTED_PRESSES.fetch_add(1, Ordering::Relaxed);
        (if wheel > 0 { &NOTCHES_UP } else { &NOTCHES_DOWN }).fetch_add(1, Ordering::Relaxed);
        Route { look_behind: false, wheel }
    }

    fn observe(&mut self, native: Option<bool>, now: Instant) {
        let Some(pending) = self.pending else { return };
        if native == Some(pending.want_on) {
            self.pending = None;
            self.model_on = pending.want_on;
            CONFIRMED.fetch_add(1, Ordering::Relaxed);
            // Lock only the sign that actually produced the native change.
            if SIGN_LOCKED.load(Ordering::Relaxed) == 0 && SIGHTS_WHEEL.load(Ordering::Relaxed) == pending.sign {
                SIGN_LOCKED.store(1, Ordering::Relaxed);
            }
            return;
        }
        if now.saturating_duration_since(pending.at) < CONFIRM_WINDOW { return; }
        self.pending = None;
        if native.is_none() || native != Some(pending.before) { return; }
        self.model_on = pending.before;
        if pending.interrupted { return; }
        UNCHANGED.fetch_add(1, Ordering::Relaxed);
        if SIGN_LOCKED.load(Ordering::Relaxed) == 0
            && SIGHTS_WHEEL.compare_exchange(pending.sign, -pending.sign, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            SIGN_FLIPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Keeps INPUT_AIM down briefly after a notch; on-foot gameplay only.
    pub fn holds_aim(&self, now: Instant) -> bool {
        self.aim_hold_until.is_some_and(|until| now < until)
    }

    /// Menus, phone, vehicles, modal ownership and loss: no notch, no hold,
    /// no sign learning. R3 stays tracked so a held click is never an edge.
    pub fn interrupt(&mut self, r3: bool) {
        self.r3_held = r3;
        if !r3 { self.r3_consumed = false; }
        self.pending = None;
        self.aim_hold_until = None;
    }
}

pub(super) fn status_json() -> serde_json::Value {
    let native = *LAST_NATIVE.lock().unwrap_or_else(|p| p.into_inner());
    serde_json::json!({
        "mapping": "Sense R3 while L2 aim in first person on foot: one mouse-wheel notch for GTA INPUT_ACCURATE_AIM (sights toggle); otherwise R3 is C look-behind",
        "aim_button_presses": AIM_PRESSES.load(Ordering::Relaxed),
        "aim_button_releases": AIM_RELEASES.load(Ordering::Relaxed),
        "routed_r3_presses": ROUTED_PRESSES.load(Ordering::Relaxed),
        "wheel_notches_up": NOTCHES_UP.load(Ordering::Relaxed),
        "wheel_notches_down": NOTCHES_DOWN.load(Ordering::Relaxed),
        "wheel_sign_for_sights": SIGHTS_WHEEL.load(Ordering::Relaxed),
        "wheel_sign_locked_by_native_change": SIGN_LOCKED.load(Ordering::Relaxed) != 0,
        "native_sights_changes_confirmed": CONFIRMED.load(Ordering::Relaxed),
        "native_sights_unchanged_after_window": UNCHANGED.load(Ordering::Relaxed),
        "wheel_sign_flips": SIGN_FLIPS.load(Ordering::Relaxed),
        "last_native_sights": native.and_then(|(on, _)| on),
        "last_native_read_age_ms": native.map(|(_, at)| at.elapsed().as_millis()),
        "first_person_on_foot": first_person_on_foot(Instant::now()),
        "scope": "button delivery and GTA's own first-person sights flag only; not hand pose, weapon geometry or rendered view",
    })
}

#[cfg(test)]
pub(super) fn reset_learning_for_test() {
    SIGHTS_WHEEL.store(INITIAL_SIGHTS_WHEEL, Ordering::Relaxed);
    SIGN_LOCKED.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub(super) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::guest_context::Sights;

    fn native(on: bool) -> impl FnOnce() -> Option<Sights> { move || Some(Sights { on, scoped_camera: false }) }
    fn unknown() -> impl FnOnce() -> Option<Sights> { || None }

    #[test]
    fn r3_without_first_person_aim_keeps_look_behind() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_learning_for_test();
        let now = Instant::now();
        for (spatial, aiming, first) in [(false, true, Some(true)), (true, false, Some(true)),
                                         (true, true, Some(false)), (true, true, None)] {
            let mut toggle = Toggle::default();
            assert_eq!(toggle.route(spatial, true, aiming, first, native(false), now),
                Route { look_behind: true, wheel: 0 }, "spatial={spatial} aiming={aiming} first={first:?}");
            assert!(!toggle.holds_aim(now));
        }
        let mut toggle = Toggle::default();
        let scoped = || Some(Sights { on: false, scoped_camera: true });
        assert_eq!(toggle.route(true, true, true, Some(true), scoped, now), Route { look_behind: true, wheel: 0 });
    }

    #[test]
    fn one_notch_per_r3_press_suppresses_look_behind_until_release_and_holds_aim() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_learning_for_test();
        let now = Instant::now();
        let mut toggle = Toggle::default();
        assert_eq!(toggle.route(true, false, true, Some(true), unknown(), now).wheel, 0);
        let pressed = toggle.route(true, true, true, Some(true), unknown(), now);
        assert_eq!(pressed, Route { look_behind: false, wheel: INITIAL_SIGHTS_WHEEL });
        assert!(toggle.holds_aim(now + AIM_HOLD_AFTER_NOTCH - Duration::from_millis(1)));
        assert!(!toggle.holds_aim(now + AIM_HOLD_AFTER_NOTCH));
        // Held R3 is not a repeat and never leaks C, even after aim ends.
        assert_eq!(toggle.route(true, true, true, Some(true), unknown(), now), Route { look_behind: false, wheel: 0 });
        assert_eq!(toggle.route(true, true, false, Some(true), unknown(), now), Route { look_behind: false, wheel: 0 });
        assert_eq!(toggle.route(true, false, true, Some(true), unknown(), now), Route { look_behind: false, wheel: 0 });
        // Without a native read the model alternates: out, then in again.
        assert_eq!(toggle.route(true, true, true, Some(true), unknown(), now).wheel, -INITIAL_SIGHTS_WHEEL);
        toggle.route(true, false, true, Some(true), unknown(), now);
        assert_eq!(toggle.route(true, true, true, Some(true), unknown(), now).wheel, INITIAL_SIGHTS_WHEEL);
    }

    #[test]
    fn native_flag_chooses_direction_and_a_confirmed_change_locks_the_sign() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_learning_for_test();
        let now = Instant::now();
        let mut toggle = Toggle::default();
        // GTA's preference already left sights on: the first press leaves them.
        assert_eq!(toggle.route(true, true, true, Some(true), native(true), now).wheel, -INITIAL_SIGHTS_WHEEL);
        toggle.route(true, false, true, Some(true), native(false), now + Duration::from_millis(40));
        assert!(CONFIRMED.load(Ordering::Relaxed) > 0);
        assert_ne!(SIGN_LOCKED.load(Ordering::Relaxed), 0);
        assert_eq!(toggle.route(true, true, true, Some(true), native(false), now + Duration::from_millis(80)).wheel, INITIAL_SIGHTS_WHEEL);
        // A locked sign never flips, even if a later press is ignored natively.
        toggle.route(true, false, true, Some(true), native(false), now + Duration::from_millis(500));
        assert_eq!(SIGHTS_WHEEL.load(Ordering::Relaxed), INITIAL_SIGHTS_WHEEL);
    }

    #[test]
    fn unchanged_native_flag_tries_other_sign_but_interruptions_never_teach() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_learning_for_test();
        let now = Instant::now();
        let mut toggle = Toggle::default();
        assert_eq!(toggle.route(true, true, true, Some(true), native(false), now).wheel, INITIAL_SIGHTS_WHEEL);
        // Aim released inside the window: no learning, direction unchanged.
        toggle.route(true, false, false, Some(true), native(false), now + Duration::from_millis(10));
        toggle.route(true, false, true, Some(true), native(false), now + CONFIRM_WINDOW);
        assert_eq!(SIGHTS_WHEEL.load(Ordering::Relaxed), INITIAL_SIGHTS_WHEEL);
        // A full aimed window with no native change tries the other sign.
        assert_eq!(toggle.route(true, true, true, Some(true), native(false), now + CONFIRM_WINDOW).wheel, INITIAL_SIGHTS_WHEEL);
        toggle.route(true, false, true, Some(true), native(false), now + CONFIRM_WINDOW * 2);
        assert_eq!(SIGHTS_WHEEL.load(Ordering::Relaxed), -INITIAL_SIGHTS_WHEEL);
        let start = now + CONFIRM_WINDOW * 3;
        assert_eq!(toggle.route(true, true, true, Some(true), native(false), start).wheel, -INITIAL_SIGHTS_WHEEL);
        toggle.route(true, false, true, Some(true), native(true), start + Duration::from_millis(30));
        assert_ne!(SIGN_LOCKED.load(Ordering::Relaxed), 0);
        assert_eq!(toggle.route(true, true, true, Some(true), native(true), start + Duration::from_millis(60)).wheel, INITIAL_SIGHTS_WHEEL);
        reset_learning_for_test();
    }

    #[test]
    fn interruption_cancels_hold_pending_learning_and_a_held_click_is_not_an_edge() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_learning_for_test();
        let now = Instant::now();
        let mut toggle = Toggle::default();
        toggle.route(true, true, true, Some(true), native(false), now);
        toggle.interrupt(true);
        assert!(!toggle.holds_aim(now));
        assert!(toggle.pending.is_none());
        // R3 held through a menu/vehicle: no notch when gameplay resumes.
        assert_eq!(toggle.route(true, true, true, Some(true), native(false), now).wheel, 0);
        toggle.interrupt(false);
        assert_eq!(toggle.route(true, true, true, Some(true), native(false), now).wheel, INITIAL_SIGHTS_WHEEL);
        assert_eq!(SIGHTS_WHEEL.load(Ordering::Relaxed), INITIAL_SIGHTS_WHEEL);
    }

    #[test]
    fn view_mode_is_first_person_only_while_fresh() {
        let now = Instant::now();
        record_follow_ped_view_mode(4);
        assert_eq!(first_person_on_foot(now), Some(true));
        assert_eq!(first_person_on_foot(Instant::now() + VIEW_FRESHNESS + Duration::from_millis(1)), None);
        record_follow_ped_view_mode(1);
        assert_eq!(first_person_on_foot(Instant::now()), Some(false));
    }
}
