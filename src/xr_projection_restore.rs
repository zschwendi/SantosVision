//! Restore only a crop that XR still owns at a CURRENT primary-camera callback.
//! No saved guest pointer is ever dereferenced by this module. SetWindow's
//! current geometry proves the replacement crop independently of allocation
//! history; address equality is an ownership filter, not a lifetime certificate.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WindowGeometry {
    words: [u32; 14], // grcViewport +816..871, including current resolution.
    crop: [u32; 4],
}

impl WindowGeometry {
    pub(super) fn from_words(words: [u32; 14]) -> Option<Self> {
        let width = i32::try_from(words[12]).ok()?;
        let height = i32::try_from(words[13]).ok()?;
        if !(1..=65_536).contains(&width) || !(1..=65_536).contains(&height)
            || words[..12].iter().any(|word| !f32::from_bits(*word).is_finite()) {
            return None;
        }
        // Recover the integer arguments only when the exact SetWindow division
        // round-trips. Approximate normalized rectangles do not confer ownership.
        fn integer(word: u32, resolution: i32) -> Option<i32> {
            let value = f32::from_bits(word);
            let integer = (f64::from(value) * f64::from(resolution)).round();
            if !(-131_072.0..=131_072.0).contains(&integer) { return None; }
            let integer = integer as i32;
            ((integer as f32 / resolution as f32).to_bits() == word).then_some(integer)
        }
        let x = integer(words[6], width)?;
        let y = integer(words[7], height)?;
        let w = integer(words[8], width)?;
        let h = integer(words[9], height)?;
        if w <= 0 || h <= 0 { return None; }
        let left = x.max(0);
        let top = y.max(0);
        let clipped_w = (width - left).min(w + x.min(0)).max(0);
        let clipped_h = (height - top).min(h + y.min(0)).max(0);
        if clipped_w <= 0 || clipped_h <= 0 { return None; }
        let clipped = [left as f32 / width as f32, top as f32 / height as f32,
            clipped_h as f32 / height as f32, clipped_w as f32 / width as f32];
        if clipped.map(f32::to_bits) != words[..4] { return None; }
        // Exact f32 operation order from original SetWindow9612. Multiplication
        // by reciprocal, including signed zero on the offsets, is intentional.
        let inverse_w = 1.0f32 / clipped_w as f32;
        let inverse_h = 1.0f32 / clipped_h as f32;
        let mut offset_x = inverse_w * (w - clipped_w) as f32;
        let mut offset_y = inverse_h * (h - clipped_h) as f32;
        if x > 0 { offset_x = -offset_x; }
        if y < 0 { offset_y = -offset_y; }
        let crop = [inverse_w * w as f32, inverse_h * h as f32, offset_x, offset_y].map(f32::to_bits);
        Some(Self { words, crop })
    }
}

pub(super) fn same_matrix(a: &[f32; 16], b: &[f32; 16]) -> bool {
    a.map(f32::to_bits) == b.map(f32::to_bits)
}

/// The ORIGINAL guest RegeneratePerspective arithmetic, with the game's fresh
/// scalar near/far and depth direction. This is not native eye qualification:
/// ordinary game far ranges (e.g.10000m) must not pass projection_scalars().
pub(super) fn guest_projection(fields: [f32; 11], reverse: bool) -> Option<[f32; 16]> {
    let [near, far, crop_x, crop_y, offset_x, offset_y, _, tan_y, _, aspect, _] = fields;
    if fields.iter().any(|x| !x.is_finite()) || near <= 0. || far <= near
        || crop_x <= 0. || crop_y <= 0. || tan_y <= 0. || aspect <= 0. {
        return None;
    }
    let sy = 1.0f32 / tan_y;
    let z = far / (near - far);
    let translation = near * z;
    let projection = [sy / aspect * crop_x, 0., 0., 0., 0., sy * crop_y, 0., 0.,
        offset_x, offset_y, if reverse { -1.0f32 - z } else { z }, -1.,
        0., 0., if reverse { -translation } else { translation }, 0.];
    projection.iter().all(|v| v.is_finite()).then_some(projection)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Snapshot {
    pub store: u64,
    pub source: u64,
    pub geometry: WindowGeometry,
    pub fields: [f32; 11],
    pub projection: [f32; 16],
    pub reverse: bool,
}

impl Snapshot {
    /// A current original game lens, before any XR projection replacement.
    /// Custom/unexplained crops and non-native matrices confer no range authority.
    pub(super) fn native_far_plane(self) -> Option<f32> {
        (self.is_fresh_game_projection() && self.crop() == self.geometry.crop
            && (1. ..=20_000.).contains(&self.fields[1])).then_some(self.fields[1])
    }

    fn crop(self) -> [u32; 4] {
        let crop: [f32; 4] = self.fields[2..6].try_into().unwrap();
        crop.map(f32::to_bits)
    }

    pub(super) fn matches(self, other: Self) -> bool {
        self.store == other.store && self.source == other.source && self.geometry == other.geometry
            && self.reverse == other.reverse && self.fields.map(f32::to_bits) == other.fields.map(f32::to_bits)
            && same_matrix(&self.projection, &other.projection)
    }

    fn is_fresh_game_projection(self) -> bool {
        self.store != 0 && self.source != 0 && guest_projection(self.fields, self.reverse)
            .is_some_and(|projection| same_matrix(&projection, &self.projection))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Owner {
    store: u64,
    source: u64,
    geometry: WindowGeometry,
    last_crop: [u32; 4],
    reverse: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Restore {
    pub before: Snapshot,
    pub fields: [f32; 11],
    pub projection: [f32; 16],
}

impl Owner {
    /// Only take ownership from a legitimate current SetWindow crop, never a
    /// prior eye's contamination or an unexplained game-specific custom crop.
    pub(super) fn acquire(before: Snapshot, eye_fields: [f32; 11]) -> Option<Self> {
        if !before.is_fresh_game_projection() || before.crop() != before.geometry.crop
            || eye_fields.iter().any(|x| !x.is_finite()) { return None; }
        let crop: [f32; 4] = eye_fields[2..6].try_into().unwrap();
        Some(Self { store: before.store, source: before.source, geometry: before.geometry,
            last_crop: crop.map(f32::to_bits), reverse: before.reverse })
    }

    /// Caller consumes ownership even on rejection. Source+Store/window checks
    /// never authorize an arbitrary delayed pointer write: before is read from
    /// the source of a currently executing, certified camera callback.
    pub(super) fn restore(self, before: Snapshot) -> Option<Restore> {
        if self.store != before.store || self.source != before.source || self.geometry != before.geometry
            || self.reverse != before.reverse || self.last_crop != before.crop()
            || !before.is_fresh_game_projection() { return None; }
        let mut fields = before.fields;
        fields[2..6].copy_from_slice(&before.geometry.crop.map(f32::from_bits));
        Some(Restore { before, fields, projection: guest_projection(fields, before.reverse)? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(x: i32, y: i32, w: i32, h: i32, width: i32, height: i32) -> [u32; 14] {
        let left = x.max(0); let top = y.max(0);
        let clipped_w = (width - left).min(w + x.min(0)).max(0);
        let clipped_h = (height - top).min(h + y.min(0)).max(0);
        let mut words = [left as f32 / width as f32, top as f32 / height as f32,
            clipped_h as f32 / height as f32, clipped_w as f32 / width as f32,
            0., 1., x as f32 / width as f32, y as f32 / height as f32,
            w as f32 / width as f32, h as f32 / height as f32, 0., 1., 0., 0.].map(f32::to_bits);
        words[12] = width as u32; words[13] = height as u32; words
    }
    fn baseline() -> Snapshot {
        let geometry = WindowGeometry::from_words(geometry(0, 0, 1920, 1080, 1920, 1080)).unwrap();
        let fields = [0.05, 10_000., 1., 1., 0., 0., 50., 0.46630767, 0.8289914, 1.7777778, 1.7777778];
        Snapshot { store: 1, source: 4096, geometry, fields,
            projection: guest_projection(fields, false).unwrap(), reverse: false }
    }
    fn eye_fields() -> [f32; 11] {
        [0.1, 999.9341, 1., 1., -0.26794896, -0.08748719, 95.2383, 1.095875, 1.366025, 1.2465153, 1.2465153]
    }

    #[test]
    fn original_far_authority_requires_exact_native_lens_and_window_crop() {
        let original = baseline();
        assert_eq!(original.native_far_plane(), Some(10_000.));
        let mut extended = original; extended.fields[1] = 20_000.;
        extended.projection = guest_projection(extended.fields, false).unwrap();
        assert_eq!(extended.native_far_plane(), Some(20_000.));
        for far in [0., f32::INFINITY, 20_001., f32::NAN] {
            let mut invalid = original; invalid.fields[1] = far;
            assert!(invalid.native_far_plane().is_none());
        }
        let mut changed = original; changed.fields[4] = 0.1;
        changed.projection = guest_projection(changed.fields, false).unwrap();
        assert!(changed.native_far_plane().is_none(), "XR/custom crop is not original lens authority");
        changed = original; changed.projection[0] += 0.01;
        assert!(changed.native_far_plane().is_none(), "custom matrix is not native lens authority");
        changed = original; changed.store = 0;
        assert!(changed.native_far_plane().is_none());
    }
    fn next_camera(mut snapshot: Snapshot, eye: [f32; 11]) -> Snapshot {
        snapshot.fields[2..6].copy_from_slice(&eye[2..6]);
        snapshot.projection = guest_projection(snapshot.fields, snapshot.reverse).unwrap(); snapshot
    }

    #[test]
    fn enable_update_disable_and_reenable_preserve_fresh_game_lens() {
        let game = baseline(); let eye = eye_fields();
        let owner = Owner::acquire(game, eye).unwrap();
        let mut next = next_camera(game, eye);
        next.fields[0] = 0.15; next.fields[1] = 20_000.; next.fields[6] = 45.;
        next.fields[7] = 0.41421357; next.fields[8] = 0.7363797;
        next.projection = guest_projection(next.fields, next.reverse).unwrap();
        let restore = owner.restore(next).unwrap();
        assert_eq!(restore.fields[2..6], game.fields[2..6]);
        for i in [0, 1, 6, 7, 8, 9, 10] { assert_eq!(restore.fields[i].to_bits(), next.fields[i].to_bits()); }
        assert_eq!(restore.projection[8], 0.); assert_eq!(restore.projection[9], 0.);
        let clean = Snapshot { fields: restore.fields, projection: restore.projection, ..next };
        let second_owner = Owner::acquire(clean, eye).unwrap();
        assert!(second_owner.restore(next_camera(clean, eye)).is_some());
    }

    #[test]
    fn legitimate_window_crops_include_clipping_and_signed_zero() {
        for (x, y, w, h) in [(0, 0, 1920, 1080), (50, 25, 800, 600), (-100, -50, 1920, 1080),
                (100, 50, 1920, 1080), (0, 0, 3840, 2160)] {
            let g = WindowGeometry::from_words(geometry(x, y, w, h, 1920, 1080)).unwrap();
            let mut game = baseline(); game.geometry = g;
            game.fields[2..6].copy_from_slice(&g.crop.map(f32::from_bits));
            game.projection = guest_projection(game.fields, game.reverse).unwrap();
            let restored = Owner::acquire(game, eye_fields()).unwrap().restore(next_camera(game, eye_fields())).unwrap();
            assert_eq!(restored.fields.map(f32::to_bits), game.fields.map(f32::to_bits));
        }
        let g = WindowGeometry::from_words(geometry(50, -25, 800, 600, 1920, 1080)).unwrap();
        assert_eq!(g.crop[2], (-0.0f32).to_bits());
    }

    #[test]
    fn changed_game_crop_geometry_identity_or_depth_direction_drops_ownership() {
        let game = baseline(); let owner = Owner::acquire(game, eye_fields()).unwrap();
        let camera = next_camera(game, eye_fields());
        for mutation in 0..6 {
            let mut changed = camera;
            match mutation {
                0 => changed.store += 1,
                1 => changed.source += 16,
                2 => changed.geometry = WindowGeometry::from_words(geometry(0,0,1280,720,1280,720)).unwrap(),
                3 => changed.fields[4] = 0.125,
                4 => changed.fields[2] = f32::from_bits(changed.fields[2].to_bits() + 1),
                _ => changed.reverse = true,
            }
            changed.projection = guest_projection(changed.fields, changed.reverse).unwrap();
            assert!(owner.restore(changed).is_none(), "mutation{mutation}");
        }
        let mut custom = game; custom.fields[4] = 0.125;
        custom.projection = guest_projection(custom.fields, custom.reverse).unwrap();
        assert!(Owner::acquire(custom, eye_fields()).is_none());
    }

    #[test]
    fn reused_address_uses_current_semantics_or_rejects_without_writes() {
        let game = baseline(); let owner = Owner::acquire(game, eye_fields()).unwrap();
        // A fresh allocation at the same address has normal constructor crop:
        // it is not mistaken for the prior XR-owned words.
        assert!(owner.restore(game).is_none());
        // An exact copy at that address is safe by CURRENT SetWindow semantics,
        // irrespective of which allocation produced those identical words.
        let copied = next_camera(game, eye_fields());
        assert_eq!(owner.restore(copied).unwrap().fields[2..6], game.fields[2..6]);
    }

    #[test]
    fn malformed_or_nonwindow_geometry_and_stale_matrix_fail_closed() {
        let valid = geometry(0,0,1920,1080,1920,1080);
        for i in 0..14 {
            let mut words = valid;
            words[i] = if i >= 12 { 0 } else { f32::NAN.to_bits() };
            assert!(WindowGeometry::from_words(words).is_none());
        }
        let mut wrong_clip = valid; wrong_clip[3] = 0.5f32.to_bits();
        assert!(WindowGeometry::from_words(wrong_clip).is_none());
        let mut unroundtrippable = valid; unroundtrippable[6] = 0.1234567f32.to_bits();
        assert!(WindowGeometry::from_words(unroundtrippable).is_none());
        let game = baseline(); let owner = Owner::acquire(game, eye_fields()).unwrap();
        let mut next = next_camera(game, eye_fields()); next.projection[8] += 0.001;
        assert!(owner.restore(next).is_none());
    }

    #[test]
    fn original_guest_far_range_and_depth_are_separate_from_eye_request_bounds() {
        let game = baseline(); let p = guest_projection(game.fields, false).unwrap();
        assert_eq!(p[10].to_bits(), (-1.000005f32).to_bits());
        assert_eq!(p[14].to_bits(), (-0.05000025f32).to_bits());
        let reverse = guest_projection(game.fields, true).unwrap();
        assert_eq!(reverse[10].to_bits(), (-1.0f32 - p[10]).to_bits());
        assert_eq!(reverse[14].to_bits(), (-p[14]).to_bits());
        for index in [0,1,2,3,7,9] {
            let mut invalid = game.fields; invalid[index] = f32::NAN;
            assert!(guest_projection(invalid, false).is_none());
        }
    }
}
