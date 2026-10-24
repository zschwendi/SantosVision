//! Read-only input context for the exact embedded game supported by this app.
//! The native script getter bodies in original game.wasm (SHA256
//! 11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0)
//! define these fields. An exact AOT hash gate prevents using them for another
//! build; no game code, memory, or saved settings are patched.

use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;

static VERIFIED_ABI: OnceLock<bool> = OnceLock::new();
const AOT_BYTES: usize = 211_066_736;
const AOT_SHA256: [u8; 32] = [
    0x4a, 0x74, 0xf6, 0x15, 0xe3, 0xdf, 0x2b, 0xb7,
    0xd5, 0x23, 0x0b, 0x49, 0xda, 0xe9, 0x8f, 0x16,
    0xfe, 0xc5, 0xad, 0x3f, 0x2c, 0x63, 0x6a, 0x2e,
    0x2a, 0xec, 0x59, 0x6a, 0xf7, 0x3f, 0x63, 0x57,
];

// CPauseMenu::IsActive(PM_ActiveFlags), called with flags=1 by
// hud_commands::CommandIsPauseMenuActive (functions 36254 and 51840).
const PAUSE_ACTIVE: u64 = 19_565_906;
const PAUSE_PENDING: u64 = 19_566_104;
const PAUSE_COMPONENT_COUNT: u64 = 19_592_984;
const PAUSE_COMPONENT_TABLE: u64 = 19_592_976;
const PAUSE_COMPONENT_INDICES: [u64; 4] = [19_566_964, 19_567_060, 19_566_976, 19_566_952];
const PAUSE_MODAL: u64 = 19_569_536;
const PAUSE_OVERLAY: u64 = 19_582_216;
// IS_MOBILE_PHONE_INSTANTIATED script wrapper (function 59243).
const PHONE_INSTANTIATED: u64 = 19_505_748;
// CGameWorld::GetMainPlayerInfo (function 47305): local ped at factory+8, then
// CPed+4328. CPlayerInfo::ProcessFPSState (function 41680) owns +4664 value 2,
// GTA's first-person sights flag, changed only by INPUT_ACCURATE_AIM while aiming.
const PED_FACTORY: u64 = 20_159_720;
const PED_PLAYER_INFO: u64 = 4_328;
const PLAYER_FPS_FLAGS: u64 = 4_664;
const FPS_SIGHTS: u64 = 2;
// camGameplayDirector at 19235320, active camera +960. This vtable is the
// scoped camFirstPersonAimCamera; its wheel is sniper zoom, not sights.
const CAMERA_DIRECTOR: u64 = 19_235_320;
const SCOPED_AIM_CAMERA_VTABLE: u64 = 4_663_048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Context {
    pub paused: bool,
    // Object existence is diagnostic only; it does not establish UI visibility.
    pub phone: bool,
}

/// Optional because an unknown executable or unreadable field cannot establish
/// the guest state. The caller keeps its compatibility fallback in that case.
pub(super) fn read() -> Option<Context> {
    let runtime = crate::RT.get()?;
    if !*VERIFIED_ABI.get_or_init(verify_embedded_abi) { return None; }
    let (base, length) = crate::mem_base_len(&runtime.mem);
    // The shared-memory allocation stays owned by RT throughout this read. Use
    // aligned atomics rather than borrowing mutable guest bytes as a Rust slice.
    read_context(|address, width| unsafe { read_word(base, length, address, width) })
}

pub(super) fn abi_verified() -> Option<bool> { VERIFIED_ABI.get().copied() }

/// Value-only first-person weapon state under the same exact-ABI gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Sights {
    pub on: bool,
    pub scoped_camera: bool,
}

pub(super) fn read_sights() -> Option<Sights> {
    let runtime = crate::RT.get()?;
    if !*VERIFIED_ABI.get_or_init(verify_embedded_abi) { return None; }
    let (base, length) = crate::mem_base_len(&runtime.mem);
    read_sights_from(|address, width| unsafe { read_word(base, length, address, width) })
}

fn read_sights_from(mut read: impl FnMut(u64, usize) -> Option<u64>) -> Option<Sights> {
    let nonzero = |value: u64| (value != 0).then_some(value);
    let factory = nonzero(read(PED_FACTORY, 8)?)?;
    let ped = nonzero(read(factory.checked_add(8)?, 8)?)?;
    let info = nonzero(read(ped.checked_add(PED_PLAYER_INFO)?, 8)?)?;
    let on = read(info.checked_add(PLAYER_FPS_FLAGS)?, 1)? & FPS_SIGHTS != 0;
    let director = read(CAMERA_DIRECTOR, 8)?;
    let camera = if director == 0 { 0 } else { read(director.checked_add(960)?, 8)? };
    let scoped_camera = camera != 0 && read(camera, 8)? == SCOPED_AIM_CAMERA_VTABLE;
    Some(Sights { on, scoped_camera })
}

fn read_context(mut read: impl FnMut(u64, usize) -> Option<u64>) -> Option<Context> {
    let active = read(PAUSE_ACTIVE, 1)? & 1 != 0;
    let pending = read(PAUSE_PENDING, 1)? & 1 != 0;
    let mut paused = active;
    if !paused && pending {
        let count = read(PAUSE_COMPONENT_COUNT, 2)?;
        let table = read(PAUSE_COMPONENT_TABLE, 8)?;
        let mut ready = true;
        for address in PAUSE_COMPONENT_INDICES {
            let index = read(address, 4)?;
            if index >= count || index > 49 {
                ready = false;
                break;
            }
            let address = table.checked_add(index.checked_mul(512)?)?.checked_add(440)?;
            if read(address, 4)? != 3 {
                ready = false;
                break;
            }
        }
        paused = ready;
    }
    if !paused { paused = read(PAUSE_MODAL, 1)? & 1 != 0; }
    if !paused { paused = read(PAUSE_OVERLAY, 4)? != 0; }
    Some(Context { paused, phone: read(PHONE_INSTANTIATED, 1)? != 0 })
}

unsafe fn read_word(base: *mut u8, length: usize, address: u64, width: usize) -> Option<u64> {
    let offset = usize::try_from(address).ok()?;
    if !matches!(width, 1 | 2 | 4 | 8) || offset % width != 0 || offset.checked_add(width)? > length { return None; }
    let pointer = unsafe { base.add(offset) };
    Some(match width {
        1 => unsafe { &*pointer.cast::<AtomicU8>() }.load(Ordering::Relaxed) as u64,
        2 => unsafe { &*pointer.cast::<AtomicU16>() }.load(Ordering::Relaxed) as u64,
        4 => unsafe { &*pointer.cast::<AtomicU32>() }.load(Ordering::Relaxed) as u64,
        8 => unsafe { &*pointer.cast::<AtomicU64>() }.load(Ordering::Relaxed),
        _ => unreachable!(),
    })
}

#[cfg(any(all(target_os = "visionos", not(feature = "jit"), game_code), test))]
fn matches_supported_aot(bytes: &[u8]) -> bool {
    if bytes.len() != AOT_BYTES { return false; }
    sha256(bytes) == AOT_SHA256
}

#[cfg(any(all(target_os = "visionos", not(feature = "jit"), game_code), test))]
fn sha256(bytes: &[u8]) -> [u8; 32] {
    // CommonCrypto is exported by libSystem on macOS and visionOS. Hash the
    // immutable mapped segment once, without allocating another game copy.
    extern "C" {
        fn CC_SHA256(data: *const std::ffi::c_void, length: u32, digest: *mut u8) -> *mut u8;
    }
    let mut digest = [0u8; 32];
    unsafe { CC_SHA256(bytes.as_ptr().cast(), bytes.len() as u32, digest.as_mut_ptr()) };
    digest
}

fn verify_embedded_abi() -> bool {
    // A separate motion host can accept only the exact AOT hash pinned and
    // verified during load_module. Its generator preserves these source fields;
    // arbitrary new AOTs still cannot borrow the original layout certificate.
    if crate::xr_motion_aim::diagnostic_enabled() { return true; }
    #[cfg(all(target_os = "visionos", not(feature = "jit"), game_code))]
    {
        extern "C" {
            #[link_name = "\x01_muguet_game_code"] static START: u8;
            #[link_name = "\x01_muguet_game_code_end"] static END: u8;
        }
        let (start, end) = (&raw const START, &raw const END);
        let verified = (end as usize).checked_sub(start as usize)
            .filter(|length| *length == AOT_BYTES)
            .is_some_and(|length| matches_supported_aot(unsafe { std::slice::from_raw_parts(start, length) }));
        eprintln!("[pad] exact guest input context ABI verified={verified}");
        verified
    }
    #[cfg(not(all(target_os = "visionos", not(feature = "jit"), game_code)))]
    { false }
}

#[cfg(test)]
mod sights_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn sights_follow_local_player_info_and_scoped_camera_without_guessing() {
        let mut memory = BTreeMap::from([
            ((PED_FACTORY, 8), 0x1000), ((0x1008, 8), 0x2000), ((0x2000 + PED_PLAYER_INFO, 8), 0x3000),
            ((0x3000 + PLAYER_FPS_FLAGS, 1), 0b1_1001), ((CAMERA_DIRECTOR, 8), 0x4000), ((0x4000 + 960, 8), 0x5000),
            ((0x5000, 8), 77),
        ]);
        let read = |m: &BTreeMap<(u64, usize), u64>| read_sights_from(|a, w| m.get(&(a, w)).copied());
        assert_eq!(read(&memory), Some(Sights { on: false, scoped_camera: false }), "other FPS bits are not sights");
        memory.insert((0x3000 + PLAYER_FPS_FLAGS, 1), 0b10);
        assert_eq!(read(&memory), Some(Sights { on: true, scoped_camera: false }));
        memory.insert((0x5000, 8), SCOPED_AIM_CAMERA_VTABLE);
        assert_eq!(read(&memory), Some(Sights { on: true, scoped_camera: true }));
        memory.insert((0x4000 + 960, 8), 0);
        assert_eq!(read(&memory), Some(Sights { on: true, scoped_camera: false }));
        // Absent local player/info or an unreadable field is unknown, never off.
        memory.insert((0x2000 + PED_PLAYER_INFO, 8), 0);
        assert_eq!(read(&memory), None);
        memory.insert((0x1008, 8), 0);
        assert_eq!(read(&memory), None);
        assert_eq!(read_sights_from(|_, _| None), None);
    }
}

#[cfg(all(test, feature = "jit"))]
mod tests {
    use super::*;

    fn from_bytes(bytes: &[u8]) -> Option<Context> {
        read_context(|address, width| {
            let start = usize::try_from(address).ok()?;
            let data = bytes.get(start..start.checked_add(width)?)?;
            let mut word = [0; 8];
            word[..width].copy_from_slice(data);
            Some(u64::from_le_bytes(word))
        })
    }

    #[test]
    fn guest_context_rejects_out_of_bounds_and_unaligned_reads() {
        assert_eq!(from_bytes(&[]), None);
        let mut storage = [0u64; 2];
        let pointer = storage.as_mut_ptr().cast();
        assert_eq!(unsafe { read_word(pointer, 16, u64::MAX, 4) }, None);
        assert_eq!(unsafe { read_word(pointer, 16, 1, 4) }, None);
        assert_eq!(unsafe { read_word(pointer, 16, 12, 8) }, None);
        assert_eq!(unsafe { read_word(pointer, 16, 0, 3) }, None);
        assert_eq!(unsafe { read_word(pointer, 16, 8, 8) }, Some(0));
    }

    #[test]
    fn game_abi_rejects_unrelated_executable_bytes() {
        assert!(!matches_supported_aot(&[]));
        assert!(!matches_supported_aot(b"different game"));
        assert_eq!(sha256(b"abc"), [0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea,
            0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3,
            0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad]);
    }

    /// Pure CPU proof against the actual unmodified getter body; no game
    /// runtime, graphics, audio, simulator, or device is initialized.
    #[test]
    #[ignore = "requires the user's exact original game.wasm and game.visionos.cwasm fixtures"]
    fn pause_predicate_matches_original_guest_getter() {
        use wasm_encoder::{CodeSection, ExportKind, ExportSection, FunctionSection,
            MemorySection, MemoryType, Module, TypeSection, ValType};
        let original = std::path::PathBuf::from(std::env::var_os("MUGUET_TRACE_ORIGINAL_WASM")
            .expect("set MUGUET_TRACE_ORIGINAL_WASM to the exact original game.wasm"));
        let wasm = std::fs::read(&original).unwrap();
        let original_hash = sha256(&wasm).iter().map(|v| format!("{v:02x}")).collect::<String>();
        assert_eq!(original_hash, "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0");
        let aot = std::fs::read(original.with_file_name("game.visionos.cwasm")).unwrap();
        assert!(matches_supported_aot(&aot));
        // Exact function body range from the validated name/function inventory:
        // function36254, CPauseMenu::IsActive(PM_ActiveFlags), (i32)->i32.
        let body = &wasm[21_673_396..21_673_790];
        let mut module = Module::new();
        let mut types = TypeSection::new();
        types.ty().function([ValType::I32], [ValType::I32]);
        module.section(&types);
        let mut functions = FunctionSection::new();
        functions.function(0);
        module.section(&functions);
        let mut memories = MemorySection::new();
        memories.memory(MemoryType { minimum: 320, maximum: Some(320), memory64: true,
            shared: false, page_size_log2: None });
        module.section(&memories);
        let mut exports = ExportSection::new();
        exports.export("getter", ExportKind::Func, 0);
        exports.export("memory", ExportKind::Memory, 0);
        module.section(&exports);
        let mut code = CodeSection::new();
        code.raw(body);
        module.section(&code);
        let mut config = wasmtime::Config::new();
        config.wasm_memory64(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let module = wasmtime::Module::new(&engine, module.finish()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let getter = instance.get_typed_func::<i32, i32>(&mut store, "getter").unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let table = 20_000_000u64;
        let mut cases = 0;
        // Includes steady gameplay, opening/closing transitions, independent
        // overlays, every subset of loaded pause components, and invalid indices.
        for active in [0u8, 1, 2, 3] {
            for pending in [0u8, 1, 2, 3] {
                for modal in [0u8, 1, 2, 3] {
                    for overlay in [0u32, 1, u32::MAX] {
                        for ready_mask in 0u8..16 {
                            for invalid_index in [false, true] {
                                let data = memory.data_mut(&mut store);
                                data[PAUSE_ACTIVE as usize] = active;
                                data[PAUSE_PENDING as usize] = pending;
                                data[PAUSE_MODAL as usize] = modal;
                                data[PAUSE_OVERLAY as usize..PAUSE_OVERLAY as usize + 4].copy_from_slice(&overlay.to_le_bytes());
                                data[PHONE_INSTANTIATED as usize] = ready_mask & 1;
                                data[PAUSE_COMPONENT_COUNT as usize..PAUSE_COMPONENT_COUNT as usize + 2].copy_from_slice(&4u16.to_le_bytes());
                                data[PAUSE_COMPONENT_TABLE as usize..PAUSE_COMPONENT_TABLE as usize + 8].copy_from_slice(&table.to_le_bytes());
                                for (index, address) in PAUSE_COMPONENT_INDICES.iter().enumerate() {
                                    let component = if invalid_index && index == 3 { u32::MAX } else { index as u32 };
                                    data[*address as usize..*address as usize + 4].copy_from_slice(&component.to_le_bytes());
                                    let address = table as usize + index * 512 + 440;
                                    let state = if ready_mask & (1 << index) != 0 { 3u32 } else { 0 };
                                    data[address..address + 4].copy_from_slice(&state.to_le_bytes());
                                }
                                let expected = getter.call(&mut store, 1).unwrap() != 0;
                                let result = from_bytes(memory.data(&store)).unwrap();
                                assert_eq!(result.paused, expected, "active={active} pending={pending} modal={modal} overlay={overlay} ready={ready_mask} invalid={invalid_index}");
                                assert_eq!(result.phone, ready_mask & 1 != 0);
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 6144);
    }
}
