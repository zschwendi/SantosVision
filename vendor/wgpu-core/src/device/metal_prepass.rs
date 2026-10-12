//! Opt-in Metal skip of empty per-pass "Pre Pass" command buffers.
//!
//! Every render and compute pass is encoded into its own HAL command buffer,
//! and a second one is then created for work that has to run just before it:
//! clears of discarded surfaces, query resets, an indirect-draw validation
//! pass and resource barriers. Metal has no barriers (its HAL transition
//! methods are empty), so when none of the other work exists that second
//! command buffer is empty. `MUGUET_METAL_SKIP_EMPTY_PREPASS=1` stops creating
//! it in exactly that case and nowhere else. It is separate from the
//! per-submit Transit switch in `queue.rs`.
//!
//! Counters are cumulative CPU-side decisions, one per encoded pass. They are
//! command-buffer counts, never GPU time.
use core::sync::atomic::Ordering;

#[cfg(supports_64bit_atomics)]
use core::sync::atomic::AtomicU64;
#[cfg(not(supports_64bit_atomics))]
use portable_atomic::AtomicU64;

/// What one pass would have to encode into its pre-pass command buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PrePassWork {
    /// Surfaces discarded earlier in this encoder that the pass reads.
    pub discard_fixups: bool,
    /// Query slots the pass begins, which are reset first.
    pub query_resets: bool,
    /// The device validates indirect draws in a pre-pass compute pass.
    pub indirect_validation: bool,
    /// That validation has nothing to validate for this pass.
    pub indirect_batch_empty: bool,
}

pub(crate) struct MetalPrePassControl {
    is_metal: bool,
    enabled: bool,
    render_skipped: AtomicU64,
    compute_skipped: AtomicU64,
    retained_disabled: AtomicU64,
    retained_discard_fixups: AtomicU64,
    retained_query_resets: AtomicU64,
    retained_indirect_validation: AtomicU64,
    retained_indirect_validation_empty_batch: AtomicU64,
}

impl MetalPrePassControl {
    pub(crate) fn new(backend: wgt::Backend) -> Self {
        let is_metal = backend == wgt::Backend::Metal;
        #[cfg(feature = "std")]
        let enabled = is_metal
            && std::env::var_os("MUGUET_METAL_SKIP_EMPTY_PREPASS")
                .as_deref() == Some(std::ffi::OsStr::new("1"));
        #[cfg(not(feature = "std"))]
        let enabled = false;
        Self {
            is_metal, enabled,
            render_skipped: AtomicU64::new(0), compute_skipped: AtomicU64::new(0),
            retained_disabled: AtomicU64::new(0),
            retained_discard_fixups: AtomicU64::new(0),
            retained_query_resets: AtomicU64::new(0),
            retained_indirect_validation: AtomicU64::new(0),
            retained_indirect_validation_empty_batch: AtomicU64::new(0),
        }
    }

    /// Decides, and counts, whether one pass goes without a pre-pass command
    /// buffer. True only on Metal with the switch on and nothing to encode
    /// into it. The indirect term is deliberately the conservative one:
    /// validation merely being enabled on the device keeps the buffer, even
    /// when this pass batched no indirect draw. The caller must then take the
    /// HAL-free tracker merge and must not open or swap an encoder.
    pub(crate) fn decide(&self, compute: bool, work: PrePassWork) -> bool {
        if !self.is_metal {
            return false;
        }
        let retained = if !self.enabled {
            &self.retained_disabled
        } else if work.discard_fixups {
            &self.retained_discard_fixups
        } else if work.query_resets {
            &self.retained_query_resets
        } else if work.indirect_validation {
            if work.indirect_batch_empty {
                self.retained_indirect_validation_empty_batch.fetch_add(1, Ordering::Relaxed);
            }
            &self.retained_indirect_validation
        } else {
            let skipped = if compute { &self.compute_skipped } else { &self.render_skipped };
            skipped.fetch_add(1, Ordering::Relaxed);
            return true;
        };
        retained.fetch_add(1, Ordering::Relaxed);
        false
    }

    /// Adds this device's cumulative counters to an instance-wide report.
    pub(crate) fn add_to(&self, report: &mut crate::global::MetalPrePassReport) {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        report.enabled |= self.enabled;
        report.render_skipped += load(&self.render_skipped);
        report.compute_skipped += load(&self.compute_skipped);
        report.retained_disabled += load(&self.retained_disabled);
        report.retained_discard_fixups += load(&self.retained_discard_fixups);
        report.retained_query_resets += load(&self.retained_query_resets);
        report.retained_indirect_validation += load(&self.retained_indirect_validation);
        report.retained_indirect_validation_empty_batch += load(&self.retained_indirect_validation_empty_batch);
    }

    /// One cumulative line, printed by the queue when its transit line prints.
    pub(crate) fn report(&self) {
        #[cfg(feature = "std")]
        if self.is_metal {
            let mut totals = crate::global::MetalPrePassReport::default();
            self.add_to(&mut totals);
            std::eprintln!("[wgpu] metal prepass: {{\"schema\":1,\"enabled\":{},\"render_skipped\":{},\"compute_skipped\":{},\"retained_disabled\":{},\"retained_discard_fixups\":{},\"retained_query_resets\":{},\"retained_indirect_validation\":{},\"retained_indirect_validation_empty_batch\":{},\"scope\":\"cumulative per-pass decisions since device creation; one skipped decision is one HAL command buffer not created; command-buffer counts, not GPU time\"}}",
                self.enabled, totals.render_skipped, totals.compute_skipped, totals.retained_disabled,
                totals.retained_discard_fixups, totals.retained_query_resets,
                totals.retained_indirect_validation, totals.retained_indirect_validation_empty_batch);
        }
    }
}
