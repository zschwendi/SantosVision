//! Exact resolved native binding metadata for private target-span capture.
//! This module is absent from ordinary builds; unsupported input is evidence,
//! never permission to change or suppress the original binding.
use super::*;

const MAX_VIEWS: usize = 4096;
const MAX_GROUPS: usize = 4096;
const MAX_ENTRIES: usize = 256;

#[derive(Clone)]
pub(super) enum Resource {
    Buffer {
        buffer: Buffer,
        offset: u64,
        size: u64,
        binding_offset: u64,
        original_dynamic_offset: Option<u32>,
        native_dummy: bool,
    },
    Texture {
        view: TextureView,
        descriptor: TextureViewDescriptor<'static>,
        native_dummy: bool,
    },
    Sampler {
        sampler: Sampler,
        descriptor: SamplerDescriptor<'static>,
        native_dummy: bool,
    },
}
#[derive(Clone)]
pub(super) struct Entry {
    pub layout: BindGroupLayoutEntry,
    pub resource: Resource,
}
#[derive(Clone)]
pub(super) struct CapturedGroup {
    pub entries: Vec<Entry>,
    pub unsupported: Option<String>,
    pub cache_uid: u64,
}
impl CapturedGroup {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
            unsupported: None,
            cache_uid: 0,
        }
    }
    pub(super) fn fail(&mut self, reason: impl Into<String>) {
        self.unsupported.get_or_insert(reason.into());
    }
    pub(super) fn push(&mut self, layout: BindGroupLayoutEntry, resource: Resource) {
        if self.entries.len() >= MAX_ENTRIES {
            self.fail("resolved binding count exceeds capture bound");
        } else {
            self.entries.push(Entry { layout, resource });
        }
    }
    pub(super) fn buffer(
        &mut self,
        layout: BindGroupLayoutEntry,
        buffer: &Buffer,
        binding_offset: u64,
        size: Option<u64>,
        dynamic: Option<u32>,
        native_dummy: bool,
    ) {
        match effective_buffer_range(&layout, buffer.size(), binding_offset, size, dynamic) {
            Ok((offset, size)) => self.push(
                layout,
                Resource::Buffer {
                    buffer: buffer.clone(),
                    offset,
                    size,
                    binding_offset,
                    original_dynamic_offset: dynamic,
                    native_dummy,
                },
            ),
            Err(reason) => self.fail(reason),
        }
    }
}
fn effective_buffer_range(
    layout: &BindGroupLayoutEntry,
    total: u64,
    offset: u64,
    size: Option<u64>,
    dynamic: Option<u32>,
) -> Result<(u64, u64), &'static str> {
    let BindingType::Buffer {
        ty,
        has_dynamic_offset,
        ..
    } = layout.ty
    else {
        return Err("buffer metadata has non-buffer layout");
    };
    if matches!(ty, BufferBindingType::Storage { read_only: false }) {
        return Err("writable buffer binding is unsupported");
    }
    if has_dynamic_offset != dynamic.is_some() {
        return Err("dynamic buffer offset contract mismatch");
    }
    let size = size.unwrap_or_else(|| total.saturating_sub(offset));
    let effective = offset
        .checked_add(dynamic.unwrap_or(0) as u64)
        .ok_or("buffer offset overflow")?;
    if size == 0 || effective.checked_add(size).is_none_or(|end| end > total) {
        return Err("resolved buffer range is outside actual allocation");
    }
    Ok((effective, size))
}
pub(super) fn supported_texture_view(
    allocation: TextureDimension,
    view: Option<TextureViewDimension>,
    samples: u32,
) -> bool {
    if samples != 1 {
        return false;
    }
    matches!(
        (allocation, view),
        (TextureDimension::D1, Some(TextureViewDimension::D1))
            | (TextureDimension::D3, Some(TextureViewDimension::D3))
            | (
                TextureDimension::D2,
                Some(
                    TextureViewDimension::D2
                        | TextureViewDimension::D2Array
                        | TextureViewDimension::Cube
                        | TextureViewDimension::CubeArray
                )
            )
    )
}
fn same_static(a: &CapturedGroup, b: &CapturedGroup) -> bool {
    a.unsupported == b.unsupported
        && a.entries.len() == b.entries.len()
        && a.entries.iter().zip(&b.entries).all(|(a, b)| {
            a.layout == b.layout
                && match (&a.resource, &b.resource) {
                    (
                        Resource::Buffer {
                            buffer: a,
                            binding_offset: ao,
                            size: az,
                            native_dummy: ad,
                            ..
                        },
                        Resource::Buffer {
                            buffer: b,
                            binding_offset: bo,
                            size: bz,
                            native_dummy: bd,
                            ..
                        },
                    ) => a == b && ao == bo && az == bz && ad == bd,
                    (
                        Resource::Texture {
                            view: a,
                            descriptor: av,
                            native_dummy: ad,
                        },
                        Resource::Texture {
                            view: b,
                            descriptor: bv,
                            native_dummy: bd,
                        },
                    ) => a.texture() == b.texture() && av == bv && ad == bd,
                    (
                        Resource::Sampler {
                            sampler: a,
                            descriptor: av,
                            native_dummy: ad,
                        },
                        Resource::Sampler {
                            sampler: b,
                            descriptor: bv,
                            native_dummy: bd,
                        },
                    ) => a == b && av == bv && ad == bd,
                    _ => false,
                }
        })
}
struct Views<K> {
    values: HashMap<K, (TextureViewDescriptor<'static>, bool)>,
    failure: Option<&'static str>,
    limit: usize,
}
impl<K: Eq + std::hash::Hash + Clone> Views<K> {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
            failure: None,
            limit: MAX_VIEWS,
        }
    }
    fn remember(&mut self, key: &K, descriptor: &TextureViewDescriptor<'_>, dummy: bool) {
        if self.failure.is_some() {
            return;
        }
        let descriptor = TextureViewDescriptor {
            label: None,
            ..descriptor.clone()
        };
        if let Some(prior) = self.values.get(key) {
            if prior != &(descriptor, dummy) {
                self.failure = Some("one native texture view has conflicting descriptors");
            }
        } else if self.values.len() >= self.limit {
            self.failure = Some("native texture view metadata bound exceeded");
        } else {
            self.values.insert(key.clone(), (descriptor, dummy));
        }
    }
}
pub(super) struct Registry {
    views: Views<TextureView>,
    transient_views: Views<TextureView>,
    groups: HashMap<u64, Arc<CapturedGroup>>,
    failure: Option<&'static str>,
}
impl Registry {
    fn new() -> Self {
        Self {
            views: Views::new(),
            transient_views: Views {
                values: HashMap::new(),
                failure: None,
                limit: MAX_ENTRIES,
            },
            groups: HashMap::new(),
            failure: None,
        }
    }
    pub(super) fn remember_view(
        &mut self,
        view: &TextureView,
        descriptor: &TextureViewDescriptor<'_>,
        dummy: bool,
        persistent: bool,
    ) {
        if persistent {
            self.views.remember(view, descriptor, dummy);
        } else {
            self.transient_views.remember(view, descriptor, dummy);
        }
    }
    pub(super) fn begin_group(&mut self) {
        self.transient_views.values.clear();
    }
    pub(super) fn texture(
        &self,
        group: &mut CapturedGroup,
        layout: BindGroupLayoutEntry,
        view: &TextureView,
    ) {
        if let Some(reason) = self
            .failure
            .or(self.views.failure)
            .or(self.transient_views.failure)
        {
            group.fail(reason);
            return;
        }
        let Some((descriptor, dummy)) = self
            .views
            .values
            .get(view)
            .or_else(|| self.transient_views.values.get(view))
        else {
            group.fail("actual native texture view descriptor was not retained");
            return;
        };
        let texture = view.texture();
        if !supported_texture_view(
            texture.dimension(),
            descriptor.dimension,
            texture.sample_count(),
        ) {
            group.fail(format!("unsupported sampled native texture: allocation={:?}, view={:?}, format={:?}, view_format={:?}, extent={}x{}x{}, mips={}, samples={}, base_mip={}, mip_count={:?}, base_layer={}, layer_count={:?}, aspect={:?}",
                texture.dimension(),descriptor.dimension,texture.format(),descriptor.format,
                texture.width(),texture.height(),texture.depth_or_array_layers(),texture.mip_level_count(),texture.sample_count(),
                descriptor.base_mip_level,descriptor.mip_level_count,descriptor.base_array_layer,descriptor.array_layer_count,descriptor.aspect));
            return;
        }
        group.push(
            layout,
            Resource::Texture {
                view: view.clone(),
                descriptor: descriptor.clone(),
                native_dummy: *dummy,
            },
        );
    }
    pub(super) fn finish(
        &mut self,
        mut current: CapturedGroup,
        uid: u64,
        cache_hit: bool,
        lifetime_valid: bool,
    ) -> Arc<CapturedGroup> {
        current.cache_uid = uid;
        if !lifetime_valid {
            current.fail("native bind-group lifetime invalidation is disabled");
        }
        if let Some(reason) = self
            .failure
            .or(self.views.failure)
            .or(self.transient_views.failure)
        {
            current.fail(reason);
        }
        if cache_hit {
            match self.groups.get(&uid) {
                Some(prior) if same_static(prior, &current) => {
                    // Proxy lookup may create a fresh equivalent view. Certify
                    // the original view actually retained by the cached group.
                    for (entry, original) in current.entries.iter_mut().zip(&prior.entries) {
                        if matches!(&entry.resource, Resource::Texture { .. }) {
                            entry.resource = original.resource.clone();
                        }
                    }
                }
                Some(_) => current.fail(
                    "cached native bind-group resources differ from current resolved entries",
                ),
                None => current
                    .fail("cached native bind-group has no original resolved resource certificate"),
            }
        } else if self.groups.len() >= MAX_GROUPS {
            self.failure = Some("native group metadata bound exceeded");
            current.fail("native group metadata bound exceeded");
        }
        let result = Arc::new(current);
        if !cache_hit && self.groups.len() < MAX_GROUPS {
            self.groups.insert(uid, result.clone());
        }
        result
    }
    pub(super) fn clear_groups(&mut self) {
        self.groups.clear();
    }
}
impl Gpu {
    pub(super) fn target_span_bindings_active(&self) -> bool {
        self.draw_capture
            .as_ref()
            .is_some_and(draw_capture::Capture::captures_target_span)
    }
    pub(super) fn capture_binding_view(
        &mut self,
        view: &TextureView,
        descriptor: &TextureViewDescriptor<'_>,
        dummy: bool,
        persistent: bool,
    ) {
        if self.target_span_bindings_active() {
            self.capture_bindings
                .get_or_insert_with(Registry::new)
                .remember_view(view, descriptor, dummy, persistent);
        }
    }
    pub(super) fn capture_binding_begin_group(&mut self) {
        self.capture_bindings
            .get_or_insert_with(Registry::new)
            .begin_group();
    }
    pub(super) fn capture_binding_sampler(
        &self,
        group: &mut CapturedGroup,
        layout: BindGroupLayoutEntry,
        id: u32,
        comparison: bool,
        sampler: &Sampler,
    ) {
        if let Some(state) = self
            .state(id)
            .filter(|state| state.sampler.as_ref() == Some(sampler))
        {
            group.push(
                layout,
                Resource::Sampler {
                    sampler: sampler.clone(),
                    descriptor: Self::sampler_descriptor(&state.words),
                    native_dummy: false,
                },
            );
        } else if (if comparison {
            self.dummy_cmp.as_ref()
        } else {
            self.dummy_smp.as_ref()
        }) == Some(sampler)
        {
            let descriptor = SamplerDescriptor {
                compare: comparison.then_some(CompareFunction::Always),
                ..Default::default()
            };
            group.push(
                layout,
                Resource::Sampler {
                    sampler: sampler.clone(),
                    descriptor,
                    native_dummy: true,
                },
            );
        } else {
            group.fail("actual native sampler descriptor unavailable");
        }
    }
    pub(super) fn capture_binding_group(
        &mut self,
        group: CapturedGroup,
        uid: u64,
        hit: bool,
    ) -> Arc<CapturedGroup> {
        self.capture_bindings
            .get_or_insert_with(Registry::new)
            .finish(group, uid, hit, self.bind_group_prepare.enabled)
    }
    pub(super) fn capture_binding_retire_texture(&mut self, id: u32) {
        if self.capture_bindings.is_none() {
            return;
        }
        let Some(texture) = self.tex(id) else {
            return;
        };
        let mut parents = vec![texture.tex.clone()];
        parents.extend(
            [
                &texture.proxy_depth,
                &texture.proxy_stencil,
                &texture.proxy_fdepth,
            ]
            .into_iter()
            .flatten()
            .map(|p| p.tex.clone()),
        );
        if let Some(registry) = &mut self.capture_bindings {
            registry
                .views
                .values
                .retain(|view, _| !parents.contains(view.texture()));
            registry
                .transient_views
                .values
                .retain(|view, _| !parents.contains(view.texture()));
        }
    }
    pub(super) fn capture_binding_cache_clear(&mut self) {
        if let Some(r) = &mut self.capture_bindings {
            r.clear_groups();
        }
    }
    pub(super) fn capture_binding_texture(
        &self,
        group: &mut CapturedGroup,
        layout: BindGroupLayoutEntry,
        view: &TextureView,
    ) {
        if let Some(registry) = &self.capture_bindings {
            registry.texture(group, layout, view);
        } else {
            group.fail("native texture descriptor registry unavailable");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn uniform(dynamic: bool) -> BindGroupLayoutEntry {
        BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::VERTEX,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: None,
            },
            count: None,
        }
    }
    #[test]
    fn texture_dimension_contract_admits_real_1d_3d_and_preserves_rejections() {
        for (allocation, view) in [
            (TextureDimension::D1, TextureViewDimension::D1),
            (TextureDimension::D3, TextureViewDimension::D3),
            (TextureDimension::D2, TextureViewDimension::D2),
            (TextureDimension::D2, TextureViewDimension::D2Array),
            (TextureDimension::D2, TextureViewDimension::Cube),
            (TextureDimension::D2, TextureViewDimension::CubeArray),
        ] {
            assert!(supported_texture_view(allocation, Some(view), 1));
            assert!(!supported_texture_view(allocation, Some(view), 4));
        }
        assert!(!supported_texture_view(
            TextureDimension::D3,
            Some(TextureViewDimension::D2Array),
            1
        ));
        assert!(!supported_texture_view(
            TextureDimension::D1,
            Some(TextureViewDimension::D2),
            1
        ));
        assert!(!supported_texture_view(
            TextureDimension::D2,
            Some(TextureViewDimension::D3),
            1
        ));
        assert!(!supported_texture_view(TextureDimension::D3, None, 1));
    }
    #[test]
    fn native_1d_and_3d_view_descriptors_are_retained_without_array_rewriting() {
        let mut registry = Views::<u64>::new();
        for (key, dimension) in [(1, TextureViewDimension::D1), (2, TextureViewDimension::D3)] {
            let descriptor = TextureViewDescriptor {
                dimension: Some(dimension),
                base_mip_level: 2,
                mip_level_count: Some(2),
                ..Default::default()
            };
            registry.remember(&key, &descriptor, false);
            assert_eq!(registry.values[&key].0, descriptor);
            assert_eq!(registry.values[&key].0.array_layer_count, None);
        }
    }
    #[test]
    fn dynamic_range_is_exact_and_preserves_selected_size() {
        assert_eq!(
            effective_buffer_range(&uniform(true), 1024, 0, Some(64), Some(256)),
            Ok((256, 64))
        );
        assert_eq!(
            effective_buffer_range(&uniform(false), 1024, 256, None, None),
            Ok((256, 768))
        );
        assert!(effective_buffer_range(&uniform(true), 1024, 0, None, Some(256)).is_err());
        assert!(effective_buffer_range(&uniform(false), 1024, 0, Some(64), Some(0)).is_err());
        assert!(
            effective_buffer_range(&uniform(true), u64::MAX, u64::MAX, Some(4), Some(1)).is_err()
        );
    }
    #[test]
    fn writable_binding_is_explicitly_unsupported() {
        let mut layout = uniform(false);
        layout.ty = BindingType::Buffer {
            ty: BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        assert!(effective_buffer_range(&layout, 1024, 0, Some(64), None).is_err());
        layout.ty = BindingType::Buffer {
            ty: BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        assert_eq!(
            effective_buffer_range(&layout, 1024, 256, Some(128), None),
            Ok((256, 128))
        );
    }
    #[test]
    fn view_identity_preserves_format_ranges_and_rejects_conflict() {
        let mut r = Views::<u64>::new();
        let d = TextureViewDescriptor {
            format: Some(TextureFormat::Rgba8UnormSrgb),
            dimension: Some(TextureViewDimension::D2Array),
            base_mip_level: 2,
            mip_level_count: Some(3),
            base_array_layer: 4,
            array_layer_count: Some(2),
            ..Default::default()
        };
        r.remember(&1, &d, false);
        r.remember(&1, &d, false);
        assert_eq!(r.values.len(), 1);
        assert_eq!(r.values[&1].0, d);
        let mut changed = d.clone();
        changed.format = Some(TextureFormat::Rgba8Unorm);
        r.remember(&1, &changed, false);
        assert!(r.failure.is_some());
    }
    #[test]
    fn view_capacity_is_explicit_not_silent_eviction() {
        let mut r = Views::<u64>::new();
        r.limit = 1;
        let d = TextureViewDescriptor::default();
        r.remember(&1, &d, true);
        r.remember(&2, &d, true);
        assert_eq!(r.values.len(), 1);
        assert_eq!(
            r.failure,
            Some("native texture view metadata bound exceeded")
        );
    }
}
