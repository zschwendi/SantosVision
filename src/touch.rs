// muguet - touch.rs
// Constantin Clerc (c22dev) - 0.1

use crate::input;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;
use std::collections::{HashMap, HashSet};
use winit::event::{Touch, TouchPhase};
use winit::window::Window;

#[path = "touch_state.rs"]
mod state;

#[derive(Clone, Copy, PartialEq)]
enum Act {
    Key(&'static str),

    Mouse(u8),

    AimToggle,

    KeyNav(&'static str),
    NavToggle,

    Horn,
}

#[derive(Clone, Copy)]
enum Anchor {
    BottomRight(f64, f64),
    TopLeft(f64, f64),
    TopRight(f64, f64),

    Nav(f64, f64),
}

struct Spec {
    anchor: Anchor,
    radius: f64,
    on_foot: Option<(&'static str, Act)>,
    in_car: Option<(&'static str, Act)>,
    nav: bool,
}

const fn b(anchor: Anchor, radius: f64, on_foot: Option<(&'static str, Act)>, in_car: Option<(&'static str, Act)>) -> Spec {
    Spec { anchor, radius, on_foot, in_car, nav: false }
}
const fn n(x: f64, y: f64, label: &'static str, key: &'static str) -> Spec {
    Spec { anchor: Anchor::Nav(x, y), radius: 21.0, on_foot: Some((label, Act::Key(key))), in_car: Some((label, Act::Key(key))), nav: true }
}

use Act::*;
use Anchor::*;
const BUTTONS: &[Spec] = &[
    b(BottomRight(62.0, 68.0), 40.0, Some(("FIRE", Mouse(1))), Some(("GAS", Key("KeyW")))),
    b(BottomRight(158.0, 46.0), 31.0, Some(("AIM", AimToggle)), Some(("BRAKE", Key("KeyS")))),
    b(BottomRight(58.0, 168.0), 28.0, Some(("JUMP", Key("Space"))), Some(("H.BRAKE", Key("Space")))),
    b(BottomRight(140.0, 132.0), 26.0, Some(("ENTER", Key("KeyF"))), Some(("EXIT", Key("KeyF")))),
    b(BottomRight(222.0, 98.0), 24.0, Some(("RELOAD", Key("KeyR"))), Some(("HORN", Horn))),
    b(BottomRight(222.0, 166.0), 24.0, Some(("USE", Key("KeyE"))), Some(("FIRE", Mouse(1)))),
    b(BottomRight(222.0, 234.0), 24.0, Some(("ABILITY", Key("CapsLock"))), Some(("LIGHTS", Key("KeyH")))),
    b(TopRight(30.0, 30.0), 22.0, Some(("WEAPON", Key("Tab"))), Some(("WEAPON", Key("Tab")))),
    b(TopRight(84.0, 30.0), 22.0, Some(("COVER", Key("KeyQ"))), Some(("RADIO", Key("KeyQ")))),
    b(TopRight(138.0, 30.0), 22.0, Some(("CROUCH", Key("ControlLeft"))), Some(("LOOK\nBACK", Key("KeyC")))),
    b(TopRight(192.0, 30.0), 22.0, Some(("CAM", Key("KeyV"))), Some(("CAM", Key("KeyV")))),
    b(TopLeft(30.0, 30.0), 22.0, Some(("PAUSE", KeyNav("Escape"))), Some(("PAUSE", KeyNav("Escape")))),
    b(TopLeft(84.0, 30.0), 22.0, Some(("PHONE", KeyNav("ArrowUp"))), Some(("PHONE", KeyNav("ArrowUp")))),
    b(TopLeft(138.0, 30.0), 22.0, Some(("MENU\nKEYS", NavToggle)), Some(("MENU\nKEYS", NavToggle))),
    b(TopLeft(192.0, 30.0), 22.0, Some(("SWITCH", Key("AltLeft"))), Some(("SWITCH", Key("AltLeft")))),
    n(-145.0, 0.0, "◀", "ArrowLeft"),
    n(-87.0, 0.0, "▲", "ArrowUp"),
    n(-29.0, 0.0, "▼", "ArrowDown"),
    n(29.0, 0.0, "▶", "ArrowRight"),
    n(87.0, 0.0, "OK", "Enter"),
    n(145.0, 0.0, "BACK", "Backspace"),
    n(-145.0, 58.0, "TAB ◀", "KeyQ"),
    n(-87.0, 58.0, "TAB ▶", "KeyE"),
    n(-29.0, 58.0, "ALT", "Tab"),
    n(29.0, 58.0, "SPACE", "Space"),
    n(87.0, 58.0, "YES", "KeyY"),
    n(145.0, 58.0, "NO", "KeyN"),
];

const STICK_RADIUS: f64 = 58.0;
const KNOB_RADIUS: f64 = 26.0;

const LOOK_GAIN: f64 = 3.0;
const WHEEL_DEADZONE: f64 = 10.0;
const WHEEL_FULL: f64 = 45.0;
const WHEEL_PUSH: f64 = 40.0;
static TOUCH_SENS: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| std::env::var("MUGUET_TOUCH_SENS").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0));

enum Grip {
    Stick { origin: (f64, f64), at: (f64, f64) },
    Look { last: (f64, f64) },
    Button { idx: usize, start: (f64, f64), last: (f64, f64) },
}

struct Btn {
    view: Retained<AnyObject>,
    label: Retained<AnyObject>,
    center: (f64, f64),
    radius: f64,
}

struct Overlay {
    root: Retained<AnyObject>,
    buttons: Vec<Btn>,
    stick_base: Retained<AnyObject>,
    stick_knob: Retained<AnyObject>,
    stick_home: (f64, f64),

    safe: (f64, f64, f64, f64),
    scale: f64,
    grips: HashMap<u64, Grip>,
    input: state::HeldInput,
    aim_latched: bool,
    nav: bool,
    in_car: bool,
    hidden: bool,
    active: bool,
}

thread_local! {
    static OVERLAY: std::cell::RefCell<Option<Overlay>> = const { std::cell::RefCell::new(None) };
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
}

fn color(white: f64, alpha: f64) -> Retained<AnyObject> {
    unsafe { msg_send![class!(UIColor), colorWithWhite: white, alpha: alpha] }
}

fn circle(parent: &AnyObject, center: (f64, f64), r: f64, white: f64, alpha: f64) -> Retained<AnyObject> {
    unsafe {
        let v: Retained<AnyObject> = msg_send![class!(UIView), new];
        let _: () = msg_send![&*v, setFrame: rect(center.0 - r, center.1 - r, 2.0 * r, 2.0 * r)];
        let _: () = msg_send![&*v, setUserInteractionEnabled: false];
        let _: () = msg_send![&*v, setBackgroundColor: &*color(white, alpha)];
        let layer: *mut AnyObject = msg_send![&*v, layer];
        let _: () = msg_send![layer, setCornerRadius: r];
        let _: () = msg_send![parent, addSubview: &*v];
        v
    }
}

fn label(parent: &AnyObject, r: f64) -> Retained<AnyObject> {
    unsafe {
        let l: Retained<AnyObject> = msg_send![class!(UILabel), new];
        let _: () = msg_send![&*l, setFrame: rect(2.0, 0.0, 2.0 * r - 4.0, 2.0 * r)];
        let _: () = msg_send![&*l, setUserInteractionEnabled: false];
        let _: () = msg_send![&*l, setTextColor: &*color(1.0, 0.9)];
        let font: Retained<AnyObject> = msg_send![class!(UIFont), boldSystemFontOfSize: (r * 0.42).clamp(9.0, 15.0)];
        let _: () = msg_send![&*l, setFont: &*font];
        let _: () = msg_send![&*l, setTextAlignment: 1isize];
        let _: () = msg_send![&*l, setNumberOfLines: 2isize];
        let _: () = msg_send![&*l, setAdjustsFontSizeToFitWidth: true];
        let _: () = msg_send![parent, addSubview: &*l];
        l
    }
}

fn set_text(l: &AnyObject, text: &str) {
    unsafe { let _: () = msg_send![l, setText: &*NSString::from_str(text)]; }
}
fn set_hidden(v: &AnyObject, hidden: bool) {
    unsafe { let _: () = msg_send![v, setHidden: hidden]; }
}
fn set_alpha_bg(v: &AnyObject, alpha: f64) {
    unsafe { let _: () = msg_send![v, setBackgroundColor: &*color(1.0, alpha)]; }
}
fn move_to(v: &AnyObject, c: (f64, f64)) {
    unsafe { let _: () = msg_send![v, setCenter: CGPoint { x: c.0, y: c.1 }]; }
}

pub fn install(window: &Window) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::UiKit(h) = handle.as_raw() else { return };
    let view = unsafe { &*(h.ui_view.as_ptr() as *const AnyObject) };

    let scale = window.scale_factor();
    let full = window.outer_size().to_logical::<f64>(scale);
    let pos = window.inner_position().map(|p| p.to_logical::<f64>(scale)).unwrap_or_default();
    let inner = window.inner_size().to_logical::<f64>(scale);
    // UIKit owns the notch/home-indicator inset; the winit fallback is only
    // used before the safe-area guide has its first layout.
    let guide: Retained<AnyObject> = unsafe { msg_send![view, safeAreaLayoutGuide] };
    let safe_frame: CGRect = unsafe { msg_send![&*guide, layoutFrame] };
    let safe = if safe_frame.size.width > 0.0 && safe_frame.size.height > 0.0 {
        (safe_frame.origin.x, safe_frame.origin.y, safe_frame.size.width, safe_frame.size.height)
    } else { (pos.x, pos.y, inner.width, inner.height) };

    let root = unsafe {
        let v: Retained<AnyObject> = msg_send![class!(UIView), new];
        let _: () = msg_send![&*v, setFrame: rect(0.0, 0.0, full.width, full.height)];
        let _: () = msg_send![&*v, setUserInteractionEnabled: false];
        let _: () = msg_send![view, addSubview: &*v];
        v
    };
    let (sx, sy, sw, sh) = safe;
    let place = |a: Anchor| match a {
        BottomRight(dx, dy) => (sx + sw - dx, sy + sh - dy),
        TopLeft(dx, dy) => (sx + dx, sy + dy),
        TopRight(dx, dy) => (sx + sw - dx, sy + dy),
        Nav(dx, dy) => (sx + sw / 2.0 + dx, sy + 94.0 + dy),
    };
    let buttons = BUTTONS
        .iter()
        .map(|s| {
            let center = place(s.anchor);
            let view = circle(&root, center, s.radius, 1.0, 0.18);
            let label = label(&view, s.radius);
            Btn { view, label, center, radius: s.radius }
        })
        .collect();
    let stick_home = (sx + 40.0 + STICK_RADIUS, sy + sh - 30.0 - STICK_RADIUS);
    let stick_base = circle(&root, stick_home, STICK_RADIUS, 1.0, 0.12);
    let stick_knob = circle(&root, stick_home, KNOB_RADIUS, 1.0, 0.3);
    let mut o = Overlay {
        root, buttons, stick_base, stick_knob, stick_home, safe, scale,
        grips: HashMap::new(), input: state::HeldInput::default(), aim_latched: false,
        nav: false, in_car: false, hidden: true, active: false,
    };
    o.relabel();
    set_hidden(&o.root, true);
    eprintln!("[touch] overlay ready ({:.0}x{:.0} pt, safe area {:.0},{:.0} {:.0}x{:.0})", full.width, full.height, sx, sy, sw, sh);
    OVERLAY.with(|cell| *cell.borrow_mut() = Some(o));
}

impl Overlay {
    fn is_wheel(&self, idx: usize) -> bool {
        if self.nav { return false; }
        match self.spec_act(idx).map(|(_, a)| a) {
            Some(Key("Tab")) | Some(Key("AltLeft")) => true,
            Some(Key("KeyQ")) => self.in_car,
            _ => false,
        }
    }

    fn spec_act(&self, idx: usize) -> Option<(&'static str, Act)> {
        let s = &BUTTONS[idx];
        if s.nav && !self.nav { return None; }
        // The navigation tray replaces gameplay actions instead of stacking
        // hit targets over them on a phone's short landscape display.
        if self.nav && matches!(s.anchor, BottomRight(..)) { return None; }
        if self.in_car { s.in_car } else { s.on_foot }
    }

    fn relabel(&mut self) {
        for i in 0..self.buttons.len() {
            let act = self.spec_act(i);
            let b = &self.buttons[i];
            set_hidden(&b.view, act.is_none());
            if let Some((text, _)) = act { set_text(&b.label, text); }
        }
        self.restyle();
    }

    fn restyle(&self) {
        let pressed: HashSet<usize> = self.grips.values().filter_map(|g| if let Grip::Button { idx, .. } = g { Some(*idx) } else { None }).collect();
        for (i, b) in self.buttons.iter().enumerate() {
            let latched = self.aim_latched && matches!(self.spec_act(i), Some((_, AimToggle)))
                || self.nav && matches!(self.spec_act(i), Some((_, NavToggle)));
            set_alpha_bg(&b.view, if pressed.contains(&i) { 0.45 } else if latched { 0.35 } else { 0.18 });
        }
    }

    fn button_at(&self, p: (f64, f64)) -> Option<usize> {
        (0..self.buttons.len()).filter(|&i| self.spec_act(i).is_some()).find(|&i| {
            let b = &self.buttons[i];
            let r = b.radius + 8.0;
            (p.0 - b.center.0).powi(2) + (p.1 - b.center.1).powi(2) <= r * r
        })
    }

    fn in_stick_zone(&self, p: (f64, f64)) -> bool {
        let (sx, sy, sw, _) = self.safe;
        p.0 < sx + sw * 0.4 && p.1 > sy + 70.0
    }

    fn touch(&mut self, t: &Touch) {
        let p = (t.location.x / self.scale, t.location.y / self.scale);
        match t.phase {
            TouchPhase::Started => {
                if let Some(idx) = self.button_at(p) {
                    self.grips.insert(t.id, Grip::Button { idx, start: p, last: p });
                    match self.spec_act(idx).map(|(_, a)| a) {
                        Some(AimToggle) => self.aim_latched = !self.aim_latched,
                        Some(NavToggle) => { self.nav = !self.nav; self.relabel(); }
                        Some(KeyNav(_)) if !self.nav => { self.nav = true; self.relabel(); }
                        _ => {}
                    }
                } else if self.in_stick_zone(p) && !self.grips.values().any(|g| matches!(g, Grip::Stick { .. })) {
                    self.grips.insert(t.id, Grip::Stick { origin: p, at: p });
                } else {
                    self.grips.insert(t.id, Grip::Look { last: p });
                }
            }
            TouchPhase::Moved => {
                let wheel = matches!(self.grips.get(&t.id), Some(Grip::Button { idx, .. }) if self.is_wheel(*idx));
                match self.grips.get_mut(&t.id) {
                    Some(Grip::Stick { at, .. }) => *at = p,
                    Some(Grip::Button { last, .. }) if wheel => *last = p,
                    Some(Grip::Look { last }) | Some(Grip::Button { last, .. }) => {
                        let (dx, dy) = (p.0 - last.0, p.1 - last.1);
                        *last = p;

                        let speed = (dx * dx + dy * dy).sqrt();
                        let gain = LOOK_GAIN * *TOUCH_SENS * (1.0 + (speed / 12.0).min(1.5));
                        input::mouse_delta_f(dx * gain, dy * gain);
                    }
                    None => {}
                }
            }
            TouchPhase::Ended => { self.grips.remove(&t.id); }
            TouchPhase::Cancelled => { self.release_all(); return; }
        }
        self.apply();
    }

    fn apply(&mut self) {
        let mut keys: HashSet<&'static str> = HashSet::new();
        let mut mouse = if self.aim_latched && !self.in_car && !self.nav { 2u8 } else { 0 };
        let mut stick = None;
        let mut horn = false;
        for g in self.grips.values() {
            match g {
                Grip::Stick { origin, at } => stick = Some((*origin, *at)),
                Grip::Button { idx, .. } => match self.spec_act(*idx).map(|(_, a)| a) {
                    Some(Key(k)) | Some(KeyNav(k)) => { keys.insert(k); }
                    Some(Mouse(bit)) => mouse |= bit,
                    Some(Horn) => horn = true,
                    _ => {}
                },
                Grip::Look { .. } => {}
            }
        }
        if let Some((o, at)) = stick {
            let (mut dx, mut dy) = (at.0 - o.0, at.1 - o.1);
            let len = (dx * dx + dy * dy).sqrt();
            let k = if len > STICK_RADIUS { STICK_RADIUS / len } else { 1.0 };
            dx *= k;
            dy *= k;
            let (nx, ny) = (dx / STICK_RADIUS, dy / STICK_RADIUS);
            if self.nav {
                if nx < -0.35 { keys.insert("ArrowLeft"); }
                if nx > 0.35 { keys.insert("ArrowRight"); }
                if ny < -0.35 { keys.insert("ArrowUp"); }
                if ny > 0.35 { keys.insert("ArrowDown"); }
            } else if self.in_car {
                if nx < -0.35 { keys.insert("KeyA"); }
                if nx > 0.35 { keys.insert("KeyD"); }
                if ny < -0.6 { keys.insert("Numpad8"); }
                if ny > 0.6 { keys.insert("Numpad2"); }
            } else {
                if nx < -0.35 { keys.insert("KeyA"); }
                if nx > 0.35 { keys.insert("KeyD"); }
                if ny < -0.35 { keys.insert("KeyW"); }
                if ny > 0.35 { keys.insert("KeyS"); }
                if len > STICK_RADIUS * 0.95 { keys.insert("ShiftLeft"); }
            }
            move_to(&self.stick_base, o);
            move_to(&self.stick_knob, (o.0 + dx, o.1 + dy));
        } else {
            move_to(&self.stick_base, self.stick_home);
            move_to(&self.stick_knob, self.stick_home);
        }
        crate::pad::HORN_TOUCH.store(horn, std::sync::atomic::Ordering::Relaxed);
        emit(self.input.update(keys, mouse, std::time::Instant::now()));
        self.restyle();
    }

    fn release_all(&mut self) {
        self.grips.clear();
        self.aim_latched = false;
        self.nav = false;
        crate::pad::HORN_TOUCH.store(false, std::sync::atomic::Ordering::Relaxed);
        emit(self.input.release());
        move_to(&self.stick_base, self.stick_home);
        move_to(&self.stick_knob, self.stick_home);
        self.relabel();
    }
}

pub fn touch(t: &Touch) {
    OVERLAY.with(|cell| {
        if let Some(o) = cell.borrow_mut().as_mut() {
            if !o.hidden && o.active { o.touch(t); }
        }
    });
}

fn emit(edges: state::Edges) {
    for (key, pressed) in edges.keys {
        if pressed { crate::mp::key_pressed(key); }
        let text = if pressed { match key { "KeyY" => Some("y"), "KeyN" => Some("n"), _ => None } } else { None };
        input::key(key, pressed, false, text, false);
    }
    for (button, pressed) in edges.buttons { input::button(button, pressed); }
}

/// Queried on the UI event loop, before a physical device emits its input.
/// Presence, not a recent key press, controls the overlay's visibility.
fn external_devices() -> (bool, bool) {
    let controller = unsafe { objc2_game_controller::GCController::controllers().iter()
        .any(|controller| controller.extendedGamepad().is_some()) };
    let keyboard: Option<Retained<AnyObject>> = unsafe { msg_send![class!(GCKeyboard), coalescedKeyboard] };
    (controller, keyboard.is_some())
}

pub fn refresh() -> bool {
    let (controller, keyboard) = external_devices();
    OVERLAY.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(o) = guard.as_mut() else { return false };
        let hide = !state::visible(o.active, crate::gpu::SUSPENDED.load(std::sync::atomic::Ordering::Relaxed), controller, keyboard);
        if hide != o.hidden {
            o.hidden = hide;
            if hide { o.release_all(); }
            set_hidden(&o.root, hide);
        }
        if hide { return false; }
        input::set_locked(true);
        if o.input.lingering { o.apply(); }
        let mut wheeling = false;
        for g in o.grips.values() {
            if let Grip::Button { idx, start, last } = g {
                if !o.is_wheel(*idx) {
                    continue;
                }
                wheeling = true;
                let (dx, dy) = (last.0 - start.0, last.1 - start.1);
                let len = (dx * dx + dy * dy).sqrt();
                if len > WHEEL_DEADZONE {
                    let push = WHEEL_PUSH * ((len - WHEEL_DEADZONE) / WHEEL_FULL).min(1.0);
                    input::mouse_delta_f(dx / len * push, dy / len * push);
                }
            }
        }
        let in_car = crate::pad::IN_VEHICLE.load(std::sync::atomic::Ordering::Relaxed);
        if in_car != o.in_car && !o.hidden {
            o.grips.retain(|_, g| !matches!(g, Grip::Button { .. }));
            o.in_car = in_car;
            if in_car { o.aim_latched = false; }
            o.apply();
            o.relabel();
        }
        o.input.lingering || wheeling
    })
}

/// Losing focus, backgrounding or returning to native UI cancels immediately.
pub fn set_active(active: bool) {
    OVERLAY.with(|cell| {
        if let Some(o) = cell.borrow_mut().as_mut() {
            o.active = active;
            if !active {
                o.release_all();
                o.hidden = true;
                set_hidden(&o.root, true);
            }
        }
    });
    if active { refresh(); }
}

pub fn resize(window: &Window) {
    let active = OVERLAY.with(|cell| {
        let mut previous = cell.borrow_mut().take();
        if let Some(o) = previous.as_mut() {
            o.release_all();
            unsafe { let _: () = msg_send![&*o.root, removeFromSuperview]; }
            o.active
        } else { false }
    });
    install(window);
    set_active(active);
}
