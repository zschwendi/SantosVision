// Structural certificate for the pending-mailbox hazard at producer-locked
// imports. It is not a proof that no sender can ever run.
//
// Checked on the exact original: import73 (_emscripten_notify_mailbox_postmessage)
// is called only by do_proxy, in its waiting_async==0 branch, and do_proxy is the
// only task enqueuer. Closing over direct callers and over every code, data or
// global-initializer literal equal to a member's table slot leaves only the JS-glue
// exports _emscripten_run_js_on_main_thread[_done] as entries; no patch-exported
// mp: command target joins that island.
//
// Assumptions this certificate cannot discharge:
// A1 every table index reaching call_indirect or host invoke_* originates from
//    such a literal (no computed or corrupted function pointer);
// A2 every host export lookup is a literal or "mp:"-prefixed name (enumerated in
//    docs/MAILBOX-REACHABILITY-20261024.md; guarded lexically below);
// A3 the running module derives from this original through factories that keep
//    these bodies canonical (patch5 changes only the Emit midpoint).
// Under them nothing reaches deliver_pending_mailbox. Senders that take
// do_proxy's atomic-notify branch are invisible to the host in any case.
// Indices are original; factories append imports after the original 85.
//
// Parser only: the private game is never instantiated, compiled or executed.

use std::collections::{BTreeMap, BTreeSet};
use wasmparser::{ElementItems, ElementKind, ExternalKind, KnownCustom, MemArg, Name, Operator as O, Parser, Payload, TypeRef};

const ORIGINAL: &str = "/Users/zach/gaming/game-assets/playgta5-full-game-v1/game.wasm";
const ORIGINAL_SHA256: &str = "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0";
const PRODUCER_MUTEX: u64 = 8348864;
const CRASHED_THREAD_ID: u64 = 29595928;
const WAITING_ASYNC: u64 = 204;

// (memarg, writes memory). Notify and wait read only.
fn memarg(op: &O) -> Option<(MemArg, bool)> {
    use O::*;
    if let I32Load { memarg } | I64Load { memarg } | F32Load { memarg } | F64Load { memarg }
        | I32Load8S { memarg } | I32Load8U { memarg } | I32Load16S { memarg } | I32Load16U { memarg }
        | I64Load8S { memarg } | I64Load8U { memarg } | I64Load16S { memarg } | I64Load16U { memarg }
        | I64Load32S { memarg } | I64Load32U { memarg } | V128Load { memarg }
        | MemoryAtomicNotify { memarg } | MemoryAtomicWait32 { memarg } | MemoryAtomicWait64 { memarg }
        | I32AtomicLoad { memarg } | I64AtomicLoad { memarg } | I32AtomicLoad8U { memarg } | I32AtomicLoad16U { memarg }
        | I64AtomicLoad8U { memarg } | I64AtomicLoad16U { memarg } | I64AtomicLoad32U { memarg } = op {
        return Some((*memarg, false));
    }
    Some((*match op {
        I32Store { memarg } | I64Store { memarg }
        | F32Store { memarg } | F64Store { memarg } | I32Store8 { memarg } | I32Store16 { memarg }
        | I64Store8 { memarg } | I64Store16 { memarg } | I64Store32 { memarg } | V128Store { memarg }
        | I32AtomicStore { memarg } | I64AtomicStore { memarg } | I32AtomicStore8 { memarg } | I32AtomicStore16 { memarg }
        | I64AtomicStore8 { memarg } | I64AtomicStore16 { memarg } | I64AtomicStore32 { memarg }
        | I32AtomicRmwAdd { memarg } | I64AtomicRmwAdd { memarg } | I32AtomicRmw8AddU { memarg } | I32AtomicRmw16AddU { memarg }
        | I64AtomicRmw8AddU { memarg } | I64AtomicRmw16AddU { memarg } | I64AtomicRmw32AddU { memarg }
        | I32AtomicRmwSub { memarg } | I64AtomicRmwSub { memarg } | I32AtomicRmw8SubU { memarg } | I32AtomicRmw16SubU { memarg }
        | I64AtomicRmw8SubU { memarg } | I64AtomicRmw16SubU { memarg } | I64AtomicRmw32SubU { memarg }
        | I32AtomicRmwAnd { memarg } | I64AtomicRmwAnd { memarg } | I32AtomicRmw8AndU { memarg } | I32AtomicRmw16AndU { memarg }
        | I64AtomicRmw8AndU { memarg } | I64AtomicRmw16AndU { memarg } | I64AtomicRmw32AndU { memarg }
        | I32AtomicRmwOr { memarg } | I64AtomicRmwOr { memarg } | I32AtomicRmw8OrU { memarg } | I32AtomicRmw16OrU { memarg }
        | I64AtomicRmw8OrU { memarg } | I64AtomicRmw16OrU { memarg } | I64AtomicRmw32OrU { memarg }
        | I32AtomicRmwXor { memarg } | I64AtomicRmwXor { memarg } | I32AtomicRmw8XorU { memarg } | I32AtomicRmw16XorU { memarg }
        | I64AtomicRmw8XorU { memarg } | I64AtomicRmw16XorU { memarg } | I64AtomicRmw32XorU { memarg }
        | I32AtomicRmwXchg { memarg } | I64AtomicRmwXchg { memarg } | I32AtomicRmw8XchgU { memarg } | I32AtomicRmw16XchgU { memarg }
        | I64AtomicRmw8XchgU { memarg } | I64AtomicRmw16XchgU { memarg } | I64AtomicRmw32XchgU { memarg }
        | I32AtomicRmwCmpxchg { memarg } | I64AtomicRmwCmpxchg { memarg } | I32AtomicRmw8CmpxchgU { memarg }
        | I32AtomicRmw16CmpxchgU { memarg } | I64AtomicRmw8CmpxchgU { memarg } | I64AtomicRmw16CmpxchgU { memarg }
        | I64AtomicRmw32CmpxchgU { memarg } => memarg,
        _ => return None,
    }, true))
}

#[derive(Default)]
struct Body {
    calls: Vec<(usize, u32)>,
    consts: Vec<(usize, u64)>,
    // (offset, memarg offset, is a store/RMW, is atomic notify)
    accesses: Vec<(usize, u64, bool, bool)>,
    ref_funcs: Vec<u32>,
    table_mutations: usize,
}

#[derive(Default)]
struct Module {
    imports: Vec<String>,
    names: BTreeMap<u32, String>,
    exports: BTreeMap<u32, String>,
    slots: BTreeMap<u32, Vec<u64>>,
    data: Vec<Vec<u8>>,
    global_consts: Vec<u64>,
    start: Option<u32>,
    bodies: BTreeMap<u32, Body>,
}

impl Module {
    fn parse(bytes: &[u8]) -> Module {
        let mut m = Module::default();
        let mut raw = Vec::new();
        for payload in Parser::new(0).parse_all(bytes) {
            match payload.unwrap() {
                Payload::ImportSection(s) => for i in s.into_imports() {
                    let i = i.unwrap();
                    if matches!(i.ty, TypeRef::Func(_)) { m.imports.push(i.name.to_string()); }
                    assert!(!matches!(i.ty, TypeRef::Table(_)), "imported tables would hide slot writers");
                },
                Payload::ExportSection(s) => for e in s {
                    let e = e.unwrap();
                    if e.kind == ExternalKind::Func { m.exports.insert(e.index, e.name.to_string()); }
                },
                Payload::StartSection { func, .. } => m.start = Some(func),
                Payload::ElementSection(s) => for e in s {
                    let e = e.unwrap();
                    let base = match &e.kind {
                        ElementKind::Active { offset_expr, .. } => match offset_expr.get_operators_reader().read().unwrap() {
                            O::I32Const { value } => value as u32 as u64,
                            O::I64Const { value } => value as u64,
                            other => panic!("unqualified element offset {other:?}"),
                        },
                        _ => panic!("unqualified passive or declared element segment"),
                    };
                    let ElementItems::Functions(items) = e.items else { panic!("unqualified element expressions") };
                    for (k, f) in items.into_iter().enumerate() { m.slots.entry(f.unwrap()).or_default().push(base + k as u64); }
                },
                Payload::DataSection(s) => for d in s { m.data.push(d.unwrap().data.to_vec()); },
                Payload::GlobalSection(s) => for g in s {
                    let mut ops = g.unwrap().init_expr.get_operators_reader();
                    while !ops.eof() {
                        match ops.read().unwrap() {
                            O::I32Const { value } => m.global_consts.push(value as u32 as u64),
                            O::I64Const { value } => m.global_consts.push(value as u64),
                            _ => {}
                        }
                    }
                },
                Payload::CodeSectionEntry(body) => raw.push(body),
                Payload::CustomSection(c) => if let KnownCustom::Name(names) = c.as_known() {
                    for n in names {
                        if let Name::Function(map) = n.unwrap() { for n in map { let n = n.unwrap(); m.names.insert(n.index, n.name.to_string()); } }
                    }
                },
                _ => {}
            }
        }
        let first = m.imports.len() as u32;
        for (n, body) in raw.into_iter().enumerate() {
            let mut b = Body::default();
            let mut ops = body.get_operators_reader().unwrap();
            while !ops.eof() {
                let at = ops.original_position() as usize;
                let op = ops.read().unwrap();
                match &op {
                    O::Call { function_index } | O::ReturnCall { function_index } => b.calls.push((at, *function_index)),
                    O::I32Const { value } => b.consts.push((at, *value as u32 as u64)),
                    O::I64Const { value } => b.consts.push((at, *value as u64)),
                    O::RefFunc { function_index } => b.ref_funcs.push(*function_index),
                    O::TableSet { .. } | O::TableInit { .. } | O::TableCopy { .. } | O::TableGrow { .. } | O::TableFill { .. } | O::ElemDrop { .. } => b.table_mutations += 1,
                    _ => {}
                }
                if let Some((arg, writes)) = memarg(&op) {
                    b.accesses.push((at, arg.offset, writes, matches!(op, O::MemoryAtomicNotify { .. })));
                }
            }
            m.bodies.insert(first + n as u32, b);
        }
        m
    }

    fn named(&self, name: &str) -> u32 {
        let found: Vec<u32> = self.names.iter().filter(|(_, n)| n.as_str() == name).map(|(f, _)| *f).collect();
        assert_eq!(found.len(), 1, "{name} must name exactly one function");
        found[0]
    }
    fn name(&self, f: u32) -> &str { self.names.get(&f).map(String::as_str).unwrap_or("?") }
    fn call_sites(&self, target: u32) -> Vec<(u32, usize)> {
        self.bodies.iter().flat_map(|(f, b)| b.calls.iter().filter(|(_, to)| *to == target).map(move |(at, _)| (*f, *at))).collect()
    }
    fn materializers(&self, value: u64) -> BTreeSet<u32> {
        self.bodies.iter().filter(|(_, b)| b.consts.iter().any(|(_, v)| *v == value)).map(|(f, _)| *f).collect()
    }
    fn in_data(&self, value: u64, width: usize) -> bool {
        let bytes = value.to_le_bytes();
        self.data.iter().any(|d| d.windows(width).any(|w| w == &bytes[..width]))
    }
}

// Lexical regression guard for A2: it catches literal names, not constructed
// strings. Comments may cite the names.
#[test]
fn host_source_has_no_literal_proxy_or_crash_export_name() {
    let forbidden = ["\"_emscripten_run_js_on_main_thread", "\"_emscripten_thread_crashed", "\"emscripten_proxy_"];
    let mut pending = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut scanned = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { pending.push(path); continue; }
            if path.extension().is_none_or(|e| e != "rs") || path.file_name().is_some_and(|n| n == "mailbox_reachability.rs") { continue; }
            let text = std::fs::read_to_string(&path).unwrap();
            for pattern in forbidden { assert!(!text.contains(pattern), "{} names {pattern}: re-certify mailbox reachability", path.display()); }
            scanned += 1;
        }
    }
    assert!(scanned > 20);
}

#[test]
#[ignore = "requires exact private original; parser only, never game instantiation"]
fn private_original_sender_island_closed_over_calls_and_literal_slots() {
    let bytes = std::fs::read(ORIGINAL).unwrap();
    assert_eq!(crate::xr_motion_patch::sha256(&bytes).unwrap(), ORIGINAL_SHA256);
    let m = Module::parse(&bytes);
    assert_eq!(m.imports.len(), 85);
    for (index, name) in [(0, "wasm_now_ms"), (37, "_emscripten_thread_set_strongref"), (38, "emscripten_exit_with_live_runtime"), (73, "_emscripten_notify_mailbox_postmessage")] {
        assert_eq!(m.imports[index], name);
    }
    assert_eq!(m.bodies.values().map(|b| b.table_mutations).sum::<usize>(), 0, "table slots are fixed by element segments");

    // The only host notification is do_proxy's !waiting_async branch.
    let do_proxy = m.named("do_proxy");
    assert_eq!(m.call_sites(73), vec![(do_proxy, 0x2e38a79)]);
    let b = &m.bodies[&do_proxy];
    let guard: Vec<_> = b.accesses.iter().filter(|(_, off, w, _)| *off == WAITING_ASYNC && !*w).map(|a| a.0).collect();
    let notify: Vec<_> = b.accesses.iter().filter(|a| a.3).map(|a| a.0).collect();
    assert_eq!((guard.len(), notify.len()), (1, 1));
    assert!(guard[0] < notify[0] && notify[0] < 0x2e38a79);
    let enqueue = m.named("em_task_queue_enqueue");
    assert!(m.call_sites(enqueue).iter().all(|(f, _)| *f == do_proxy) && !m.call_sites(enqueue).is_empty());

    // Close over direct callers and every code site materializing a member's
    // table slot as a literal. No slot literal may appear in data, globals or
    // ref.func. Computed slots are assumption A1, not checked.
    let ref_funcs: BTreeSet<u32> = m.bodies.values().flat_map(|b| b.ref_funcs.iter().copied()).collect();
    // A slot no code materializes (a directized pthread_once target) adds no
    // reachability; that exact set is pinned below.
    let mut island = BTreeSet::from([do_proxy]);
    let mut unmaterialized = BTreeSet::new();
    let mut work = vec![do_proxy];
    while let Some(f) = work.pop() {
        assert!(!ref_funcs.contains(&f) && m.start != Some(f), "{} escapes", m.name(f));
        let mut reach: BTreeSet<u32> = m.call_sites(f).into_iter().map(|(c, _)| c).collect();
        for slot in m.slots.get(&f).into_iter().flatten() {
            assert!(!m.in_data(*slot, 4) && !m.global_consts.contains(slot), "slot {slot} of {} escapes", m.name(f));
            let sites = m.materializers(*slot);
            if sites.is_empty() { unmaterialized.insert(m.name(f)); }
            reach.extend(sites);
        }
        for r in reach { if island.insert(r) { work.push(r); } }
    }
    assert_eq!(unmaterialized, BTreeSet::from(["init_active_ctxs"]));
    let entries: BTreeSet<&str> = island.iter().filter_map(|f| m.exports.get(f).map(String::as_str)).collect();
    assert_eq!(entries, BTreeSet::from(["_emscripten_run_js_on_main_thread", "_emscripten_run_js_on_main_thread_done"]));
    let names: BTreeSet<&str> = island.iter().map(|f| m.name(*f)).collect();
    assert!(["call_then_finish_task", "cancel_ctx", "cancel_active_ctxs", "emscripten_proxy_sync_with_ctx", "init_active_ctxs"].iter().all(|n| names.contains(n)), "{names:?}");

    // A2's "mp:" names resolve only to patch-added exports: no original export
    // has the prefix and patch5 exports mp: names only for mp::COMMANDS.
    assert!(m.exports.values().all(|e| !e.starts_with("mp:")));
    let by_name: BTreeMap<&str, u32> = m.names.iter().map(|(f, n)| (n.as_str(), *f)).collect();
    let commands: Vec<u32> = crate::mp::COMMANDS.iter().filter_map(|(_, f)| by_name.get(f).copied()).collect();
    assert!(!commands.is_empty());
    for f in commands { assert!(!island.contains(&f), "mp: command {} enters the sender island", m.name(f)); }

    // Yield's strongref/exit branch needs a crash word whose only literal
    // writer is the _emscripten_thread_crashed export, which has no callers.
    let (crashed, yield_fn) = (m.named("_emscripten_thread_crashed"), m.named("_emscripten_yield"));
    assert_eq!(m.materializers(CRASHED_THREAD_ID), BTreeSet::from([crashed, yield_fn]));
    assert!(m.exports.contains_key(&crashed) && m.call_sites(crashed).is_empty() && !m.slots.contains_key(&crashed));
    assert!(m.call_sites(37).iter().chain(m.call_sites(38).iter()).all(|(f, _)| *f == yield_fn));

    // Literal-offset RMW on the producer mutex is confined to the host's named
    // functions, each reached only by direct call; the address literal does
    // not appear in data or globals.
    let owners: BTreeSet<&str> = m.bodies.iter()
        .filter(|(_, b)| b.accesses.iter().any(|(_, off, w, _)| *off == PRODUCER_MUTEX && *w))
        .map(|(f, _)| m.name(*f)).collect();
    assert_eq!(owners, BTreeSet::from(super::PRODUCER_LOCK_FUNCTIONS));
    for name in super::PRODUCER_LOCK_FUNCTIONS {
        let f = m.named(name);
        assert!(!m.exports.contains_key(&f) && !m.slots.contains_key(&f) && !ref_funcs.contains(&f), "{name} is indirectly reachable");
    }
    let materialized: BTreeSet<&str> = m.materializers(PRODUCER_MUTEX).into_iter().map(|f| m.name(f)).collect();
    assert!(materialized.is_subset(&BTreeSet::from(super::PRODUCER_LOCK_FUNCTIONS)), "{materialized:?}");
    assert!(!m.in_data(PRODUCER_MUTEX, 8) && !m.global_consts.contains(&PRODUCER_MUTEX));

    // The reviewed under-lock clock calls remain exactly Reserve's two sites.
    let reserve = m.named("wasm_gpu::Reserve(unsigned int)");
    let clock: Vec<usize> = m.bodies[&reserve].calls.iter().filter(|(_, to)| *to == 0).map(|(at, _)| *at).collect();
    assert_eq!(clock, vec![0x17c22f, 0x17c266]);
    let reserve_callers: BTreeSet<&str> = m.call_sites(reserve).into_iter().map(|(f, _)| m.name(f)).collect();
    assert_eq!(reserve_callers, BTreeSet::from([super::PRODUCER_LOCK_FUNCTIONS[1], super::PRODUCER_LOCK_FUNCTIONS[2]]));
}
