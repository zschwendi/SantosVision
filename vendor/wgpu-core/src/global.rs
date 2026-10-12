use alloc::{borrow::ToOwned as _, sync::Arc};
use core::fmt;

use crate::{
    hub::{Hub, HubReport},
    instance::{Instance, Surface},
    registry::{Registry, RegistryReport},
    resource_log,
};

/// Cumulative decisions of the opt-in Metal empty Pre Pass skip, summed over
/// this instance's devices. One skipped decision is one HAL command buffer
/// that was not created. Command-buffer counts, not GPU time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetalPrePassReport {
    /// Some Metal device of this instance has the switch on.
    pub enabled: bool,
    pub render_skipped: u64,
    pub compute_skipped: u64,
    pub retained_disabled: u64,
    pub retained_discard_fixups: u64,
    pub retained_query_resets: u64,
    /// Passes that batched an indirect draw on a device that validates them.
    pub retained_indirect_validation: u64,
    /// Of `render_skipped`, passes on a device that validates indirect draws
    /// which batched none.
    pub render_skipped_empty_indirect_batch: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GlobalReport {
    pub surfaces: RegistryReport,
    pub hub: HubReport,
    pub metal_prepass: MetalPrePassReport,
}

impl GlobalReport {
    pub fn surfaces(&self) -> &RegistryReport {
        &self.surfaces
    }
    pub fn hub_report(&self) -> &HubReport {
        &self.hub
    }
}

pub struct Global {
    pub(crate) surfaces: Registry<Arc<Surface>>,
    pub(crate) hub: Hub,
    // the instance must be dropped last
    pub instance: Instance,
}

impl Global {
    pub fn new(
        name: &str,
        instance_desc: wgt::InstanceDescriptor,
        telemetry: Option<hal::Telemetry>,
    ) -> Self {
        profiling::scope!("Global::new");
        Self {
            instance: Instance::new(name, instance_desc, telemetry),
            surfaces: Registry::new(),
            hub: Hub::new(),
        }
    }

    /// # Safety
    ///
    /// Refer to the creation of wgpu-hal Instance for every backend.
    pub unsafe fn from_hal_instance<A: hal::Api>(name: &str, hal_instance: A::Instance) -> Self {
        profiling::scope!("Global::new");

        Self {
            instance: Instance::from_hal_instance::<A>(name.to_owned(), hal_instance),
            surfaces: Registry::new(),
            hub: Hub::new(),
        }
    }

    /// # Safety
    ///
    /// - The raw instance handle returned must not be manually destroyed.
    pub unsafe fn instance_as_hal<A: hal::Api>(&self) -> Option<&A::Instance> {
        unsafe { self.instance.as_hal::<A>() }
    }

    /// # Safety
    ///
    /// - The raw handles obtained from the Instance must not be manually destroyed
    pub unsafe fn from_instance(instance: Instance) -> Self {
        profiling::scope!("Global::new");
        Self {
            instance,
            surfaces: Registry::new(),
            hub: Hub::new(),
        }
    }

    pub fn generate_report(&self) -> GlobalReport {
        let mut metal_prepass = MetalPrePassReport::default();
        for (_, device) in self.hub.devices.read().iter() {
            device.metal_prepass.add_to(&mut metal_prepass);
        }
        GlobalReport {
            surfaces: self.surfaces.generate_report(),
            hub: self.hub.generate_report(),
            metal_prepass,
        }
    }
}

impl fmt::Debug for Global {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Global").finish()
    }
}

impl Drop for Global {
    fn drop(&mut self) {
        profiling::scope!("Global::drop");
        resource_log!("Global::drop");
    }
}

#[cfg(send_sync)]
fn _test_send_sync(global: &Global) {
    fn test_internal<T: Send + Sync>(_: T) {}
    test_internal(global)
}
