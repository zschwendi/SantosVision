//! Touch-owned input edges. Short taps survive a game tick; cancellation never
//! waits for that minimum hold, so a newly connected device starts neutral.
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const MIN_HOLD: Duration = Duration::from_millis(120);

#[derive(Default, Debug, PartialEq)]
pub struct Edges {
    pub keys: Vec<(&'static str, bool)>,
    pub buttons: Vec<(usize, bool)>,
}

#[derive(Default)]
pub struct HeldInput {
    keys: HashSet<&'static str>,
    since: HashMap<&'static str, Instant>,
    mouse: u8,
    mouse_since: [Option<Instant>; 2],
    pub lingering: bool,
}

impl HeldInput {
    pub fn update(&mut self, mut keys: HashSet<&'static str>, mut mouse: u8, now: Instant) -> Edges {
        let mut edges = Edges::default();
        let young = |t: Option<&Instant>| t.is_some_and(|t| now.saturating_duration_since(*t) < MIN_HOLD);
        self.lingering = false;
        for &key in &self.keys {
            if keys.contains(key) { continue; }
            if young(self.since.get(key)) {
                keys.insert(key);
                self.lingering = true;
            } else {
                edges.keys.push((key, false));
                self.since.remove(key);
            }
        }
        for &key in keys.difference(&self.keys) {
            edges.keys.push((key, true));
            self.since.insert(key, now);
        }
        self.keys = keys;
        for (i, (bit, button)) in [(1, 0), (2, 2)].into_iter().enumerate() {
            if self.mouse & bit != 0 && mouse & bit == 0 && young(self.mouse_since[i].as_ref()) {
                mouse |= bit;
                self.lingering = true;
            }
            if (mouse ^ self.mouse) & bit != 0 {
                let pressed = mouse & bit != 0;
                edges.buttons.push((button, pressed));
                self.mouse_since[i] = pressed.then_some(now);
            }
        }
        self.mouse = mouse;
        edges
    }

    pub fn release(&mut self) -> Edges {
        let keys = self.keys.drain().map(|key| (key, false)).collect();
        let buttons = [(1, 0), (2, 2)].into_iter()
            .filter_map(|(bit, button)| (self.mouse & bit != 0).then_some((button, false))).collect();
        self.since.clear();
        self.mouse_since = [None; 2];
        self.mouse = 0;
        self.lingering = false;
        Edges { keys, buttons }
    }
}

pub fn visible(active: bool, suspended: bool, controller: bool, keyboard: bool) -> bool {
    active && !suspended && !controller && !keyboard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_survives_tick_but_releases_once() {
        let mut input = HeldInput::default();
        let t = Instant::now();
        assert_eq!(input.update(HashSet::from(["Space"]), 1, t),
            Edges { keys: vec![("Space", true)], buttons: vec![(0, true)] });
        assert_eq!(input.update(HashSet::new(), 0, t + Duration::from_millis(20)), Edges::default());
        assert!(input.lingering);
        assert_eq!(input.update(HashSet::new(), 0, t + MIN_HOLD),
            Edges { keys: vec![("Space", false)], buttons: vec![(0, false)] });
        assert!(!input.lingering);
        assert_eq!(input.update(HashSet::new(), 0, t + MIN_HOLD), Edges::default());
    }

    #[test]
    fn hide_cancel_or_suspend_releases_young_keys_fire_and_aim_immediately() {
        let mut input = HeldInput::default();
        let t = Instant::now();
        input.update(HashSet::from(["KeyW", "ShiftLeft"]), 3, t);
        input.update(HashSet::new(), 0, t + Duration::from_millis(1));
        let released = input.release();
        assert_eq!(released.keys.into_iter().collect::<HashSet<_>>(), HashSet::from([("KeyW", false), ("ShiftLeft", false)]));
        assert_eq!(released.buttons, vec![(0, false), (2, false)]);
        assert!(!input.lingering);
        assert_eq!(input.release(), Edges::default());
        assert_eq!(input.update(HashSet::new(), 0, t + MIN_HOLD), Edges::default());
        assert_eq!(input.update(HashSet::from(["KeyW"]), 0, t + MIN_HOLD).keys, vec![("KeyW", true)]);
    }

    #[test]
    fn connection_and_app_lifecycle_never_leave_touch_enabled() {
        assert!(visible(true, false, false, false));
        assert!(!visible(true, false, true, false));
        assert!(!visible(true, false, false, true));
        assert!(!visible(true, false, true, true));
        assert!(!visible(false, false, false, false));
        assert!(!visible(true, true, false, false));
        assert!(visible(true, false, false, false)); // both external devices removed
    }
}
