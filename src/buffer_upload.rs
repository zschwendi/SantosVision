//! Buffer writes keep the legacy queue-write prelude order. Retained mapped
//! pages are bounded; oversize/cap misses use one-shot sources in the SAME
//! prelude, never an unordered mixture of explicit copies and queue writes.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;
use wgpu::*;

pub(crate) const PAGE_BYTES: u64 = 1 << 20;
pub(crate) const PAGE_CAP: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UploadMode { Queue, Pool }

impl UploadMode {
    fn parse(value: Option<&str>) -> Result<Self, ()> {
        match value {
            Some("queue") => Ok(Self::Queue), Some("pool") => Ok(Self::Pool),
            None => Ok(Self::Queue),
            _ => Err(()),
        }
    }
    fn name(self) -> &'static str { match self { Self::Queue => "queue", Self::Pool => "pool" } }
}

// The public pool is opt-in until finite source-FPS acceptance. Constant
// retained budget, with only two explicitly supported page configurations.
fn page_bytes(value: Option<&str>) -> Result<u64, ()> {
    match value { None | Some("1048576") => Ok(PAGE_BYTES), Some("65536") => Ok(65536), _ => Err(()) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageState { Free, Active, Pending, Failed }

struct Page {
    buffer: Buffer,
    offset: u64,
    generation: u64,
    state: PageState,
    submitted_before: bool,
}

#[derive(Clone, Copy)]
struct MapResult { page: usize, generation: u64, success: bool }

// Each callback carries its own generation through the channel. An old event
// cannot overwrite a newer event, unlike a single unqualified atomic per page.
fn apply_map_result(state: &mut PageState, generation: u64, result: MapResult) -> bool {
    if *state != PageState::Pending || generation != result.generation { return false; }
    *state = if result.success { PageState::Free } else { PageState::Failed };
    true
}

#[derive(Default)]
struct Interval {
    calls: u64, bytes: u64, pooled_calls: u64, transient_calls: u64,
    new_retained_pages: u64, reused_pages: u64, transient_allocations: u64,
    transient_allocated_bytes: u64, cpu_copy_ns: u64, cpu_copy_max_ns: u64,
    submissions: u64, upload_only_submissions: u64, map_errors: u64,
    stale_callbacks: u64, deferred_destroys: u64, invalid_queue_calls: u64,
    epoch_bytes_max: u64, pending_bytes_peak: u64,
    submit_calls: u64, submit_cpu_ns: u64, submit_cpu_max_ns: u64,
    poll_calls: u64, poll_errors: u64, poll_collect_cpu_ns: u64, poll_collect_cpu_max_ns: u64,
}

struct TransientBatch {
    // Keep sources alive and accounted for until this exact submit completes.
    _buffers: Vec<Buffer>,
    bytes: u64,
    done: Arc<AtomicBool>,
}

pub(crate) struct BufferUploads {
    mode: UploadMode,
    page_bytes: u64,
    page_cap: usize,
    pages: Vec<Page>,
    next_generation: u64,
    prelude: Option<CommandEncoder>,
    destinations: HashSet<Buffer>,
    deferred_destroy: HashSet<Buffer>,
    transient_active: Vec<Buffer>,
    transient_pending: Vec<TransientBatch>,
    sender: mpsc::Sender<MapResult>,
    receiver: mpsc::Receiver<MapResult>,
    epoch_bytes: u64,
    stats: Interval,
}

impl BufferUploads {
    pub(crate) fn from_environment() -> Self {
        let value = std::env::var("MUGUET_BUFFER_UPLOAD_MODE").ok();
        let mode = UploadMode::parse(value.as_deref()).unwrap_or_else(|_| {
            eprintln!("[gpu] invalid MUGUET_BUFFER_UPLOAD_MODE; expected queue or pool, using queue");
            UploadMode::Queue
        });
        let page_value = std::env::var("MUGUET_BUFFER_UPLOAD_PAGE_BYTES").ok();
        let page_bytes = page_bytes(page_value.as_deref()).unwrap_or_else(|_| {
            eprintln!("[gpu] invalid MUGUET_BUFFER_UPLOAD_PAGE_BYTES; expected 65536 or 1048576, using 1048576");
            PAGE_BYTES
        });
        let retained_cap = PAGE_BYTES * PAGE_CAP as u64;
        let page_cap = (retained_cap / page_bytes) as usize;
        eprintln!("[gpu] buffer upload mode: {} (page bytes {}; retained page cap {} bytes; transient sources separately accounted)", mode.name(), page_bytes, retained_cap);
        Self::new(mode, page_bytes, page_cap)
    }

    fn new(mode: UploadMode, page_bytes: u64, page_cap: usize) -> Self {
        assert!(page_bytes >= MAP_ALIGNMENT && page_bytes.is_multiple_of(MAP_ALIGNMENT));
        let (sender, receiver) = mpsc::channel();
        Self { mode, page_bytes, page_cap, pages: Vec::with_capacity(page_cap), next_generation: 0,
            prelude: None, destinations: HashSet::new(), deferred_destroy: HashSet::new(),
            transient_active: Vec::new(), transient_pending: Vec::new(), sender, receiver,
            epoch_bytes: 0, stats: Interval::default() }
    }

    pub(crate) fn mode(&self) -> &'static str { self.mode.name() }

    pub(crate) fn write(&mut self, device: &Device, queue: &Queue, destination: &Buffer, offset: u64, data: &[u8]) {
        self.stats.calls += 1;
        self.stats.bytes = self.stats.bytes.saturating_add(data.len() as u64);
        if self.mode == UploadMode::Queue {
            queue.write_buffer(destination, offset, data);
            return;
        }
        // Invalid/empty calls preserve queue validation. These cannot execute a
        // valid transfer, so they cannot reorder any successful prelude write.
        let size = data.len() as u64;
        if size == 0 || !offset.is_multiple_of(COPY_BUFFER_ALIGNMENT)
            || !size.is_multiple_of(COPY_BUFFER_ALIGNMENT)
            || offset.checked_add(size).is_none_or(|end| end > destination.size())
            || !destination.usage().contains(BufferUsages::COPY_DST) {
            self.stats.invalid_queue_calls += 1;
            queue.write_buffer(destination, offset, data);
            return;
        }
        self.collect_callbacks();
        let (source, source_offset) = self.allocate(device, size);
        {
            let mut mapped = source.slice(source_offset..source_offset + size)
                .get_mapped_range_mut().expect("upload source is mapped and exclusively owned");
            let started = Instant::now();
            mapped.copy_from_slice(data);
            let ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            self.stats.cpu_copy_ns = self.stats.cpu_copy_ns.saturating_add(ns);
            self.stats.cpu_copy_max_ns = self.stats.cpu_copy_max_ns.max(ns);
        }
        let prelude = self.prelude.get_or_insert_with(|| device.create_command_encoder(&CommandEncoderDescriptor { label: Some("buffer upload prelude") }));
        prelude.copy_buffer_to_buffer(&source, source_offset, destination, offset, size);
        // Capture the real handle at write time, never look up a reused guest id
        // during flush. A clone also preserves dropped/replaced destinations.
        self.destinations.insert(destination.clone());
        self.epoch_bytes = self.epoch_bytes.saturating_add(size);
        self.stats.epoch_bytes_max = self.stats.epoch_bytes_max.max(self.epoch_bytes);
        self.stats.pending_bytes_peak = self.stats.pending_bytes_peak.max(self.pending_bytes());
    }

    fn allocate(&mut self, device: &Device, size: u64) -> (Buffer, u64) {
        if size <= self.page_bytes {
            let active = self.pages.iter().position(|p| p.state == PageState::Active
                && align(p.offset).checked_add(size).is_some_and(|end| end <= self.page_bytes));
            if let Some(index) = active {
                let page = &mut self.pages[index];
                let offset = align(page.offset);
                page.offset = offset + size;
                self.stats.pooled_calls += 1;
                return (page.buffer.clone(), offset);
            }
            let free = self.pages.iter().position(|p| p.state == PageState::Free);
            let index = if let Some(index) = free {
                self.stats.reused_pages += u64::from(self.pages[index].submitted_before);
                Some(index)
            } else if self.pages.len() < self.page_cap {
                self.pages.push(Page { buffer: staging(device, self.page_bytes), offset: 0,
                    generation: 0, state: PageState::Free, submitted_before: false });
                self.stats.new_retained_pages += 1;
                Some(self.pages.len() - 1)
            } else { None };
            if let Some(index) = index {
                self.next_generation = self.next_generation.checked_add(1).expect("upload generation exhausted");
                let page = &mut self.pages[index];
                page.generation = self.next_generation;
                page.state = PageState::Active;
                page.offset = size;
                self.stats.pooled_calls += 1;
                return (page.buffer.clone(), 0);
            }
        }
        // A cap/size miss MUST use this prelude as well, so overlapping writes
        // retain call order. Never fall back to a valid Queue::write_buffer here.
        let source = staging(device, size);
        self.transient_active.push(source.clone());
        self.stats.transient_calls += 1;
        self.stats.transient_allocations += 1;
        self.stats.transient_allocated_bytes = self.stats.transient_allocated_bytes.saturating_add(size);
        (source, 0)
    }

    pub(crate) fn destroy(&mut self, destination: Buffer) {
        if self.mode == UploadMode::Pool && self.destinations.contains(&destination) {
            if self.deferred_destroy.insert(destination) { self.stats.deferred_destroys += 1; }
        } else { destination.destroy(); }
    }

    pub(crate) fn submit(&mut self, queue: &Queue, main: Option<CommandBuffer>) -> Option<SubmissionIndex> {
        let started = Instant::now();
        self.stats.submit_calls += 1;
        self.collect_callbacks();
        let Some(prelude) = self.prelude.take() else {
            let submission = main.map(|main| queue.submit([main]));
            self.stats.submissions += u64::from(submission.is_some());
            self.record_submit_wall(started);
            return submission;
        };
        let upload_only = main.is_none();
        for (index, page) in self.pages.iter_mut().enumerate() {
            if page.state != PageState::Active { continue; }
            // Every view was dropped in write(). No subsequent CPU writes until
            // a successful callback for this exact activation generation.
            page.buffer.unmap();
            page.state = PageState::Pending;
            page.submitted_before = true;
            let generation = page.generation;
            let sender = self.sender.clone();
            prelude.map_buffer_on_submit(&page.buffer, MapMode::Write, .., move |result| {
                let _ = sender.send(MapResult { page: index, generation, success: result.is_ok() });
            });
        }
        if !self.transient_active.is_empty() {
            let buffers = std::mem::take(&mut self.transient_active);
            let bytes = buffers.iter().map(Buffer::size).sum();
            for source in &buffers { source.unmap(); }
            let done = Arc::new(AtomicBool::new(false));
            let callback_done = done.clone();
            prelude.on_submitted_work_done(move || callback_done.store(true, Ordering::Release));
            self.transient_pending.push(TransientBatch { _buffers: buffers, bytes, done });
        }
        // Internal texture queue writes still precede both encoders. Buffer
        // uploads retain their original order before every command in main,
        // including consumers encoded BEFORE a no-overwrite write.
        let index = queue.submit(std::iter::once(prelude.finish()).chain(main));
        self.stats.submissions += 1;
        self.stats.upload_only_submissions += u64::from(upload_only);
        self.epoch_bytes = 0;
        self.destinations.clear();
        // Explicit destruction AFTER submit validation is safe: wgpu retains
        // the recorded raw resource through its actual submission completion.
        for destination in self.deferred_destroy.drain() { destination.destroy(); }
        self.collect_callbacks();
        self.record_submit_wall(started);
        Some(index)
    }

    fn record_submit_wall(&mut self, started: Instant) {
        let ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.stats.submit_cpu_ns = self.stats.submit_cpu_ns.saturating_add(ns);
        self.stats.submit_cpu_max_ns = self.stats.submit_cpu_max_ns.max(ns);
    }

    pub(crate) fn poll(&mut self, device: &Device) {
        if !self.has_pending_callbacks() { self.collect_callbacks(); return; }
        let started = Instant::now();
        self.stats.poll_calls += 1;
        self.stats.poll_errors += u64::from(device.poll(PollType::Poll).is_err());
        self.collect_callbacks();
        let ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.stats.poll_collect_cpu_ns = self.stats.poll_collect_cpu_ns.saturating_add(ns);
        self.stats.poll_collect_cpu_max_ns = self.stats.poll_collect_cpu_max_ns.max(ns);
    }

    pub(crate) fn has_pending_callbacks(&self) -> bool {
        self.pages.iter().any(|p| p.state == PageState::Pending) || !self.transient_pending.is_empty()
    }

    fn collect_callbacks(&mut self) {
        while let Ok(result) = self.receiver.try_recv() {
            let Some(page) = self.pages.get_mut(result.page) else { self.stats.stale_callbacks += 1; continue; };
            if !apply_map_result(&mut page.state, page.generation, result) {
                self.stats.stale_callbacks += 1;
            } else if !result.success { self.stats.map_errors += 1; }
            else { page.offset = 0; }
        }
        self.transient_pending.retain(|batch| !batch.done.load(Ordering::Acquire));
        // Failed sources remain quarantined and counted within the fixed cap;
        // never reuse unmapped memory or pretend a failed map completed safely.
    }

    fn pending_bytes(&self) -> u64 {
        self.pages.iter().filter(|p| matches!(p.state, PageState::Active | PageState::Pending))
            .count() as u64 * self.page_bytes
            + self.transient_active.iter().map(Buffer::size).sum::<u64>()
            + self.transient_pending.iter().map(|batch| batch.bytes).sum::<u64>()
    }

    pub(crate) fn metrics(&self, interval_s: f64) -> serde_json::Value {
        let s = &self.stats;
        serde_json::json!({"schema": 1, "mode": self.mode(), "interval_s": interval_s,
            "retained_cap_bytes": self.page_bytes * self.page_cap as u64, "page_bytes": self.page_bytes,
            "retained_pages": self.pages.len(), "retained_bytes": self.pages.len() as u64 * self.page_bytes,
            "active_bytes": self.pages.iter().filter(|p| p.state == PageState::Active).count() as u64 * self.page_bytes + self.transient_active.iter().map(Buffer::size).sum::<u64>(),
            "pending_bytes": self.pending_bytes(), "transient_pending_bytes": self.transient_pending.iter().map(|batch| batch.bytes).sum::<u64>(),
            "pending_pages": self.pages.iter().filter(|p| p.state == PageState::Pending).count(),
            "failed_pages": self.pages.iter().filter(|p| p.state == PageState::Failed).count(),
            "pending_destinations": self.destinations.len(), "deferred_destinations": self.deferred_destroy.len(),
            "stats_interval": {"calls": s.calls, "bytes": s.bytes, "pooled_calls": s.pooled_calls, "transient_calls": s.transient_calls,
                "new_retained_pages": s.new_retained_pages, "reused_pages": s.reused_pages, "transient_allocations": s.transient_allocations,
                "transient_allocated_bytes": s.transient_allocated_bytes, "cpu_copy_ns": s.cpu_copy_ns, "cpu_copy_max_ns": s.cpu_copy_max_ns,
                "submissions": s.submissions, "upload_only_submissions": s.upload_only_submissions, "map_errors": s.map_errors,
                "stale_callbacks": s.stale_callbacks, "deferred_destroys": s.deferred_destroys, "invalid_queue_calls": s.invalid_queue_calls,
                "epoch_bytes_max": s.epoch_bytes_max, "pending_bytes_peak": s.pending_bytes_peak,
                "submit_calls": s.submit_calls, "submit_cpu_ns": s.submit_cpu_ns, "submit_cpu_max_ns": s.submit_cpu_max_ns,
                "poll_calls": s.poll_calls, "poll_errors": s.poll_errors, "poll_collect_cpu_ns": s.poll_collect_cpu_ns, "poll_collect_cpu_max_ns": s.poll_collect_cpu_max_ns},
            "memory_scope": "16MiB retained cap; one-shot sources separately counted, not a total staging cap",
            "timing_scope": "CPU only, not GPU time: copy_ns is memcpy; route timer includes allocation/validation/copy/encoding; submit_cpu_ns includes unmap/map callback registration/queue.submit/destroy/drain; poll_collect_cpu_ns is extra pending-source poll plus drain; guest snapshot excluded"})
    }

    pub(crate) fn reset_interval(&mut self) {
        self.stats = Interval { pending_bytes_peak: self.pending_bytes(), epoch_bytes_max: self.epoch_bytes, ..Default::default() };
    }
}

fn align(offset: u64) -> u64 { offset.div_ceil(MAP_ALIGNMENT) * MAP_ALIGNMENT }
fn staging(device: &Device, size: u64) -> Buffer {
    device.create_buffer(&BufferDescriptor { label: Some("buffer upload source"), size,
        usage: BufferUsages::MAP_WRITE | BufferUsages::COPY_SRC, mapped_at_creation: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_and_failed_or_stale_maps_never_reuse_sources() {
        assert_eq!(UploadMode::parse(None), Ok(UploadMode::Queue));
        assert_eq!(page_bytes(None), Ok(1048576));
        assert_eq!(page_bytes(Some("65536")), Ok(65536));
        assert_eq!(page_bytes(Some("1048576")), Ok(1048576));
        for value in ["0", "1", "32768", "2097152", "-65536", "invalid", ""] {
            assert_eq!(page_bytes(Some(value)), Err(()));
        }
        for page in [65536, 1048576] { assert_eq!(page * (16 * 1048576 / page), 16 * 1048576); }
        assert_eq!(UploadMode::parse(Some("queue")), Ok(UploadMode::Queue));
        assert_eq!(UploadMode::parse(Some("pool")), Ok(UploadMode::Pool));
        assert_eq!(UploadMode::parse(Some("POOL")), Err(()));
        let mut state = PageState::Pending;
        assert!(!apply_map_result(&mut state, 4, MapResult { page: 0, generation: 3, success: true }));
        assert_eq!(state, PageState::Pending);
        assert!(apply_map_result(&mut state, 4, MapResult { page: 0, generation: 4, success: false }));
        assert_eq!(state, PageState::Failed);
        assert!(!apply_map_result(&mut state, 4, MapResult { page: 0, generation: 4, success: true }));
        assert_eq!(state, PageState::Failed);
        state = PageState::Pending;
        assert!(apply_map_result(&mut state, 5, MapResult { page: 0, generation: 5, success: true }));
        assert_eq!(state, PageState::Free);
        assert!(!apply_map_result(&mut state, 5, MapResult { page: 0, generation: 4, success: false }));
        assert_eq!(state, PageState::Free);
    }

    #[cfg(target_os = "macos")]
    fn metal() -> (Device, Queue) {
        let mut desc = InstanceDescriptor::new_without_display_handle();
        desc.backends = Backends::METAL;
        let instance = Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).expect("real Metal test adapter");
        pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).expect("real Metal test device")
    }

    fn target(device: &Device, size: u64) -> Buffer {
        device.create_buffer(&BufferDescriptor { label: Some("upload correctness target"), size,
            usage: BufferUsages::COPY_SRC | BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    fn read(device: &Device, queue: &Queue, buffer: &Buffer, size: u64) -> Vec<u8> {
        let output = device.create_buffer(&BufferDescriptor { label: Some("upload correctness readback"), size,
            usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &output, 0, size);
        let (tx, rx) = mpsc::channel();
        encoder.map_buffer_on_submit(&output, MapMode::Read, .., move |r| { let _ = tx.send(r); });
        let submission = queue.submit([encoder.finish()]);
        device.poll(PollType::Wait { submission_index: Some(submission), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap().unwrap();
        let bytes = output.slice(..).get_mapped_range().unwrap().to_vec();
        output.unmap();
        bytes
    }

    #[cfg(target_os = "macos")]
    fn texture_pixels(device: &Device, queue: &Queue, texture: &Texture) -> Vec<u8> {
        let output = device.create_buffer(&BufferDescriptor { label: Some("Transit texture readback"), size: 512,
            usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(TexelCopyTextureInfo { texture, mip_level: 0,
            origin: Origin3d::ZERO, aspect: TextureAspect::All }, TexelCopyBufferInfo { buffer: &output,
            layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(2) } },
            Extent3d { width: 2, height: 2, depth_or_array_layers: 1 });
        let (tx, rx) = mpsc::channel();
        encoder.map_buffer_on_submit(&output, MapMode::Read, .., move |result| { let _ = tx.send(result); });
        let submitted = queue.submit([encoder.finish()]);
        device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap().unwrap();
        let mapped = output.slice(..).get_mapped_range().unwrap();
        let pixels = [&mapped[..8], &mapped[256..264]].concat();
        drop(mapped); output.unmap();
        pixels
    }

    // Run this same behavioral test in separate env=0 and env=1 processes.
    // The first submit has initialized sources and implicit destination init,
    // so backend diagnostics prove the optional empty-Transit branch executes.
    #[test]
    #[cfg(target_os = "macos")]
    fn metal_transit_initialized_copy_and_partial_buffer_tail_are_byte_exact() {
        let (device, queue) = metal();
        let scope = device.push_error_scope(ErrorFilter::Validation);
        let source = target(&device, 64);
        let copied = target(&device, 64);
        let expected: Vec<u8> = (0..64).map(|i| (i * 3 + 1) as u8).collect();
        queue.write_buffer(&source, 0, &expected);
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&source, 0, &copied, 0, 64);
        let submitted = queue.submit([encoder.finish()]);
        device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        assert_eq!(read(&device, &queue, &copied, 64), expected);

        // Untouched bytes on either side must be cleared without erasing the
        // partial PendingWrites transfer that precedes the main command.
        let partial = target(&device, 32);
        queue.write_buffer(&partial, 4, &[19, 23, 29, 31, 37, 41, 43, 47]);
        let mut expected = vec![0; 32];
        expected[4..12].copy_from_slice(&[19, 23, 29, 31, 37, 41, 43, 47]);
        assert_eq!(read(&device, &queue, &partial, 32), expected);
        assert!(pollster::block_on(scope.pop()).is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_transit_fresh_memory_and_discarded_textures_are_zeroed() {
        let (device, queue) = metal();
        let scope = device.push_error_scope(ErrorFilter::Validation);
        // First submit needs a genuine clear and must retain Transit even
        // when the environment enables eliding an empty one.
        assert_eq!(read(&device, &queue, &target(&device, 64), 64), [0; 64]);
        let make_texture = || device.create_texture(&TextureDescriptor { label: Some("Transit init/discard target"),
            size: Extent3d { width: 2, height: 2, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: TextureDimension::D2, format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[] });
        assert_eq!(texture_pixels(&device, &queue, &make_texture()), [0; 16]);
        let partial = make_texture();
        queue.write_texture(TexelCopyTextureInfo { texture: &partial, mip_level: 0,
            origin: Origin3d { x: 1, y: 1, z: 0 }, aspect: TextureAspect::All }, &[11, 17, 23, 29],
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
        let mut expected = vec![0; 16]; expected[12..16].copy_from_slice(&[11, 17, 23, 29]);
        assert_eq!(texture_pixels(&device, &queue, &partial), expected);

        // Discard state must still propagate on the skip path, so a later
        // read initializes the surface rather than exposing discarded data.
        let view = partial.create_view(&TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        encoder.begin_render_pass(&RenderPassDescriptor { label: Some("Transit discard proof"),
            color_attachments: &[Some(RenderPassColorAttachment { view: &view, depth_slice: None,
                resolve_target: None, ops: Operations { load: LoadOp::Clear(Color { r: 1., g: 0.5, b: 0.25, a: 1. }), store: StoreOp::Discard } })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
        let submitted = queue.submit([encoder.finish()]);
        device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        assert_eq!(texture_pixels(&device, &queue, &partial), [0; 16]);
        assert!(pollster::block_on(scope.pop()).is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_transit_empty_submission_and_deferred_occlusion_results_complete() {
        let (device, queue) = metal();
        let scope = device.push_error_scope(ErrorFilter::Validation);
        // Eliding an empty public command buffer must still signal its fence
        // and fire completion callbacks, even with no HAL input commands.
        let empty = device.create_command_encoder(&CommandEncoderDescriptor::default()).finish();
        let submitted = queue.submit([empty]);
        let (tx, rx) = mpsc::channel();
        queue.on_submitted_work_done(move || { let _ = tx.send(()); });
        device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();

        let color = device.create_texture(&TextureDescriptor { label: Some("Transit query target"),
            size: Extent3d { width: 4, height: 4, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: TextureDimension::D2, format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::RENDER_ATTACHMENT, view_formats: &[] });
        let view = color.create_view(&TextureViewDescriptor::default());
        let shader = device.create_shader_module(ShaderModuleDescriptor { label: Some("Transit query triangle"), source: ShaderSource::Wgsl(r#"
            @vertex fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
                let positions = array<vec2f, 3>(vec2f(-1., -1.), vec2f(3., -1.), vec2f(-1., 3.));
                return vec4f(positions[index], 0., 1.);
            }
            @fragment fn fs() -> @location(0) vec4f { return vec4f(1., 0., 0., 1.); }
        "#.into()) });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor { label: Some("Transit query pipeline"), layout: None,
            vertex: VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: PrimitiveState::default(), depth_stencil: None, multisample: MultisampleState::default(),
            fragment: Some(FragmentState { module: &shader, entry_point: Some("fs"), compilation_options: Default::default(),
                targets: &[Some(ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: ColorWrites::ALL })] }),
            multiview_mask: None, cache: None });
        let queries = device.create_query_set(&QuerySetDescriptor { label: Some("Transit deferred query proof"), ty: QueryType::Occlusion, count: 2 });
        let results = device.create_buffer(&BufferDescriptor { label: Some("Transit query results"), size: 16,
            usage: BufferUsages::QUERY_RESOLVE | BufferUsages::COPY_SRC, mapped_at_creation: false });
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor { label: Some("Transit query pass"),
                color_attachments: &[Some(RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None,
                    ops: Operations { load: LoadOp::Clear(Color::BLACK), store: StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: Some(&queries), multiview_mask: None });
            pass.set_pipeline(&pipeline);
            pass.begin_occlusion_query(0);
            pass.draw(0..3, 0..1);
            pass.end_occlusion_query();
        }
        encoder.resolve_query_set(&queries, 0..2, &results, 0);
        let submitted = queue.submit([encoder.finish()]);
        device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        let bytes = read(&device, &queue, &results, 16);
        assert!(u64::from_le_bytes(bytes[..8].try_into().unwrap()) > 0, "drawn query must retain its result");
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 0, "unused query is explicitly cleared");
        assert!(pollster::block_on(scope.pop()).is_none());
    }

    // This exercises public Metal copies/mapping, not a mock that mirrors the
    // allocator. Deliberately tiny caps force both exhaustion and oversize
    // paths and compare every byte with the existing queue route.
    #[test]
    #[cfg(target_os = "macos")]
    fn metal_pool_preserves_overlaps_fallback_replacement_and_padded_tails() {
        let (device, queue) = metal();
        let scope = device.push_error_scope(ErrorFilter::Validation);
        let mut outputs = Vec::new();
        for mode in [UploadMode::Queue, UploadMode::Pool] {
            let mut uploads = BufferUploads::new(mode, 16, 1);
            let old = target(&device, 64);
            uploads.write(&device, &queue, &old, 0, &[1; 8]);
            uploads.write(&device, &queue, &old, 0, &[2; 32]); // oversize
            uploads.write(&device, &queue, &old, 4, &[3; 8]); // last overlapping write
            uploads.write(&device, &queue, &old, 32, &[9, 8, 7, 0]); // original padded tail
            // Same guest slot is now replaced; only actual handles are passed.
            let replacement = target(&device, 64);
            uploads.write(&device, &queue, &replacement, 0, &[4; 8]); // cap exhausted
            let old_copy = target(&device, 64);
            let mut main = device.create_command_encoder(&CommandEncoderDescriptor::default());
            main.copy_buffer_to_buffer(&old, 0, &old_copy, 0, 64);
            uploads.submit(&queue, Some(main.finish())).unwrap();
            let a = read(&device, &queue, &old_copy, 64);
            let b = read(&device, &queue, &replacement, 64);
            assert_eq!(&a[..4], &[2; 4]);
            assert_eq!(&a[4..12], &[3; 8]);
            assert_eq!(&a[12..32], &[2; 20]);
            assert_eq!(&a[32..36], &[9, 8, 7, 0]);
            assert_eq!(&b[..8], &[4; 8]);
            if mode == UploadMode::Pool {
                assert_eq!(uploads.pages.len(), 1);
                assert_eq!(uploads.stats.transient_allocations, 3);
                assert_eq!(uploads.stats.epoch_bytes_max, 60);
            }
            outputs.push((a, b));
        }
        assert_eq!(outputs[0], outputs[1]);
        assert!(pollster::block_on(scope.pop()).is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_pool_defers_exact_destroy_until_submission_even_without_main() {
        let (device, queue) = metal();
        let scope = device.push_error_scope(ErrorFilter::Validation);
        let mut uploads = BufferUploads::new(UploadMode::Pool, 32, 1);
        let original = target(&device, 16);
        uploads.write(&device, &queue, &original, 0, &[6; 16]);
        uploads.destroy(original.clone());
        let replacement = target(&device, 16);
        uploads.write(&device, &queue, &replacement, 0, &[7; 16]);
        assert_eq!(uploads.deferred_destroy.len(), 1);
        let original_copy = target(&device, 16);
        let mut main = device.create_command_encoder(&CommandEncoderDescriptor::default());
        main.copy_buffer_to_buffer(&original, 0, &original_copy, 0, 16);
        uploads.submit(&queue, Some(main.finish())).unwrap();
        assert!(uploads.deferred_destroy.is_empty());
        assert_eq!(read(&device, &queue, &original_copy, 16), [6; 16]);
        assert_eq!(read(&device, &queue, &replacement, 16), [7; 16]);
        let only = target(&device, 16);
        uploads.write(&device, &queue, &only, 0, &[8; 16]);
        uploads.destroy(only);
        let submission = uploads.submit(&queue, None).unwrap();
        device.poll(PollType::Wait { submission_index: Some(submission), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        uploads.poll(&device);
        assert_eq!(uploads.stats.upload_only_submissions, 1);
        assert!(uploads.deferred_destroy.is_empty());
        assert!(pollster::block_on(scope.pop()).is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_pool_prelude_precedes_earlier_consumers_and_preserves_counter_stream() {
        let (device, queue) = metal();
        let scope = device.push_error_scope(ErrorFilter::Validation);
        let mut outputs = Vec::new();
        for mode in [UploadMode::Queue, UploadMode::Pool] {
            let mut uploads = BufferUploads::new(mode, 64, 1);
            let source = target(&device, 16);
            let output = target(&device, 16);
            let mut main = device.create_command_encoder(&CommandEncoderDescriptor::default());
            // Legacy no_overwrite queue write happens after this consumer is
            // recorded but executes before it, in the queue's upload prelude.
            main.copy_buffer_to_buffer(&source, 0, &output, 0, 4);
            uploads.write(&device, &queue, &source, 0, &17u32.to_le_bytes());
            let counter = target(&device, 4);
            uploads.write(&device, &queue, &source, 4, &23u32.to_le_bytes());
            main.copy_buffer_to_buffer(&source, 4, &counter, 0, 4);
            main.copy_buffer_to_buffer(&counter, 0, &output, 4, 4);
            uploads.write(&device, &queue, &source, 8, &29u32.to_le_bytes());
            main.copy_buffer_to_buffer(&source, 8, &counter, 0, 4);
            main.copy_buffer_to_buffer(&counter, 0, &output, 8, 4);
            uploads.submit(&queue, Some(main.finish())).unwrap();
            let data = read(&device, &queue, &output, 16);
            assert_eq!(u32::from_le_bytes(data[..4].try_into().unwrap()), 17);
            assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), 23);
            assert_eq!(u32::from_le_bytes(data[8..12].try_into().unwrap()), 29);
            outputs.push(data);
        }
        assert_eq!(outputs[0], outputs[1]);
        assert!(pollster::block_on(scope.pop()).is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_pool_reuses_only_successfully_remapped_pages_and_releases_transients() {
        let (device, queue) = metal();
        let scope = device.push_error_scope(ErrorFilter::Validation);
        let mut uploads = BufferUploads::new(UploadMode::Pool, 16, 1);
        let destination = target(&device, 64);
        uploads.write(&device, &queue, &destination, 0, &[1; 16]);
        uploads.write(&device, &queue, &destination, 16, &[2; 32]);
        assert_eq!(uploads.pending_bytes(), 48);
        let submitted = uploads.submit(&queue, None).unwrap();
        device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        uploads.poll(&device);
        assert_eq!(uploads.pages[0].state, PageState::Free);
        assert_eq!(uploads.pending_bytes(), 0);
        assert!(uploads.transient_pending.is_empty());
        let generation = uploads.pages[0].generation;
        uploads.reset_interval();
        uploads.write(&device, &queue, &destination, 0, &[3; 16]);
        assert_eq!(uploads.pages.len(), 1);
        assert_eq!(uploads.stats.reused_pages, 1);
        assert_eq!(uploads.stats.new_retained_pages, 0);
        assert!(uploads.pages[0].generation > generation);
        let submitted = uploads.submit(&queue, None).unwrap();
        device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        uploads.poll(&device);
        assert_eq!(&read(&device, &queue, &destination, 64)[..16], &[3; 16]);
        assert!(pollster::block_on(scope.pop()).is_none());
    }
}
