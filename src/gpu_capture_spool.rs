//! Private full-frame capture storage. Only natural submissions enter this spool.
//! GPU staging stays charged until map completion AND the writer releases it.
use super::*;
use std::sync::{Mutex, Condvar, mpsc};

pub(super) const MAX_SPOOLED_BYTES:u64=1<<30;
const MAX_SPOOLED_BLOBS:usize=32768;
const MAX_BATCHES:usize=8192;

#[derive(Default)]
struct Status {
    submitted:usize, completed:usize, raw_bytes:u64, padded_bytes:u64,
    names:std::collections::HashSet<String>, blobs:Vec<Value>, epochs:Vec<u64>,
    failure:Option<String>, closed:bool, mapped_bytes:u64, peak_mapped_bytes:u64,
}
struct Shared {state:Mutex<Status>,changed:Condvar}
struct PollCredit(Arc<AtomicU64>);
impl Drop for PollCredit {fn drop(&mut self){self.0.fetch_sub(1,Ordering::SeqCst);}}
struct Batch {readbacks:Vec<Readback>, mapped:mpsc::Receiver<(usize,Result<(),String>)>, _poll:PollCredit}
pub(super) struct Spool {sender:Option<mpsc::Sender<Batch>>, shared:Arc<Shared>, inflight:Arc<AtomicU64>}

fn admit(state:&mut Status, plans:&[(String,u64,u64)], epoch:u64)->Result<(),String> {
    if state.closed || state.failure.is_some() {return Err("capture spool closed or failed".into());}
    if plans.is_empty() {return Ok(());}
    if state.submitted>=MAX_BATCHES || state.names.len().checked_add(plans.len()).is_none_or(|n|n>MAX_SPOOLED_BLOBS) {
        return Err("capture spool batch/blob bound".into());
    }
    if epoch==0 || state.epochs.last().is_some_and(|e|*e>=epoch) {return Err("capture spool natural epochs not increasing".into());}
    let mut raw=state.raw_bytes;let mut padded=state.padded_bytes;let mut seen=std::collections::HashSet::new();
    for (name,bytes,allocation) in plans {
        if name.is_empty() || name.contains('/') || name.contains('\\') || matches!(name.as_str(),"."|".."|"capture.json"|"rejected.json")
            || state.names.contains(name) || !seen.insert(name.clone()) || *bytes==0 || allocation<bytes {
            return Err("capture spool immutable payload identity invalid".into());
        }
        raw=raw.checked_add(*bytes).filter(|n|*n<=MAX_SPOOLED_BYTES).ok_or("capture spool total payload bound")?;
        padded=padded.checked_add(*allocation).ok_or("capture spool staging total overflow")?;
    }
    state.raw_bytes=raw;state.padded_bytes=padded;state.names.extend(seen);
    state.submitted+=1;state.epochs.push(epoch);Ok(())
}

fn write_batch(batch:Batch,directory:&std::path::Path,errors:&AtomicU64,expected_errors:u64,shared:&Shared)->Result<Vec<Value>,String> {
    let mut mapped=std::collections::HashSet::new();
    for _ in &batch.readbacks {
        let (i,result)=batch.mapped.recv_timeout(Duration::from_secs(60)).map_err(|e|format!("stream map completion: {e}"))?;
        result?;
        if i>=batch.readbacks.len() || !mapped.insert(i) {return Err("capture stream map callback identity".into());}
    }
    if errors.load(Ordering::SeqCst)!=expected_errors {return Err("GPU error during streamed native capture".into());}
    let mut blobs=Vec::with_capacity(batch.readbacks.len());
    for rb in batch.readbacks {
        let view=rb.buffer.slice(..).get_mapped_range().map_err(|e|format!("stream mapped range: {e:?}"))?;
        let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(directory.join(&rb.plan.name)).map_err(|e|e.to_string())?;
        let mut hash=ShaRows::new();
        for row in 0..rb.plan.rows as usize {
            let bytes=&view[row*rb.plan.pitch as usize..row*rb.plan.pitch as usize+rb.plan.row as usize];
            file.write_all(bytes).map_err(|e|e.to_string())?;hash.update(bytes);
        }
        // Atomic final manifest plus independent rehash admission is the same
        // crash boundary used by ordinary whole-target captures.
        drop(file);drop(view);rb.buffer.unmap();
        {let mut state=shared.state.lock().map_err(|_|"spool mutex poisoned")?;
            state.mapped_bytes=state.mapped_bytes.checked_sub(rb.plan.bytes).ok_or("spool mapped byte accounting")?;}
        blobs.push(json!({"path":rb.plan.name,"byte_length":rb.plan.row as u64*rb.plan.rows as u64,
            "sha256":hash.finish(),"packed_row_bytes":rb.plan.row,"row_count":rb.plan.rows,"readback_pitch":rb.plan.pitch}));
        // Dropping rb releases its CPU reservation. The natural-submission
        // reservation independently survives until original GPU completion.
    }
    Ok(blobs)
}

impl Spool {
    pub(super) fn new(directory:PathBuf,errors:Arc<AtomicU64>,expected_errors:u64,inflight:Arc<AtomicU64>)->Result<Self,String> {
        if expected_errors!=0 {return Err("stream capture starts with GPU errors".into());}
        let (sender,receiver)=mpsc::channel::<Batch>();
        let shared=Arc::new(Shared{state:Mutex::new(Status::default()),changed:Condvar::new()});
        let worker=shared.clone();
        std::thread::Builder::new().name("private-capture-spool".into()).spawn(move|| {
            for batch in receiver {
                let failed=worker.state.lock().map_or(true,|s|s.failure.is_some());
                let result=if failed {Err("prior capture spool batch failed".into())}else{write_batch(batch,&directory,&errors,expected_errors,&worker)};
                if let Ok(mut state)=worker.state.lock() {
                    match result {Ok(mut blobs)=>state.blobs.append(&mut blobs),Err(e)=>{state.failure.get_or_insert(e);}}
                    state.completed+=1;worker.changed.notify_all();
                }
            }
            if let Ok(mut state)=worker.state.lock(){state.closed=true;worker.changed.notify_all();}
        }).map_err(|e|e.to_string())?;
        Ok(Self{sender:Some(sender),shared,inflight})
    }
    /// Caller has just made this encoder's original natural submission. This
    /// method never creates/submits an encoder and never waits on the GPU thread.
    pub(super) fn submitted(&self,readbacks:Vec<Readback>,epoch:u64)->Result<(),String> {
        if readbacks.is_empty(){return Ok(());}
        let plans=readbacks.iter().map(|r|(r.plan.name.clone(),r.plan.row as u64*r.plan.rows as u64,r.plan.bytes)).collect::<Vec<_>>();
        {let mut state=self.shared.state.lock().map_err(|_|"spool mutex poisoned")?;admit(&mut state,&plans,epoch)?;}
        self.inflight.fetch_add(1,Ordering::SeqCst);
        let poll=PollCredit(self.inflight.clone());
        let (tx,rx)=mpsc::channel();
        for (i,readback) in readbacks.iter().enumerate(){let tx=tx.clone();let shared=self.shared.clone();let bytes=readback.plan.bytes;
            readback.buffer.slice(..).map_async(MapMode::Read,move|r|{
                if r.is_ok(){if let Ok(mut state)=shared.state.lock(){state.mapped_bytes+=bytes;state.peak_mapped_bytes=state.peak_mapped_bytes.max(state.mapped_bytes);}}
                let _=tx.send((i,r.map_err(|e|format!("{e:?}"))));});
        }
        drop(tx);
        if self.sender.as_ref().ok_or("spool sender closed")?.send(Batch{readbacks,mapped:rx,_poll:poll}).is_err(){
            if let Ok(mut state)=self.shared.state.lock(){state.failure=Some("capture spool worker unavailable".into());self.shared.changed.notify_all();}
            return Err("capture spool worker unavailable".into());
        }Ok(())
    }
    pub(super) fn failure(&self)->Option<String>{self.shared.state.lock().map_or(Some("spool mutex poisoned".into()),|s|s.failure.clone())}
    /// Final background writer only. Closing the channel prevents late batches.
    pub(super) fn finish(mut self)->Result<(Vec<Value>,Value),String>{
        self.sender.take();
        let state=self.shared.state.lock().map_err(|_|"spool mutex poisoned")?;
        let (mut state,timeout)=self.shared.changed.wait_timeout_while(state,Duration::from_secs(120),|s|!s.closed&&s.failure.is_none()).map_err(|_|"spool mutex poisoned")?;
        if let Some(e)=&state.failure{return Err(e.clone());}
        if timeout.timed_out()||!state.closed||state.completed!=state.submitted||state.blobs.len()!=state.names.len()||state.mapped_bytes!=0||state.peak_mapped_bytes>FINISHED_STAGING_BYTES{return Err("capture spool did not finish every bounded natural batch".into());}
        let receipt=json!({"natural_submission_epochs":state.epochs,"submitted_batches":state.submitted,
            "completed_batches":state.completed,"raw_payload_bytes":state.raw_bytes,"cumulative_padded_staging_bytes":state.padded_bytes,
            "payload_bound":MAX_SPOOLED_BYTES,"live_staging_bound":FINISHED_STAGING_BYTES,"peak_completed_map_bytes":state.peak_mapped_bytes,"capture_forced_submissions":0});
        Ok((std::mem::take(&mut state.blobs),receipt))
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test]fn spool_budget_is_atomic_and_names_are_immutable(){
        let mut s=Status::default();admit(&mut s,&[("one.bin".into(),64,256)],1).unwrap();
        for p in [vec![("one.bin".into(),64,256)],vec![("../bad".into(),64,256)],vec![("two.bin".into(),MAX_SPOOLED_BYTES,MAX_SPOOLED_BYTES)],vec![("two.bin".into(),64,32)]]{
            assert!(admit(&mut s,&p,2).is_err());assert_eq!(s.raw_bytes,64);assert_eq!(s.submitted,1);assert_eq!(s.names.len(),1);
        }
        assert!(admit(&mut s,&[("two.bin".into(),64,256)],1).is_err());
        admit(&mut s,&[("two.bin".into(),64,256)],2).unwrap();assert_eq!(s.raw_bytes,128);assert_eq!(s.padded_bytes,512);
    }
    #[test]fn spool_distinguishes_cumulative_disk_from_live_gpu_bound(){
        let mut s=Status::default();
        for i in 1..=4 {admit(&mut s,&[(format!("{i}.bin"),MAX_BYTES,MAX_BYTES)],i).unwrap();}
        assert_eq!(s.raw_bytes,MAX_SPOOLED_BYTES);
        assert!(admit(&mut s,&[("fifth.bin".into(),1,256)],5).is_err());
        assert_eq!(s.submitted,4);assert_eq!(s.padded_bytes,MAX_SPOOLED_BYTES);
        s.closed=true;assert!(admit(&mut s,&[("later.bin".into(),1,256)],6).is_err());
    }
}

#[cfg(test)]
mod metal_tests {
    use super::*;
    #[test]
    #[ignore = "explicit local Metal and immutable output directory required"]
    fn natural_batches_stream_beyond_cumulative_live_limit() {
        let root=PathBuf::from(std::env::var_os("MUGUET_SPOOL_ORACLE_OUTPUT").expect("oracle output"));
        assert!(root.is_absolute());std::fs::create_dir(&root).unwrap();
        let mut descriptor=InstanceDescriptor::new_without_display_handle();descriptor.backends=Backends::METAL;
        let instance=Instance::new(descriptor);
        let adapter=pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).unwrap();
        assert_eq!(adapter.get_info().backend,Backend::Metal);
        let (device,queue)=pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).unwrap();
        let errors=Arc::new(AtomicU64::new(0));let captured=errors.clone();
        device.on_uncaptured_error(Arc::new(move |_|{captured.fetch_add(1,Ordering::SeqCst);}));
        let inflight=Arc::new(AtomicU64::new(0));let live=Arc::new(AtomicU64::new(0));
        let spool=Spool::new(root.clone(),errors.clone(),0,inflight.clone()).unwrap();
        const SIZE:u64=2<<20;const BATCHES:u64=140;
        let mut expected=Vec::new();let mut peak=0;
        for index in 1..=BATCHES {
            let source=device.create_buffer(&BufferDescriptor{label:Some("spool oracle original input"),size:SIZE,usage:BufferUsages::COPY_SRC,mapped_at_creation:true});
            let value=(index%251) as u8;
            {source.slice(..).get_mapped_range_mut().unwrap().copy_from_slice(&vec![value;SIZE as usize]);}source.unmap();
            let buffer=device.create_buffer(&BufferDescriptor{label:Some("spool oracle readback"),size:SIZE,usage:BufferUsages::COPY_DST|BufferUsages::MAP_READ,mapped_at_creation:false});
            live.fetch_add(SIZE,Ordering::SeqCst);peak=peak.max(live.load(Ordering::SeqCst));assert!(peak<=MAX_BYTES);
            let reservation=Arc::new(SnapshotReservation{bytes:SIZE,live:live.clone()});
            let mut encoder=device.create_command_encoder(&Default::default());encoder.copy_buffer_to_buffer(&source,0,&buffer,0,SIZE);
            let submission=queue.submit([encoder.finish()]);
            let gpu_owner=reservation.clone();queue.on_submitted_work_done(move ||drop(gpu_owner));
            let name=format!("batch-{index}.bin");
            let readback=Readback{plan:Plan{name:name.clone(),source:Source::Buffer{buffer:source,offset:0,size:SIZE},row:SIZE as u32,rows:1,image_rows:1,pitch:SIZE as u32,bytes:SIZE},buffer,_reservation:Some(reservation)};
            spool.submitted(vec![readback],index).unwrap();
            expected.push((name,value));
            device.poll(PollType::Wait{submission_index:Some(submission),timeout:Some(Duration::from_secs(10))}).unwrap();
            if index%4==0 {
                let deadline=Instant::now()+Duration::from_secs(10);
                while inflight.load(Ordering::SeqCst)>0 {
                    assert!(Instant::now()<deadline,"spool writer timeout");
                    device.poll(PollType::Poll).unwrap();std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        let (blobs,receipt)=spool.finish().unwrap();
        assert_eq!(errors.load(Ordering::SeqCst),0);assert_eq!(inflight.load(Ordering::SeqCst),0);assert_eq!(live.load(Ordering::SeqCst),0);
        assert_eq!(blobs.len(),BATCHES as usize);assert_eq!(receipt["raw_payload_bytes"],SIZE*BATCHES);assert!(SIZE*BATCHES>MAX_BYTES);
        for (name,value) in expected {
            let bytes=std::fs::read(root.join(&name)).unwrap();assert_eq!(bytes.len(),SIZE as usize);assert!(bytes.iter().all(|b|*b==value));
            let blob=blobs.iter().find(|b|b["path"]==name).unwrap();assert_eq!(blob["sha256"],sha(&bytes));
        }
        let result=json!({"kind":"authored_natural_submission_spool_oracle","backend":"Metal","batches":BATCHES,"bytes":SIZE*BATCHES,"peak_live_staging_bytes":peak,"gpu_errors":0,"all_bytes_exact":true,"final_live_bytes":0,"final_poll_credits":0,"spool":receipt});
        publish_new(&root.join("receipt.json"),&serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
}
