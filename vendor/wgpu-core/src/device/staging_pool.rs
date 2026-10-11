//! Opt-in Metal upload allocation reuse within the original PendingWrites stream.
//!
//! Only fence-completed, successfully tracked submissions can publish sources.
//! Publication happens in UserClosures::fire after all core guards are gone;
//! mapping is synchronous on later checkout. Neither path adds submit/poll/wait.
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(supports_64bit_atomics)]
use core::sync::atomic::AtomicU64;
#[cfg(not(supports_64bit_atomics))]
use portable_atomic::AtomicU64;

use crate::{
    device::{Device, DeviceError},
    lock::{rank, Mutex},
    resource::{FlushedStagingBuffer, RawStagingAllocation, StagingBuffer},
    SubmissionIndex,
};

const MAX_SIZE: u64 = 65536;
const BYTE_CAP: u64 = 16 * 1024 * 1024;
const OBJECT_CAP: u64 = 1024;

#[derive(Debug, Default)]
pub(crate) struct Counters {
    #[cfg(test)]
    pub test_fault: AtomicU64,
    #[cfg(test)]
    next_id: AtomicU64,
    pub write_calls: AtomicU64,
    pub write_bytes: AtomicU64,
    pub eligible_calls: AtomicU64,
    pub allocation_calls: AtomicU64,
    pub allocated_bytes: AtomicU64,
    pub reuse_hits: AtomicU64,
    pub cap_misses: AtomicU64,
    pub size_misses: AtomicU64,
    pub retired_objects: AtomicU64,
    pub published_objects: AtomicU64,
    pub rejected_retirements: AtomicU64,
    pub quarantined_write_sources: AtomicU64,
    pub allocation_errors: AtomicU64,
    pub remap_errors: AtomicU64,
    pub destroyed_allocations: AtomicU64,
    pub reservation_releases_without_allocation: AtomicU64,
    pub allocation_cpu_wall_ns: AtomicU64,
    pub remap_cpu_wall_ns: AtomicU64,
    pub publication_cpu_wall_ns: AtomicU64,
    pub retained_bytes: AtomicU64,
    pub retained_objects: AtomicU64,
    pub free_bytes: AtomicU64,
    pub free_objects: AtomicU64,
    pub one_shot_bytes: AtomicU64,
    pub one_shot_objects: AtomicU64,
    pub retained_peak_bytes: AtomicU64,
    pub retained_peak_objects: AtomicU64,
}

/// Accounting only: no Queue/Device/pool ownership, no HAL or lock in Drop.
/// The raw owner releases this token only *after* destroying its allocation.
#[derive(Debug)]
pub(crate) struct StagingAccounting {
    counters: Arc<Counters>,
    size: u64,
    retained: bool,
    created: bool,
    #[cfg(test)]
    id: u64,
}

impl StagingAccounting {
    fn new(counters: &Arc<Counters>, size: u64, retained: bool) -> Self {
        let (bytes, objects) = if retained {
            (&counters.retained_bytes, &counters.retained_objects)
        } else {
            (&counters.one_shot_bytes, &counters.one_shot_objects)
        };
        let total_bytes = bytes.fetch_add(size, Ordering::Relaxed) + size;
        let total_objects = objects.fetch_add(1, Ordering::Relaxed) + 1;
        if retained {
            counters.retained_peak_bytes.fetch_max(total_bytes, Ordering::Relaxed);
            counters.retained_peak_objects.fetch_max(total_objects, Ordering::Relaxed);
        }
        Self { counters: counters.clone(), size, retained, created: false,
            #[cfg(test)]
            id: counters.next_id.fetch_add(1, Ordering::Relaxed) + 1 }
    }

    pub(crate) fn created(&mut self) {
        assert!(!self.created);
        self.created = true;
        self.counters.allocation_calls.fetch_add(1, Ordering::Relaxed);
        self.counters.allocated_bytes.fetch_add(self.size, Ordering::Relaxed);
    }

    pub(crate) fn is_retained(&self) -> bool { self.retained }
    pub(crate) fn belongs_to(&self, counters: &Arc<Counters>) -> bool {
        self.retained && Arc::ptr_eq(&self.counters, counters)
    }
    pub(crate) fn record_retired(&self) {
        self.counters.retired_objects.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(test)]
    pub(crate) fn take_test_fault(&self, fault: u64) -> bool {
        self.counters.test_fault.compare_exchange(fault, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    }
    #[cfg(test)]
    pub(crate) fn id(&self) -> u64 { self.id }
}

impl Drop for StagingAccounting {
    fn drop(&mut self) {
        let (bytes, objects) = if self.retained {
            (&self.counters.retained_bytes, &self.counters.retained_objects)
        } else {
            (&self.counters.one_shot_bytes, &self.counters.one_shot_objects)
        };
        bytes.fetch_sub(self.size, Ordering::Relaxed);
        objects.fetch_sub(1, Ordering::Relaxed);
        let count = if self.created { &self.counters.destroyed_allocations }
            else { &self.counters.reservation_releases_without_allocation };
        count.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
struct PoolState { free: BTreeMap<u64, Vec<RawStagingAllocation>> }

#[derive(Debug)]
pub(crate) struct StagingPool {
    enabled: bool,
    is_metal: bool,
    closing: Arc<AtomicBool>,
    // Option permits exclusive teardown via into_inner, even under registry guards.
    state: Option<Mutex<PoolState>>,
    pub(super) counters: Arc<Counters>,
    #[cfg(feature = "std")]
    epoch: std::time::Instant,
    #[cfg(feature = "std")]
    last_report_ns: AtomicU64,
}

impl StagingPool {
    pub(super) fn new(device: &Device) -> Arc<Self> {
        let is_metal = device.backend() == wgt::Backend::Metal;
        #[cfg(feature = "std")]
        let enabled = is_metal && std::env::var_os("MUGUET_METAL_INTERNAL_UPLOAD_POOL")
            .as_deref() == Some(std::ffi::OsStr::new("1"));
        #[cfg(not(feature = "std"))]
        let enabled = false;
        Arc::new(Self {
            enabled, is_metal, closing: device.internal_upload_pool_closed.clone(),
            state: Some(Mutex::new(rank::INTERNAL_UPLOAD_POOL, PoolState::default())),
            counters: Arc::new(Counters::default()),
            #[cfg(feature = "std")]
            epoch: std::time::Instant::now(),
            #[cfg(feature = "std")]
            last_report_ns: AtomicU64::new(0),
        })
    }

    pub(super) fn close(&self) { self.closing.store(true, Ordering::Release); }
    #[cfg(test)]
    pub(super) fn take_submission_test_fault(&self) -> bool {
        self.counters.test_fault.compare_exchange(3, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    }

    fn eligible(size: u64) -> bool { size != 0 && size <= MAX_SIZE && size % 4 == 0 }

    pub(super) fn stage(&self, device: &Arc<Device>, size: wgt::BufferSize)
        -> Result<StagingBuffer, DeviceError>
    {
        if !self.is_metal { return StagingBuffer::new(device, size); }
        let bytes = size.get();
        self.counters.write_calls.fetch_add(1, Ordering::Relaxed);
        self.counters.write_bytes.fetch_add(bytes, Ordering::Relaxed);
        let eligible = Self::eligible(bytes);
        if eligible { self.counters.eligible_calls.fetch_add(1, Ordering::Relaxed); }
        let mut reused = None;
        let mut token = None;
        if self.enabled && eligible {
            let mut state = self.state.as_ref().unwrap().lock();
            if !self.closing.load(Ordering::Acquire) {
                reused = state.free.get_mut(&bytes).and_then(Vec::pop);
                if reused.is_some() {
                    if state.free.get(&bytes).is_some_and(Vec::is_empty) { state.free.remove(&bytes); }
                    self.counters.free_bytes.fetch_sub(bytes, Ordering::Relaxed);
                    self.counters.free_objects.fetch_sub(1, Ordering::Relaxed);
                } else if self.counters.retained_bytes.load(Ordering::Relaxed) <= BYTE_CAP - bytes
                    && self.counters.retained_objects.load(Ordering::Relaxed) < OBJECT_CAP {
                    // All reservations serialize under this lock. Cleanup only
                    // decrements atomics, so concurrent releases cannot oversubscribe.
                    token = Some(StagingAccounting::new(&self.counters, bytes, true));
                } else { self.counters.cap_misses.fetch_add(1, Ordering::Relaxed); }
            }
        } else if self.enabled {
            self.counters.size_misses.fetch_add(1, Ordering::Relaxed);
        }
        // No pool/core lock spans create, map, error handling, or destruction.
        if let Some(allocation) = reused {
            #[cfg(feature = "std")]
            let started = std::time::Instant::now();
            let result = allocation.map();
            #[cfg(feature = "std")]
            self.counters.remap_cpu_wall_ns.fetch_add(ns(started), Ordering::Relaxed);
            if result.is_ok() { self.counters.reuse_hits.fetch_add(1, Ordering::Relaxed); }
            else { self.counters.remap_errors.fetch_add(1, Ordering::Relaxed); }
            result
        } else {
            let token = token.unwrap_or_else(|| StagingAccounting::new(&self.counters, bytes, false));
            #[cfg(feature = "std")]
            let started = std::time::Instant::now();
            let result = StagingBuffer::with_accounting(device, size, token);
            #[cfg(feature = "std")]
            self.counters.allocation_cpu_wall_ns.fetch_add(ns(started), Ordering::Relaxed);
            if result.is_err() { self.counters.allocation_errors.fetch_add(1, Ordering::Relaxed); }
            result
        }
    }

    pub(super) fn publication(self: &Arc<Self>, batch: Vec<RetiredStaging>)
        -> Option<RetirementPublication>
    {
        if batch.is_empty() && !(self.enabled && self.closing.load(Ordering::Acquire)) { return None; }
        Some(RetirementPublication { pool: self.clone(), batch })
    }

    fn publish(&self, batch: Vec<RetiredStaging>) {
        #[cfg(feature = "std")]
        let started = std::time::Instant::now();
        let mut rejected = Vec::new();
        {
            let mut state = self.state.as_ref().unwrap().lock();
            if self.closing.load(Ordering::Acquire) {
                let free = core::mem::take(&mut state.free);
                self.counters.free_bytes.store(0, Ordering::Relaxed);
                self.counters.free_objects.store(0, Ordering::Relaxed);
                for (_, allocations) in free { rejected.extend(allocations); }
            }
            for retired in batch {
                let allocation = retired.buffer.into_allocation();
                if !self.closing.load(Ordering::Acquire) && allocation.belongs_to(&self.counters) {
                    let bytes = allocation.size().get();
                    state.free.entry(bytes).or_default().push(allocation);
                    self.counters.free_bytes.fetch_add(bytes, Ordering::Relaxed);
                    self.counters.free_objects.fetch_add(1, Ordering::Relaxed);
                    self.counters.published_objects.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.counters.rejected_retirements.fetch_add(1, Ordering::Relaxed);
                    rejected.push(allocation);
                }
            }
        }
        // Raw destruction and Device drops never occur under the pool mutex.
        drop(rejected);
        #[cfg(feature = "std")]
        self.counters.publication_cpu_wall_ns.fetch_add(ns(started), Ordering::Relaxed);
    }

    pub(super) fn maybe_report(&self) {
        #[cfg(feature = "std")]
        if self.is_metal {
            let now = ns(self.epoch);
            let last = self.last_report_ns.load(Ordering::Relaxed);
            if last != 0 && now.saturating_sub(last) < 10_000_000_000 { return; }
            if self.last_report_ns.compare_exchange(last, now.max(1), Ordering::Relaxed, Ordering::Relaxed).is_err() { return; }
            let c = &self.counters;
            let fields = [
                ("write_calls", &c.write_calls), ("write_bytes", &c.write_bytes),
                ("eligible_calls", &c.eligible_calls), ("allocation_calls", &c.allocation_calls),
                ("allocated_bytes", &c.allocated_bytes), ("reuse_hits", &c.reuse_hits),
                ("cap_misses", &c.cap_misses), ("size_misses", &c.size_misses),
                ("retired_objects", &c.retired_objects), ("published_objects", &c.published_objects),
                ("rejected_retirements", &c.rejected_retirements), ("allocation_errors", &c.allocation_errors),
                ("quarantined_write_sources", &c.quarantined_write_sources),
                ("remap_errors", &c.remap_errors), ("destroyed_allocations", &c.destroyed_allocations),
                ("reservation_releases_without_allocation", &c.reservation_releases_without_allocation),
                ("allocation_cpu_wall_ns", &c.allocation_cpu_wall_ns), ("remap_cpu_wall_ns", &c.remap_cpu_wall_ns),
                ("publication_cpu_wall_ns", &c.publication_cpu_wall_ns),
                ("retained_bytes", &c.retained_bytes), ("retained_objects", &c.retained_objects),
                ("free_bytes", &c.free_bytes), ("free_objects", &c.free_objects),
                ("one_shot_bytes", &c.one_shot_bytes), ("one_shot_objects", &c.one_shot_objects),
                ("retained_peak_bytes", &c.retained_peak_bytes), ("retained_peak_objects", &c.retained_peak_objects),
            ];
            use core::fmt::Write;
            let mut json = alloc::format!("{{\"schema\":1,\"enabled\":{},\"closed\":{},\"elapsed_s\":{:.6},\"semantics\":\"cumulative counts and CPU wall, current memory snapshots; not GPU time\",\"eligibility_max_bytes\":65536,\"alignment\":4,\"retained_byte_cap\":16777216,\"retained_object_cap\":1024",
                self.enabled, self.closing.load(Ordering::Acquire), now as f64 / 1e9);
            for (name, value) in fields { let _ = write!(json, ",\"{name}\":{}", value.load(Ordering::Relaxed)); }
            json.push('}');
            std::eprintln!("[wgpu] metal internal upload pool: {json}");
        }
    }
}

impl Drop for StagingPool {
    fn drop(&mut self) {
        self.close();
        let state = self.state.take().unwrap().into_inner();
        self.counters.free_bytes.store(0, Ordering::Relaxed);
        self.counters.free_objects.store(0, Ordering::Relaxed);
        drop(state); // No lock and no Queue/pool reference in these owners.
    }
}

#[cfg(feature = "std")]
fn ns(started: std::time::Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// Construct only after a successfully tracked submission's fence has completed
/// AND its encoder has reset/dropped. Never construct on generic cleanup/error.
#[derive(Debug)]
pub(crate) struct RetiredStaging { buffer: FlushedStagingBuffer, _submission: SubmissionIndex }

impl RetiredStaging {
    pub(super) fn after_completed_encoder_reset(buffer: FlushedStagingBuffer, submission: SubmissionIndex) -> Self {
        assert!(submission != 0 && buffer.is_retained());
        buffer.record_retired();
        Self { buffer, _submission: submission }
    }
}

#[derive(Debug)]
pub(crate) struct RetirementPublication { pool: Arc<StagingPool>, batch: Vec<RetiredStaging> }

impl RetirementPublication {
    pub(super) fn dispatch(self) { self.pool.publish(self.batch); }
}

#[cfg(all(test, feature = "metal", feature = "std", target_os = "macos"))]
mod tests {
    use super::*;
    use crate::{global::Global, id, resource::{Buffer, RawResourceAccess}};
    use std::{boxed::Box, panic::{catch_unwind, AssertUnwindSafe}, sync::Barrier, thread, time::Duration};
    use objc2_metal::{MTLCommandQueue, MTLDevice, MTLSharedEvent};

    struct Context {
        global: Arc<Global>, device: Arc<Device>, queue: Arc<super::super::queue::Queue>,
        device_id: id::DeviceId, queue_id: id::QueueId,
    }

    fn context() -> Context {
        let mut desc = wgt::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgt::Backends::METAL;
        let global = Arc::new(Global::new("internal upload behavior", desc, None));
        let adapter = global.enumerate_adapters(wgt::Backends::METAL, false).into_iter().next().unwrap();
        let (device_id, queue_id) = global.adapter_request_device(adapter, &Default::default(), None, None).unwrap();
        let device = global.hub.devices.get(device_id);
        let queue = global.hub.queues.get(queue_id);
        let expected = std::env::var_os("MUGUET_METAL_INTERNAL_UPLOAD_POOL").as_deref()
            == Some(std::ffi::OsStr::new("1"));
        assert_eq!(queue.upload_pool.enabled, expected);
        Context { global, device, queue, device_id, queue_id }
    }

    fn target(c: &Context, size: u64) -> Arc<Buffer> {
        c.device.create_buffer(&wgt::BufferDescriptor { label: None, size,
            usage: wgt::BufferUsages::MAP_READ | wgt::BufferUsages::COPY_DST,
            mapped_at_creation: false }).unwrap()
    }

    fn wait(c: &Context, index: SubmissionIndex) -> super::super::UserClosures {
        let (closures, result) = c.device.poll_and_return_closures(wgt::PollType::Wait {
            submission_index: Some(index), timeout: Some(Duration::from_secs(10)) });
        result.unwrap();
        closures
    }

    fn bytes(c: &Context, buffer: &Buffer) -> Vec<u8> {
        let guard = c.device.snatchable_lock.read();
        let raw = buffer.try_raw(&guard).unwrap();
        // Fully written target, successful fence wait, exclusive test read.
        let mapping = unsafe { c.device.raw().map_buffer(raw, 0..buffer.size) }.unwrap();
        let data = unsafe { core::slice::from_raw_parts(mapping.ptr.as_ptr(), buffer.size as usize) }.to_vec();
        unsafe { c.device.raw().unmap_buffer(raw) };
        data
    }

    fn value(counter: &AtomicU64) -> u64 { counter.load(Ordering::Relaxed) }

    // A deliberately blocked GPU must also be released during failed-test
    // unwind, before Context/Queue teardown waits for idle.
    struct ReleaseOnDrop<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for ReleaseOnDrop<F> {
        fn drop(&mut self) { if let Some(release) = self.0.take() { release(); } }
    }

    #[test]
    fn exact_size_reuse_requires_completed_reset_and_unlocked_publication() {
        let c = context(); let pool = &c.queue.upload_pool;
        let first = target(&c, 64); let second = target(&c, 64);
        c.queue.write_buffer(first.clone(), 0, &[7; 64]).unwrap();
        c.queue.write_buffer(second.clone(), 0, &[11; 64]).unwrap();
        assert_eq!(value(&pool.counters.allocation_calls), 2);
        assert_eq!(value(&pool.counters.free_objects), 0);
        let index = c.queue.test_submit_pending_without_maintenance().unwrap();
        let closures = wait(&c, index);
        assert_eq!(bytes(&c, &first), [7; 64]); assert_eq!(bytes(&c, &second), [11; 64]);
        assert_eq!(value(&pool.counters.free_objects), 0);
        if pool.enabled {
            assert_eq!(value(&pool.counters.retired_objects), 2);
            assert_eq!(value(&pool.counters.retained_objects), 2);
            assert_eq!(closures.upload_retirements.len(), 1);
        }
        // A retired-unpublished source must still not be checked out.
        let held = pool.stage(&c.device, wgt::BufferSize::new(64).unwrap()).unwrap();
        assert_eq!(value(&pool.counters.reuse_hits), 0);
        drop(held);
        closures.fire();
        let original_allocations = value(&pool.counters.allocation_calls);
        let reused = pool.stage(&c.device, wgt::BufferSize::new(64).unwrap()).unwrap();
        if pool.enabled {
            assert_eq!(value(&pool.counters.reuse_hits), 1);
            assert_eq!(value(&pool.counters.allocation_calls), original_allocations);
            assert!(reused.allocation_id() <= 2);
        } else { assert_eq!(value(&pool.counters.reuse_hits), 0); }
        let different = pool.stage(&c.device, wgt::BufferSize::new(68).unwrap()).unwrap();
        assert_eq!(different.size.get(), 68);
        assert_eq!(value(&pool.counters.reuse_hits), u64::from(pool.enabled));
        drop(different); drop(reused);
        std::eprintln!("internal pool proof enabled={} positive_reuse={} retired={} published={} no_reuse_before_publication=true",
            pool.enabled, value(&pool.counters.reuse_hits), value(&pool.counters.retired_objects), value(&pool.counters.published_objects));
    }

    #[test]
    fn gpu_blocked_submission_does_not_reuse_its_upload_source() {
        let c = context();
        // Submit initial zero-buffer setup before enabling the relay, so the
        // subsequent upload CB is created with the strict wait installed.
        let initial = c.queue.submit(&[]).unwrap(); wait(&c, initial).fire();
        let metal = c.queue.raw().as_any().downcast_ref::<hal::metal::Queue>().unwrap();
        metal.enable_strict_event_sync().unwrap();
        let event = metal.as_raw().device().newSharedEvent().unwrap();
        let _release = ReleaseOnDrop(Some(|| event.setSignaledValue(1)));
        metal.add_wait_event(event.clone(), 1);
        let first = target(&c, 64); let second = target(&c, 64);
        c.queue.write_buffer(first.clone(), 0, &[23; 64]).unwrap();
        let index = c.queue.test_submit_pending_without_maintenance().unwrap();
        c.device.poll(wgt::PollType::Poll).unwrap();
        assert_eq!(value(&c.queue.upload_pool.counters.retired_objects), 0);
        assert_eq!(value(&c.queue.upload_pool.counters.free_objects), 0);
        c.queue.write_buffer(second.clone(), 0, &[37; 64]).unwrap();
        assert_eq!(value(&c.queue.upload_pool.counters.reuse_hits), 0);
        event.setSignaledValue(1);
        wait(&c, index).fire();
        assert_eq!(bytes(&c, &first), [23; 64]);
        let index = c.queue.submit(&[]).unwrap(); wait(&c, index).fire();
        assert_eq!(bytes(&c, &second), [37; 64]);
    }

    #[test]
    fn byte_and_object_caps_include_mapped_pending_and_unpublished_owners() {
        let c = context(); let pool = &c.queue.upload_pool;
        let mut held = Vec::new();
        for _ in 0..256 { held.push(pool.stage(&c.device, wgt::BufferSize::new(MAX_SIZE).unwrap()).unwrap()); }
        let excess = pool.stage(&c.device, wgt::BufferSize::new(MAX_SIZE).unwrap()).unwrap();
        if pool.enabled {
            assert_eq!(value(&pool.counters.retained_bytes), BYTE_CAP);
            assert_eq!(value(&pool.counters.retained_objects), 256);
            assert_eq!(value(&pool.counters.one_shot_objects), 1);
            assert_eq!(value(&pool.counters.cap_misses), 1);
        }
        drop(excess); drop(held);
        assert_eq!(value(&pool.counters.retained_bytes), 0);
        let mut held = Vec::new();
        for _ in 0..OBJECT_CAP { held.push(pool.stage(&c.device, wgt::BufferSize::new(4).unwrap()).unwrap()); }
        let excess = pool.stage(&c.device, wgt::BufferSize::new(4).unwrap()).unwrap();
        if pool.enabled {
            assert_eq!(value(&pool.counters.retained_objects), OBJECT_CAP);
            assert_eq!(value(&pool.counters.retained_bytes), OBJECT_CAP * 4);
            assert_eq!(value(&pool.counters.one_shot_objects), 1);
        }
        let oversize = pool.stage(&c.device, wgt::BufferSize::new(MAX_SIZE + 4).unwrap()).unwrap();
        let unaligned = pool.stage(&c.device, wgt::BufferSize::new(5).unwrap()).unwrap();
        assert!(!oversize.is_retained());
        assert!(!unaligned.is_retained());
        drop(oversize); drop(unaligned); drop(excess); drop(held);
        assert_eq!(value(&pool.counters.retained_objects), 0);
        assert_eq!(value(&pool.counters.one_shot_objects), 0);
        assert!(value(&pool.counters.retained_peak_bytes) <= BYTE_CAP);
        assert!(value(&pool.counters.retained_peak_objects) <= OBJECT_CAP);
    }

    #[test]
    fn failed_create_remap_and_unsubmitted_cleanup_never_recycle() {
        let size = wgt::BufferSize::new(64).unwrap();
        {
            let c = context(); let pool = &c.queue.upload_pool;
            pool.counters.test_fault.store(1, Ordering::Relaxed);
            assert!(pool.stage(&c.device, size).is_err());
            assert_eq!(value(&pool.counters.allocation_calls), 0);
            assert_eq!(value(&pool.counters.reservation_releases_without_allocation), 1);
            assert!(pool.closing.load(Ordering::Acquire));
            assert!(!c.device.is_valid());
        }
        {
            let c = context(); let pool = &c.queue.upload_pool;
            pool.counters.test_fault.store(2, Ordering::Relaxed);
            assert!(pool.stage(&c.device, size).is_err());
            assert_eq!(value(&pool.counters.destroyed_allocations), 1);
            assert_eq!(value(&pool.counters.retained_objects), 0);
            assert!(pool.closing.load(Ordering::Acquire));
        }
        {
            let c = context(); let pool = &c.queue.upload_pool;
            let staged = pool.stage(&c.device, size).unwrap();
            assert!(catch_unwind(AssertUnwindSafe(move || { let mut staged = staged; staged.write(&[1; 4]); })).is_err());
            let flushed = pool.stage(&c.device, size).unwrap().flush(); drop(flushed);
            assert_eq!(value(&pool.counters.retained_objects), 0);
            assert_eq!(value(&pool.counters.free_objects), 0);
        }
        {
            let c = context(); let pool = &c.queue.upload_pool;
            let buffer = target(&c, 64); c.queue.write_buffer(buffer, 0, &[1; 64]).unwrap();
            let index = c.queue.test_submit_pending_without_maintenance().unwrap(); wait(&c, index).fire();
            if pool.enabled {
                pool.counters.test_fault.store(2, Ordering::Relaxed);
                assert!(pool.stage(&c.device, size).is_err());
                assert_eq!(value(&pool.counters.remap_errors), 1);
                assert_eq!(value(&pool.counters.free_objects), 0);
                assert_eq!(value(&pool.counters.retained_objects), 0);
                assert!(pool.closing.load(Ordering::Acquire));
            }
        }
    }

    #[test]
    fn failed_submission_attempt_never_supplies_retirement_proof() {
        let c = context(); let counts = c.queue.upload_pool.counters.clone();
        let buffer = target(&c, 64); c.queue.write_buffer(buffer, 0, &[83; 64]).unwrap();
        c.device.handle_hal_error(hal::DeviceError::Lost);
        assert!(c.queue.test_submit_pending_without_maintenance().is_err());
        assert_eq!(c.device.last_successful_submission_index.load(Ordering::Acquire), 0);
        assert_eq!(value(&counts.retired_objects), 0); assert_eq!(value(&counts.reuse_hits), 0);
        c.global.queue_drop(c.queue_id); drop(c.queue);
        assert_eq!(value(&counts.retained_objects), 0); assert_eq!(value(&counts.one_shot_objects), 0);
    }

    #[test]
    fn prepared_pending_execution_submit_error_resets_and_destroys_without_reuse() {
        let c = context(); let pool = &c.queue.upload_pool;
        let buffer = target(&c, 64);
        c.queue.write_buffer(buffer, 0, &[89; 64]).unwrap();
        assert_eq!(value(&pool.counters.allocation_calls), 1);
        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback_flag = callback_ran.clone();
        let counts = pool.counters.clone();
        let global = Arc::downgrade(&c.global);
        let device_id = c.device_id;
        c.global.device_set_device_lost_closure(device_id, Box::new(move |_, _| {
            // Prepared source disposal precedes this reentrant callback.
            assert_eq!(value(&counts.destroyed_allocations), 1);
            assert_eq!(value(&counts.retained_objects), 0);
            assert_eq!(value(&counts.one_shot_objects), 0);
            let global = global.upgrade().unwrap();
            global.device_set_device_lost_closure(device_id, Box::new(|_, _| {}));
            global.poll_all_devices(false).unwrap();
            callback_flag.store(true, Ordering::Release);
        }));
        pool.counters.test_fault.store(3, Ordering::Relaxed);
        assert!(c.queue.test_submit_pending_without_maintenance().is_err());
        assert!(callback_ran.load(Ordering::Acquire));
        assert_eq!(value(&pool.counters.test_fault), 0);
        assert_eq!(c.device.last_successful_submission_index.load(Ordering::Acquire), 0);
        assert_eq!(value(&pool.counters.retired_objects), 0);
        assert_eq!(value(&pool.counters.published_objects), 0);
        assert_eq!(value(&pool.counters.reuse_hits), 0);
        assert_eq!(value(&pool.counters.destroyed_allocations), 1);
        assert_eq!(value(&pool.counters.retained_objects), 0);
        assert_eq!(value(&pool.counters.one_shot_objects), 0);
        assert!(pool.closing.load(Ordering::Acquire));
        assert!(!c.device.is_valid());
    }

    #[test]
    fn pending_destroy_replacement_invalid_write_and_loss_preserve_ownership() {
        let c = context(); let pool = &c.queue.upload_pool;
        let original = target(&c, 64); c.queue.write_buffer(original.clone(), 0, &[43; 64]).unwrap();
        original.destroy();
        let replacement = target(&c, 64); c.queue.write_buffer(replacement.clone(), 0, &[59; 64]).unwrap();
        assert!(c.queue.write_buffer(replacement.clone(), 2, &[1; 4]).is_err());
        assert_eq!(c.device.last_successful_submission_index.load(Ordering::Acquire), 0);
        assert_eq!(value(&pool.counters.reuse_hits), 0);
        let index = c.queue.test_submit_pending_without_maintenance().unwrap();
        let closures = wait(&c, index);
        assert_eq!(bytes(&c, &replacement), [59; 64]);
        // Dropping an unpublished completion proof destroys; it does not recycle.
        drop(closures);
        assert_eq!(value(&pool.counters.free_objects), 0);
        let index = c.queue.submit(&[]).unwrap(); wait(&c, index).fire();
        c.global.device_destroy(c.device_id);
        assert!(pool.closing.load(Ordering::Acquire));
        assert!(c.queue.write_buffer(replacement, 0, &[1; 64]).is_err());
        c.device.poll(wgt::PollType::Poll).unwrap();
        assert_eq!(value(&pool.counters.free_objects), 0);
    }

    #[test]
    fn observable_hal_loss_closes_and_rejects_retirement() {
        let c = context(); let pool = &c.queue.upload_pool;
        let buffer = target(&c, 64);
        c.queue.write_buffer(buffer.clone(), 0, &[71; 64]).unwrap();
        let index = c.queue.test_submit_pending_without_maintenance().unwrap();
        let closures = wait(&c, index);
        c.device.handle_hal_error(hal::DeviceError::Lost);
        assert!(pool.closing.load(Ordering::Acquire));
        assert!(!c.device.is_valid());
        closures.fire();
        assert_eq!(value(&pool.counters.free_objects), 0);
        assert_eq!(value(&pool.counters.retained_objects), 0);
        assert_eq!(value(&pool.counters.rejected_retirements), u64::from(pool.enabled));
        assert!(c.queue.write_buffer(buffer, 0, &[3; 64]).is_err());
    }

    #[test]
    fn a_completed_source_cannot_be_published_into_another_pool_identity() {
        let c = context(); let original = &c.queue.upload_pool;
        let destination = target(&c, 64);
        c.queue.write_buffer(destination.clone(), 0, &[97; 64]).unwrap();
        let index = c.queue.test_submit_pending_without_maintenance().unwrap();
        let mut closures = wait(&c, index);
        assert_eq!(bytes(&c, &destination), [97; 64]);
        let other = StagingPool::new(&c.device);
        if original.enabled {
            let publication = closures.upload_retirements.pop().unwrap();
            // The batch has real successful fence/reset proof, but its unique
            // accounting identity must never be transferred to a different pool.
            RetirementPublication { pool: other.clone(), batch: publication.batch }.dispatch();
            assert_eq!(value(&other.counters.rejected_retirements), 1);
            assert_eq!(value(&other.counters.free_objects), 0);
            assert_eq!(value(&original.counters.retained_objects), 0);
        }
        closures.fire();
    }

    #[test]
    fn concurrent_writes_poll_all_and_last_queue_drop_do_not_invert_locks() {
        let c = context(); let pool = c.queue.upload_pool.clone();
        let counts = pool.counters.clone();
        let start = Arc::new(Barrier::new(5));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let device = c.device.clone(); let queue = c.queue.clone(); let start = start.clone();
            workers.push(thread::spawn(move || {
                let buffer = device.create_buffer(&wgt::BufferDescriptor { label: None, size: 64,
                    usage: wgt::BufferUsages::MAP_READ | wgt::BufferUsages::COPY_DST, mapped_at_creation: false }).unwrap();
                start.wait();
                for i in 0..64 { queue.write_buffer(buffer.clone(), 0, &[i; 64]).unwrap(); queue.submit(&[]).unwrap(); }
            }));
        }
        start.wait();
        for _ in 0..128 { c.global.poll_all_devices(false).unwrap(); thread::yield_now(); }
        for worker in workers { worker.join().unwrap(); }
        c.device.poll(wgt::PollType::wait_indefinitely()).unwrap();
        assert!(value(&counts.retained_peak_bytes) <= BYTE_CAP);
        assert!(value(&counts.retained_peak_objects) <= OBJECT_CAP);
        // Race a Device::maintain transient Queue Arc with its last public owner.
        let global = c.global.clone(); let ready = Arc::new(Barrier::new(2));
        let signal = ready.clone();
        let poller = thread::spawn(move || { signal.wait(); for _ in 0..128 { global.poll_all_devices(false).unwrap(); } });
        c.global.queue_drop(c.queue_id); ready.wait(); drop(c.queue);
        poller.join().unwrap();
        assert!(pool.closing.load(Ordering::Acquire));
        drop(pool);
        assert_eq!(value(&counts.retained_objects), 0);
        assert_eq!(value(&counts.one_shot_objects), 0);
    }
}
