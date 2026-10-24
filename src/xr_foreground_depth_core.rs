//! Std-only core of first-person foreground depth attribution. No guest memory,
//! wgpu object, environment or global lives here. The crate includes this file
//! from xr_foreground_depth.rs; it also compiles standalone:
//! `rustc --edition 2021 --test src/xr_foreground_depth_core.rs`.
//!
//! Producer: after original 44415/44355 completed their first-person model and
//! the host corrected it (src/xr_viewmodel.rs), the invocation's draws are
//! rasterised as view*M' = inverse(R)*S*inverse(O) through the zero-translation
//! clone. That equals the displaced parent eye inverse(B0*R) applied to the
//! native placement B0*S*inverse(O), with the parent's own projection. Only that
//! exact equivalence (bitwise camera basis, projection and projection state) may
//! attribute the invocation's emissions to the parent view. The attribution is
//! a generation, never a mutable "first person now" flag.
//!
//! GPU: actual foreground depth is either written into the live qualified world
//! depth (combined natively), or written after the pre-UI world copy was frozen.
//! The second case keeps the frozen world wherever the foreground did not write
//! and takes the actual foreground depth where it did; "where it did" is the
//! exact bitwise difference between the depth target immediately before the
//! first and after the last attributed write, with no foreign writer between.
#![allow(dead_code)]

pub const STREAM_PED: u32 = 44415;
pub const SKINNED: u32 = 44355;
/// Mat34 coordinates; the w words of each column are padding.
pub const CAMERA_COORDINATES: [usize; 12] = [0, 1, 2, 4, 5, 6, 8, 9, 10, 12, 13, 14];
const BASIS: [usize; 9] = [0, 1, 2, 4, 5, 6, 8, 9, 10];

pub fn family_bit(family: u32) -> u32 {
    match family { STREAM_PED => 1, SKINNED => 2, _ => 0 }
}

/// Words read at the completed-model seam, as bits. Offsets are grcViewport
/// +192 camera, +320 projection, +872 eleven projection scalars, +916 scalar
/// state byte and +928 reverse-Z byte, as certified for xr_view's setters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeamWords {
    pub parent_camera: [u32; 16],
    pub parent_projection: [u32; 16],
    pub parent_scalars: [u32; 11],
    pub parent_flags: [u8; 2],
    pub clone_camera: [u32; 16],
    pub clone_projection: [u32; 16],
    pub clone_scalars: [u32; 11],
    pub clone_flags: [u8; 2],
    pub model: [u32; 16],
}

/// Bits of the view the producer had current before SetCurrent(clone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterKey { pub camera: [u32; 16], pub projection: [u32; 16] }

/// What every later emission of the invocation must still observe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Certificate { pub clone_camera: [u32; 12], pub projection: [u32; 16], pub model: [u32; 12] }

pub fn coordinates(words: &[u32; 16]) -> [u32; 12] { CAMERA_COORDINATES.map(|index| words[index]) }

/// Exact, bitwise: the clone is the displaced parent's basis at the origin with
/// the identical projection and projection state, and the parent is still the
/// view whose camera/projection the GPU commands were tagged with.
pub fn certify_clone(words: &SeamWords, parent_view: &RasterKey) -> Result<Certificate, &'static str> {
    if coordinates(&words.parent_camera) != coordinates(&parent_view.camera) { return Err("parent_camera_is_not_displaced_view"); }
    if words.parent_projection != parent_view.projection { return Err("parent_projection_is_not_displaced_view"); }
    if BASIS.iter().any(|&index| words.clone_camera[index] != words.parent_camera[index])
        || [12, 13, 14].iter().any(|&index| words.clone_camera[index] != 0) { return Err("clone_is_not_zeroed_parent_basis"); }
    if words.clone_projection != words.parent_projection { return Err("clone_projection_differs_from_parent"); }
    if words.clone_scalars != words.parent_scalars || words.clone_flags != words.parent_flags {
        return Err("clone_projection_state_differs_from_parent");
    }
    if CAMERA_COORDINATES.iter().any(|&index| !f32::from_bits(words.model[index]).is_finite()) { return Err("corrected_model_nonfinite"); }
    Ok(Certificate { clone_camera: coordinates(&words.clone_camera), projection: words.clone_projection, model: coordinates(&words.model) })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Binding<V> {
    pub generation: u64,
    pub family: u32,
    pub epoch: u64,
    pub store: u64,
    pub pthread: u64,
    pub clone: u64,
    pub parent: u64,
    pub frame: u64,
    pub view: V,
    pub certificate: Certificate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// Original SetCurrent(parent) continuation.
    Balanced,
    /// Any other SetCurrent: later emissions are no longer this invocation's clone.
    OtherSetCurrent,
    /// A guest-call boundary entered before the binding exited (normal, error,
    /// LongJmp or panic) without the original restoration.
    Boundary,
    Epoch,
    /// A second seam before any SetCurrent; both fail closed.
    Superseded,
    Mismatch(&'static str),
}

#[derive(Clone, Copy, Debug)]
struct Current<V> { epoch: u64, pointer: u64, view: Option<V>, displaced_pointer: u64, displaced_view: Option<V> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryToken { floor: u64, mailbox: bool }

/// One per producer OS thread. Every transition is explicit; nothing is
/// inferred from a later camera or pointer coincidence.
pub struct Producer<V> { current: Option<Current<V>>, binding: Option<Binding<V>>, suspended: u32 }

impl<V: Copy> Producer<V> {
    pub const fn new() -> Self { Self { current: None, binding: None, suspended: 0 } }

    /// SetCurrent pre-hook entry. Every binding ends at the next SetCurrent.
    pub fn end_at_set_current(&mut self, viewport: u64) -> Option<(Binding<V>, End)> {
        let binding = self.binding.take()?;
        Some((binding, if viewport != 0 && viewport == binding.parent { End::Balanced } else { End::OtherSetCurrent }))
    }

    /// SetCurrent pre-hook exit: the new current viewport and what it displaced.
    pub fn note_current(&mut self, epoch: u64, viewport: u64, view: Option<V>) {
        let previous = self.current.filter(|current| current.epoch == epoch);
        self.current = Some(Current { epoch, pointer: viewport, view,
            displaced_pointer: previous.map_or(0, |current| current.pointer),
            displaced_view: previous.and_then(|current| current.view) });
    }

    /// The parent that SetCurrent(clone) displaced on this thread, with the
    /// exact view its commands carried.
    pub fn displaced(&self, epoch: u64, clone: u64) -> Result<(u64, V), &'static str> {
        let current = self.current.filter(|current| current.epoch == epoch).ok_or("no_current_viewport_this_epoch")?;
        if clone == 0 || current.pointer != clone { return Err("clone_is_not_current_viewport"); }
        if current.view.is_some() { return Err("clone_matched_a_world_view"); }
        if current.displaced_pointer == 0 { return Err("no_displaced_parent_viewport"); }
        let view = current.displaced_view.ok_or("displaced_parent_has_no_world_view")?;
        Ok((current.displaced_pointer, view))
    }

    pub fn bind(&mut self, binding: Binding<V>) -> Result<(), (&'static str, Option<(Binding<V>, End)>)> {
        if let Some(previous) = self.binding.take() { return Err(("nested_seam_without_set_current", Some((previous, End::Superseded)))); }
        if binding.generation == 0 { return Err(("generation_exhausted", None)); }
        self.binding = Some(binding);
        Ok(())
    }

    pub fn active(&self) -> Option<&Binding<V>> { self.binding.as_ref() }
    pub fn suspended(&self) -> bool { self.suspended != 0 }

    /// Emission boundary. Mailbox descendants neither carry nor revoke the
    /// binding. Any identity or memory mismatch revokes it permanently.
    pub fn emission(&mut self, epoch: u64, store: u64, pthread: u64,
        check: impl FnOnce(&Binding<V>) -> Result<(), &'static str>) -> (Option<Binding<V>>, Option<(Binding<V>, End)>) {
        let Some(binding) = self.binding else { return (None, None); };
        if self.suspended != 0 { return (None, None); }
        let end = if binding.epoch != epoch { Some(End::Epoch) }
            else if binding.store != store || binding.pthread != pthread { Some(End::Mismatch("foreign_store_or_pthread")) }
            else { check(&binding).err().map(End::Mismatch) };
        match end {
            None => (Some(binding), None),
            Some(end) => { self.binding = None; (None, Some((binding, end))) }
        }
    }

    /// `next_generation` is the value the next binding would receive.
    pub fn boundary_enter(&mut self, mailbox: bool, next_generation: u64) -> BoundaryToken {
        if mailbox { self.suspended = self.suspended.saturating_add(1); }
        BoundaryToken { floor: next_generation, mailbox }
    }

    /// A binding opened inside this boundary must have been restored by the
    /// original SetCurrent(parent) before the boundary exits, on every path.
    pub fn boundary_exit(&mut self, token: BoundaryToken) -> Option<(Binding<V>, End)> {
        if token.mailbox { self.suspended = self.suspended.saturating_sub(1); }
        match self.binding {
            Some(binding) if binding.generation >= token.floor => { self.binding = None; Some((binding, End::Boundary)) }
            _ => None,
        }
    }
}

impl<V: Copy> Default for Producer<V> { fn default() -> Self { Self::new() } }

/// How an attributed foreground depth write relates to the qualified world
/// depth record of its depth target. "Same" means bit-identical raster state:
/// view identity, camera, stage and projection including its depth row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation { LiveSame, LiveDifferent, FrozenSame, FrozenDifferent, Absent }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Not certified here; the existing world-depth rules apply unchanged.
    Generic(&'static str),
    /// Written into the live qualified world depth with the same raster.
    Combined,
    /// First write after the world depth was frozen: snapshot before drawing.
    OpenOverlay,
    ContinueOverlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Opening, Open, Closed, Rejected(&'static str) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlay {
    pub target: u32,
    pub identity: u64,
    pub first_generation: u64,
    pub last_generation: u64,
    pub families: u32,
    pub draws: u64,
    pub phase: Phase,
}

pub const MAX_OVERLAYS: usize = 2;

/// Per-present bookkeeping; consumed at every presentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sessions {
    pub overlays: Vec<Overlay>,
    pub combined_draws: u64,
    pub generic_draws: u64,
    pub last_generic: Option<&'static str>,
}

impl Sessions {
    pub fn is_empty(&self) -> bool { self.overlays.is_empty() }
    fn find(&mut self, target: u32) -> Option<&mut Overlay> { self.overlays.iter_mut().find(|overlay| overlay.target == target) }
    pub fn overlay(&self, target: u32) -> Option<Overlay> { self.overlays.iter().find(|overlay| overlay.target == target).copied() }

    pub fn reject(&mut self, target: u32, reason: &'static str) {
        if let Some(overlay) = self.find(target) {
            if !matches!(overlay.phase, Phase::Rejected(_)) { overlay.phase = Phase::Rejected(reason); }
        }
    }

    fn generic(&mut self, target: u32, reason: &'static str) -> Decision {
        self.reject(target, reason);
        self.generic_draws += 1;
        self.last_generic = Some(reason);
        Decision::Generic(reason)
    }

    pub fn foreground_depth_draw(&mut self, target: u32, identity: u64, relation: Relation, generation: u64, family: u32) -> Decision {
        match relation {
            Relation::LiveSame => {
                // A qualified world write after a foreground overlay was a
                // foreign writer and already closed it; never merge both.
                self.reject(target, "live_world_depth_after_overlay");
                self.combined_draws += 1;
                Decision::Combined
            }
            Relation::LiveDifferent | Relation::FrozenDifferent => self.generic(target, "foreground_raster_differs_from_world_depth"),
            Relation::Absent => self.generic(target, "no_qualified_world_depth_record"),
            Relation::FrozenSame => {
                let full = self.overlays.len() >= MAX_OVERLAYS;
                let Some(overlay) = self.find(target) else {
                    if full { return self.generic(target, "overlay_capacity"); }
                    self.overlays.push(Overlay { target, identity, first_generation: generation, last_generation: generation,
                        families: family_bit(family), draws: 1, phase: Phase::Opening });
                    return Decision::OpenOverlay;
                };
                let reason = match overlay.phase {
                    Phase::Open if overlay.identity == identity => {
                        overlay.draws += 1;
                        overlay.last_generation = generation;
                        overlay.families |= family_bit(family);
                        return Decision::ContinueOverlay;
                    }
                    Phase::Open => "depth_target_identity_changed",
                    Phase::Opening => "pre_foreground_snapshot_incomplete",
                    Phase::Closed => "foreground_after_foreign_depth_writer",
                    Phase::Rejected(reason) => reason,
                };
                self.generic(target, reason)
            }
        }
    }

    pub fn opened(&mut self, target: u32) {
        if let Some(overlay) = self.find(target) { if overlay.phase == Phase::Opening { overlay.phase = Phase::Open; } }
    }

    /// A non-attributed write is about to change the target. Returns true when
    /// the caller must snapshot the actual post-foreground depth first.
    pub fn foreign_write(&mut self, target: u32) -> bool {
        let Some(overlay) = self.find(target) else { return false; };
        match overlay.phase {
            Phase::Open => { overlay.phase = Phase::Closed; true }
            Phase::Opening => { overlay.phase = Phase::Rejected("foreign_writer_during_pre_snapshot"); false }
            Phase::Closed | Phase::Rejected(_) => false,
        }
    }

    /// Publishable when at least one attributed write happened and no rule
    /// rejected it. `Open` means the caller snapshots the post image now.
    pub fn publishable(&self, target: u32) -> Result<Overlay, &'static str> {
        let overlay = self.overlays.iter().find(|overlay| overlay.target == target).ok_or("no_foreground_overlay")?;
        match overlay.phase {
            Phase::Open | Phase::Closed if overlay.draws > 0 => Ok(*overlay),
            Phase::Open | Phase::Closed => Err("overlay_without_foreground_write"),
            Phase::Opening => Err("pre_foreground_snapshot_incomplete"),
            Phase::Rejected(reason) => Err(reason),
        }
    }
}

/// CPU reference of MERGE_WGSL. Exact bits decide "written"; no epsilon, no
/// colour test. A foreground fragment that stored exactly the pre value is
/// indistinguishable and keeps the world value (measure-zero: the cleared
/// value is a projection limit, not a finite foreground distance).
pub fn merge_reference(world: &[f32], before: &[f32], after: &[f32]) -> Option<Vec<f32>> {
    if world.len() != before.len() || world.len() != after.len() { return None; }
    Some(world.iter().zip(before).zip(after).map(|((&world, &before), &after)|
        if before.to_bits() != after.to_bits() { after } else { world }).collect())
}

/// Fullscreen merge into an R32Float colour target. Unfilterable textureLoad
/// only; explicit layout in the adapter.
pub const MERGE_WGSL: &str = "
@group(0) @binding(0) var world: texture_2d<f32>;
@group(0) @binding(1) var before: texture_2d<f32>;
@group(0) @binding(2) var after: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    var p = array<vec2f, 3>(vec2f(-1.0, -1.0), vec2f(3.0, -1.0), vec2f(-1.0, 3.0));
    return vec4f(p[i], 0.0, 1.0);
}
@fragment fn fs(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let texel = vec2i(position.xy);
    let a = textureLoad(before, texel, 0).x;
    let b = textureLoad(after, texel, 0).x;
    let w = textureLoad(world, texel, 0).x;
    return vec4f(select(w, b, bitcast<u32>(a) != bitcast<u32>(b)), 0.0, 0.0, 1.0);
}
";

/// Bounded raw-image capture budget. Exceeding images are recorded as omitted.
pub const MAX_CAPTURE_IMAGE_BYTES: u64 = 96 << 20;
pub const MAX_CAPTURE_TOTAL_BYTES: u64 = 448 << 20;
pub const MAX_CAPTURE_IMAGES: usize = 8;
pub const MAX_CAPTURE_EVENTS: usize = 8192;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureBudget { pub used: u64, pub images: usize, pub omitted: usize }

impl CaptureBudget {
    pub fn admit(&mut self, bytes: u64) -> bool {
        if bytes == 0 || bytes > MAX_CAPTURE_IMAGE_BYTES || self.images >= MAX_CAPTURE_IMAGES
            || self.used.checked_add(bytes).is_none_or(|total| total > MAX_CAPTURE_TOTAL_BYTES) {
            self.omitted += 1;
            return false;
        }
        self.used += bytes;
        self.images += 1;
        true
    }
}

/// Row pitch for texture-to-buffer copies (256-byte alignment).
pub fn padded_row_bytes(width: u32, bytes_per_texel: u32) -> Option<u32> {
    let row = width.checked_mul(bytes_per_texel)?;
    Some(row.checked_add(255)? / 256 * 256)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(m: [f32; 16]) -> [u32; 16] { m.map(f32::to_bits) }
    fn camera() -> [f32; 16] {
        let (s, c) = 0.4f32.sin_cos();
        [c, 0., -s, f32::from_bits(0x7fc0_0001), 0., 1., 0., 0., s, 0., c, 0., 1210.5, 29.75, -1353.25, 1.]
    }
    fn projection() -> [f32; 16] { [1.1, 0., 0., 0., 0., 1.3, 0., 0., 0.02, -0.01, 0.0001, -1., 0., 0., 0.05, 0.] }
    fn seam() -> (SeamWords, RasterKey) {
        let parent = bits(camera());
        let mut clone = parent;
        clone[12] = 0; clone[13] = 0; clone[14] = 0;
        clone[3] = 0xdead_beef; // padding never qualifies or disqualifies
        let projection = bits(projection());
        let scalars = [7u32; 11];
        let mut model = [0u32; 16];
        for (index, value) in model.iter_mut().enumerate() { *value = (index as f32 * 0.25).to_bits(); }
        (SeamWords { parent_camera: parent, parent_projection: projection, parent_scalars: scalars, parent_flags: [1, 1],
            clone_camera: clone, clone_projection: projection, clone_scalars: scalars, clone_flags: [1, 1], model },
         RasterKey { camera: parent, projection })
    }

    #[test]
    fn clone_certificate_requires_exact_parent_basis_projection_and_state() {
        let (words, key) = seam();
        let certificate = certify_clone(&words, &key).unwrap();
        assert_eq!(certificate.projection, key.projection);
        assert_eq!(certificate.clone_camera[9..], [0, 0, 0]);
        type Mutation = fn(&mut SeamWords, &mut RasterKey);
        let cases: [(&str, Mutation); 9] = [
            ("parent_camera_is_not_displaced_view", |_, k| k.camera[12] ^= 1),
            ("parent_projection_is_not_displaced_view", |_, k| k.projection[10] ^= 1),
            ("clone_is_not_zeroed_parent_basis", |w, _| w.clone_camera[13] = 0.01f32.to_bits()),
            ("clone_is_not_zeroed_parent_basis", |w, _| w.clone_camera[13] = (-0.0f32).to_bits()),
            ("clone_is_not_zeroed_parent_basis", |w, _| w.clone_camera[5] ^= 1),
            ("clone_projection_differs_from_parent", |w, _| w.clone_projection[14] ^= 1),
            ("clone_projection_state_differs_from_parent", |w, _| w.clone_scalars[6] ^= 1),
            ("clone_projection_state_differs_from_parent", |w, _| w.clone_flags[1] = 0),
            ("corrected_model_nonfinite", |w, _| w.model[13] = f32::NAN.to_bits()),
        ];
        for (expected, mutate) in cases {
            let (mut words, mut key) = seam();
            mutate(&mut words, &mut key);
            assert_eq!(certify_clone(&words, &key), Err(expected));
        }
        // Padding words differ freely; they are not raster state.
        let (mut words, key) = seam();
        words.parent_camera[7] ^= 1; words.clone_camera[7] = words.parent_camera[7] ^ 2;
        assert!(certify_clone(&words, &key).is_ok());
    }

    fn binding(generation: u64, parent: u64) -> Binding<u32> {
        let (words, key) = seam();
        Binding { generation, family: STREAM_PED, epoch: 3, store: 11, pthread: 22, clone: 700, parent, frame: 572,
            view: 99, certificate: certify_clone(&words, &key).unwrap() }
    }

    #[test]
    fn set_current_tracks_displaced_parent_and_ends_binding_balanced_only_for_parent() {
        let mut p = Producer::<u32>::new();
        assert_eq!(p.displaced(3, 700), Err("no_current_viewport_this_epoch"));
        p.note_current(3, 500, Some(99));
        p.note_current(3, 700, None);
        assert_eq!(p.displaced(3, 700), Ok((500, 99)));
        assert_eq!(p.displaced(4, 700), Err("no_current_viewport_this_epoch"));
        assert_eq!(p.displaced(3, 701), Err("clone_is_not_current_viewport"));
        p.bind(binding(5, 500)).unwrap();
        assert_eq!(p.end_at_set_current(500).map(|(_, end)| end), Some(End::Balanced));
        assert!(p.active().is_none());
        p.bind(binding(6, 500)).unwrap();
        assert_eq!(p.end_at_set_current(501).map(|(_, end)| end), Some(End::OtherSetCurrent));
        assert_eq!(p.end_at_set_current(500), None);
        // A clone that itself matched a view, or a parent without one, never binds.
        p.note_current(3, 800, Some(1)); assert_eq!(p.displaced(3, 800), Err("clone_matched_a_world_view"));
        p.note_current(3, 850, None); p.note_current(3, 900, None);
        assert_eq!(p.displaced(3, 900), Err("displaced_parent_has_no_world_view"));
        // Epoch change forgets the displaced parent.
        let mut q = Producer::<u32>::new();
        q.note_current(3, 500, Some(99)); q.note_current(4, 700, None);
        assert_eq!(q.displaced(4, 700), Err("no_displaced_parent_viewport"));
    }

    #[test]
    fn nested_seam_and_exhausted_generation_fail_closed() {
        let mut p = Producer::<u32>::new();
        p.bind(binding(5, 500)).unwrap();
        let (reason, revoked) = p.bind(binding(6, 500)).unwrap_err();
        assert_eq!(reason, "nested_seam_without_set_current");
        assert_eq!(revoked.map(|(b, end)| (b.generation, end)), Some((5, End::Superseded)));
        assert!(p.active().is_none());
        assert_eq!(p.bind(binding(0, 500)).unwrap_err().0, "generation_exhausted");
    }

    #[test]
    fn emission_checks_epoch_store_pthread_and_memory_and_revokes_permanently() {
        let mut p = Producer::<u32>::new();
        p.bind(binding(5, 500)).unwrap();
        assert_eq!(p.emission(3, 11, 22, |_| Ok(())).0.map(|b| b.generation), Some(5));
        for (epoch, store, pthread, check, end) in [
            (4, 11, 22, Ok(()), End::Epoch),
            (3, 12, 22, Ok(()), End::Mismatch("foreign_store_or_pthread")),
            (3, 11, 23, Ok(()), End::Mismatch("foreign_store_or_pthread")),
            (3, 11, 22, Err("clone_projection_changed"), End::Mismatch("clone_projection_changed")),
        ] {
            let mut p = Producer::<u32>::new();
            p.bind(binding(5, 500)).unwrap();
            let (tag, ended) = p.emission(epoch, store, pthread, |_| check);
            assert!(tag.is_none());
            assert_eq!(ended.map(|(_, end)| end), Some(end));
            assert_eq!(p.emission(3, 11, 22, |_| Ok(())), (None, None), "revocation is permanent");
        }
    }

    #[test]
    fn mailbox_descendants_neither_carry_nor_revoke_and_parent_resumes() {
        let mut p = Producer::<u32>::new();
        p.bind(binding(5, 500)).unwrap();
        let mailbox = p.boundary_enter(true, 6);
        assert_eq!(p.emission(3, 11, 22, |_| Err("would_revoke")), (None, None));
        let nested = p.boundary_enter(false, 6);
        assert_eq!(p.emission(3, 11, 22, |_| Ok(())), (None, None));
        assert_eq!(p.boundary_exit(nested), None);
        assert_eq!(p.boundary_exit(mailbox), None);
        assert_eq!(p.emission(3, 11, 22, |_| Ok(())).0.map(|b| b.generation), Some(5));
    }

    #[test]
    fn boundary_exit_revokes_only_bindings_opened_inside_it_on_every_path() {
        // Outer binding survives an inner boundary (error or success alike).
        let mut p = Producer::<u32>::new();
        p.bind(binding(5, 500)).unwrap();
        let inner = p.boundary_enter(false, 6);
        assert_eq!(p.boundary_exit(inner), None);
        assert!(p.active().is_some());
        // A binding opened inside a boundary that exits without the original
        // SetCurrent(parent) (trap, LongJmp, panic or unbalanced return) is revoked.
        let mut p = Producer::<u32>::new();
        let outer = p.boundary_enter(false, 7);
        p.bind(binding(7, 500)).unwrap();
        assert_eq!(p.boundary_exit(outer).map(|(b, end)| (b.generation, end)), Some((7, End::Boundary)));
        // Balanced restoration inside the boundary leaves nothing to revoke.
        let outer = p.boundary_enter(false, 8);
        p.bind(binding(8, 500)).unwrap();
        assert_eq!(p.end_at_set_current(500).map(|(_, end)| end), Some(End::Balanced));
        assert_eq!(p.boundary_exit(outer), None);
    }

    #[test]
    fn overlay_opens_once_continues_closes_on_foreign_writer_and_rejects_late_foreground() {
        let mut s = Sessions::default();
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::FrozenSame, 5, STREAM_PED), Decision::OpenOverlay);
        assert_eq!(s.publishable(9), Err("pre_foreground_snapshot_incomplete"));
        s.opened(9);
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::FrozenSame, 6, SKINNED), Decision::ContinueOverlay);
        let overlay = s.publishable(9).unwrap();
        assert_eq!((overlay.draws, overlay.families, overlay.first_generation, overlay.last_generation), (2, 3, 5, 6));
        assert!(s.foreign_write(9));
        assert!(!s.foreign_write(9), "post snapshot is taken once");
        assert_eq!(s.publishable(9).unwrap().phase, Phase::Closed);
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::FrozenSame, 7, SKINNED), Decision::Generic("foreground_after_foreign_depth_writer"));
        assert_eq!(s.publishable(9), Err("foreground_after_foreign_depth_writer"));
    }

    #[test]
    fn overlay_rejections_capacity_and_combined_live_writes() {
        let mut s = Sessions::default();
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::LiveSame, 5, STREAM_PED), Decision::Combined);
        assert!(s.is_empty());
        assert_eq!(s.combined_draws, 1);
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::Absent, 5, STREAM_PED), Decision::Generic("no_qualified_world_depth_record"));
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::LiveDifferent, 5, STREAM_PED), Decision::Generic("foreground_raster_differs_from_world_depth"));
        // Target replacement between attributed writes.
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::FrozenSame, 5, STREAM_PED), Decision::OpenOverlay);
        s.opened(9);
        assert_eq!(s.foreground_depth_draw(9, 2, Relation::FrozenSame, 5, STREAM_PED), Decision::Generic("depth_target_identity_changed"));
        assert_eq!(s.publishable(9), Err("depth_target_identity_changed"));
        // A mismatched raster later on the same target rejects the overlay.
        assert_eq!(s.foreground_depth_draw(10, 1, Relation::FrozenSame, 5, STREAM_PED), Decision::OpenOverlay);
        s.opened(10);
        assert_eq!(s.foreground_depth_draw(10, 1, Relation::FrozenDifferent, 6, STREAM_PED), Decision::Generic("foreground_raster_differs_from_world_depth"));
        assert_eq!(s.publishable(10), Err("foreground_raster_differs_from_world_depth"));
        assert_eq!(s.foreground_depth_draw(11, 1, Relation::FrozenSame, 5, STREAM_PED), Decision::Generic("overlay_capacity"));
        // Live world depth after an overlay never merges both.
        let mut s = Sessions::default();
        s.foreground_depth_draw(9, 1, Relation::FrozenSame, 5, STREAM_PED); s.opened(9);
        assert!(s.foreign_write(9));
        assert_eq!(s.foreground_depth_draw(9, 1, Relation::LiveSame, 6, STREAM_PED), Decision::Combined);
        assert_eq!(s.publishable(9), Err("live_world_depth_after_overlay"));
        // Foreign write while the pre snapshot is being taken fails closed.
        let mut s = Sessions::default();
        s.foreground_depth_draw(9, 1, Relation::FrozenSame, 5, STREAM_PED);
        assert!(!s.foreign_write(9));
        assert_eq!(s.publishable(9), Err("foreign_writer_during_pre_snapshot"));
    }

    /// Reverse-Z raster simulation of the frozen-world overlay: GTA clears depth
    /// to far after the world copy, then draws the corrected foreground with
    /// GreaterEqual. Merged depth keeps every untouched world texel and takes
    /// the actual foreground depth where it wrote, including texels where the
    /// foreground is behind world geometry (it was drawn over it).
    #[test]
    fn overlay_merge_keeps_untouched_world_and_uses_actual_foreground_writes() {
        let (w, h) = (8usize, 6usize);
        let world: Vec<f32> = (0..w * h).map(|i| 0.002 + (i % w) as f32 * 0.0005).collect();
        let mut depth = vec![0.0f32; w * h]; // UI clear to reverse-Z far
        let before = depth.clone();
        let mut covered = vec![false; w * h];
        for (x0, x1, value) in [(1usize, 4usize, 0.08f32), (3, 6, 0.12)] {
            for y in 2..5 { for x in x0..x1 {
                let i = y * w + x;
                if value >= depth[i] { depth[i] = value; covered[i] = true; }
            }}
        }
        let merged = merge_reference(&world, &before, &depth).unwrap();
        for i in 0..w * h {
            if covered[i] { assert_eq!(merged[i].to_bits(), depth[i].to_bits()); assert!(merged[i] > world[i]); }
            else { assert_eq!(merged[i].to_bits(), world[i].to_bits()); }
        }
        // Nearest foreground wins inside the overlap, as rasterised.
        assert_eq!(merged[2 * w + 3], 0.12);
        // A cleared target with no foreground write is the frozen world exactly.
        assert_eq!(merge_reference(&world, &before, &before).unwrap(), world);
        assert!(merge_reference(&world, &before, &depth[1..]).is_none());
    }

    #[test]
    fn capture_budget_and_row_pitch_are_bounded() {
        let mut budget = CaptureBudget::default();
        assert!(!budget.admit(0));
        assert!(!budget.admit(MAX_CAPTURE_IMAGE_BYTES + 1));
        for _ in 0..4 { assert!(budget.admit(MAX_CAPTURE_IMAGE_BYTES)); }
        assert!(!budget.admit(MAX_CAPTURE_IMAGE_BYTES), "total cap");
        assert!(budget.admit(32 << 20));
        for _ in 0..3 { assert!(budget.admit(1)); }
        assert!(!budget.admit(1), "image count cap");
        assert_eq!(budget.omitted, 4);
        assert_eq!(padded_row_bytes(1, 4), Some(256));
        assert_eq!(padded_row_bytes(64, 4), Some(256));
        assert_eq!(padded_row_bytes(65, 4), Some(512));
        assert_eq!(padded_row_bytes(u32::MAX, 4), None);
    }
}
