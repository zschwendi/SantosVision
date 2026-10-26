//! Default-off prototype: on qualified warm bindings, perform the original
//! resource effects and key lookup before cloning native binding resources.
//! Unqualified/proxied/dummy/counter/storage-texture bindings use the unchanged
//! original walk. Native views must already exist and lifetime invalidation in
//! Gpu::set_obj/destroy remains authoritative; no resource generation is hidden.
use super::*;

fn ordinary_view_key(dimension: TextureViewDimension) -> &'static str {
    match dimension {
        TextureViewDimension::D1 => "D1",
        TextureViewDimension::D2 => "D2",
        TextureViewDimension::D2Array => "D2Array",
        TextureViewDimension::Cube => "Cube",
        TextureViewDimension::CubeArray => "CubeArray",
        TextureViewDimension::D3 => "D3",
    }
}

#[derive(Clone, Copy)]
enum Plan {
    Constant,
    Uniform {
        slot: usize,
        chunk: usize,
        size: u64,
    },
    Sampler {
        id: u32,
    },
    View {
        id: u32,
        key: &'static str,
    },
    Buffer {
        id: u32,
        offset: u64,
        size: Option<u64>,
    },
}
enum Owned {
    Buffer(Buffer, u64, Option<u64>),
    View(TextureView),
    Sampler(Sampler),
}

impl Gpu {
    /// Pure borrowed eligibility: never packs, clones a GPU handle, emits a
    /// clear, marks resource usage or partially consumes the reference path.
    pub(super) fn key_first_bindings_eligible(
        &self,
        stage: usize,
        prog: &Program,
        bindings: &[Binding],
    ) -> bool {
        bindings.iter().all(|binding|match binding.kind {
            Kind::Uniform=>binding.binding!=15 || prog.consts.is_some(),
            Kind::Sampler|Kind::SamplerCmp=>{
                let id=*self.s.smp[stage].get((binding.binding as usize).wrapping_sub(16)).unwrap_or(&0);
                matches!(self.obj(id),Some(Obj::State(state)) if state.sampler.is_some()
                    && ((state.words.first().copied().unwrap_or(0)&0x80)!=0)==(binding.kind==Kind::SamplerCmp))
            }
            Kind::Texture=>{
                let id=*self.s.srv[stage].get((binding.binding as usize).wrapping_sub(32)).unwrap_or(&0);
                let Some(view)=self.view_obj(id).filter(|view|!view.is_buf) else{return false;};
                let Some(texture)=self.tex(view.res) else{return false;};
                // Exclude exactly the two proxy branches of srv_view: they
                // can refresh GPU contents even when a bind group hits.
                if is_depth(texture.format)!=(binding.sample==Sample::Depth) {return false;}
                view.views.contains_key(ordinary_view_key(binding.dim))
            }
            Kind::ReadOnlyStorage|Kind::Storage=>{
                if binding.binding>=176{return false;}
                let id=if binding.binding>=160 {
                    *self.s.uav[stage].get((binding.binding-160)as usize).unwrap_or(&0)
                } else {*self.s.srv[stage].get((binding.binding as usize).wrapping_sub(32)).unwrap_or(&0)};
                let Some(view)=self.view_obj(id).filter(|view|view.is_buf) else{return false;};
                let Some(buffer)=self.buf(view.res).filter(|buffer|buffer.gpu.is_some()) else{return false;};
                let stride=if buffer.stride==0 {4}else{buffer.stride}as u64;
                let offset=view.a as u64*stride;
                // Preserve reference warnings and malformed offset behavior.
                offset==0 || (offset%256==0 && offset<=buffer.bytes)
            }
            Kind::StorageTexture=>false,
        })
    }

    pub(super) fn stage_bind_group_key_first(
        &mut self,
        stage: usize,
        prog: &Program,
        bindings: &[Binding],
        bgl: &(BindGroupLayout, u64),
    ) -> Option<BgResult> {
        let bindings = self.render_prepare.bindings(bindings);
        let mut plans = EntryScratch::<(u32, Plan)>::new(true);
        let mut key = KeyScratch::new(true, &[bgl.1, prog.uid, stage as u64]);
        let mut dyn_off = None;
        let mut mix = [0u64; 4];
        for binding in bindings.as_ref() {
            match binding.kind {
                Kind::Uniform => {
                    mix[0] += 1;
                    if binding.binding == 15 {
                        plans.push((15, Plan::Constant));
                        continue;
                    }
                    // The fresh snapshot/dynamic offset is prepared exactly
                    // once at its original point, including on cache hits.
                    let (chunk, offset) = self.pack_copy(prog, stage)?;
                    let c = &self.ubo_slots[self.ubo_slot][chunk];
                    plans.push((
                        0,
                        Plan::Uniform {
                            slot: self.ubo_slot,
                            chunk,
                            size: prog.pack.as_ref().unwrap().size,
                        },
                    ));
                    dyn_off = Some((c.base + offset) as u32);
                    key.push(c.id);
                }
                Kind::Sampler | Kind::SamplerCmp => {
                    mix[1] += 1;
                    let id = *self.s.smp[stage]
                        .get((binding.binding as usize).wrapping_sub(16))
                        .unwrap_or(&0);
                    key.push(id as u64);
                    plans.push((binding.binding, Plan::Sampler { id }));
                }
                Kind::Texture => {
                    mix[2] += 1;
                    let id = *self.s.srv[stage]
                        .get((binding.binding as usize).wrapping_sub(32))
                        .unwrap_or(&0);
                    let resource = self.view_obj(id)?.res;
                    if self.pending_clears.contains_key(&resource) {
                        self.materialize_clears(resource);
                    }
                    self.use_tex(resource);
                    self.tex_mut(resource)?.in_bg = true;
                    key.push(id as u64);
                    key.push(0);
                    plans.push((
                        binding.binding,
                        Plan::View {
                            id,
                            key: ordinary_view_key(binding.dim),
                        },
                    ));
                }
                Kind::ReadOnlyStorage | Kind::Storage => {
                    mix[3] += 1;
                    let id = if binding.binding >= 160 {
                        *self.s.uav[stage]
                            .get((binding.binding - 160) as usize)
                            .unwrap_or(&0)
                    } else {
                        *self.s.srv[stage]
                            .get((binding.binding as usize).wrapping_sub(32))
                            .unwrap_or(&0)
                    };
                    let (resource, first, num) = {
                        let view = self.view_obj(id)?;
                        (view.res, view.a, view.b)
                    };
                    let buffer = self.buf(resource)?;
                    let stride = if buffer.stride == 0 { 4 } else { buffer.stride } as u64;
                    let offset = first as u64 * stride;
                    let size = if offset != 0 && num != 0 && num != u32::MAX {
                        Some((num as u64 * stride).min(buffer.bytes - offset))
                    } else {
                        None
                    };
                    self.use_buf(resource);
                    self.buf_mut(resource)?.in_bg = true;
                    if binding.kind == Kind::Storage {
                        self.op47_dedup_gpu_write(
                            resource,
                            crate::upload_dedup::GpuWrite::StorageBinding,
                        );
                    }
                    key.push(id as u64);
                    key.push(offset);
                    plans.push((
                        binding.binding,
                        Plan::Buffer {
                            id: resource,
                            offset,
                            size,
                        },
                    ));
                }
                Kind::StorageTexture => unreachable!("preflight excludes storage textures"),
            }
        }
        let cached = self.bind_groups.get(key.as_slice());
        self.render_prepare.bind_group_lookup(cached.is_some());
        self.bind_group_prepare
            .lookup(cached.is_some(), key.spilled(), plans.spilled());
        self.bind_group_prepare
            .key_first_lookup(cached.is_some(), plans.len(), mix);
        if let Some((group, uid)) = cached {
            return Some(BgResult {
                bg: group.clone(),
                uid: *uid,
                dyn_off,
            });
        }
        self.bind_group_prepare.materialized_miss(plans.len());
        // No guest callback, object replacement or buffer upload can run in
        // this exclusive Gpu call. The eligible native handles remain stable
        // across the resource-effect walk; clone them only for an actual miss.
        let entries: Vec<(u32, Owned)> = plans
            .into_vec()
            .into_iter()
            .map(|(binding, plan)| {
                let resource = match plan {
                    Plan::Constant => Owned::Buffer(prog.consts.as_ref()?.clone(), 0, None),
                    Plan::Uniform { slot, chunk, size } => {
                        Owned::Buffer(self.ubo_slots[slot][chunk].buf.clone(), 0, Some(size))
                    }
                    Plan::Sampler { id } => match self.obj(id)? {
                        Obj::State(state) => Owned::Sampler(state.sampler.as_ref()?.clone()),
                        _ => return None,
                    },
                    Plan::View { id, key } => {
                        Owned::View(self.view_obj(id)?.views.get(key)?.clone())
                    }
                    Plan::Buffer { id, offset, size } => {
                        Owned::Buffer(self.buf(id)?.gpu.as_ref()?.clone(), offset, size)
                    }
                };
                Some((binding, resource))
            })
            .collect::<Option<Vec<_>>>()?;
        let entries: Vec<BindGroupEntry> = entries
            .iter()
            .map(|(binding, resource)| BindGroupEntry {
                binding: *binding,
                resource: match resource {
                    Owned::Buffer(buffer, offset, size) => BindingResource::Buffer(BufferBinding {
                        buffer,
                        offset: *offset,
                        size: size.and_then(std::num::NonZeroU64::new),
                    }),
                    Owned::View(view) => BindingResource::TextureView(view),
                    Owned::Sampler(sampler) => BindingResource::Sampler(sampler),
                },
            })
            .collect();
        let group = self.scoped(&format!("bind group {}", prog.label), |device| {
            device.create_bind_group(&BindGroupDescriptor {
                label: None,
                layout: &bgl.0,
                entries: &entries,
            })
        })?;
        if self.bind_groups.len() > BG_LIMIT * 3 / 2 {
            self.bind_groups.clear();
            self.bind_group_prepare.invalidated();
        }
        let uid = self.uid();
        self.bind_groups
            .insert(key.into_vec(), (group.clone(), uid));
        Some(BgResult {
            bg: group,
            uid,
            dyn_off,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    mod metal {
        include!("bind_group_key_first_metal_tests.rs");
    }
    #[test]
    fn static_srv_key_is_exactly_the_reference_debug_key_for_every_dimension() {
        for dim in [
            TextureViewDimension::D1,
            TextureViewDimension::D2,
            TextureViewDimension::D2Array,
            TextureViewDimension::Cube,
            TextureViewDimension::CubeArray,
            TextureViewDimension::D3,
        ] {
            assert_eq!(ordinary_view_key(dim), format!("{dim:?}"));
        }
    }

    #[cfg(target_os = "macos")]
    fn fixture(textures: usize) -> (Gpu, Program, (BindGroupLayout, u64)) {
        let mut config = wasmtime::Config::new();
        config.wasm_threads(true);
        config.shared_memory(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let memory = SharedMemory::new(&engine, wasmtime::MemoryType::shared(4, 4)).unwrap();
        let mut gpu = Gpu::new(memory, 0, 65536, 0, None).unwrap();
        gpu.bind_group_prepare = BindGroupPrepare::new(true);
        gpu.render_prepare.enabled = true;
        gpu.uniform_pack.enabled = true;
        gpu.uniform_snapshot = UniformSnapshot::new(true);
        gpu.create_buffer(1, 16, 4, 0, 0);
        gpu.s.cb[2][0] = Cb { buf: 1, first: 0 };
        gpu.create_buffer(2, 64, BIND_UAV, 0, 4);
        gpu.set_obj(
            3,
            Obj::Uav(ViewObj {
                res: 2,
                is_buf: true,
                fmt: 28,
                vdim: 4,
                a: 0,
                b: 16,
                c: 0,
                d: 0,
                views: HashMap::new(),
            }),
        );
        gpu.s.uav[2][0] = 3;
        let mut text="@group(0) @binding(0) var<uniform> c: vec4u;\n@group(0) @binding(160) var<storage,read_write> out: array<u32>;\n".to_owned();
        if textures != 0 {
            gpu.create_texture(10, 2, 1, 1, 1, 1, 1, 28, BIND_SRV | BIND_RT, 1);
            gpu.set_obj(
                11,
                Obj::Srv(ViewObj {
                    res: 10,
                    is_buf: false,
                    fmt: 28,
                    vdim: 4,
                    a: 0,
                    b: 1,
                    c: 0,
                    d: 1,
                    views: HashMap::new(),
                }),
            );
            let sampler = gpu.device.create_sampler(&SamplerDescriptor::default());
            gpu.set_obj(
                12,
                Obj::State(Rc::new(StateObj {
                    words: vec![0],
                    sampler: Some(sampler),
                })),
            );
            gpu.s.smp[2][0] = 12;
            text.push_str("@group(0) @binding(16) var sample0: sampler;\n");
            for index in 0..textures {
                gpu.s.srv[2][index] = 11;
                text.push_str(&format!(
                    "@group(0) @binding({}) var image{index}: texture_2d<f32>;\n",
                    32 + index
                ));
            }
        }
        text.push_str("@compute @workgroup_size(1) fn main() { var value=0.0;\n");
        for index in 0..textures {
            text.push_str(&format!(
                "value+=textureSampleLevel(image{index},sample0,vec2f(0.5),0.0).x;\n"
            ));
        }
        text.push_str("out[c.y]=c.x+u32(value*255.0); }");
        let module = gpu.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("key-first actual-site fixture"),
            source: ShaderSource::Wgsl(text.as_str().into()),
        });
        let info = parse_wgsl(&text);
        let group_bindings = BTreeMap::from([(0, info.bindings.clone())]);
        let program = Program {
            hud_kind: None,
            module,
            info,
            consts: None,
            pack: Some(Pack {
                members: vec![PackMember {
                    binding: 0,
                    size: 16,
                    offset: 0,
                }],
                size: 16,
            }),
            text,
            flat: vec![],
            group_bindings,
            variants: RefCell::new(HashMap::new()),
            label: "key-first actual-site fixture".into(),
            uid: gpu.uid(),
            vid: false,
            iid: false,
            out_locs: vec![],
            in_locs: vec![],
            clip: false,
        };
        let layout = gpu.bgl_for(&program, 2, 0);
        (gpu, program, layout)
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "explicit actual-code CPU-only bind preparation benchmark; creates Metal objects but submits no commands"]
    fn actual_site_cpu_bind_key_first_benchmark() {
        fn trial(textures: usize, changed: bool) -> serde_json::Value {
            let (mut old, op, ol) = fixture(textures);
            let (mut new, np, nl) = fixture(textures);
            new.bind_group_prepare.key_first = true;
            for (gpu, p, l) in [(&mut old, &op, &ol), (&mut new, &np, &nl)] {
                for _ in 0..64 {
                    std::hint::black_box(gpu.stage_bind_group(2, p, 0, l).unwrap());
                }
                gpu.ubo_slots[0][0].stage.resize(RING_CHUNK as usize, 0);
            }
            let mut records = Vec::new();
            for round in 0..9 {
                let order = if round % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for key_first in order {
                    let (gpu, p, l) = if key_first {
                        (&mut new, &np, &nl)
                    } else {
                        (&mut old, &op, &ol)
                    };
                    // No submission can consume these synthetic CPU-only
                    // snapshots. Rewind the bounded ring between batches to
                    // exclude new native allocation/staging growth from timing.
                    gpu.ubo_off = 0;
                    gpu.ubo_chunk = 0;
                    gpu.uniform_snapshot = UniformSnapshot::new(true);
                    let first = gpu.stage_bind_group(2, p, 0, l).unwrap();
                    let started = Instant::now();
                    for index in 0..5000u32 {
                        if changed {
                            let buffer = gpu.buf_mut(1).unwrap();
                            buffer.version += 1;
                            buffer.shadow.as_mut().unwrap()[..4]
                                .copy_from_slice(&index.to_le_bytes());
                        }
                        let result = gpu.stage_bind_group(2, p, 0, l).unwrap();
                        assert_eq!(result.uid, first.uid);
                        std::hint::black_box(result);
                    }
                    records.push(serde_json::json!({"round":round,"key_first":key_first,"calls":5000,"ns_per_call":started.elapsed().as_nanos()as f64/5000.}));
                    assert_eq!(gpu.submits, 0);
                    assert!(gpu.enc.is_none());
                }
            }
            let median = |mode| {
                let mut values: Vec<_> = records
                    .iter()
                    .filter(|r| r["key_first"] == mode)
                    .map(|r| r["ns_per_call"].as_f64().unwrap())
                    .collect();
                values.sort_by(f64::total_cmp);
                values[values.len() / 2]
            };
            serde_json::json!({"textures":textures,"uniform_changed_every_call":changed,
                "reference_median_ns":median(false),"key_first_median_ns":median(true),
                "median_saved_ns":median(false)-median(true),"reference":old.bind_group_prepare.metrics(),
                "key_first":new.bind_group_prepare.metrics(),"raw_trials":records,
                "actual_stage_bind_group_calls":true,"gpu_submissions":0,"scope":"CPU actual production call, real Metal handles, synthetic warm bindings; no game/frame/FPS proof"})
        }
        let results = [trial(0, false), trial(8, false), trial(8, true)];
        println!(
            "KEY_FIRST_CPU_BENCH={}",
            serde_json::json!({"schema":1,"cases":results})
        );
    }
}
