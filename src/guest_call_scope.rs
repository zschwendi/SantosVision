//! Inactive guest-call cleanup prerequisite; this does not install foreground hooks.
//!
//! Production observation is permanently OFF. There is no environment switch,
//! producer-scope API, attribution query, or new guest import. The signed patch5
//! does not export __tls_base, so production TLS is explicitly unknown. Enabling
//! this later requires certified producer hooks, actual instance/TLS identity and
//! packet provenance; a selected script at TLS+3088 is a different identity.
//!
//! One HostState is created per Store, and instantiate currently creates exactly
//! one Instance in that Store. The nonce identifies that HostState lifetime, NOT
//! arbitrary additional Instances a future caller might put into the same Store.
//! Such multi-instance use needs a distinct certified instance token first.
use std::sync::atomic::{AtomicU64, Ordering};
use wasmtime::AsContextMut;
use crate::HostState;

static NEXT_LIFETIME: AtomicU64 = AtomicU64::new(1);

pub(crate) struct StoreLifetime {
    nonce: u64,
    // Synthetic tests only. No runtime caller can manufacture known TLS here.
    #[cfg(test)]
    tls: Option<u64>,
}
impl StoreLifetime {
    pub(crate) fn new() -> Self {
        // Exhaustion fails closed instead of reusing an old identity.
        let nonce = NEXT_LIFETIME.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |value| value.checked_add(1)).unwrap_or(0);
        Self { nonce, #[cfg(test)] tls: None }
    }
}

/// Read-only producer identity for future packet registration. This is the
/// current Store/HostState lifetime, not a TLS or general Instance identity.
#[allow(dead_code)]
pub(crate) fn producer_store_lifetime(state:&HostState)->u64 { state.guest_calls.nonce }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind { Ordinary, Instantiate, Mailbox }

#[inline(always)]
fn observing() -> bool {
    #[cfg(test)] { return ledger::ENABLED.with(|flag| flag.get()); }
    #[cfg(not(test))] { false }
}

/// Finish cleanup BEFORE returning Result, including before a caller catches
/// LongJmp/Unwind or invokes stack_restore/setThrew. Does not inspect/consume the
/// existing host PENDING_* flags, or change guest error/panic handling policy.
#[inline(always)]
pub(crate) fn call<S, T, E>(store: &mut S, kind: Kind,
    invoke: impl FnOnce(&mut S) -> Result<T, E>) -> Result<T, E>
where S: AsContextMut<Data = HostState> {
    if !observing() {
        if !crate::xr_motion_aim::gameplay_enabled() { return invoke(store); }
        let prefix=store.as_context().data().motion_aim.boundary_prefix();
        let arm_prefix=store.as_context_mut().data_mut().motion_arms.boundary_prefix();
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||invoke(store)));
        match result{
            Ok(result)=>{if result.is_err(){store.as_context_mut().data_mut().motion_aim.recover_boundary(&prefix);store.as_context_mut().data_mut().motion_arms.recover_boundary(&arm_prefix);}
                else{store.as_context_mut().data_mut().motion_arms.finish_boundary(&arm_prefix);}return result;},
            Err(panic)=>{store.as_context_mut().data_mut().motion_aim.recover_boundary(&prefix);store.as_context_mut().data_mut().motion_arms.recover_boundary(&arm_prefix);std::panic::resume_unwind(panic);}
        }
    }
    let before = ledger::Execution::capture(store.as_context().data());
    let guard = ledger::Guard::enter(before, kind);
    // No TLS RefCell borrow or lock is held while invoking/reentering Wasmtime.
    let result = invoke(store);
    let after = ledger::Execution::capture(store.as_context().data());
    guard.finish(after, result.is_ok());
    result
}

// Kept private: this ledger conveys no production foreground authority.
#[allow(dead_code)]
mod ledger {
    use super::*;
    use std::{cell::RefCell, marker::PhantomData, rc::Rc};
    #[cfg(test)] use std::cell::Cell;
    const MAX_FRAMES: usize = 64;
    const MAX_MARKS: usize = 64;
    thread_local! {
        static STATE: RefCell<Ledger> = RefCell::new(Ledger::default());
        #[cfg(test)] pub(super) static ENABLED: Cell<bool> = const { Cell::new(false) };
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Execution { store: u64, pthread: u64, tls: Option<u64> }
    impl Execution {
        pub(super) fn capture(state: &HostState) -> Self {
            Self { store: state.guest_calls.nonce, pthread: state.pthread,
                #[cfg(test)] tls: state.guest_calls.tls,
                #[cfg(not(test))] tls: None }
        }
        fn known(self) -> bool { self.store != 0 && self.tls.is_some_and(|tls| tls != 0) }
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Mark { token: u64, invocation: u64, execution: Execution }
    struct Frame {
        token: u64, execution: Execution, prefix: Vec<Mark>, floor: usize,
        suspended: bool,
    }
    #[derive(Default)]
    struct Ledger { frames: Vec<Frame>, marks: Vec<Mark>, sequence: u64, poisoned: bool }
    impl Ledger {
        fn poison(&mut self) { self.poisoned = true; self.marks.clear(); }
        fn next(&mut self) -> Option<u64> {
            let next = self.sequence.checked_add(1);
            match next { Some(value) => self.sequence = value, None => self.poison() }
            next
        }
        fn enter(&mut self, execution: Execution, kind: Kind) -> Option<u64> {
            if self.frames.is_empty() { self.marks.clear(); self.poisoned = false; }
            if self.frames.len() >= MAX_FRAMES { self.poison(); return None; }
            let token = self.next()?;
            let mut floor = self.marks.len();
            let mut suspended = kind == Kind::Mailbox;
            if let Some(parent) = self.frames.last() {
                suspended |= parent.suspended;
                if parent.execution.store == execution.store && parent.execution != execution {
                    // Same Store but changed pthread/TLS is not a new trusted context.
                    self.poison();
                    floor = 0;
                } else if parent.execution == execution && kind == Kind::Ordinary && !suspended {
                    floor = parent.floor;
                }
            }
            self.frames.push(Frame { token, execution, prefix: self.marks.clone(), floor, suspended });
            Some(token)
        }
        fn finish(&mut self, token: u64, after: Execution, success: bool, panic: bool) {
            if self.frames.last().map(|f| f.token) != Some(token) {
                self.poison(); return;
            }
            let frame = self.frames.pop().unwrap();
            let prefix_unchanged = self.marks.get(..frame.prefix.len()) == Some(frame.prefix.as_slice());
            if panic || after != frame.execution || !prefix_unchanged
                || (success && self.marks.len() != frame.prefix.len()) {
                self.poison();
            } else {
                // An error discards only child marks. A balanced normal return
                // retains the exact prior tokens, including a valid outer scope.
                self.marks.truncate(frame.prefix.len());
            }
            if self.frames.is_empty() { self.marks.clear(); self.poisoned = false; }
        }
        #[cfg(test)]
        fn mark(&mut self) -> Option<u64> {
            let frame = self.frames.last()?;
            if self.poisoned || frame.suspended || !frame.execution.known() { return None; }
            if self.marks.len() >= MAX_MARKS { self.poison(); return None; }
            let execution = frame.execution;
            let invocation = frame.token;
            let token = self.next()?;
            self.marks.push(Mark { token, invocation, execution });
            Some(token)
        }
        #[cfg(test)]
        fn leave(&mut self, token: u64) {
            let valid = self.frames.last().zip(self.marks.last()).is_some_and(|(f,m)|
                m.token == token && m.invocation == f.token && m.execution == f.execution);
            if valid { self.marks.pop(); } else { self.poison(); }
        }
        #[cfg(test)]
        fn visible(&self) -> Option<u64> {
            let frame = self.frames.last()?;
            if self.poisoned || frame.suspended || !frame.execution.known() { return None; }
            self.marks.get(frame.floor..)?.last()
                .filter(|m| m.execution == frame.execution).map(|m| m.token)
        }
    }
    pub(super) struct Guard {
        token: Option<u64>, before: Execution,
        // Cleanup belongs to this OS thread; guards cannot migrate across threads.
        _not_send: PhantomData<Rc<()>>,
    }
    impl Guard {
        pub(super) fn enter(before: Execution, kind: Kind) -> Self {
            let token = STATE.with(|s| s.borrow_mut().enter(before, kind));
            Self { token, before, _not_send: PhantomData }
        }
        pub(super) fn finish(mut self, after: Execution, success: bool) {
            if let Some(token) = self.token.take() {
                STATE.with(|s| s.borrow_mut().finish(token, after, success, false));
            }
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(token) = self.token.take() {
                STATE.with(|s| s.borrow_mut().finish(token, self.before, false, true));
            }
        }
    }
    #[cfg(test)]
    pub(super) mod fixture {
        use super::*;
        pub(in crate::guest_call_scope) struct Enable;
        impl Enable {
            pub(in crate::guest_call_scope) fn new() -> Self {
                STATE.with(|s| *s.borrow_mut() = Ledger::default());
                ENABLED.with(|s| assert!(!s.replace(true)));
                Self
            }
        }
        impl Drop for Enable {
            fn drop(&mut self) {
                ENABLED.with(|s| s.set(false));
                STATE.with(|s| *s.borrow_mut() = Ledger::default());
            }
        }
        pub(in crate::guest_call_scope) fn mark() -> Option<u64> { STATE.with(|s| s.borrow_mut().mark()) }
        pub(in crate::guest_call_scope) fn leave(token: u64) { STATE.with(|s| s.borrow_mut().leave(token)); }
        pub(in crate::guest_call_scope) fn visible() -> Option<u64> { STATE.with(|s| s.borrow().visible()) }
        pub(in crate::guest_call_scope) fn empty() -> bool {
            STATE.with(|s| { let s=s.borrow(); s.frames.is_empty() && s.marks.is_empty() && !s.poisoned })
        }
        pub(in crate::guest_call_scope) fn corrupt_prefix() {
            STATE.with(|s| { let mut s=s.borrow_mut(); s.marks[0].token += 1000; });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledger::fixture::*;
    use wasmtime::{Engine, Store};
    fn state(tls: Option<u64>) -> HostState {
        let mut state=HostState::new(7); state.guest_calls.tls=tls; state
    }
    fn store() -> Store<HostState> { Store::new(&Engine::default(),state(Some(4096))) }
    fn success<S: AsContextMut<Data=HostState>>(s: &mut S, kind: Kind, f: impl FnOnce(&mut S)) {
        call(s,kind,|s| { f(s); Ok::<_, ()>(()) }).unwrap();
    }
    #[test]
    fn default_off_returns_values_and_errors_without_a_ledger() {
        assert!(!observing()); let mut s=store();
        assert_eq!(call(&mut s,Kind::Ordinary, |_| { assert!(empty()); assert_eq!(mark(),None); Ok::<_,i32>(42) }),Ok(42));
        assert_eq!(call(&mut s,Kind::Ordinary, |_| Err::<(),_>(19)),Err(19));
        assert!(empty());
    }
    #[test]
    fn nested_success_and_recovered_error_preserve_exact_outer_token() {
        let _enabled=Enable::new(); let mut s=store();
        success(&mut s,Kind::Ordinary,|s| {
            let outer=mark().unwrap();
            success(s,Kind::Ordinary, |_| { assert_eq!(visible(),Some(outer)); let child=mark().unwrap(); leave(child); });
            let result=call(s,Kind::Ordinary, |_| { mark().unwrap(); Err::<(),_>(crate::host::LongJmp) });
            assert!(result.is_err()); assert_eq!(visible(),Some(outer));
            // A recovery guest call sees only the original parent after cleanup.
            success(s,Kind::Ordinary, |_| assert_eq!(visible(),Some(outer)));
            leave(outer);
        }); assert!(empty());
    }
    #[test]
    fn mailbox_and_all_descendants_suspend_parent_attribution() {
        let _enabled=Enable::new(); let mut s=store();
        success(&mut s,Kind::Ordinary,|s| {
            let outer=mark().unwrap();
            success(s,Kind::Mailbox,|s| {
                assert_eq!(visible(),None); assert_eq!(mark(),None);
                success(s,Kind::Ordinary, |_| { assert_eq!(visible(),None); assert_eq!(mark(),None); });
            }); assert_eq!(visible(),Some(outer)); leave(outer);
        }); assert!(empty());
    }
    #[test]
    fn same_thread_different_stores_are_isolated_then_resume_parent() {
        let _enabled=Enable::new(); let mut a=store(); let mut b=store();
        assert_ne!(a.data().guest_calls.nonce,b.data().guest_calls.nonce);
        success(&mut a,Kind::Ordinary, |_| {
            let outer=mark().unwrap();
            success(&mut b,Kind::Ordinary, |_| { assert_eq!(visible(),None); let child=mark().unwrap(); assert_eq!(visible(),Some(child)); leave(child); });
            assert_eq!(visible(),Some(outer)); leave(outer);
        }); assert!(empty());
    }
    #[test]
    fn unknown_tls_and_changed_pthread_or_tls_fail_closed() {
        let _enabled=Enable::new(); let mut s=Store::new(&Engine::default(),state(None));
        success(&mut s,Kind::Ordinary, |_| assert_eq!(mark(),None));
        for change_pthread in [false,true] {
            s.data_mut().guest_calls.tls=Some(4096);
            success(&mut s,Kind::Ordinary,|s| {
                mark().unwrap();
                if change_pthread { s.data_mut().pthread+=1; } else { s.data_mut().guest_calls.tls=Some(8192); }
                success(s,Kind::Ordinary, |_| { assert_eq!(visible(),None); assert_eq!(mark(),None); });
                assert_eq!(visible(),None);
            }); assert!(empty());
        }
        // Initialization can mutate pthread during the call without changing its result.
        assert_eq!(call(&mut s,Kind::Instantiate,|s| {s.data_mut().pthread+=1;Ok::<_,()>(5)}),Ok(5));
        assert!(empty());
    }
    #[test]
    fn normal_unbalanced_scope_and_modified_parent_prefix_fail_closed() {
        let _enabled=Enable::new(); let mut s=store();
        success(&mut s,Kind::Ordinary,|s| {
            mark().unwrap();
            success(s,Kind::Ordinary, |_| { mark().unwrap(); });
            assert_eq!(visible(),None);
        }); assert!(empty());
        success(&mut s,Kind::Ordinary,|s| {
            mark().unwrap();
            let _:Result<(),()> = call(s,Kind::Ordinary, |_| { corrupt_prefix(); Err(()) });
            assert_eq!(visible(),None); assert_eq!(mark(),None);
        }); assert!(empty());
        success(&mut s,Kind::Ordinary, |_| { let token=mark().unwrap(); leave(token+1); assert_eq!(visible(),None); });
        assert!(empty());
    }
    #[test]
    fn panic_unwind_invalidates_parent_and_next_root_is_clean() {
        let _enabled=Enable::new(); let mut s=store();
        success(&mut s,Kind::Ordinary,|s| {
            mark().unwrap();
            let panic=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                call(s,Kind::Ordinary, |_| -> Result<(),()> {mark().unwrap();panic!("synthetic guest host panic")})
            })); assert!(panic.is_err()); assert_eq!(visible(),None);
        }); assert!(empty());
        success(&mut s,Kind::Ordinary, |_| { let token=mark().unwrap(); leave(token); });
        assert!(empty());
    }
    #[test]
    fn bounded_frame_and_scope_caps_fail_closed_and_recover_next_root() {
        let _enabled=Enable::new(); let mut s=store();
        fn nest(s:&mut Store<HostState>,remaining:usize) {
            success(s,Kind::Ordinary,|s| {if remaining>0 {nest(s,remaining-1)} else {assert_eq!(mark(),None);}});
        }
        nest(&mut s,66); assert!(empty());
        success(&mut s,Kind::Ordinary, |_| { for _ in 0..64 {mark().unwrap();} assert_eq!(mark(),None); assert_eq!(visible(),None); });
        assert!(empty());
        success(&mut s,Kind::Ordinary, |_| { let token=mark().unwrap(); leave(token); });
    }

    #[cfg(feature="jit")]
    mod wasm {
        use super::*;
        use std::sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}};
        use wasmtime::{Caller,Linker,Module,Instance,Ref};
        use wasm_encoder::{CodeSection,EntityType,ExportKind,ExportSection,Function,FunctionSection,
            ImportSection,Instruction as I,TableSection,TableType,TypeSection,ValType,RefType};
        // Actual Wasm calls host::invoke through an adapter, then traps/longjmps
        // through a nested exported function; recovery exports call back into Rust.
        fn module(start_traps:bool) -> Vec<u8> {
            let mut m=wasm_encoder::Module::new(); let mut types=TypeSection::new();
            types.ty().function([ValType::I32],[]); // event
            types.ty().function([],[]); // invoke / outer / child / ok / trap
            types.ty().function([],[ValType::I64]); // stack get
            types.ty().function([ValType::I64],[]); // restore
            types.ty().function([ValType::I64,ValType::I32],[]); // setThrew
            m.section(&types);
            let mut imports=ImportSection::new(); imports.import("host","event",EntityType::Function(0));
            imports.import("host","invoke",EntityType::Function(1)); m.section(&imports);
            let mut functions=FunctionSection::new(); for t in [1,1,2,3,4,1,1,1] { functions.function(t); } m.section(&functions);
            let mut tables=TableSection::new(); tables.table(TableType { element_type:RefType::FUNCREF,
                table64:false,minimum:1,maximum:Some(1),shared:false }); m.section(&tables);
            let mut exports=ExportSection::new();
            for (name,index) in [("outer",2),("child",3),("emscripten_stack_get_current",4),("_emscripten_stack_restore",5),("setThrew",6),("ok",7),("trap",8),("unwind",9)] {
                exports.export(name,ExportKind::Func,index);
            } exports.export("__indirect_function_table",ExportKind::Table,0); m.section(&exports);
            if start_traps {m.section(&wasm_encoder::StartSection { function_index:8 });}
            let mut code=CodeSection::new();
            let bodies:Vec<Vec<I<'static>>>=vec![
                vec![I::I32Const(0),I::Call(0),I::Call(1),I::I32Const(1),I::Call(0)],
                vec![I::I32Const(2),I::Call(0),I::I32Const(3),I::Call(0)],
                vec![I::I64Const(64)],
                vec![I::I32Const(4),I::Call(0)],
                vec![I::I32Const(5),I::Call(0)],
                vec![I::I32Const(6),I::Call(0)],
                vec![I::I32Const(2),I::Call(0),I::Unreachable],
                vec![I::I32Const(2),I::Call(0),I::I32Const(8),I::Call(0)],
            ];
            for body in bodies {let mut f=Function::new([]); for op in body {f.instruction(&op);} f.instruction(&I::End); code.function(&f);} m.section(&code); m.finish()
        }
        fn setup(start_traps:bool) -> (Store<HostState>,Result<Instance,wasmtime::Error>,Arc<Mutex<Vec<i32>>>) {
            let engine=Engine::default(); let module=Module::new(&engine,module(start_traps)).unwrap();
            let mut store=Store::new(&engine,state(Some(4096))); let mut linker=Linker::new(&engine);
            let parent=Arc::new(AtomicU64::new(0)); let events=Arc::new(Mutex::new(Vec::new()));
            let sink=events.clone();
            linker.func_wrap("host","event",move |_caller:Caller<'_,HostState>,event:i32| -> wasmtime::Result<()> {
                sink.lock().unwrap().push(event);
                match event {
                    0=>parent.store(mark().unwrap(),Ordering::Relaxed),
                    1=>{let p=parent.load(Ordering::Relaxed);assert_eq!(visible(),Some(p));leave(p);},
                    2=>{mark().unwrap();},
                    3=>return crate::host::guest_scope_test_longjmp().map_err(wasmtime::Error::from_anyhow),
                    8=>return crate::host::guest_scope_test_unwind().map_err(wasmtime::Error::from_anyhow),
                    4|5=>assert_eq!(visible(),Some(parent.load(Ordering::Relaxed)),"child cleaned before actual recovery export"),
                    6=>{assert_eq!(visible(),None);let t=mark().unwrap();leave(t);},
                    _=>panic!("unexpected fixture event"),
                } Ok(())
            }).unwrap();
            linker.func_wrap("host","invoke",|mut caller:Caller<'_,HostState>| -> wasmtime::Result<()> {
                crate::host::guest_scope_test_invoke(&mut caller,&[wasmtime::Val::I32(0)],&mut []).map_err(wasmtime::Error::from_anyhow)
            }).unwrap();
            let instance=call(&mut store,Kind::Instantiate,|s|linker.instantiate(s,&module));
            if let Ok(instance)=instance {
                let child=instance.get_func(&mut store,"child").unwrap();
                instance.get_table(&mut store,"__indirect_function_table").unwrap().set(&mut store,0,Ref::Func(Some(child))).unwrap();
                (store,Ok(instance),events)
            } else {(store,instance,events)}
        }
        #[test]
        fn actual_host_invoke_longjmp_cleans_before_stack_restore_and_set_threw() {
            let _enabled=Enable::new(); assert!(!crate::host::take_longjmp());
            let (mut s,instance,events)=setup(false); let instance=instance.unwrap();
            let outer=instance.get_typed_func::<(),()>(&mut s,"outer").unwrap();
            call(&mut s,Kind::Ordinary,|s|outer.call(s,())).unwrap();
            assert_eq!(*events.lock().unwrap(),[0,2,3,4,5,1]);
            assert!(!crate::host::take_longjmp()); assert!(empty());
            let ok=instance.get_typed_func::<(),()>(&mut s,"ok").unwrap();
            call(&mut s,Kind::Ordinary,|s|ok.call(s,())).unwrap(); assert!(empty());
        }
        #[test]
        fn actual_wasm_trap_then_subsequent_call_has_no_scope_leak() {
            let _enabled=Enable::new(); let (mut s,instance,_)=setup(false); let instance=instance.unwrap();
            let trap=instance.get_typed_func::<(),()>(&mut s,"trap").unwrap();
            assert!(call(&mut s,Kind::Ordinary,|s|trap.call(s,())).is_err()); assert!(empty());
            let ok=instance.get_typed_func::<(),()>(&mut s,"ok").unwrap();
            call(&mut s,Kind::Ordinary,|s|ok.call(s,())).unwrap(); assert!(empty());
        }
        #[test]
        fn actual_wasm_unwind_cleans_before_live_runtime_error_handler() {
            let _enabled=Enable::new(); assert!(!crate::host::take_unwind());
            let (mut s,instance,_)=setup(false); let instance=instance.unwrap();
            let unwind=instance.get_typed_func::<(),()>(&mut s,"unwind").unwrap();
            match call(&mut s,Kind::Ordinary,|s|unwind.call(s,())) {
                Err(_) if crate::host::take_unwind()=>assert!(empty()),
                _=>panic!("expected unchanged live-runtime unwind behavior"),
            }
            assert!(!crate::host::take_unwind());
            let ok=instance.get_typed_func::<(),()>(&mut s,"ok").unwrap();
            call(&mut s,Kind::Ordinary,|s|ok.call(s,())).unwrap(); assert!(empty());
        }
        #[test]
        fn actual_wasm_start_import_then_trap_cleans_instantiation_boundary() {
            let _enabled=Enable::new(); let (_,instance,events)=setup(true);
            assert!(instance.is_err()); assert_eq!(*events.lock().unwrap(),[2]); assert!(empty());
        }
    }
}
