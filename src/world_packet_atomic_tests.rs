//! Tiny CPU-only memory64 Wasm fixture, not the original game or native GPU.
//! Guest stores and shared-head publication are real Wasmtime operations.
//! The consumer/512-command acknowledgement are explicit protocol models;
//! these tests do not certify the original guest, live adapters or rendering.
use super::*;
use crate::{guest_call_scope, HostState};
use std::sync::{atomic::{AtomicU32, Ordering}, mpsc, Arc, Mutex};
use std::time::Duration;
use wasmtime::{Caller, Config, Engine, Linker, MemoryTypeBuilder, Module, SharedMemory, Store, TypedFunc};
use wasm_encoder::{BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function,
    FunctionSection, GlobalSection, GlobalType, ConstExpr, ImportSection, Instruction as I,
    MemArg, TypeSection, ValType as V};

const BYTES: u64 = 65536;
const TIMEOUT: Duration = Duration::from_secs(5);
type Pause = (mpsc::SyncSender<()>, Arc<Mutex<mpsc::Receiver<()>>>);

fn word(offset:u64)->MemArg { MemArg{offset,align:2,memory_index:0} }
fn fixture_wasm()->Vec<u8> {
    let mut wasm=wasm_encoder::Module::new();
    let mut types=TypeSection::new();
    types.ty().function([V::I64], []); // host midpoint
    types.ty().function([V::I64,V::I32,V::I32], []); // emit(cursor,value,publish)
    types.ty().function([V::I32], []); // publish head
    types.ty().function([V::I64], []); // raw wrap / abort
    types.ty().function([V::I64,V::I32], []); // overwrite after ack
    wasm.section(&types);
    let mut imports=ImportSection::new();
    imports.import("fixture","midpoint",EntityType::Function(0));
    imports.import("fixture","memory",EntityType::Memory(wasm_encoder::MemoryType {
        minimum:1,maximum:Some(1),memory64:true,shared:true,page_size_log2:None }));
    wasm.section(&imports);
    let mut functions=FunctionSection::new();
    for ty in [1,2,3,4,3] {functions.function(ty);}
    wasm.section(&functions);
    let mut globals=GlobalSection::new();
    globals.global(GlobalType{val_type:V::I32,mutable:true,shared:false},&ConstExpr::i32_const(0));
    wasm.section(&globals);
    let mut exports=ExportSection::new();
    for (name,index) in [("emit",1),("publish",2),("wrap",3),("overwrite",4),("abort",5)] {
        exports.export(name,ExportKind::Func,index);
    }
    wasm.section(&exports);
    let mut code=CodeSection::new();
    let mut emit=Function::new([]);
    // Batchable packet [2,1,value], completed before the host midpoint. The
    // explicit publish argument models a forced flush, not the opcode mask.
    emit.instruction(&I::LocalGet(0)).instruction(&I::I32Const(2)).instruction(&I::I32Store(word(0)));
    emit.instruction(&I::LocalGet(0)).instruction(&I::I32Const(1)).instruction(&I::I32Store(word(4)));
    emit.instruction(&I::LocalGet(0)).instruction(&I::LocalGet(1)).instruction(&I::I32Store(word(8)));
    emit.instruction(&I::LocalGet(0)).instruction(&I::Call(0));
    emit.instruction(&I::GlobalGet(0)).instruction(&I::I32Const(1)).instruction(&I::I32Add).instruction(&I::GlobalSet(0));
    emit.instruction(&I::LocalGet(2)).instruction(&I::GlobalGet(0)).instruction(&I::I32Const(64))
        .instruction(&I::I32GeU).instruction(&I::I32Or).instruction(&I::If(BlockType::Empty));
    emit.instruction(&I::I64Const(0)).instruction(&I::LocalGet(0)).instruction(&I::I32WrapI64)
        .instruction(&I::I32Const(12)).instruction(&I::I32Add).instruction(&I::I32AtomicStore(word(0)));
    emit.instruction(&I::I32Const(0)).instruction(&I::GlobalSet(0));
    emit.instruction(&I::End).instruction(&I::End);code.function(&emit);
    let mut publish=Function::new([]);
    publish.instruction(&I::I64Const(0)).instruction(&I::LocalGet(0))
        .instruction(&I::I32AtomicStore(word(0))).instruction(&I::I32Const(0)).instruction(&I::GlobalSet(0))
        .instruction(&I::End);code.function(&publish);
    let mut wrap=Function::new([]);
    wrap.instruction(&I::LocalGet(0)).instruction(&I::I64Const(1))
        .instruction(&I::I64Store(MemArg{align:3,..word(0)}));
    wrap.instruction(&I::I64Const(0)).instruction(&I::I32Const(64))
        .instruction(&I::I32AtomicStore(word(0))).instruction(&I::I32Const(0)).instruction(&I::GlobalSet(0))
        .instruction(&I::End);code.function(&wrap);
    let mut overwrite=Function::new([]);
    overwrite.instruction(&I::LocalGet(0)).instruction(&I::LocalGet(1))
        .instruction(&I::I32Store(word(0))).instruction(&I::End);code.function(&overwrite);
    let mut abort=Function::new([]);
    abort.instruction(&I::LocalGet(0)).instruction(&I::I32Const(42)).instruction(&I::I32Const(0))
        .instruction(&I::Call(1)).instruction(&I::Unreachable).instruction(&I::End);code.function(&abort);
    wasm.section(&code);wasm.finish()
}

// The fixture owns this fixed memory and grants byte access only while the
// producer is paused at its hook, or after published-head load and before ack.
// Checked alignment/extents precede native atomic access; no memory grow occurs.
fn atomic(memory:&SharedMemory,address:u64)->&AtomicU32 {
    assert_eq!(address%4,0);assert!(address.checked_add(4).unwrap()<=memory.data_size() as u64);
    unsafe { &*memory.data().as_ptr().cast::<u8>().add(address as usize).cast::<AtomicU32>() }
}
fn owned(memory:&SharedMemory,address:u64,bytes:usize)->Vec<u8> {
    assert!(address.checked_add(bytes as u64).unwrap()<=memory.data_size() as u64);
    let mut result=vec![0;bytes];
    unsafe {std::ptr::copy_nonoverlapping(memory.data().as_ptr().cast::<u8>().add(address as usize),result.as_mut_ptr(),bytes);}
    result
}
fn packet(memory:&SharedMemory,address:u64)->([u32;2],Vec<u8>) {
    let h=owned(memory,address,8);
    let header=[u32::from_le_bytes(h[..4].try_into().unwrap()),u32::from_le_bytes(h[4..].try_into().unwrap())];
    let n=header[1] as usize*4;assert!(n<=MAX_PACKET_BYTES);
    (header,owned(memory,address+8,n))
}

struct Fixture {
    store:Store<HostState>, memory:SharedMemory, transport:Arc<Mutex<Transport>>,session:Session,
    emit:TypedFunc<(i64,i32,i32),()>,publish:TypedFunc<i32,()>,wrap:TypedFunc<i64,()>,
    overwrite:TypedFunc<(i64,i32),()>,abort:TypedFunc<i64,()>,
}
impl Fixture {
    fn new(pause:Option<Pause>)->Self {
        let mut config=Config::new();config.wasm_memory64(true).wasm_threads(true).shared_memory(true);
        let engine=Engine::new(&config).unwrap();
        let mt=MemoryTypeBuilder::new().min(1).max(Some(1)).memory64(true).shared(true).build().unwrap();
        let memory=SharedMemory::new(&engine,mt).unwrap();
        atomic(&memory,0).store(64,Ordering::SeqCst);atomic(&memory,4).store(64,Ordering::SeqCst);
        let mut state=Transport::new(true);
        let session=state.register(Registration{process_id:std::process::id(),runtime_lifetime:1,
            memory_identity:memory.data().as_ptr() as usize as u64,memory_bytes:BYTES,ring_address:0,ring_bytes:BYTES}).unwrap();
        let transport=Arc::new(Mutex::new(state));
        let mut linker=Linker::<HostState>::new(&engine);
        let snapshot_memory=memory.clone();let state=transport.clone();
        linker.func_wrap("fixture","midpoint",move |caller:Caller<'_,HostState>,address:i64|->Result<(),wasmtime::Error>{
            let (header,operands)=packet(&snapshot_memory,address as u64);
            let producer=Producer{store_lifetime:guest_call_scope::producer_store_lifetime(caller.data()),observed_pthread:caller.data().pthread};
            // Release the transport lock before the test's optional guest pause.
            // Diagnostic rejection taints observation but never traps the guest.
            // A live hook must not leak Emit's mutex by propagating this error.
            let _=state.lock().unwrap().publish(session,producer,address as u64,header,Some(operands),None);
            if let Some((seen,resume))=&pause {seen.send(()).unwrap();resume.lock().unwrap().recv_timeout(TIMEOUT).unwrap();}
            Ok(())
        }).unwrap();
        let mut store=Store::new(&engine,HostState::new(256));
        linker.define(&store,"fixture","memory",memory.clone()).unwrap();
        let module=Module::new(&engine,fixture_wasm()).unwrap();
        let instance=linker.instantiate(&mut store,&module).unwrap();
        let emit=instance.get_typed_func(&mut store,"emit").unwrap();
        let publish=instance.get_typed_func(&mut store,"publish").unwrap();
        let wrap=instance.get_typed_func(&mut store,"wrap").unwrap();
        let overwrite=instance.get_typed_func(&mut store,"overwrite").unwrap();
        let abort=instance.get_typed_func(&mut store,"abort").unwrap();
        Self{store,memory,transport,session,emit,publish,wrap,overwrite,abort}
    }
}

// Explicit bounded consumer model. No Gpu::run/exec or rendering is invoked.
struct Consumer {cursor:u64,since:u32,head_snapshot:Option<u64>,acknowledgements:Vec<u64>}
impl Consumer {
    fn new()->Self {Self{cursor:64,since:0,head_snapshot:None,acknowledgements:vec![]}}
    fn one(&mut self,memory:&SharedMemory,transport:&Mutex<Transport>,session:Session)->Option<(Consumption,Vec<u8>)> {
        // Retain one head snapshot for the whole modeled native batch.
        let published=*self.head_snapshot.get_or_insert_with(||atomic(memory,0).load(Ordering::SeqCst) as u64);
        if self.cursor==published {self.head_snapshot=None;return None;}
        let (header,bytes)=packet(memory,self.cursor);
        let result=transport.lock().unwrap().consume(session,self.cursor,header,&bytes);
        // A diagnostic rejection cannot stall the modeled native consumer.
        // Fixtures use valid ring bytes even when attribution has been tainted.
        if header[0]==1 {self.cursor=64;}
        else {
            self.cursor+=8+header[1] as u64*4;self.since+=1;
            if self.since==512 {self.ack(memory);}
        }
        // Gpu::run publishes final tail after run_commands reaches sampled head,
        // even for one ordinary command or a wrap-only batch.
        if self.cursor==published {self.ack(memory);self.head_snapshot=None;}
        Some((result,bytes))
    }
    fn ack(&mut self,memory:&SharedMemory) {
        // The real worker also notifies/polls maps. Those effects are not modeled.
        atomic(memory,4).store(self.cursor as u32,Ordering::SeqCst);
        self.acknowledgements.push(self.cursor);self.since=0;
    }
}
fn matched(c:Consumption)->Metadata {match c{Consumption::Matched(m)=>m,other=>panic!("{other:?}")}}

#[test]
fn actual_memory64_midpoint_is_not_visible_until_atomic_head_store() {
    let (seen,waiting)=mpsc::sync_channel(1);let (resume,go)=mpsc::sync_channel(1);
    let mut fixture=Fixture::new(Some((seen,Arc::new(Mutex::new(go)))));
    let memory=fixture.memory.clone();let transport=fixture.transport.clone();let session=fixture.session;
    let producer=std::thread::spawn(move|| {fixture.emit.call(&mut fixture.store,(64,123,1)).unwrap();fixture});
    waiting.recv_timeout(TIMEOUT).unwrap();
    let mut consumer=Consumer::new();
    assert_eq!(atomic(&memory,0).load(Ordering::SeqCst),64);
    assert!(consumer.one(&memory,&transport,session).is_none());
    resume.send(()).unwrap();let _fixture=producer.join().unwrap();
    let (entry,bytes)=consumer.one(&memory,&transport,session).unwrap();
    assert_eq!(bytes,123i32.to_le_bytes());let entry=matched(entry);
    assert_eq!(entry.serial,1);assert!(entry.producer.store_lifetime!=0);
    assert!(!entry.publication_success_certified);assert!(entry.family_ancestry.is_none());
}

#[test]
fn actual_batched_head_exposes_multiple_older_midpoint_snapshots_in_order() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    f.emit.call(&mut f.store,(64,11,0)).unwrap();f.emit.call(&mut f.store,(76,22,0)).unwrap();
    assert!(c.one(&f.memory,&f.transport,f.session).is_none());
    f.publish.call(&mut f.store,88).unwrap();
    for (serial,value) in [(1,11i32),(2,22i32)] {
        let (entry,bytes)=c.one(&f.memory,&f.transport,f.session).unwrap();
        assert_eq!(matched(entry).serial,serial);assert_eq!(bytes,value.to_le_bytes());
    }
    assert!(c.one(&f.memory,&f.transport,f.session).is_none());
}

#[test]
fn batchable_packets_stay_unpublished_until_real_guest_64_count_flush() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    for i in 0..63 {f.emit.call(&mut f.store,(64+i*12,i as i32,0)).unwrap();}
    assert_eq!(atomic(&f.memory,0).load(Ordering::SeqCst),64);
    assert!(c.one(&f.memory,&f.transport,f.session).is_none());
    f.emit.call(&mut f.store,(64+63*12,63,0)).unwrap();
    assert_eq!(atomic(&f.memory,0).load(Ordering::SeqCst),64+64*12);
    for i in 0..64 {let (entry,bytes)=c.one(&f.memory,&f.transport,f.session).unwrap();
        assert_eq!(matched(entry).serial,i+1);assert_eq!(bytes,(i as i32).to_le_bytes());}
    assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),64+64*12);
}

#[test]
fn raw_reserve_wrap_publishes_without_hook_then_new_pointer_has_new_serial() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    f.emit.call(&mut f.store,(64,1,1)).unwrap();
    let first=matched(c.one(&f.memory,&f.transport,f.session).unwrap().0);
    // Wrap appears before any future Emit midpoint, exactly the ordering gap.
    f.wrap.call(&mut f.store,76).unwrap();
    assert!(matches!(c.one(&f.memory,&f.transport,f.session).unwrap().0,Consumption::UninstrumentedReserveWrap{raw_cursor:76,..}));
    assert!(c.one(&f.memory,&f.transport,f.session).is_none());
    f.emit.call(&mut f.store,(64,2,1)).unwrap();
    let second=matched(c.one(&f.memory,&f.transport,f.session).unwrap().0);
    assert_eq!(first.raw_command_address,second.raw_command_address);assert!(second.serial>first.serial);
}

#[test]
fn pre_ack_owned_bytes_survive_512_boundary_and_reused_guest_storage() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    for i in 0..513 {f.emit.call(&mut f.store,(64+i*12,i as i32,0)).unwrap();}
    f.publish.call(&mut f.store,(64+513*12) as i32).unwrap();
    let mut retained=Vec::new();
    for i in 0..513 {
        let (entry,bytes)=c.one(&f.memory,&f.transport,f.session).unwrap();
        assert_eq!(matched(entry).serial,i+1);retained.push(bytes);
        assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),if i<511{64}else if i==511{64+512*12}else{64+513*12});
    }
    assert_eq!(c.acknowledgements,vec![64+512*12,64+513*12]);
    // Reuse only after the model's actual512 and final-batch acknowledgements.
    for i in 0..513 {f.overwrite.call(&mut f.store,(64+i*12+8,-1)).unwrap();}
    for (i,bytes) in retained.iter().enumerate() {assert_eq!(bytes,&(i as i32).to_le_bytes());}
    assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),64+513*12);
}

#[test]
fn tainted_midpoint_observation_never_traps_guest_and_consumer_rejects() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    f.transport.lock().unwrap().invalidate(Rejection::Invalidated);
    f.emit.call(&mut f.store,(64,7,1)).unwrap();
    assert_eq!(atomic(&f.memory,0).load(Ordering::SeqCst),76);
    assert_eq!(c.one(&f.memory,&f.transport,f.session).unwrap().0,Consumption::Rejected(Rejection::Invalidated));
    assert_eq!(c.cursor,76);assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),76);
}

#[test]
fn guest_trap_after_midpoint_leaves_unpublished_metadata_unknown() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    assert!(f.abort.call(&mut f.store,64).is_err());
    assert_eq!(atomic(&f.memory,0).load(Ordering::SeqCst),64);
    assert!(c.one(&f.memory,&f.transport,f.session).is_none());
    assert_eq!(f.transport.lock().unwrap().packets.len(),1);
    // This tiny fixture has no guest mutex/recovery. Do not model a subsequent
    // real Emit after trap; its cleanup/unlock/liveness are uncertified.
    f.transport.lock().unwrap().invalidate(Rejection::Invalidated);
    assert!(f.transport.lock().unwrap().packets.is_empty());
}

#[test]
fn raw_wrap_head_exposes_older_unpublished_batch_without_next_hook() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    for i in 0..3 {f.emit.call(&mut f.store,(64+i*12,i as i32,0)).unwrap();}
    f.publish.call(&mut f.store,100).unwrap();
    for _ in 0..3 {matched(c.one(&f.memory,&f.transport,f.session).unwrap().0);}
    assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),100);
    f.emit.call(&mut f.store,(100,44,0)).unwrap();
    assert!(c.one(&f.memory,&f.transport,f.session).is_none());
    // A modeled wrapping head can expose an older pending packet as well as
    // its unhooked wrap. This does not certify Reserve's free-space predicate.
    f.wrap.call(&mut f.store,112).unwrap();
    let (entry,bytes)=c.one(&f.memory,&f.transport,f.session).unwrap();
    assert_eq!(matched(entry).serial,4);assert_eq!(bytes,44i32.to_le_bytes());
    assert!(matches!(c.one(&f.memory,&f.transport,f.session).unwrap().0,Consumption::UninstrumentedReserveWrap{raw_cursor:112,..}));
    assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),64);
}

#[test]
fn consumer_finishes_old_head_snapshot_before_new_publication() {
    let mut f=Fixture::new(None);let mut c=Consumer::new();
    f.emit.call(&mut f.store,(64,1,0)).unwrap();f.emit.call(&mut f.store,(76,2,1)).unwrap();
    matched(c.one(&f.memory,&f.transport,f.session).unwrap().0);
    f.emit.call(&mut f.store,(88,3,1)).unwrap();
    matched(c.one(&f.memory,&f.transport,f.session).unwrap().0);
    assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),88);
    assert_eq!(atomic(&f.memory,0).load(Ordering::SeqCst),100);
    matched(c.one(&f.memory,&f.transport,f.session).unwrap().0);
    assert_eq!(atomic(&f.memory,4).load(Ordering::SeqCst),100);
}
