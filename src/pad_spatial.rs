//! Plain controller sample shared by the established gamepad mapper and the
//! two independently connected spatial-controller halves. No pose is applied.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Sample {
    pub left_stick: [f32; 2],
    pub right_stick: [f32; 2],
    pub left_trigger: f32,
    pub right_trigger: f32,
    // Cross, Circle, Square, Triangle, left grip/shoulder, right grip/shoulder.
    pub face_and_shoulders: u8,
    pub left_click: bool,
    pub right_click: bool,
    pub menu: bool,
    pub camera: bool,
    // Up, Down, Left, Right. Spatial controllers have no physical D-pad.
    pub dpad: u8,
    pub spatial: Option<SpatialIdentity>,
}
impl Sample {
    pub fn neutral(&self) -> bool {
        self.left_stick.iter().chain(self.right_stick.iter()).all(|v| v.abs() < 0.3)
            && self.left_trigger < 0.2 && self.right_trigger < 0.2
            && self.face_and_shoulders == 0 && !self.left_click && !self.right_click
            && !self.menu && !self.camera && self.dpad == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SpatialIdentity {
    pub hands: u32,
    pub left_device: u64,
}

fn axis(value: f32) -> f32 {
    if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 }
}

fn trigger(value: f32) -> f32 {
    if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 }
}

/// ABI v1: hand bits left=1/right=2; six floats LX,LY,RX,RY,LT,RT;
/// buttons: right A/B, left A/B, left/right grip, left/right stick click,
/// left/right Menu. Never carry values from a missing half into the sample.
pub(super) fn spatial_sample(hands: u32, buttons: u32, values: [f32; 6], left_device: u64) -> Option<Sample> {
    if hands & !3 != 0 || hands == 0 { return None; }
    let left = hands & 1 != 0;
    let right = hands & 2 != 0;
    let mut sample = Sample::default();
    sample.spatial = Some(SpatialIdentity { hands, left_device });
    if left {
        sample.left_stick = [axis(values[0]), axis(values[1])];
        sample.left_trigger = trigger(values[4]);
        sample.face_and_shoulders |= ((buttons >> 2) & 3) as u8 * 4;
        sample.face_and_shoulders |= ((buttons >> 4) & 1) as u8 * 16;
        sample.left_click = buttons & (1 << 6) != 0;
        // The left Create/Menu control replaces the absent touchpad for camera.
        sample.camera = buttons & (1 << 8) != 0;
    }
    if right {
        sample.right_stick = [axis(values[2]), axis(values[3])];
        sample.right_trigger = trigger(values[5]);
        sample.face_and_shoulders |= (buttons & 3) as u8;
        sample.face_and_shoulders |= ((buttons >> 5) & 1) as u8 * 32;
        sample.right_click = buttons & (1 << 7) != 0;
        sample.menu = buttons & (1 << 9) != 0;
    }
    Some(sample)
}

/// Sense has no D-pad. Holding L3 reserves the left stick for D-pad actions;
/// releasing an unused L3 press emits a 120 ms normal click. State belongs to
/// the input lifecycle, and never survives a lost/replaced left controller.
#[derive(Default)]
pub(super) struct Router {
    left_device: Option<u64>,
    require_release: bool,
    click_held: bool,
    used_direction: bool,
    tap_until: Option<Instant>,
    camera_chord: bool,
    camera_until: Option<Instant>,
    right_release_required: bool,
}

impl Router {
    pub fn apply(&mut self, mut sample: Sample, now: Instant) -> Sample {
        let left_device = sample.spatial.filter(|id| id.hands & 1 != 0).map(|id| id.left_device);
        if left_device != self.left_device {
            *self = Self { left_device, require_release: true, ..Self::default() };
        }
        // Standard controllers keep their original L3 and D-pad meanings.
        if sample.spatial.is_none() { return sample; }
        if left_device.is_none() {
            *self = Self::default();
            return sample;
        }
        let click = sample.left_click;
        sample.left_click = false;
        if self.require_release {
            self.require_release = click;
            if click { sample.left_stick = [0.0; 2]; }
            return sample;
        }
        // Both stick clicks are always available on Sense and replace touchpad.
        // Consume both normal clicks, never emit crouch/horn/rear-view afterward.
        let camera_chord = click && sample.right_click;
        if camera_chord && !self.camera_chord {
            self.camera_until = Some(now + Duration::from_millis(120));
            self.right_release_required = true;
        }
        self.camera_chord = camera_chord;
        if self.right_release_required {
            self.right_release_required = sample.right_click;
            sample.right_click = false;
        }
        if camera_chord || self.camera_until.is_some_and(|until| now < until) {
            self.used_direction = true;
            self.click_held = click;
            self.tap_until = None;
            sample.camera = true;
            sample.left_stick = [0.0; 2];
            sample.left_click = false;
            return sample;
        }
        if click {
            if !self.click_held {
                self.used_direction = false;
                self.tap_until = None;
            }
            let [x, y] = sample.left_stick;
            let mut directions = 0;
            if y > 0.3 { directions |= 1; }
            if y < -0.3 { directions |= 2; }
            if x < -0.3 { directions |= 4; }
            if x > 0.3 { directions |= 8; }
            sample.dpad = directions;
            sample.left_stick = [0.0; 2];
            self.used_direction |= directions != 0;
        } else if self.click_held && !self.used_direction {
            self.tap_until = Some(now + Duration::from_millis(120));
        }
        self.click_held = click;
        sample.left_click = !click && self.tap_until.is_some_and(|until| now < until);
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_halves_route_physical_faces_sticks_triggers_and_menus() {
        let values = [-0.75, 0.5, 0.25, -0.8, 0.9, 0.6];
        for bit in 0..10 {
            let sample = spatial_sample(3, 1 << bit, values, 42).unwrap();
            assert_eq!(sample.left_stick, [-0.75, 0.5]);
            assert_eq!(sample.right_stick, [0.25, -0.8]);
            assert_eq!((sample.left_trigger, sample.right_trigger), (0.9, 0.6));
            assert_eq!(sample.face_and_shoulders, if bit < 6 { 1 << bit } else { 0 });
            assert_eq!(sample.left_click, bit == 6);
            assert_eq!(sample.right_click, bit == 7);
            assert_eq!(sample.camera, bit == 8);
            assert_eq!(sample.menu, bit == 9);
            assert_eq!(sample.dpad, 0);
        }
    }

    #[test]
    fn disconnect_neutralizes_only_the_missing_half_including_held_inputs() {
        let values = [1.0; 6];
        let all = (1 << 10) - 1;
        let left = spatial_sample(1, all, values, 42).unwrap();
        assert_eq!(left.face_and_shoulders, 4 | 8 | 16);
        assert_eq!(left.right_stick, [0.0; 2]);
        assert_eq!(left.right_trigger, 0.0);
        assert!(!left.right_click && !left.menu);
        let right = spatial_sample(2, all, values, 0).unwrap();
        assert_eq!(right.face_and_shoulders, 1 | 2 | 32);
        assert_eq!(right.left_stick, [0.0; 2]);
        assert_eq!(right.left_trigger, 0.0);
        assert!(!right.left_click && !right.camera);
        assert_eq!(spatial_sample(0, all, values, 0), None);
    }

    #[test]
    fn malformed_or_nonfinite_snapshot_cannot_generate_stuck_motion() {
        assert_eq!(spatial_sample(4, u32::MAX, [1.0; 6], 42), None);
        let sample = spatial_sample(3, 0, [f32::NAN, f32::INFINITY, -2.0, 2.0, -1.0, 2.0], 42).unwrap();
        assert_eq!(sample.left_stick, [0.0, 0.0]);
        assert_eq!(sample.right_stick, [-1.0, 1.0]);
        assert_eq!((sample.left_trigger, sample.right_trigger), (0.0, 1.0));
    }

    fn left(click: bool, stick: [f32; 2], device: u64) -> Sample {
        let mut sample = spatial_sample(1, 0, [stick[0], stick[1], 0.0, 0.0, 0.0, 0.0], device).unwrap();
        sample.left_click = click;
        sample
    }

    #[test]
    fn modifier_routes_direction_without_moving_or_clicking_the_player() {
        let now = Instant::now();
        let mut router = Router::default();
        router.apply(left(false, [0.0; 2], 42), now);
        let held = router.apply(left(true, [0.8, -0.9], 42), now);
        assert_eq!(held.dpad, 2 | 8);
        assert_eq!(held.left_stick, [0.0; 2]);
        assert!(!held.left_click);
        let released = router.apply(left(false, [0.0; 2], 42), now);
        assert_eq!(released.dpad, 0);
        assert!(!released.left_click, "a directional chord must never emit L3 afterward");
    }

    #[test]
    fn unmodified_l3_tap_is_one_bounded_pulse_and_normal_motion_survives() {
        let now = Instant::now();
        let mut router = Router::default();
        assert_eq!(router.apply(left(false, [0.0, 0.9], 42), now).left_stick, [0.0, 0.9]);
        assert!(!router.apply(left(true, [0.0; 2], 42), now).left_click);
        assert!(router.apply(left(false, [0.0; 2], 42), now).left_click);
        assert!(router.apply(left(false, [0.0; 2], 42), now + Duration::from_millis(119)).left_click);
        assert!(!router.apply(left(false, [0.0; 2], 42), now + Duration::from_millis(120)).left_click);
        assert!(!router.apply(left(false, [0.0; 2], 42), now + Duration::from_millis(200)).left_click);
    }

    #[test]
    fn both_clicks_cycle_camera_without_crouch_horn_rearview_or_delayed_l3() {
        let now = Instant::now();
        let mut router = Router::default();
        router.apply(left(false, [0.0; 2], 42), now);
        let mut chord = left(true, [0.0; 2], 42);
        chord.right_click = true;
        let routed = router.apply(chord, now);
        assert!(routed.camera);
        assert!(!routed.left_click && !routed.right_click);
        let routed = router.apply(left(false, [0.0; 2], 42), now + Duration::from_millis(121));
        assert!(!routed.camera && !routed.left_click && !routed.right_click);
    }

    #[test]
    fn disconnect_reconnect_backend_switch_and_new_identity_cancel_pending_actions() {
        let now = Instant::now();
        let mut router = Router::default();
        router.apply(left(false, [0.0; 2], 42), now);
        router.apply(left(true, [0.0; 2], 42), now);
        let right = spatial_sample(2, 0, [0.0; 6], 0).unwrap();
        assert!(!router.apply(right, now).left_click);
        assert!(!router.apply(left(false, [0.0; 2], 42), now).left_click);
        router.apply(left(true, [0.0; 2], 42), now);
        assert!(!router.apply(left(false, [0.0; 2], 99), now).left_click);
        router.apply(left(true, [0.0; 2], 99), now);
        let standard = Sample { left_click: true, dpad: 4, ..Sample::default() };
        assert_eq!(router.apply(standard, now), standard);
        // A held click at connection/resume is blocked until released.
        let reconnect = router.apply(left(true, [0.0, 1.0], 99), now);
        assert!(!reconnect.left_click);
        assert_eq!(reconnect.left_stick, [0.0; 2]);
        assert_eq!(reconnect.dpad, 0);
        assert!(!router.apply(left(false, [0.0; 2], 99), now).left_click);
    }
}
