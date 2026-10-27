//! Private, feature-gated native draw-span capture. This module never submits,
//! renders an alternate frame, changes a camera, or publishes a WorldFrame.
use super::*;
use serde_json::{json, Value};
use std::io::Write;

const VS: &str = "6a6a0898a51cf403";
const PS: &str = "75cf2d53bf9497c9";
const VS_TEXT: &str = "04f353256b2a27c7141076f68c572d451f551a0b8370bafd6f521fc8dc90cb94";
const PS_TEXT: &str = "1e31e158d1f81369d39401dabee56c4b988eec1110f81b21f5b99ad3c8e6c5af";
const MAX_BYTES: u64 = 256 << 20;
const MAX_PIPELINES: usize = 4096;
const MAX_PIPELINE_BYTES: usize = 8 << 20;

pub(super) struct Resolved<'a> {
    pub kind: u32, pub count: u32, pub instances: u32, pub start: u32,
    pub base_vertex: i32, pub start_instance: u32, pub unclamped: bool,
    pub vs: &'a Program, pub ps: Option<&'a Program>, pub pipeline: &'a Pl,
    pub vertices: &'a [(u32, Buffer, u64, u64)],
    pub index: Option<&'a (Buffer, IndexFormat, u64, u32)>,
    pub groups: [Option<&'a BgResult>; 2], pub viewport: [f32; 6],
    pub scissor: [u32; 4], pub blend: [f64; 4], pub stencil: u32,
}

enum Source {
    Buffer { buffer: Buffer, offset: u64, size: u64 },
    Texture { texture: Texture, mip: u32, aspect: TextureAspect, width: u32, height: u32 },
}
struct Plan { name: String, source: Source, row: u32, rows: u32, pitch: u32, bytes: u64 }
struct Readback { plan: Plan, buffer: Buffer }
struct Prepared { draw: Value, view: Value, domain: Value, targets: Vec<(Texture, TextureAspect, String)>, plans: Vec<Plan> }
struct Pending {
    epoch: u64, errors: u64, view: Value, domain: Value, draws: Vec<Value>,
    attachments: Vec<Value>, targets: Vec<(Texture, TextureAspect, String)>,
    readbacks: Vec<Readback>, bytes: u64, final_bytes: u64, finished: bool,
    intervening_commands: Vec<Value>,
    candidate_key: Vec<u64>,
}

// One retained native parent ties the demonstrated PS t5 null transition to a
// later resolved draw. This diagnostic is compiled only with private capture.
struct ColorNullBinding {
    sequence:u64, opcode:u32, view:u32, resource:u32, parent:Texture,
    raw:[u32;6], mips:[u32;2], layers:[u32;2],
}

pub(super) struct Capture {
    directory: PathBuf, pipelines: HashMap<Vec<i64>, Value>, pending: Option<Pending>,
    done: bool, attempts: u32, sequence: u64, epoch_bytes: u64,
    rejected: BTreeMap<String, u64>, pipeline_bytes: usize,
    rejected_starts: std::collections::HashSet<Vec<u64>>,
    live_bytes: Arc<AtomicU64>,
    feedback_recorded: bool,
    color_null_binding: RefCell<Option<ColorNullBinding>>,
    color_null_recorded: bool,
}

fn sha(bytes: &[u8]) -> String {
    extern "C" { fn CC_SHA256(p: *const std::ffi::c_void, n: u32, out: *mut u8) -> *mut u8; }
    assert!(bytes.len() <= u32::MAX as usize);
    let mut result = [0u8; 32];
    unsafe { CC_SHA256(bytes.as_ptr().cast(), bytes.len() as u32, result.as_mut_ptr()); }
    result.iter().map(|b| format!("{b:02x}")).collect()
}
// Layout from the macOS SDK CommonCrypto/CommonDigest.h. Streaming mapped rows
// avoids allocating a second full attachment while its staging buffer is live.
#[repr(C)]
struct ShaContext { count:[u32;2], hash:[u32;8], wbuf:[u32;16] }
struct ShaRows(ShaContext);
impl ShaRows {
    fn new()->Self {
        extern "C" {fn CC_SHA256_Init(c:*mut ShaContext)->i32;}
        let mut value=Self(ShaContext{count:[0;2],hash:[0;8],wbuf:[0;16]});
        unsafe {CC_SHA256_Init(&mut value.0);}value
    }
    fn update(&mut self,bytes:&[u8]) {
        extern "C" {fn CC_SHA256_Update(c:*mut ShaContext,p:*const std::ffi::c_void,n:u32)->i32;}
        assert!(bytes.len()<=u32::MAX as usize);unsafe {CC_SHA256_Update(&mut self.0,bytes.as_ptr().cast(),bytes.len()as u32);}
    }
    fn finish(mut self)->String {
        extern "C" {fn CC_SHA256_Final(out:*mut u8,c:*mut ShaContext)->i32;}
        let mut out=[0u8;32];unsafe {CC_SHA256_Final(out.as_mut_ptr(),&mut self.0);}
        out.iter().map(|b|format!("{b:02x}")).collect()
    }
}
fn add(a: u64, b: u64) -> Result<u64, String> {
    a.checked_add(b).filter(|n| *n <= MAX_BYTES).ok_or_else(|| "256 MiB capture bound exceeded".into())
}
fn reservation_fits(live: u64, added: u64, final_reserved: u64) -> bool {
    add(live,added).and_then(|n|add(n,final_reserved)).is_ok()
}
fn sampled_ranges(desc:TexDesc,view:&ViewObj,dimension:TextureViewDimension)->([u32;2],[u32;2]) {
    let count=if view.b==0 || view.b==u32::MAX {desc.mips.saturating_sub(view.a)}else{view.b};
    let first=view.a.min(desc.mips-1);let count=count.min(desc.mips.saturating_sub(view.a)).max(1);
    let layers=if matches!(dimension,TextureViewDimension::D2|TextureViewDimension::D2Array|TextureViewDimension::Cube|TextureViewDimension::CubeArray) {
        let count=match dimension {TextureViewDimension::Cube=>6,TextureViewDimension::D2=>1,_=>if view.d!=0{view.d}else{desc.d.saturating_sub(view.c)}};
        let first=view.c.min(desc.d-1);[first,first+count.min(desc.d.saturating_sub(view.c)).max(1)]
    }else{[0,1]};
    ([first,first+count],layers)
}
fn contains_subresource(mips:[u32;2],layers:[u32;2],mip:u32,layer:u32)->bool {
    mip>=mips[0] && mip<mips[1] && layer>=layers[0] && layer<layers[1]
}
fn buffer_plan(name: String, buffer: &Buffer, offset: u64, size: u64) -> Result<Plan, String> {
    if size == 0 || size % 4 != 0 || offset % 4 != 0 || size > MAX_BYTES
        || !offset.checked_add(size).is_some_and(|n| n <= buffer.size())
        || !buffer.usage().contains(BufferUsages::COPY_SRC) { return Err("uncopyable or invalid buffer range".into()); }
    Ok(Plan { name, source: Source::Buffer { buffer: buffer.clone(), offset, size }, row: size as u32,
        rows: 1, pitch: size as u32, bytes: size })
}
fn texture_plan(name: String, texture: &Texture, mip: u32, aspect: TextureAspect) -> Result<Plan, String> {
    if texture.dimension() != TextureDimension::D2 || texture.depth_or_array_layers() != 1
        || texture.sample_count() != 1 || mip >= texture.mip_level_count()
        || !texture.usage().contains(TextureUsages::COPY_SRC) { return Err("uncopyable texture shape or usage".into()); }
    let (width, height) = ((texture.width() >> mip).max(1), (texture.height() >> mip).max(1));
    let (bw, bh) = texture.format().block_dimensions();
    let block = texture.format().block_copy_size(Some(aspect)).ok_or("uncopyable texture aspect")?;
    let row = width.div_ceil(bw).checked_mul(block).ok_or("texture row overflow")?;
    let rows = height.div_ceil(bh);
    let pitch = row.checked_add(255).ok_or("texture pitch overflow")? & !255;
    let bytes = (pitch as u64).checked_mul(rows as u64).ok_or("texture bytes overflow")?;
    if bytes == 0 || bytes > MAX_BYTES { return Err("texture exceeds capture bound".into()); }
    // wgpu copies compressed mip tails in whole physical blocks, including
    // logical 2x2/1x1 mip levels. Export metadata keeps the logical extent.
    let copy_width=width.div_ceil(bw).checked_mul(bw).ok_or("physical mip width overflow")?;
    let copy_height=height.div_ceil(bh).checked_mul(bh).ok_or("physical mip height overflow")?;
    Ok(Plan { name, source: Source::Texture { texture: texture.clone(), mip, aspect,
        width:copy_width, height:copy_height }, row, rows, pitch, bytes })
}
fn plan_bytes(plans: &[Plan]) -> Result<u64, String> { plans.iter().try_fold(0, |n, p| add(n, p.bytes)) }
fn enqueue(gpu: &mut Gpu, plans: Vec<Plan>) -> Vec<Readback> {
    plans.into_iter().map(|plan| {
        let buffer = gpu.device.create_buffer(&BufferDescriptor { label: Some("private native draw capture"),
            size: plan.bytes, usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        match &plan.source {
            Source::Buffer { buffer: source, offset, size } => gpu.encoder().copy_buffer_to_buffer(source, *offset, &buffer, 0, *size),
            Source::Texture { texture, mip, aspect, width, height } => gpu.encoder().copy_texture_to_buffer(
                TexelCopyTextureInfo { texture, mip_level: *mip, origin: Origin3d::ZERO, aspect: *aspect },
                TexelCopyBufferInfo { buffer: &buffer, layout: TexelCopyBufferLayout { offset: 0,
                    bytes_per_row: Some(plan.pitch), rows_per_image: Some(plan.rows) } },
                Extent3d { width: *width, height: *height, depth_or_array_layers: 1 }),
        }
        Readback { plan, buffer }
    }).collect()
}

impl Capture {
    pub(super) fn color_srv_binding(&self,gpu:&Gpu,opcode:u32,stage:usize,slot:usize,input:u32,resolved:u32) {
        if self.color_null_recorded || stage!=1 || slot!=5 {return;}
        // An explicit later bind (including guest NULL) supersedes the cause.
        // OM unbinding visits only non-null slots, so persistent NULL retains
        // its original cause until the guest explicitly binds this slot again.
        let mut prior=self.color_null_binding.borrow_mut();
        *prior=None;
        if input==0 || resolved!=0 {return;}
        let Some(view)=gpu.view_obj(input).filter(|v|!v.is_buf && v.vdim==4) else{return;};
        let Some(source)=gpu.tex(view.res) else{return;};
        if source.desc.dim!=TextureDimension::D2 || source.desc.mips==0 || source.desc.d==0 {return;}
        let (mips,layers)=sampled_ranges(source.desc,view,TextureViewDimension::D2);
        *prior=Some(ColorNullBinding{sequence:self.sequence,opcode,view:input,resource:view.res,parent:source.tex.clone(),
            raw:[view.fmt,view.vdim,view.a,view.b,view.c,view.d],mips,layers});
    }
    fn record_color_null_draw(&mut self,gpu:&Gpu,d:&Resolved<'_>) {
        if self.color_null_recorded || !gpu.bind_group_prepare.enabled || gpu.s.srv[1][5]!=0 {return;}
        let Some(Obj::Shader(shader))=gpu.obj(gpu.s.shader[1]) else{return;};
        if shader.hash!="7f531ca66548aefe" {return;}
        let Some(ps)=d.ps else{return;};
        let Some(bindings)=ps.group_bindings.get(&1) else{return;};
        // Real texture key entries also end in zero, so a trailing zero alone
        // cannot certify a dummy. Pin this shader's complete binding/key shape:
        // prefix3 + packed uniform1 + samplers2 + real t2 pair2 + null t5 word1.
        let contract=[(0,Kind::Uniform),(18,Kind::Sampler),(21,Kind::Sampler),(34,Kind::Texture),(37,Kind::Texture)];
        if bindings.len()!=contract.len() || !bindings.iter().zip(contract).all(|(b,(number,kind))|b.binding==number && b.kind==kind) {return;}
        let binding=&bindings[4];
        if binding.dim!=TextureViewDimension::D2 {return;}
        let Some(group)=d.groups[1] else{return;};
        let Some((key,_))=gpu.bind_groups.iter().find(|(_,(_,uid))|*uid==group.uid) else{return;};
        if key.len()!=9 || key[1]!=ps.uid || key[2]!=1 || gpu.s.srv[1][2]==0
            || key[6]!=gpu.s.srv[1][2] as u64 || key[7]!=0 || key[8]!=0 {return;}
        let Some(pass)=gpu.pass_info.as_ref() else{return;};
        let prior=self.color_null_binding.borrow();
        let Some(cause)=prior.as_ref() else{return;};
        let dummy_key=format!("{:?}|{:?}|{}",binding.dim,binding.sample,binding.ms);
        let Some(dummy)=gpu.dummy_tex.get(&dummy_key) else{return;};
        if dummy.texture()==&cause.parent {return;}
        let mut targets=Vec::new();
        for (slot,target) in gpu.s.rt.iter().enumerate() {
            let Some(format)=pass.rt_formats.get(slot).copied().flatten() else{continue;};
            let Some(texture)=gpu.tex(target.tex) else{continue;};
            let Some(attached)=texture.views.get(&(format,target.mip,target.slice)) else{continue;};
            if attached.texture()!=&cause.parent || !contains_subresource(cause.mips,cause.layers,target.mip,target.slice) {continue;}
            targets.push(json!({"slot":slot,"resource":target.tex,"format":format,"mip":target.mip,"layer":target.slice,
                "extent":[texture.desc.w,texture.desc.h,texture.desc.d],"native_parent_matches_retained_null_cause":true}));
        }
        if targets.is_empty(){return;}
        let value=json!({"schema":1,"kind":"first_corrected_color_null_draw","command_sequence":self.sequence,
            "errors_before":gpu.errors.load(Ordering::SeqCst),"presents":gpu.presents,"draws_before":gpu.draws,
            "null_cause":{"command_sequence":cause.sequence,"opcode":cause.opcode,"stage":1,"slot":5,
                "input_view":cause.view,"input_resource":cause.resource,"resolved_view":0,"view_raw":cause.raw,
                "mips":cause.mips,"layers":cause.layers,"recorded_after_actual_production_binding_mutation":true},
            "pixel_shader":{"guest_id":gpu.s.shader[1],"guest_hash":shader.hash,"label":ps.label,"wgsl_sha256":sha(ps.text.as_bytes())},
            "resolved_binding":{"group_uid":group.uid,"group_key":key,"binding":37,"slot_state":0,
                "binding_contract":[0,18,21,34,37],"exact_key_words":9,"texture2_key_words":[key[6],key[7]],
                "texture5_key_words":[key[8]],"dummy_parent_differs_from_output":true},
            "targets":targets,"pipeline_uid":d.pipeline.uid,
            "draw":{"kind":d.kind,"count":d.count,"instances":d.instances,"start":d.start,"base_vertex":d.base_vertex},
            "native_commands_changed_by_diagnostic":false,
            "scope":"exact previously failing PS_BlitDepthToAlpha native 2D t5 path; resolved before pass binding, GPU success is separately checked"});
        drop(prior);
        self.color_null_recorded=true;
        self.color_null_binding.borrow_mut().take();
        if let Ok(bytes)=serde_json::to_vec_pretty(&value) {if bytes.len()<=16*1024 {if let Some(parent)=self.directory.parent(){
            if let Err(e)=publish_new(&parent.join("first-color-null-draw.json"),&bytes){log(&format!("first corrected color null draw record failed: {e}"));}
            else {log("first corrected PS_BlitDepthToAlpha null draw recorded; diagnostic changed no commands");}
        }}}
    }
    fn record_color_feedback(&mut self,gpu:&Gpu,d:&Resolved<'_>) {
        if self.feedback_recorded || !gpu.bind_group_prepare.enabled {return;}
        let Some(pass)=gpu.pass_info.as_ref() else{return;};
        let mut overlaps=Vec::new();
        for (stage,program) in [(0,Some(d.vs)),(1,d.ps)] {
            let Some(program)=program else{continue;};
            let Some(group)=d.groups[stage] else{continue;};
            for b in &program.info.bindings {
                if b.group!=stage as u32 || b.kind!=Kind::Texture || b.binding<32 {continue;}
                let Some(&id)=gpu.s.srv[stage].get((b.binding-32)as usize) else{continue;};
                let Some(view)=gpu.view_obj(id).filter(|v|!v.is_buf) else{continue;};
                let Some(source)=gpu.tex(view.res) else{continue;};
                if source.desc.mips==0 || source.desc.d==0 {continue;}
                // Proxies are separate allocations and not stored under this
                // ordinary view key. No new view or resource is created here.
                let Some(sampled)=view.views.get(&format!("{:?}",b.dim)) else{continue;};
                if sampled.texture()!=&source.tex {continue;}
                let (mips,layers)=sampled_ranges(source.desc,view,b.dim);
                for (slot,target) in gpu.s.rt.iter().enumerate() {
                    let Some(format)=pass.rt_formats.get(slot).copied().flatten() else{continue;};
                    let Some(destination)=gpu.tex(target.tex) else{continue;};
                    let Some(attached)=destination.views.get(&(format,target.mip,target.slice)) else{continue;};
                    let layer=if destination.desc.dim==TextureDimension::D3 {0}else{target.slice};
                    if sampled.texture()!=attached.texture() || !contains_subresource(mips,layers,target.mip,layer) {continue;}
                    // Walk the cache only after a native overlap is found;
                    // ordinary draws do not pay a full cache scan for logging.
                    let Some((key,_))=gpu.bind_groups.iter().find(|(_,(_,uid))|*uid==group.uid) else{continue;};
                    if key.get(1)!=Some(&program.uid) || key.get(2)!=Some(&(stage as u64)) {continue;}
                    overlaps.push(json!({"stage":stage,"binding":b.binding,"binding_declared_in_layout":true,
                        "binding_group_uid":group.uid,"binding_group_key":key,"shader_program_uid":program.uid,
                        "shader_label":program.label,"sampled_guest_view":id,"sampled_guest_resource":view.res,
                        "sampled_dimension":b.dim,"sampled_type":format!("{:?}",b.sample),
                        "sampled_view_raw":[view.fmt,view.vdim,view.a,view.b,view.c,view.d],
                        "sampled_mips":mips,"sampled_layers":layers,"sampled_storage_format":source.format,
                        "target_slot":slot,"target_guest_resource":target.tex,"target_format":format,
                        "target_mip":target.mip,"target_layer":layer,"target_raw_slice":target.slice,
                        "native_view_parents_equal":true,"sampled_parent_matches_current_resource":true,
                        "native_identity_basis":"wgpu TextureView::texture Eq on resolved cached sampled view and retained attachment view; not guest ID equality",
                        "target_extent":[destination.desc.w,destination.desc.h,destination.desc.d],
                        "fragment_output_declared":d.ps.is_some_and(|p|p.info.outputs.contains(&(slot as u32)))}));
                    if overlaps.len()>=16 {break;}
                }
                if overlaps.len()>=16 {break;}
            }
            if overlaps.len()>=16 {break;}
        }
        if overlaps.is_empty(){return;}
        self.feedback_recorded=true;
        let shaders=[(0,Some(d.vs)),(1,d.ps)].into_iter().map(|(stage,program)|program.map(|p|json!({
            "stage":stage,"guest_id":gpu.s.shader[stage],"label":p.label,"wgsl_sha256":sha(p.text.as_bytes()),
            "guest_hash":match gpu.obj(gpu.s.shader[stage]){Some(Obj::Shader(s))=>Some(s.hash.as_str()),_=>None},
            "translated_wgsl":if p.text.len()<=256*1024 {Some(p.text.as_str())}else{None}}))).collect::<Vec<_>>();
        let value=json!({"schema":1,"kind":"first_resolved_color_feedback","command_sequence":self.sequence,
            "encoder_epoch":gpu.enc_epoch,"draws_before":gpu.draws,"presents":gpu.presents,
            "errors_before":gpu.errors.load(Ordering::SeqCst),"overlaps":overlaps,"shaders":shaders,
            "draw":{"kind":d.kind,"count":d.count,"instances":d.instances,"start":d.start,"base_vertex":d.base_vertex},
            "pipeline_uid":d.pipeline.uid,"viewport":d.viewport,"scissor":d.scissor,
            "state":gpu.command_trace_state(),"world_view":gpu.world_view.as_ref().map(crate::world_ui::marker),
            "native_commands_changed":false,"query":"all declared current resolved bind-group texture entries, including shader-unused entries; before pass.set_bind_group",
            "scope":"single first overlapping color attachment; depth/stencil overlap rules are not inferred"});
        if let Ok(bytes)=serde_json::to_vec_pretty(&value) {
            if bytes.len()<=1024*1024 {if let Some(parent)=self.directory.parent(){
                if let Err(e)=publish_new(&parent.join("first-color-feedback.json"),&bytes){log(&format!("first color feedback record failed: {e}"));}
                else {log("first resolved color feedback recorded before binding; original draw unchanged");}
            }}
        }
    }
    fn prepare(&self, gpu: &Gpu, d: &Resolved<'_>, ordinal: usize) -> Result<Prepared,String> {
        let ps = d.ps.ok_or("pixel program missing")?;
        let pi = gpu.pass_info.as_ref().ok_or("pass state missing")?;
        let view = gpu.world_view.ok_or("world command view missing")?;
        // Sampler and view descriptors below resolve through the retained
        // bind-group cache key. Its state/view replacement invalidation must
        // be enabled; guest IDs alone are not lifetime evidence.
        if !gpu.bind_group_prepare.enabled {return Err("binding lifetime invalidation is required for exact capture".into());}
        if gpu.errors.load(Ordering::SeqCst)!=0 {return Err("GPU validation/device error precedes candidate".into());}
        if gpu.occlusion_active.is_some() || gpu.occlusion_pass_slot.is_some() || !gpu.pending_resolve.is_empty() {
            return Err("active query or pending resolve forbids a pass split".into());
        }
        if d.kind != 1 || d.instances != 1 || d.start_instance != 0 || !d.unclamped || d.count < 3 || d.count > 65536
            || gpu.s.topo != 4 || d.vs.vid || d.vs.iid || d.vs.clip || ps.info.writes_fragment_depth
            || !view.headset_applied || !view.depth_reverse_z || gpu.hud_draw.is_some()
            || !ps.flat.is_empty() || gpu.inter_stage_remap(d.vs,Some(ps)).is_some()
            || gpu.state(gpu.s.blend.0).is_some_and(|s|s.words.first().copied().unwrap_or(0)!=0)
            || sha(d.vs.text.as_bytes()) != VS_TEXT || sha(ps.text.as_bytes()) != PS_TEXT {
            return Err("pinned direct draw/shader/world contract not qualified".into());
        }
        if pi.width == 0 || pi.height == 0 || pi.width > 4096 || pi.height > 4096 || pi.samples != 1
            || pi.rt_formats != vec![Some(TextureFormat::Bgra8Unorm);4]
            || pi.ds_format != Some(TextureFormat::Depth32FloatStencil8) || pi.ro_depth || pi.ro_stencil
            || d.viewport != [0.,0.,pi.width as f32,pi.height as f32,0.,1.]
            || d.scissor != [0,0,pi.width,pi.height] || gpu.s.ds.flags != 0 {
            return Err("full four-MRT D32S8 pass not qualified".into());
        }
        let (key,_) = gpu.pipelines.iter().find(|(_,entry)| matches!(entry,PipeEntry::Done(Some(pl)) if pl.uid==d.pipeline.uid))
            .ok_or("resolved pipeline cache identity unavailable")?;
        let pipeline = self.pipelines.get(key).ok_or("exact pipeline job metadata unavailable or cap reached")?.clone();
        let mut targets = Vec::new(); let mut domain = Vec::new(); let mut target_ids = Vec::new();
        for (slot,r) in gpu.s.rt.iter().take(4).enumerate() {
            let t = gpu.tex(r.tex).ok_or("target missing")?;
            if r.mip != 0 || r.slice != 0 || t.format != TextureFormat::Bgra8Unorm
                || t.desc.w != pi.width || t.desc.h != pi.height || t.desc.samples != 1 || t.desc.d != 1
                || target_ids.contains(&r.tex) { return Err("target subresource or alias unsupported".into()); }
            target_ids.push(r.tex); targets.push((t.tex.clone(),TextureAspect::All,format!("color{slot}")));
            domain.push(json!({"role":format!("color{slot}"),"guest_id":r.tex,"format":t.format,"mip":0,"slice":0}));
        }
        let ds = &gpu.s.ds; let t = gpu.tex(ds.tex).ok_or("depth target missing")?;
        if ds.mip != 0 || ds.slice != 0 || target_ids.contains(&ds.tex) || t.format != TextureFormat::Depth32FloatStencil8
            || t.desc.w != pi.width || t.desc.h != pi.height || t.desc.samples != 1 || t.desc.d != 1 {
            return Err("depth target subresource unsupported".into());
        }
        target_ids.push(ds.tex);
        targets.push((t.tex.clone(),TextureAspect::DepthOnly,"depth".into()));
        targets.push((t.tex.clone(),TextureAspect::StencilOnly,"stencil".into()));
        domain.push(json!({"role":"depth_stencil","guest_id":ds.tex,"format":t.format,"mip":0,"slice":0}));
        let view = json!({"provenance_epoch":view.provenance_epoch,"camera_sequence":view.camera_sequence,
            "request_frame":view.request_frame,"head_pose_frame":view.head_pose_frame,"eye_index":view.eye_index,
            "view_count":view.view_count,"presentation_time_seconds":view.presentation_time_seconds,
            "game_from_eye":view.game_from_eye,"projection_from_eye":view.projection_from_eye,
            "headset_applied":view.headset_applied,"depth_reverse_z":view.depth_reverse_z,
            "request_record":crate::xr_view::consumed_request_receipt(&view).ok_or("consumed request receipt unavailable")?});
        let domain = json!({"width":pi.width,"height":pi.height,"attachments":domain,"retained_native_parents_compared":true});
        let mut plans = Vec::new(); let mut vertices = Vec::new();
        for (slot,buffer,offset,id) in d.vertices {
            let name = format!("draw{ordinal}-vb{slot}.bin");
            plans.push(buffer_plan(name.clone(),buffer,0,buffer.size())?);
            vertices.push(json!({"slot":slot,"offset":offset,"blob":name,"guest_id":id,"size":buffer.size(),
                "renderer_zero_buffer":*id==u64::MAX}));
        }
        let (ib,format,offset,id) = d.index.ok_or("index binding missing")?;
        let ib_name = format!("draw{ordinal}-ib.bin"); plans.push(buffer_plan(ib_name.clone(),ib,0,ib.size())?);
        let mut bindings = Vec::new();
        for (stage,program) in [(0usize,d.vs),(1usize,ps)] {
            let expected = if stage==0 {vec![(0,Kind::Uniform)]} else {vec![(0,Kind::Uniform),(16,Kind::Sampler),(32,Kind::Texture)]};
            let actual = program.group_bindings.get(&(stage as u32)).ok_or("program group absent")?;
            if program.info.bindings.len()!=expected.len() || actual.iter().map(|b|(b.binding,b.kind)).collect::<Vec<_>>()!=expected {
                return Err("unsupported shader binding contract".into());
            }
            let group = d.groups[stage].ok_or("resolved group absent")?;
            let (bg_key,_) = gpu.bind_groups.iter().find(|(_,(_,uid))|*uid==group.uid).ok_or("resolved group cache absent")?;
            if bg_key.len()!=if stage==0 {4}else{7} || bg_key[1]!=program.uid || bg_key[2]!=stage as u64 {
                return Err("resolved group key contract changed".into());
            }
            for b in actual {
                let mut layout = Gpu::layout_entry(b,stage as u32);
                let mut entry = json!({"group":stage,"binding":b.binding});
                match b.kind {
                    Kind::Uniform => {
                        let pack = program.pack.as_ref().ok_or("packed uniform absent")?;
                        let offset = group.dyn_off.ok_or("actual uniform offset absent")? as u64;
                        let (buffer,_) = gpu.ubo_shared.iter().find(|(_,id)|*id==bg_key[3]).ok_or("actual uniform ring buffer absent")?;
                        let name = format!("draw{ordinal}-group{stage}-ubo.bin");
                        plans.push(buffer_plan(name.clone(),buffer,offset,pack.size)?);
                        if let BindingType::Buffer { has_dynamic_offset,.. } = &mut layout.ty { *has_dynamic_offset=false; }
                        entry["kind"]="uniform".into(); entry["blob"]=name.into(); entry["offset"]=0.into(); entry["size"]=pack.size.into();
                        entry["original_dynamic_offset"]=offset.into(); entry["original_ring_id"]=bg_key[3].into();
                        entry["pack_members"]=json!(pack.members.iter().map(|m|json!({"binding":m.binding,"offset":m.offset,"size":m.size})).collect::<Vec<_>>());
                    }
                    Kind::Sampler => {
                        let id = gpu.s.smp[stage][(b.binding-16)as usize];
                        let state = gpu.state(id).ok_or("actual sampler state absent")?;
                        if bg_key[4]!=id as u64 || state.sampler.is_none() || state.words.first().copied().unwrap_or(0)&0x80!=0 {
                            return Err("fallback or comparison sampler unsupported".into());
                        }
                        entry["kind"]="sampler".into(); entry["descriptor"]=json!(Gpu::sampler_descriptor(&state.words)); entry["guest_words"]=json!(state.words);
                    }
                    Kind::Texture => {
                        let id = gpu.s.srv[stage][(b.binding-32)as usize];
                        let v = gpu.view_obj(id).filter(|v|!v.is_buf).ok_or("actual sampled texture view absent")?;
                        let t = gpu.tex(v.res).ok_or("sampled texture absent")?;
                        if bg_key[5]!=id as u64 || bg_key[6]!=0 || b.dim!=TextureViewDimension::D2 || b.ms
                            || b.sample!=Sample::Float || t.desc.dim!=TextureDimension::D2 || t.desc.samples!=1 || t.desc.d!=1
                            || is_depth(t.format) || target_ids.contains(&v.res) || t.desc.mips==0 || t.desc.mips>16 {
                            return Err("sampled material view, alias or format unsupported".into());
                        }
                        let cached = v.views.get("D2").ok_or("actual cached sampled view absent")?;
                        if cached.texture()!=&t.tex { return Err("cached sampled view parent mismatch".into()); }
                        let total=t.desc.mips; let count=if v.b==0 || v.b==u32::MAX {total.saturating_sub(v.a)}else{v.b};
                        let mut vd=TextureViewDescriptor { dimension:Some(TextureViewDimension::D2),base_mip_level:v.a.min(total-1),
                            mip_level_count:Some(count.min(total.saturating_sub(v.a)).max(1)),base_array_layer:0,array_layer_count:Some(1),..Default::default() };
                        if v.c!=0 {return Err("sampled array slice unsupported".into());}
                        if v.fmt!=0 {
                            let original=map_format(v.fmt,0).format;
                            let mapped=if t.bc_source.is_some(){crate::bc::decoded_format(original).unwrap_or(original)}else{original};
                            if mapped!=t.format && mapped.is_srgb()!=t.format.is_srgb() && mapped.remove_srgb_suffix()==t.format.remove_srgb_suffix() {vd.format=Some(mapped);}
                        }
                        let mut subresources=Vec::new();
                        for mip in 0..total {
                            let name=format!("draw{ordinal}-material{stage}-mip{mip}.bin"); let plan=texture_plan(name.clone(),&t.tex,mip,TextureAspect::All)?;
                            subresources.push(json!({"mip_level":mip,"layer":0,"width":(t.desc.w>>mip).max(1),
                                "height":(t.desc.h>>mip).max(1),"packed_row_bytes":plan.row,"blob":name})); plans.push(plan);
                        }
                        entry["kind"]="texture".into(); entry["descriptor"]=json!({"width":t.desc.w,"height":t.desc.h,"depth_or_array_layers":1,
                            "mip_level_count":total,"sample_count":1,"dimension":t.desc.dim,"format":t.format});
                        entry["view"]=json!(vd);entry["subresources"]=json!(subresources);entry["guest_id"]=v.res.into();
                    }
                    _=>return Err("unsupported resource".into()),
                }
                entry["layout"]=json!(layout);bindings.push(entry);
            }
        }
        let draw=json!({"draw":{"kind":d.kind,"count":d.count,"instances":d.instances,"start_index":d.start,
            "base_vertex":d.base_vertex,"start_instance":d.start_instance},"command_sequence":self.sequence,
            "vertex_hash":VS,"pixel_hash":PS,"vertex_wgsl":d.vs.text,"pixel_wgsl":ps.text,
            "vertex_wgsl_sha256":VS_TEXT,"pixel_wgsl_sha256":PS_TEXT,"pipeline":pipeline,
            "vertex_buffers":vertices,"index_buffer":{"blob":ib_name,"format":format,"offset":offset,"guest_id":id},
            "bindings":bindings,"viewport":d.viewport,"scissor":d.scissor,"blend_constant":d.blend,
            "stencil_reference":d.stencil,"world_view":view,"target_domain":domain});
        Ok(Prepared{draw,view,domain,targets,plans})
    }
    pub(super) fn from_env() -> Option<Self> {
        let path = std::env::var_os("MUGUET_NATIVE_DRAW_CAPTURE")?;
        let directory = PathBuf::from(path);
        if !directory.is_absolute() || std::fs::create_dir(&directory).is_err() {
            log("native draw capture requires an absolute NEW directory with existing parent"); return None;
        }
        Some(Self { directory, pipelines: HashMap::new(), pending: None, done: false,
            attempts: 0, sequence: 0, epoch_bytes: 0, rejected: BTreeMap::new(), pipeline_bytes: 0,
            rejected_starts: Default::default(), live_bytes: Arc::new(AtomicU64::new(0)), feedback_recorded:false,
            color_null_binding:RefCell::new(None),color_null_recorded:false })
    }
    pub(super) fn remember_pipeline(&mut self, key: &[i64], job: &PipeJob) {
        if self.done || self.pipelines.len() >= MAX_PIPELINES { return; }
        let vertex_layouts: Vec<_> = job.vbs.iter().map(|v| v.as_ref().map(|(stride, step, attributes)|
            json!({"array_stride":stride,"step_mode":step,"attributes":attributes}))).collect();
        let value=json!({"primitive":job.primitive,
            "depth_stencil":job.depth_stencil,"multisample":job.multisample,
            "targets":job.targets,"vertex_layouts":vertex_layouts});
        let size=value.to_string().len();
        if size>65536 || self.pipeline_bytes.checked_add(size).is_none_or(|n|n>MAX_PIPELINE_BYTES) {return;}
        if self.pipelines.contains_key(key) {return;}
        self.pipeline_bytes+=size;self.pipelines.insert(key.to_vec(),value);
    }
    fn reject(&mut self, reason: &str) {
        let count=self.rejected.entry(reason.to_owned()).or_default();
        if *count==0 {log(&format!("native draw capture candidate rejected: {reason}"));}
        *count+=1;
        if let Some(p)=self.pending.take() {self.rejected_starts.insert(p.candidate_key);}
    }
    pub(super) fn command(&mut self, op: u32) {
        self.sequence = self.sequence.saturating_add(1);
        // ClearState supplies its own NULL bindings; a later draw must not
        // attribute those to an earlier color conflict.
        if op==33 {self.color_null_binding.borrow_mut().take();}
        // Only binding/state commands may intervene. Resource writes, including
        // uniform uploads, are rejected even though inputs are snapshotted.
        if self.pending.as_ref().is_some_and(|p| !p.finished)
            && !matches!(op, 0 | 1 | 2 | 9 | 10 | 18..=25 | 27..=32) {
            self.reject("intervening non-state command");
        }
        if let Some(p)=self.pending.as_mut().filter(|p|!p.finished) {
            if p.intervening_commands.len()>=256 {self.reject("intervening command bound exceeded");return;}
            p.intervening_commands.push(json!({"sequence":self.sequence,"opcode":op}));
        }
    }
    pub(super) fn skipped_draw(&mut self) {
        if self.pending.as_ref().is_some_and(|p| !p.finished) { self.reject("intervening skipped draw"); }
    }
    pub(super) fn before(&mut self, gpu: &mut Gpu, draw: &Resolved<'_>) {
        self.record_color_null_draw(gpu,draw);
        self.record_color_feedback(gpu,draw);
        if self.done || self.pending.as_ref().is_some_and(|p| p.finished) || self.attempts >= 128 { return; }
        let matching = matches!((gpu.obj(gpu.s.shader[0]),gpu.obj(gpu.s.shader[1])),
            (Some(Obj::Shader(v)),Some(Obj::Shader(p))) if v.hash==VS && p.hash==PS);
        if !matching { if self.pending.is_some() { self.reject("next draw has a different shader family"); } return; }
        let ordinal = self.pending.as_ref().map(|p| p.draws.len()).unwrap_or(0);
        // A failed first draw need not consume the bounded export allocation on
        // every later frame. This is only candidate selection, never admission.
        let mut candidate_key=vec![draw.count as u64,draw.start as u64,draw.base_vertex as u64,
            gpu.s.ib.0 as u64,gpu.s.ib.2 as u64,gpu.s.srv[1][0] as u64];
        for (slot,_,offset,id) in draw.vertices {candidate_key.extend([*slot as u64,*offset,*id]);}
        candidate_key.extend(gpu.s.rt.iter().map(|r|r.tex as u64));candidate_key.push(gpu.s.ds.tex as u64);
        if ordinal==0 && self.rejected_starts.contains(&candidate_key) {return;}
        let prepared = match self.prepare(gpu, draw, ordinal) {
            Ok(value) => value, Err(reason) => { self.reject(&reason); return; }
        };
        if let Some(p) = &self.pending {
            if p.epoch != gpu.enc_epoch || p.view != prepared.view || p.domain != prepared.domain
                || p.targets.len()!=prepared.targets.len()
                || !p.targets.iter().zip(&prepared.targets).all(|(a,b)|a.0==b.0 && a.1==b.1 && a.2==b.2) {
                self.reject("eye, camera, target or encoder changed within span"); return;
            }
        }
        let mut initial = Vec::new(); let mut attachments = Vec::new(); let mut final_bytes = 0;
        if ordinal == 0 {
            for (texture, aspect, role) in &prepared.targets {
                let initial_name = format!("initial-{role}.bin"); let final_name = format!("final-{role}.bin");
                match (texture_plan(initial_name.clone(), texture, 0, *aspect), texture_plan(final_name.clone(), texture, 0, *aspect)) {
                    (Ok(first), Ok(last)) => { final_bytes = match add(final_bytes, last.bytes) { Ok(v)=>v,Err(e)=>{self.reject(&e);return;} }; initial.push(first); },
                    _ => { self.reject("attachment snapshot preflight failed"); return; }
                }
                attachments.push(json!({"role":role,"format":texture.format(),"width":texture.width(),"height":texture.height(),
                    "mip":0,"slice":0,"initial_blob":initial_name,"final_blob":final_name}));
            }
        }
        let new_bytes = match plan_bytes(&initial).and_then(|n| add(n,plan_bytes(&prepared.plans)?)) { Ok(v)=>v,Err(e)=>{self.reject(&e);return;} };
        let reserved = self.pending.as_ref().map(|p| p.final_bytes).unwrap_or(final_bytes);
        if !reservation_fits(self.live_bytes.load(Ordering::SeqCst), new_bytes, reserved) {
            self.reject("live capture allocation budget exceeded; waiting for natural completion"); return;
        }
        // ensure_pass already applied any original pending clears. Ending it
        // commits those original LoadOps; reopening uses Load, never fake zero.
        let errors_before_encoding=gpu.errors.load(Ordering::SeqCst);
        if ordinal==0 {self.attempts+=1;}
        gpu.end_pass();
        self.epoch_bytes += new_bytes;
        self.live_bytes.fetch_add(new_bytes,Ordering::SeqCst);
        let mut readbacks = enqueue(gpu, initial);
        readbacks.extend(enqueue(gpu, prepared.plans));
        if let Some(pending) = &mut self.pending {
            pending.draws.push(prepared.draw); pending.readbacks.extend(readbacks); pending.bytes += new_bytes;
        } else {
            self.pending = Some(Pending { epoch: gpu.enc_epoch, errors: errors_before_encoding,
                view: prepared.view, domain: prepared.domain, draws: vec![prepared.draw], attachments,
                targets: prepared.targets, readbacks, bytes: new_bytes, final_bytes, finished: false,
                intervening_commands: Vec::new(), candidate_key });
        }
    }
    pub(super) fn after(&mut self, gpu: &mut Gpu) {
        let Some(pending) = &mut self.pending else { return; };
        if pending.finished || pending.draws.len() != 2 { return; }
        let plans = pending.targets.iter().map(|(t,a,r)| texture_plan(format!("final-{r}.bin"),t,0,*a)).collect::<Result<Vec<_>,_>>();
        let Ok(plans) = plans else { self.reject("final snapshot preflight failed"); return; };
        gpu.end_pass(); pending.readbacks.extend(enqueue(gpu,plans));
        pending.bytes += pending.final_bytes; self.epoch_bytes += pending.final_bytes; pending.finished = true;
        self.live_bytes.fetch_add(pending.final_bytes,Ordering::SeqCst);
    }
    pub(super) fn submitted(&mut self, gpu: &Gpu, epoch: u64) {
        let submitted_bytes=std::mem::take(&mut self.epoch_bytes);
        if submitted_bytes!=0 {
            let live_bytes=self.live_bytes.clone(); let inflight=gpu.inflight.clone();
            inflight.fetch_add(1,Ordering::SeqCst);
            // Aborted candidates are still retained by the command buffer.
            // Release their reservation only after this natural submission
            // completes, not when Rust handles or the encoder are dropped.
            gpu.queue.on_submitted_work_done(move || {
                live_bytes.fetch_sub(submitted_bytes,Ordering::SeqCst);
                inflight.fetch_sub(1,Ordering::SeqCst);
            });
        }
        let Some(pending) = self.pending.take() else { return; };
        if !pending.finished || pending.epoch != epoch {
            self.rejected_starts.insert(pending.candidate_key);
            self.reject("natural submission before span closure"); return;
        }
        self.done = true;
        let directory = self.directory.clone(); let rejected = self.rejected.clone();
        let errors = gpu.errors.clone(); let inflight = gpu.inflight.clone();
        let backend = format!("{:?}",gpu.adapter.get_info().backend);
        let (tx,rx) = std::sync::mpsc::channel();
        for (i, readback) in pending.readbacks.iter().enumerate() {
            let tx = tx.clone();
            readback.buffer.slice(..).map_async(MapMode::Read,move |result| { let _ = tx.send((i,result.map_err(|e|format!("{e:?}")))); });
        }
        drop(tx); inflight.fetch_add(1,Ordering::SeqCst);
        let failed_spawn_inflight=inflight.clone();
        let writer=std::thread::Builder::new().name("private-draw-capture".into()).spawn(move || {
            let result = complete(pending,rx,&directory,&errors,&backend,rejected);
            if let Err(reason) = result {
                let _ = publish_new(&directory.join("rejected.json"),&serde_json::to_vec_pretty(&json!({"schema":2,"status":"rejected","reason":reason})).unwrap());
                log(&format!("native draw capture rejected: {reason}"));
            }
            inflight.fetch_sub(1,Ordering::SeqCst);
        });
        if let Err(error)=writer {failed_spawn_inflight.fetch_sub(1,Ordering::SeqCst);log(&format!("native draw capture writer unavailable: {error}"));}
    }
}

fn write_new(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e|e.to_string())?;
    file.write_all(bytes).and_then(|_|file.sync_all()).map_err(|e|e.to_string())
}
fn publish_new(path:&std::path::Path,bytes:&[u8])->Result<(),String> {
    // Darwin's exclusive rename is atomic and cannot replace an existing
    // receipt. Readers see either no manifest or the fully synced manifest.
    extern "C" {fn renamex_np(from:*const std::ffi::c_char,to:*const std::ffi::c_char,flags:u32)->i32;}
    let temporary=path.with_extension("json.pending");
    write_new(&temporary,bytes)?;
    let from=std::ffi::CString::new(temporary.as_os_str().as_encoded_bytes()).map_err(|e|e.to_string())?;
    let to=std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e|e.to_string())?;
    if unsafe{renamex_np(from.as_ptr(),to.as_ptr(),0x00000004)}!=0 {
        let error=std::io::Error::last_os_error().to_string();let _=std::fs::remove_file(&temporary);return Err(error);
    }
    Ok(())
}
fn complete(pending: Pending, rx: std::sync::mpsc::Receiver<(usize,Result<(),String>)>, directory: &std::path::Path,
    errors: &AtomicU64, backend: &str, rejected: BTreeMap<String,u64>) -> Result<(),String> {
    let mut mapped = std::collections::HashSet::new();
    for _ in &pending.readbacks {
        let (i,result) = rx.recv_timeout(Duration::from_secs(60)).map_err(|e|format!("map completion: {e}"))?;
        result?; if !mapped.insert(i) { return Err("duplicate map callback".into()); }
    }
    if errors.load(Ordering::SeqCst) != pending.errors { return Err("GPU validation/device error during capture".into()); }
    let mut blobs = Vec::new();
    for rb in pending.readbacks {
        let view = rb.buffer.slice(..).get_mapped_range().map_err(|e|format!("mapped range: {e:?}"))?;
        let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(directory.join(&rb.plan.name)).map_err(|e|e.to_string())?;
        let mut hash=ShaRows::new();
        for row in 0..rb.plan.rows as usize {
            let bytes=&view[row*rb.plan.pitch as usize..row*rb.plan.pitch as usize+rb.plan.row as usize];
            file.write_all(bytes).map_err(|e|e.to_string())?;hash.update(bytes);
        }
        file.sync_all().map_err(|e|e.to_string())?;
        drop(view); rb.buffer.unmap();
        blobs.push(json!({"path":rb.plan.name,"byte_length":rb.plan.row as u64*rb.plan.rows as u64,"sha256":hash.finish(),
            "packed_row_bytes":rb.plan.row,"row_count":rb.plan.rows,"readback_pitch":rb.plan.pitch}));
    }
    let manifest = json!({"schema":2,"kind":"native_draw_span","status":"captured",
        "source":{"method":"Gpu::do_draw resolved bindings","backend":backend,"encoder_epoch":pending.epoch},
        "world_view":pending.view,"target_domain":pending.domain,"draws":pending.draws,
        "attachments":pending.attachments,"blobs":blobs,"allocated_readback_bytes":pending.bytes,
        "intervening_commands":pending.intervening_commands,
        "closure":{"ordered_two_draws":true,"unmodelled_commands":0,"resource_mutation_rejections":0,
            "initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true},
        "prior_rejected_candidates":rejected,"normal_frame_publication_changed":false,
        "uniform_binding_normalization":"exact selected GPU range copied to immutable offset-zero static binding; original dynamic offset separately recorded"});
    publish_new(&directory.join("capture.json"),&serde_json::to_vec_pretty(&manifest).map_err(|e|e.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture() -> Capture {
        Capture { directory:PathBuf::new(),pipelines:HashMap::new(),pending:None,done:false,attempts:0,sequence:0,
            epoch_bytes:0,rejected:BTreeMap::new(),pipeline_bytes:0,rejected_starts:Default::default(),
            live_bytes:Arc::new(AtomicU64::new(0)),feedback_recorded:false,
            color_null_binding:RefCell::new(None),color_null_recorded:false }
    }
    fn pending() -> Pending {
        Pending {epoch:1,errors:0,view:Value::Null,domain:Value::Null,draws:vec![Value::Null],attachments:vec![],
            targets:vec![],readbacks:vec![],bytes:0,final_bytes:0,finished:false,intervening_commands:vec![],candidate_key:vec![23,42]}
    }
    #[test]
    fn state_only_span_rejects_every_write_query_copy_dispatch_and_unknown() {
        for op in 0..=48 {
            let mut c=capture();c.pending=Some(pending());c.command(op);
            let expected=matches!(op,0|1|2|9|10|18..=25|27..=32);
            assert_eq!(c.pending.is_some(),expected,"opcode {op}");
            if expected {assert_eq!(c.pending.unwrap().intervening_commands[0]["opcode"],op);}
            else {assert!(c.rejected_starts.contains(&vec![23,42]));}
        }
    }
    #[test]
    fn skipped_draw_and_bounded_command_trace_cannot_certify_a_span() {
        let mut c=capture();c.pending=Some(pending());c.skipped_draw();assert!(c.pending.is_none());
        c.pending=Some(pending());for _ in 0..256 {c.command(24);}assert!(c.pending.is_some());
        c.command(24);assert!(c.pending.is_none());
        c.pending=Some(pending());c.pending.as_mut().unwrap().finished=true;c.command(39);
        assert!(c.pending.as_ref().unwrap().finished,"closed snapshot unaffected by later commands");
    }
    #[test]
    fn outstanding_encoders_and_reserved_final_contents_share_one_budget() {
        assert!(reservation_fits(0,MAX_BYTES/2,MAX_BYTES/2));
        assert!(!reservation_fits(1,MAX_BYTES/2,MAX_BYTES/2));
        assert!(!reservation_fits(160<<20,80<<20,80<<20),"previous natural submit still live");
        assert!(reservation_fits(0,80<<20,80<<20),"completion releases prior allocation");
        assert!(!reservation_fits(u64::MAX,1,0));
        assert!(!reservation_fits(0,u64::MAX,1));
        assert!(!reservation_fits(0,0,u64::MAX));
    }
    #[test]
    fn apple_sha_matches_standard_vector() {
        assert_eq!(sha(b"abc"),"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let mut rows=ShaRows::new();rows.update(b"a");rows.update(b"");rows.update(b"bc");assert_eq!(rows.finish(),sha(b"abc"));
    }
    #[test]
    fn capture_file_never_overwrites_an_existing_payload() {
        let directory=std::env::temp_dir().join(format!("v-draw-capture-test-{}-{}",std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&directory).unwrap();let path=directory.join("blob.bin");
        write_new(&path,b"first").unwrap();assert!(write_new(&path,b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(),b"first");
        std::fs::remove_file(path).unwrap();std::fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn final_manifest_publication_is_atomic_create_only() {
        let directory=std::env::temp_dir().join(format!("v-draw-capture-publish-test-{}-{}",std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&directory).unwrap();let path=directory.join("capture.json");
        publish_new(&path,b"{\"complete\":true}").unwrap();assert!(!directory.join("capture.json.pending").exists());
        assert!(publish_new(&path,b"replacement").is_err());assert_eq!(std::fs::read(&path).unwrap(),b"{\"complete\":true}");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(),1);
        std::fs::remove_file(path).unwrap();std::fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn feedback_probe_keeps_disjoint_mips_layers_and_cube_faces_separate() {
        let desc=TexDesc{w:64,h:64,d:12,mips:7,samples:1,dim:TextureDimension::D2};
        let mut view=ViewObj{res:1,is_buf:false,fmt:28,vdim:5,a:2,b:2,c:3,d:2,views:HashMap::new()};
        let (m,l)=sampled_ranges(desc,&view,TextureViewDimension::D2Array);
        assert_eq!((m,l),([2,4],[3,5]));
        for mip in 0..7 {for layer in 0..12 {assert_eq!(contains_subresource(m,l,mip,layer),
            [2,3].contains(&mip)&&[3,4].contains(&layer));}}
        view.a=6;view.b=u32::MAX;view.c=6;view.d=0;
        assert_eq!(sampled_ranges(desc,&view,TextureViewDimension::Cube),([6,7],[6,12]));
        assert_eq!(sampled_ranges(desc,&view,TextureViewDimension::D2),([6,7],[6,7]));
    }
}
