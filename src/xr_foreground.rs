//! Inactive host-side foreground transform planner. No guest hooks, GPU state,
//! globals, or render authorization live here. A successful plan proves only
//! matrix consistency, NOT draw ownership, complete foreground coverage, a
//! matching captured depth texture, or a safe MRT/postprocessing boundary.
//!
//! Source proof: target/verification/immersive-recording-20261013/
//! viewmodel-transform-report.md (original WASM SHA 11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0).
//! Matrices use column vectors and column-major storage, in meters. All work is
//! f64 and origin-relative; callers must not first round large world poses to
//! f32 and expect this planner to recover the lost eye/head offsets.

pub type Matrix = [f64; 16];

const IDENTITY: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];
// B(renderer right/up/back) * S = C(game right/forward/up).
const GAME_TO_RENDERER: Matrix = [
    1., 0., 0., 0., 0., 0., -1., 0., 0., 1., 0., 0., 0., 0., 0., 1.,
];
const RIGID_EPS: f64 = 1e-5;
const POSE_POSITION_EPS_M: f64 = 1e-4;

/// Source-known function families, not evidence that a particular draw belongs
/// to that family. The future adapter must verify exact flags, pass and lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundFamily {
    FixedEntity44395,
    StreamPed44415,
    SkinnedEntity44355,
}

#[derive(Clone, Copy, Debug)]
pub enum ModelInput {
    /// Model in world-axis coordinates relative to the ORIGINAL camera origin.
    /// Already contains original aim, e.g. the producer 42494 prop transform.
    /// It must not contain current headset motion. Only rigid models are planned.
    FixedEntity { origin_from_model: Matrix },
    /// O from the same frame's fwCameraRelativeExtension, including any ancestor
    /// attachment offset composed by 44501. O = entity_from_game_camera.
    CameraRelativeExtension { entity_from_game_camera: Matrix },
}

#[derive(Clone, Copy, Debug)]
pub struct ForegroundProjection {
    /// Sparse RH pinhole, [0,1] finite reverse-Z. Calibration/shear conversion is
    /// deliberately outside this helper. Do not pass the old 50-degree FOV as
    /// a universal arms projection: the skinned families inherit projection.
    pub projection_from_eye: Matrix,
    pub near_m: f64,
    pub far_m: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct PlanInput {
    pub family: ForegroundFamily,
    pub model: ModelInput,
    /// Original gameplay camera C, columns right/forward/up (not renderer B).
    pub world_from_game_camera: Matrix,
    /// Reference-head-relative physical delta H, expressed in the original
    /// renderer RUB axes: reference_from_current_head, translations in meters.
    /// This is NOT an absolute tracking-space head pose or gameplay aim rotation.
    pub reference_from_head: Matrix,
    /// Physical eye pose in head coordinates, including IPD; identity for mono.
    pub head_from_eye: Matrix,
    /// Expected actual source eye camera in world coordinates, right/up/back.
    /// Independent consistency check, not proof of same-frame guest ownership.
    pub world_from_source_eye: Matrix,
    /// Foreground's own projection; world depth must not be substituted.
    pub foreground_projection: ForegroundProjection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    FamilyModelMismatch,
    NonFiniteTransform(&'static str),
    NonRigidTransform(&'static str),
    SourceEyePoseMismatch,
    NonFiniteProjection,
    UnsupportedProjection,
    InvalidDepthRange,
    ProjectionDepthMismatch,
    NonFiniteResult,
    InvalidDepthSample,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FamilyModelMismatch => f.write_str("model encoding does not match foreground family"),
            Self::NonFiniteTransform(n) => write!(f, "non-finite {n} transform"),
            Self::NonRigidTransform(n) => write!(f, "{n} must be a proper rigid transform"),
            Self::SourceEyePoseMismatch => f.write_str("source eye differs from original camera * head * eye"),
            Self::NonFiniteProjection => f.write_str("non-finite foreground projection"),
            Self::UnsupportedProjection => f.write_str("foreground projection must be sparse finite RH reverse-Z"),
            Self::InvalidDepthRange => f.write_str("foreground near/far range is invalid"),
            Self::ProjectionDepthMismatch => f.write_str("foreground near/far do not match its projection"),
            Self::NonFiniteResult => f.write_str("foreground matrix calculation overflowed"),
            Self::InvalidDepthSample => f.write_str("depth sample must be finite and in [0,1]"),
        }
    }
}

/// Mathematical proposal only. Intentionally has no GPU handles, guest pointers,
/// epoch/coverage certification, installation method, or production adapter.
#[derive(Clone, Copy, Debug)]
pub struct ForegroundPlan {
    pub family: ForegroundFamily,
    pub origin_from_model: Matrix,
    pub eye_from_origin: Matrix,
    pub clip_from_model: Matrix,
    pub depth: ForegroundDepth,
}

#[derive(Clone, Copy, Debug)]
pub struct ForegroundDepth {
    /// World axes, with origin at the original gameplay camera position.
    pub world_origin_m: [f64; 3],
    pub origin_from_eye: Matrix,
    pub projection_from_eye: Matrix,
    /// Effective values derived from the exact retained projection coefficients.
    pub near_m: f64,
    pub far_m: f64,
}

impl ForegroundDepth {
    /// Reconstruct in the SAME origin-relative space as the planned model.
    /// ndc_y is mathematical up; texture-coordinate inversion belongs to adapter.
    /// Only valid for depth actually generated by this plan, not frozen world
    /// depth or final mixed-color pixels whose foreground coverage is unknown.
    pub fn reconstruct_origin(&self, ndc_x: f64, ndc_y: f64, depth: f64) -> Result<[f64; 3], Reject> {
        if !ndc_x.is_finite() || !ndc_y.is_finite() || !depth.is_finite() || !(0.0..=1.0).contains(&depth) {
            return Err(Reject::InvalidDepthSample);
        }
        let p = self.projection_from_eye;
        let distance = p[14] / (depth + p[10]);
        let eye = [distance * (ndc_x + p[8]) / p[0], distance * (ndc_y + p[9]) / p[5], -distance, 1.];
        let origin = transform(&self.origin_from_eye, eye);
        if !origin.iter().all(|x| x.is_finite()) { return Err(Reject::NonFiniteResult); }
        Ok([origin[0], origin[1], origin[2]])
    }
}

pub fn plan(input: &PlanInput) -> Result<ForegroundPlan, Reject> {
    let compatible = matches!((input.family, input.model),
        (ForegroundFamily::FixedEntity44395, ModelInput::FixedEntity { .. }) |
        (ForegroundFamily::StreamPed44415 | ForegroundFamily::SkinnedEntity44355,
         ModelInput::CameraRelativeExtension { .. }));
    if !compatible { return Err(Reject::FamilyModelMismatch); }
    validate_rigid(&input.world_from_game_camera, "original game camera")?;
    validate_rigid(&input.reference_from_head, "relative head")?;
    validate_rigid(&input.head_from_eye, "eye")?;
    validate_rigid(&input.world_from_source_eye, "source eye")?;
    let (near, far) = validate_projection(&input.foreground_projection)?;
    let original = input.world_from_game_camera;
    let world_origin_m = [original[12], original[13], original[14]];
    let mut original_rotation = original;
    original_rotation[12..15].fill(0.);
    // B0 = C * S^-1. Preserve the world origin separately from small offsets.
    let reference = multiply(&original_rotation, &inverse_rigid(&GAME_TO_RENDERER));
    let origin_from_eye = multiply(&multiply(&reference, &input.reference_from_head), &input.head_from_eye);
    let mut expected_world_eye = origin_from_eye;
    for axis in 0..3 { expected_world_eye[12 + axis] += world_origin_m[axis]; }
    if (0..16).any(|i| (expected_world_eye[i] - input.world_from_source_eye[i]).abs()
        > if (12..15).contains(&i) { POSE_POSITION_EPS_M } else { RIGID_EPS }) {
        return Err(Reject::SourceEyePoseMismatch);
    }
    let origin_from_model = match input.model {
        ModelInput::FixedEntity { origin_from_model } => {
            validate_rigid(&origin_from_model, "fixed model")?;
            origin_from_model
        }
        ModelInput::CameraRelativeExtension { entity_from_game_camera } => {
            validate_rigid(&entity_from_game_camera, "camera-relative extension")?;
            // Original aim C is used here, NEVER the current head/eye camera.
            multiply(&original_rotation, &inverse_rigid(&entity_from_game_camera))
        }
    };
    let eye_from_origin = inverse_rigid(&origin_from_eye);
    let projection = input.foreground_projection.projection_from_eye;
    let clip_from_model = multiply(&projection, &multiply(&eye_from_origin, &origin_from_model));
    if !origin_from_eye.iter().chain(origin_from_model.iter()).chain(eye_from_origin.iter())
        .chain(clip_from_model.iter()).all(|x| x.is_finite()) { return Err(Reject::NonFiniteResult); }
    Ok(ForegroundPlan { family: input.family, origin_from_model, eye_from_origin, clip_from_model,
        depth: ForegroundDepth { world_origin_m, origin_from_eye, projection_from_eye: projection, near_m: near, far_m: far } })
}

fn validate_projection(input: &ForegroundProjection) -> Result<(f64, f64), Reject> {
    let p = input.projection_from_eye;
    if !p.iter().all(|x| x.is_finite()) { return Err(Reject::NonFiniteProjection); }
    if p[0] <= 0. || p[5] <= 0. || p[10] <= 0. || p[14] <= 0. || p[11] != -1.
        || [1,2,3,4,6,7,12,13,15].into_iter().any(|i| p[i] != 0.) { return Err(Reject::UnsupportedProjection); }
    if !input.near_m.is_finite() || !input.far_m.is_finite() || input.near_m <= 0. || input.far_m <= input.near_m {
        return Err(Reject::InvalidDepthRange);
    }
    let (near, far) = (p[14] / (p[10] + 1.), p[14] / p[10]);
    if !near.is_finite() || !far.is_finite() || near <= 0. || far <= near { return Err(Reject::InvalidDepthRange); }
    if (near - input.near_m).abs() > input.near_m * 1e-6 || (far - input.far_m).abs() > input.far_m * 1e-6 {
        return Err(Reject::ProjectionDepthMismatch);
    }
    Ok((near, far))
}

fn validate_rigid(m: &Matrix, name: &'static str) -> Result<(), Reject> {
    if !m.iter().all(|x| x.is_finite()) { return Err(Reject::NonFiniteTransform(name)); }
    let dot = |a: usize, b: usize| (0..3).map(|r| m[a*4+r] * m[b*4+r]).sum::<f64>();
    let determinant = m[0]*(m[5]*m[10]-m[9]*m[6]) - m[4]*(m[1]*m[10]-m[9]*m[2]) + m[8]*(m[1]*m[6]-m[5]*m[2]);
    // Source affine row constants are exact. Never silently discard projective
    // terms in inverse_rigid; only rounded rotation entries get a tolerance.
    if [3,7,11].into_iter().any(|i| m[i] != 0.) || m[15] != 1.
        || (0..3).any(|a| (0..3).any(|b| (dot(a,b) - if a == b {1.} else {0.}).abs() > RIGID_EPS))
        || (determinant - 1.).abs() > RIGID_EPS { return Err(Reject::NonRigidTransform(name)); }
    Ok(())
}

fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|i| (0..4).map(|k| a[k*4+i%4] * b[(i/4)*4+k]).sum())
}
fn transform(m: &Matrix, v: [f64;4]) -> [f64;4] {
    std::array::from_fn(|r| (0..4).map(|c| m[c*4+r]*v[c]).sum())
}
fn inverse_rigid(m: &Matrix) -> Matrix {
    let mut result = IDENTITY;
    for r in 0..3 { for c in 0..3 { result[c*4+r] = m[r*4+c]; } }
    for r in 0..3 { result[12+r] = -(0..3).map(|k| result[k*4+r] * m[12+k]).sum::<f64>(); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pose(yaw: f64, pitch: f64, translation: [f64;3]) -> Matrix {
        let (sy,cy) = yaw.sin_cos(); let (sp,cp) = pitch.sin_cos();
        let mut m = multiply(&[cy,0.,-sy,0., 0.,1.,0.,0., sy,0.,cy,0., 0.,0.,0.,1.],
            &[1.,0.,0.,0., 0.,cp,sp,0., 0.,-sp,cp,0., 0.,0.,0.,1.]);
        m[12..15].copy_from_slice(&translation); m
    }
    fn projection(near: f64, far: f64) -> ForegroundProjection {
        ForegroundProjection { projection_from_eye: [1.2,0.,0.,0., 0.,1.5,0.,0., 0.17,-0.13,near/(far-near),-1., 0.,0.,near*far/(far-near),0.], near_m: near, far_m: far }
    }
    fn input() -> PlanInput {
        let b0 = pose(0.3,-0.2,[1234.,51.,-78.]);
        let mut i = PlanInput { family: ForegroundFamily::StreamPed44415,
            model: ModelInput::CameraRelativeExtension { entity_from_game_camera: pose(-0.1,0.2,[0.15,0.5,0.1]) },
            world_from_game_camera: multiply(&b0,&GAME_TO_RENDERER), reference_from_head: IDENTITY, head_from_eye: IDENTITY,
            world_from_source_eye: b0, foreground_projection: projection(0.046,1000.) };
        refresh_eye(&mut i); i
    }
    fn refresh_eye(i: &mut PlanInput) {
        let b0 = multiply(&i.world_from_game_camera,&inverse_rigid(&GAME_TO_RENDERER));
        i.world_from_source_eye = multiply(&multiply(&b0,&i.reference_from_head),&i.head_from_eye);
    }
    fn close(a: &[f64], b: &[f64], eps: f64) { assert_eq!(a.len(),b.len()); for (a,b) in a.iter().zip(b) { assert!((a-b).abs() <= eps,"{a} != {b}"); } }
    fn ndc(p: &ForegroundPlan, v: [f64;4]) -> [f64;3] { let c=transform(&p.clip_from_model,v); [c[0]/c[3],c[1]/c[3],c[2]/c[3]] }

    #[test]
    fn original_aim_stable_and_head_applied_once_across_families() {
        for family in [ForegroundFamily::StreamPed44415,ForegroundFamily::SkinnedEntity44355] {
            let mut i=input(); i.family=family; let baseline=plan(&i).unwrap();
            for yaw in [-0.7,0.,0.2] { for pitch in [-0.2,0.,0.1] { for position in [[0.;3],[0.12,-0.02,0.07]] { for eye in [-0.032,0.,0.032] {
                i.reference_from_head=pose(yaw,pitch,position); i.head_from_eye=pose(0.,0.,[eye,0.,0.]); refresh_eye(&mut i);
                let p=plan(&i).unwrap(); close(&p.origin_from_model,&baseline.origin_from_model,1e-12);
                let ModelInput::CameraRelativeExtension{entity_from_game_camera:o}=i.model else {unreachable!()};
                let expected=multiply(&i.foreground_projection.projection_from_eye,
                    &multiply(&multiply(&multiply(&inverse_rigid(&i.head_from_eye),&inverse_rigid(&i.reference_from_head)),&GAME_TO_RENDERER),&inverse_rigid(&o)));
                close(&p.clip_from_model,&expected,2e-12);
            }}}}
        }
    }

    #[test]
    fn noncommuting_head_and_eye_rotation_preserves_composition_order() {
        let mut i=input();
        let (s,c)=0.31_f64.sin_cos();
        let roll=[c,s,0.,0., -s,c,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.];
        i.reference_from_head=multiply(&pose(0.43,-0.27,[0.12,-0.04,0.07]),&roll);
        i.head_from_eye=pose(-0.06,0.03,[0.032,0.004,-0.008]); refresh_eye(&mut i);
        let p=plan(&i).unwrap();
        let ModelInput::CameraRelativeExtension{entity_from_game_camera:o}=i.model else {unreachable!()};
        let expected=multiply(&i.foreground_projection.projection_from_eye,
            &multiply(&multiply(&multiply(&inverse_rigid(&i.head_from_eye),&inverse_rigid(&i.reference_from_head)),&GAME_TO_RENDERER),&inverse_rigid(&o)));
        close(&p.clip_from_model,&expected,2e-12);
        let wrong_order=multiply(&i.foreground_projection.projection_from_eye,
            &multiply(&multiply(&multiply(&inverse_rigid(&i.reference_from_head),&inverse_rigid(&i.head_from_eye)),&GAME_TO_RENDERER),&inverse_rigid(&o)));
        assert!(wrong_order.iter().zip(p.clip_from_model).any(|(a,b)| (a-b).abs()>0.005));
        let old_head_cancelled=multiply(&i.foreground_projection.projection_from_eye,&multiply(&GAME_TO_RENDERER,&inverse_rigid(&o)));
        assert!(old_head_cancelled.iter().zip(p.clip_from_model).any(|(a,b)| (a-b).abs()>0.1));
    }

    #[test]
    fn zero_origin_clone_does_not_drop_head_translation_or_eye_offset() {
        let mut i=input(); i.reference_from_head=pose(0.23,-0.1,[0.12,0.04,-0.07]); i.head_from_eye=pose(0.,0.,[0.032,0.,0.]); refresh_eye(&mut i);
        let p=plan(&i).unwrap(); assert!(p.depth.origin_from_eye[12..15].iter().any(|x| x.abs()>0.05));
        let mut zeroed=p.depth.origin_from_eye; zeroed[12..15].fill(0.);
        let wrong=multiply(&p.depth.projection_from_eye,&multiply(&inverse_rigid(&zeroed),&p.origin_from_model));
        assert!(wrong.iter().zip(p.clip_from_model).any(|(a,b)| (a-b).abs()>0.01));
        // Input claiming the original zeroed clone as the actual source eye is rejected.
        i.world_from_source_eye=zeroed;
        assert_eq!(plan(&i).unwrap_err(),Reject::SourceEyePoseMismatch);
    }

    #[test]
    fn fixed_entity_keeps_original_aim_and_has_expected_stereo_disparity() {
        let mut i=input(); i.family=ForegroundFamily::FixedEntity44395;
        i.world_from_game_camera=GAME_TO_RENDERER;
        i.model=ModelInput::FixedEntity { origin_from_model: pose(0.,0.,[0.,0.,-0.5]) };
        i.head_from_eye=pose(0.,0.,[-0.032,0.,0.]); refresh_eye(&mut i); let left=plan(&i).unwrap();
        i.head_from_eye=pose(0.,0.,[0.032,0.,0.]); refresh_eye(&mut i); let right=plan(&i).unwrap();
        close(&left.origin_from_model,&right.origin_from_model,0.);
        let l=ndc(&left,[0.,0.,0.,1.]); let r=ndc(&right,[0.,0.,0.,1.]);
        assert!(((l[0]-r[0])-1.2*0.064/0.5).abs()<1e-12); assert!((l[2]-r[2]).abs()<1e-12);
    }

    #[test]
    fn large_world_origin_never_enters_small_model_or_eye_math() {
        let mut i=input(); i.reference_from_head=pose(0.2,-0.1,[0.013,-0.009,0.019]); i.head_from_eye=pose(0.,0.,[0.032,0.,0.]);
        i.world_from_game_camera[12..15].fill(0.); refresh_eye(&mut i); let baseline=plan(&i).unwrap();
        for origin in [[200000.,-300000.,99999.],[1e9,-1e9,1e9]] {
            i.world_from_game_camera[12..15].copy_from_slice(&origin); refresh_eye(&mut i); let p=plan(&i).unwrap();
            close(&p.clip_from_model,&baseline.clip_from_model,0.); close(&p.depth.origin_from_eye,&baseline.depth.origin_from_eye,0.);
            assert_eq!(p.depth.world_origin_m,origin);
        }
    }

    #[test]
    fn depth_round_trip_matches_model_and_eye_space_for_distinct_foreground_ranges() {
        let mut i=input(); i.family=ForegroundFamily::FixedEntity44395; i.world_from_game_camera=GAME_TO_RENDERER;
        i.model=ModelInput::FixedEntity { origin_from_model: pose(0.1,-0.1,[0.1,-0.05,-0.7]) };
        i.reference_from_head=pose(0.08,-0.12,[0.02,0.01,0.03]); i.head_from_eye=pose(0.,0.,[-0.032,0.,0.]); refresh_eye(&mut i);
        for (near,far) in [(0.01,5.),(0.046,1000.),(0.1,100.)] {
            i.foreground_projection=projection(near,far); let p=plan(&i).unwrap();
            for v in [[0.,0.,0.,1.],[0.2,0.1,-0.2,1.],[-0.1,-0.2,-0.5,1.]] {
                let n=ndc(&p,v); let actual=p.depth.reconstruct_origin(n[0],n[1],n[2]).unwrap(); let expected=transform(&p.origin_from_model,v);
                close(&actual,&expected[..3],1e-12);
            }
            for (depth,distance) in [(1.,near),(0.,far)] {
                let point=p.depth.reconstruct_origin(0.,0.,depth).unwrap(); let eye=transform(&p.eye_from_origin,[point[0],point[1],point[2],1.]);
                assert!((eye[2]+distance).abs()<1e-10);
            }
        }
    }

    #[test]
    fn reject_mismatched_family_pose_and_invalid_transforms() {
        let mut i=input(); i.family=ForegroundFamily::FixedEntity44395;
        assert_eq!(plan(&i).unwrap_err(),Reject::FamilyModelMismatch);
        let mut i=input(); i.world_from_source_eye[12]+=0.001;
        assert_eq!(plan(&i).unwrap_err(),Reject::SourceEyePoseMismatch);
        let mut i=input(); i.reference_from_head[0]=f64::NAN;
        assert_eq!(plan(&i).unwrap_err(),Reject::NonFiniteTransform("relative head"));
        let mut i=input(); i.head_from_eye[0]=-1.;
        assert_eq!(plan(&i).unwrap_err(),Reject::NonRigidTransform("eye"));
        let mut i=input(); i.head_from_eye[3]=1e-8;
        assert_eq!(plan(&i).unwrap_err(),Reject::NonRigidTransform("eye"));
        let mut i=input(); i.head_from_eye[15]+=1e-8;
        assert_eq!(plan(&i).unwrap_err(),Reject::NonRigidTransform("eye"));
        let mut i=input(); i.world_from_game_camera[5]*=2.;
        assert_eq!(plan(&i).unwrap_err(),Reject::NonRigidTransform("original game camera"));
    }

    #[test]
    fn reject_foreground_depth_metadata_substitution_and_unsupported_projections() {
        let mut i=input(); i.foreground_projection.near_m=0.1;
        assert_eq!(plan(&i).unwrap_err(),Reject::ProjectionDepthMismatch);
        let mut i=input(); i.foreground_projection.projection_from_eye[1]=1e-8;
        assert_eq!(plan(&i).unwrap_err(),Reject::UnsupportedProjection);
        let mut i=input(); i.foreground_projection.projection_from_eye[10]=0.;
        assert_eq!(plan(&i).unwrap_err(),Reject::UnsupportedProjection);
        let mut i=input(); i.foreground_projection.far_m=f64::INFINITY;
        assert_eq!(plan(&i).unwrap_err(),Reject::InvalidDepthRange);
        let p=plan(&input()).unwrap(); assert_eq!(p.depth.reconstruct_origin(0.,0.,1.01),Err(Reject::InvalidDepthSample));
    }
}
