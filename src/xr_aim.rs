//! Controller aim remains the original game's gameplay ray. Head tracking
//! moves the rendered view independently; a reticle must project this ray in
//! each eye rather than reuse the flat image's centre pixel.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use wasmtime::{AsContextMut, Caller, Ref, Table};
use crate::HostState;

pub const AIM_VERSION: u32 = 1;
pub const AIM_SIZE: u32 = 104;
pub const AIM_VALID: u32 = 1;
pub const AIM_VISIBLE: u32 = 2;
pub const AIM_NATIVE_RETICLE_HIDDEN: u32 = 4;
pub const AIM_IN_VEHICLE: u32 = 8;
pub const AIM_FIRST_PERSON: u32 = 16;
pub const AIM_FREE_AIMING: u32 = 32;
const MAX_AGE: Duration = Duration::from_millis(100);
static RETICLE_READY: AtomicBool = AtomicBool::new(false);
static RETICLE_READY_EPOCH: AtomicU64 = AtomicU64::new(0);
// Certified original game.wasm element segment: table0 offset1, element48294
// is function51781, hud_commands::CommandHideHudComponentThisFrame(int).
// patch5 adds function imports but leaves element/table slot ordering intact.
const RETICLE_HIDE_TABLE_SLOT: u64 = 48_295;
const RETICLE_COMPONENT: i32 = 14;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SVXRAimSnapshot {
    pub version: u32,
    pub size: u32,
    pub sequence: u64,
    pub view_epoch: u64,
    pub capture_time_seconds: f64,
    pub age_seconds: f64,
    pub origin: [f64; 3],
    pub direction: [f64; 3],
    pub flags: u32,
    pub ped_view_mode: i32,
    pub vehicle_view_mode: i32,
    pub reserved: u32,
}
const _: () = assert!(std::mem::size_of::<SVXRAimSnapshot>() == AIM_SIZE as usize);

#[derive(Clone, Copy)]
pub(crate) struct GameplayAim {
    pub origin: [f32; 3],
    pub rotation_degrees: [f32; 3],
    pub ped_view_mode: i32,
    pub vehicle_view_mode: i32,
    pub in_vehicle: bool,
    pub armed: bool,
    pub free_aiming: bool,
    pub aiming_control: bool,
    pub shooting: bool,
}

#[derive(Default)]
struct AimState {
    sequence: u64,
    samples: u64,
    hidden: u64,
    failures: u64,
    latest: Option<(SVXRAimSnapshot, Instant)>,
    last_error: Option<String>,
}
static STATE: Mutex<AimState> = Mutex::new(AimState {
    sequence: 0, samples: 0, hidden: 0, failures: 0, latest: None, last_error: None,
});

fn make_snapshot(sample: GameplayAim, epoch: u64, time: f64) -> Option<SVXRAimSnapshot> {
    if !time.is_finite() || time < 0.
        || sample.origin.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.)
        || sample.rotation_degrees.iter().any(|v| !v.is_finite() || v.abs() > 36_000.) {
        return None;
    }
    // GTA's existing multiplayer aim code uses rotation order2: heading about
    // world Z, pitch about X, forward +Y. Roll does not change the aim ray.
    let pitch = (sample.rotation_degrees[0] as f64).to_radians();
    let yaw = (sample.rotation_degrees[2] as f64).to_radians();
    let direction = [-yaw.sin() * pitch.cos(), yaw.cos() * pitch.cos(), pitch.sin()];
    if direction.iter().any(|v| !v.is_finite()) { return None; }
    let first_person = if sample.in_vehicle { sample.vehicle_view_mode == 4 } else { sample.ped_view_mode == 4 };
    let visible = sample.armed && (first_person || sample.free_aiming || sample.aiming_control || sample.shooting);
    Some(SVXRAimSnapshot {
        version: AIM_VERSION, size: AIM_SIZE, view_epoch: epoch, capture_time_seconds: time,
        origin: sample.origin.map(f64::from), direction,
        flags: AIM_VALID | if visible { AIM_VISIBLE } else { 0 }
            | if sample.in_vehicle { AIM_IN_VEHICLE } else { 0 }
            | if first_person { AIM_FIRST_PERSON } else { 0 }
            | if sample.free_aiming { AIM_FREE_AIMING } else { 0 },
        ped_view_mode: sample.ped_view_mode, vehicle_view_mode: sample.vehicle_view_mode,
        ..Default::default()
    })
}

pub(crate) fn valid_gameplay_aim(sample: GameplayAim) -> bool {
    make_snapshot(sample, 0, 0.).is_some()
}

/// The owner enables this only after its replacement reticle pipeline exists.
/// Return0=accepted,-2=invalid Boolean,-3=no active immersive camera owner.
#[no_mangle]
pub extern "C" fn sv_xr_set_reticle_ready(enabled: u32) -> i32 {
    if enabled > 1 { return -2; }
    if enabled == 0 { RETICLE_READY.store(false, Ordering::Release); clear("replacement reticle frontend disabled"); return 0; }
    if !crate::xr_view::world_camera_enabled() { return -3; }
    let epoch = crate::xr_view::view_epoch();
    RETICLE_READY_EPOCH.store(epoch, Ordering::Relaxed);
    RETICLE_READY.store(true, Ordering::Release);
    if !reticle_ready_for_epoch(epoch) { RETICLE_READY.store(false, Ordering::Release); return -3; }
    0
}

pub(crate) fn reticle_ready_for_epoch(epoch: u64) -> bool {
    RETICLE_READY.load(Ordering::Acquire) && RETICLE_READY_EPOCH.load(Ordering::Relaxed) == epoch
        && crate::xr_view::world_camera_enabled() && crate::xr_view::view_epoch_is_active(epoch)
}

pub(crate) fn clear(reason: &str) {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if state.latest.is_none() && state.last_error.as_deref() == Some(reason) { return; }
    state.latest = None;
    state.last_error = Some(reason.to_owned());
}

pub(crate) fn record_failure(reason: &str) {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    state.latest = None; state.failures += 1; state.last_error = Some(reason.to_owned());
}

pub(crate) fn publish(sample: GameplayAim, epoch: u64, native_reticle_hidden: bool) -> bool {
    if !native_reticle_hidden || !reticle_ready_for_epoch(epoch) || !crate::xr_view::world_camera_enabled()
        || !crate::xr_view::view_epoch_is_active(epoch) { clear("aim publication inactive/unverified"); return false; }
    let time = crate::RT.get().map(|rt| rt.start.elapsed().as_secs_f64()).unwrap_or(0.);
    let Some(mut snapshot) = make_snapshot(sample, epoch, time) else { record_failure("invalid original gameplay camera ray"); return false; };
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    state.sequence = state.sequence.saturating_add(1); state.samples += 1; state.hidden += 1;
    snapshot.sequence = state.sequence; snapshot.flags |= AIM_NATIVE_RETICLE_HIDDEN;
    state.latest = Some((snapshot, Instant::now())); state.last_error = None;
    true
}

fn fresh_snapshot(state: &AimState, now: Instant, epoch: u64, active: bool) -> Option<SVXRAimSnapshot> {
    let (mut snapshot, captured) = state.latest?;
    let age = now.checked_duration_since(captured)?;
    if !active || snapshot.view_epoch != epoch || age > MAX_AGE { return None; }
    snapshot.age_seconds = age.as_secs_f64();
    Some(snapshot)
}

/// Copies one fresh controller/gameplay ray. No guest calls occur here.
/// Return0=valid,1=no current ray,-1=null output,-2=ABI size mismatch.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_get_aim_snapshot(output: *mut SVXRAimSnapshot, byte_size: u32) -> i32 {
    if output.is_null() { return -1; }
    if byte_size != AIM_SIZE { return -2; }
    let epoch = crate::xr_view::view_epoch();
    let active = reticle_ready_for_epoch(epoch);
    let snapshot = fresh_snapshot(&STATE.lock().unwrap_or_else(|p| p.into_inner()), Instant::now(), epoch, active);
    if let Some(snapshot) = snapshot.filter(|_| reticle_ready_for_epoch(epoch)) { unsafe { output.write_unaligned(snapshot) }; 0 }
    else { unsafe { output.write_unaligned(SVXRAimSnapshot::default()) }; 1 }
}

pub fn status_json() -> serde_json::Value {
    let state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let epoch = crate::xr_view::view_epoch();
    let snapshot = fresh_snapshot(&state, Instant::now(), epoch, reticle_ready_for_epoch(epoch));
    serde_json::json!({"abi_version": AIM_VERSION,"byte_size": AIM_SIZE,"samples":state.samples,
        "native_reticle_hides":state.hidden,"failures":state.failures,"fresh":snapshot.is_some(),
        "replacement_frontend_ready":reticle_ready_for_epoch(epoch),"maximum_age_ms":MAX_AGE.as_millis(),
        "sequence":snapshot.map(|s|s.sequence),"view_epoch":snapshot.map(|s|s.view_epoch),
        "age_seconds":snapshot.map(|s|s.age_seconds),"origin":snapshot.map(|s|s.origin),
        "direction":snapshot.map(|s|s.direction),"flags":snapshot.map(|s|s.flags),
        "last_error":state.last_error,"aim_source":"original controller gameplay camera; head movement is free look",
        "bullet_scope":"camera forward ray; weapon spread, recoil, muzzle parallax and assisted target are not synthesized"})
}

fn invoke_reticle_hider<T: AsContextMut<Data = HostState>>(store: &mut T, table: Table, certified: bool) -> anyhow::Result<()> {
    anyhow::ensure!(certified, "reticle table call requires exact embedded AOT certificate");
    let Some(Ref::Func(Some(function))) = table.get(&mut *store, RETICLE_HIDE_TABLE_SLOT) else {
        anyhow::bail!("certified original reticle command table slot unavailable");
    };
    let function = function.typed::<i32, ()>(&mut *store)?;
    crate::guest_call_scope::call(store, crate::guest_call_scope::Kind::Ordinary, |store| function.call(store, RETICLE_COMPONENT))?;
    Ok(())
}

/// Only call inside the existing selected guest script TLS/scratch scope.
pub(crate) fn hide_original_reticle(caller: &mut Caller<'_, HostState>, certified: bool) -> anyhow::Result<()> {
    anyhow::ensure!(certified, "reticle hide requires exact embedded AOT certificate");
    anyhow::ensure!(reticle_ready_for_epoch(crate::xr_view::view_epoch()), "replacement reticle frontend is not ready");
    let table = caller.get_export("__indirect_function_table").and_then(|e| e.into_table())
        .ok_or_else(|| anyhow::anyhow!("original command table is not exported"))?;
    invoke_reticle_hider(caller, table, certified)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> GameplayAim { GameplayAim { origin:[2.,3.,4.],rotation_degrees:[0.,0.,0.],
        ped_view_mode:4,vehicle_view_mode:1,in_vehicle:false,armed:true,free_aiming:false,aiming_control:false,shooting:false } }
    #[test]
    fn controller_aim_direction_preserves_game_axes_and_first_person_mode() {
        let s = make_snapshot(sample(),7,1.).unwrap(); assert_eq!(s.origin,[2.,3.,4.]);
        assert_eq!(s.direction,[0.,1.,0.]); assert_ne!(s.flags&AIM_VISIBLE,0);
        let mut input=sample();input.rotation_degrees=[30.,17.,90.];let s=make_snapshot(input,7,1.).unwrap();
        assert!((s.direction[0]+3_f64.sqrt()/2.).abs()<1e-12);assert!(s.direction[1].abs()<1e-12);assert!((s.direction[2]-0.5).abs()<1e-12);
        input.ped_view_mode=1;assert_eq!(make_snapshot(input,7,1.).unwrap().flags&AIM_VISIBLE,0);
        input.free_aiming=true;assert_ne!(make_snapshot(input,7,1.).unwrap().flags&AIM_VISIBLE,0);
        input.armed=false;assert_eq!(make_snapshot(input,7,1.).unwrap().flags&AIM_VISIBLE,0);
    }
    #[test]
    fn invalid_rays_and_expired_or_foreign_scene_snapshots_fail_closed() {
        let mut input=sample();input.origin[0]=f32::NAN;assert!(make_snapshot(input,7,1.).is_none());
        input=sample();input.rotation_degrees[2]=f32::INFINITY;assert!(make_snapshot(input,7,1.).is_none());
        let now=Instant::now();let snapshot=make_snapshot(sample(),7,1.).unwrap();
        let state=AimState { latest:Some((snapshot,now)),..Default::default() };
        assert!(fresh_snapshot(&state,now+Duration::from_millis(99),7,true).is_some());
        assert!(fresh_snapshot(&state,now+Duration::from_millis(101),7,true).is_none());
        assert!(fresh_snapshot(&state,now,8,true).is_none());assert!(fresh_snapshot(&state,now,7,false).is_none());
    }
    #[test]
    fn replacement_frontend_cannot_be_enabled_without_active_camera_owner() {
        assert_eq!(sv_xr_set_reticle_ready(2),-2);
        assert_eq!(sv_xr_set_reticle_ready(0),0);
        assert!(!reticle_ready_for_epoch(crate::xr_view::view_epoch()));
        assert_eq!(sv_xr_set_reticle_ready(1),-3);
        let mut output=SVXRAimSnapshot::default();
        assert_eq!(unsafe {sv_xr_get_aim_snapshot(&mut output,AIM_SIZE)},1);
        assert_eq!(unsafe {sv_xr_get_aim_snapshot(&mut output,AIM_SIZE-1)},-2);
    }
    #[test]
    fn original_table_dispatch_requires_certificate_and_exact_argument_type() {
        use std::sync::{Arc,atomic::{AtomicI32,Ordering}};
        use wasmtime::{Engine,Func,RefType,Store,TableType};
        let engine=Engine::default();let mut store=Store::new(&engine,HostState::new(0));
        let received=Arc::new(AtomicI32::new(-1));let sink=received.clone();
        let function=Func::wrap(&mut store,move |component:i32| {sink.store(component,Ordering::Relaxed);});
        let table=Table::new(&mut store,TableType::new(RefType::FUNCREF,48296,Some(48296)),Ref::Func(None)).unwrap();
        table.set(&mut store,RETICLE_HIDE_TABLE_SLOT,Ref::Func(Some(function))).unwrap();
        assert!(invoke_reticle_hider(&mut store,table,false).is_err());assert_eq!(received.load(Ordering::Relaxed),-1);
        invoke_reticle_hider(&mut store,table,true).unwrap();assert_eq!(received.load(Ordering::Relaxed),14);
        let wrong=Func::wrap(&mut store,|_:f32| {});table.set(&mut store,RETICLE_HIDE_TABLE_SLOT,Ref::Func(Some(wrong))).unwrap();
        assert!(invoke_reticle_hider(&mut store,table,true).is_err());
    }

    #[cfg(feature = "jit")]
    #[test]
    #[ignore = "requires the user's exact original game.wasm fixture via MUGUET_TRACE_ORIGINAL_WASM"]
    fn exact_original_and_patch5_keep_certified_reticle_table_command() {
        use wasmparser::{ElementItems,ElementKind,KnownCustom,Name,Operator,Parser,Payload,TypeRef,ValType};
        // An explicit exact-input verification; never use another downloaded or
        // guessed game image as the table-slot certificate.
        let path=std::env::var_os("MUGUET_TRACE_ORIGINAL_WASM").expect("set exact original game.wasm verification input");
        let original=std::fs::read(path).unwrap();
        extern "C" { fn CC_SHA256(data:*const std::ffi::c_void,length:u32,digest:*mut u8)->*mut u8; }
        let mut hash=[0u8;32];unsafe {CC_SHA256(original.as_ptr().cast(),original.len() as u32,hash.as_mut_ptr());}
        assert_eq!(hash.iter().map(|b|format!("{b:02x}")).collect::<String>(),
            "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0");
        fn certify(bytes:&[u8]) {
            let mut names=std::collections::BTreeMap::new();let mut types=Vec::new();let mut function_types=Vec::new();let mut function=None;
            for payload in Parser::new(0).parse_all(bytes) {
                match payload.unwrap() {
                    Payload::TypeSection(s)=>for ty in s.into_iter_err_on_gc_types(){types.push(ty.unwrap());},
                    Payload::ImportSection(s)=>for import in s.into_imports(){if let TypeRef::Func(ty)=import.unwrap().ty{function_types.push(ty);}},
                    Payload::FunctionSection(s)=>for ty in s{function_types.push(ty.unwrap());},
                    Payload::ElementSection(s)=>for element in s {
                        let element=element.unwrap();
                        if let ElementKind::Active{table_index,offset_expr}=element.kind {
                            if table_index.unwrap_or(0)!=0{continue;}
                            let mut ops=offset_expr.get_operators_reader();
                            let base=match ops.read().unwrap(){Operator::I32Const{value}=>u64::try_from(value).unwrap(),Operator::I64Const{value}=>u64::try_from(value).unwrap(),other=>panic!("unsupported offset {other:?}")};
                            if let ElementItems::Functions(functions)=element.items {
                                for (offset,index) in functions.into_iter().enumerate(){if base+offset as u64==RETICLE_HIDE_TABLE_SLOT{assert!(function.is_none());function=Some(index.unwrap());}}
                            }
                        }
                    },
                    Payload::CustomSection(s)=>if let KnownCustom::Name(s)=s.as_known(){for sub in s{if let Name::Function(s)=sub.unwrap(){for name in s{let name=name.unwrap();names.insert(name.index,name.name.to_owned());}}}},
                    _=>{}
                }
            }
            let function=function.expect("original reticle table entry missing");
            assert_eq!(names[&function],"hud_commands::CommandHideHudComponentThisFrame(int)");
            let ty=&types[function_types[function as usize] as usize];assert_eq!(ty.params(),[ValType::I32]);assert!(ty.results().is_empty());
        }
        certify(&original);
        let patched=crate::patch::patch(&original,crate::mp::COMMANDS).unwrap();certify(&patched);
    }
}
