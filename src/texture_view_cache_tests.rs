//! Real offscreen Metal regression: persistent guest SRV/UAV IDs must follow a
//! texture's new allocation, with bind-group preparation both off and on.
//! No game, surface, simulator, device, or original assets. Run serially.
use super::*;

fn gpu(fast: bool) -> Gpu {
    let mut config=wasmtime::Config::new();config.wasm_threads(true);config.shared_memory(true);
    let engine=wasmtime::Engine::new(&config).unwrap();
    let mem=SharedMemory::new(&engine,wasmtime::MemoryType::shared(4,4)).unwrap();
    let mut g=Gpu::new(mem,0,65536,0,None).unwrap();g.bind_group_prepare=BindGroupPrepare::new(fast);g
}
fn texture(g: &mut Gpu,id: u32,color: [u8;4]) {
    g.create_texture(id,2,1,1,1,1,1,28,BIND_SRV|BIND_UAV,1);
    assert!(crate::write_bytes(&g.mem,0x1_1000,&color));
    g.upload_texture(id,0,0,0,0,0,1,1,1,4,1,4,0x1_1000);g.flush();
}
fn view(g: &mut Gpu,id: u32,res: u32,uav: bool) {
    let v=ViewObj {res,is_buf:false,fmt:28,vdim:4,a:0,b:1,c:0,d:1,views:HashMap::new()};
    g.set_obj(id,if uav {Obj::Uav(v)} else {Obj::Srv(v)});
}
fn program(g: &mut Gpu) -> Program {
    let text="@group(0) @binding(32) var source: texture_2d<f32>;\n@group(0) @binding(160) var destination: texture_storage_2d<rgba8unorm, write>;\n@compute @workgroup_size(1) fn main() { textureStore(destination,vec2<i32>(0),textureLoad(source,vec2<i32>(0),0)); }".to_owned();
    let module=g.device.create_shader_module(ShaderModuleDescriptor {label:Some("texture cache fixture"),source:ShaderSource::Wgsl(text.as_str().into())});
    let info=parse_wgsl(&text);assert_eq!(info.bindings.len(),2);
    let group_bindings=BTreeMap::from([(0,info.bindings.clone())]);
    Program {module,info,consts:None,pack:None,text,flat:vec![],group_bindings,variants:RefCell::new(HashMap::new()),label:"texture cache fixture".into(),uid:g.uid(),vid:false,iid:false,out_locs:vec![],in_locs:vec![],clip:false}
}
fn copy(g: &mut Gpu,p: &Program,bgl: &(BindGroupLayout,u64),pipeline: &ComputePipeline) -> u64 {
    let bound=g.stage_bind_group(2,p,0,bgl).expect("production bind group");
    let mut encoder=g.device.create_command_encoder(&CommandEncoderDescriptor::default());
    {let mut pass=encoder.begin_compute_pass(&ComputePassDescriptor::default());pass.set_pipeline(pipeline);pass.set_bind_group(0,&bound.bg,&[]);pass.dispatch_workgroups(1,1,1);}
    let submission=g.queue.submit([encoder.finish()]);
    g.device.poll(PollType::Wait {submission_index:Some(submission),timeout:Some(Duration::from_secs(10))}).unwrap();bound.uid
}
fn pixel(g: &mut Gpu,id: u32) -> [u8;4] {
    g.flush();let tex=g.tex(id).unwrap().tex.clone();
    let output=g.device.create_buffer(&BufferDescriptor {label:Some("texture cache pixel readback"),size:256,usage:BufferUsages::COPY_DST|BufferUsages::MAP_READ,mapped_at_creation:false});
    let mut encoder=g.device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(TexelCopyTextureInfo {texture:&tex,mip_level:0,origin:Origin3d::ZERO,aspect:TextureAspect::All},TexelCopyBufferInfo {buffer:&output,layout:TexelCopyBufferLayout {offset:0,bytes_per_row:Some(256),rows_per_image:Some(1)}},Extent3d {width:1,height:1,depth_or_array_layers:1});
    let (tx,rx)=std::sync::mpsc::channel();encoder.map_buffer_on_submit(&output,MapMode::Read,..,move|r|{let _=tx.send(r);});
    let submission=g.queue.submit([encoder.finish()]);g.device.poll(PollType::Wait {submission_index:Some(submission),timeout:Some(Duration::from_secs(10))}).unwrap();rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    let result=output.slice(..).get_mapped_range().unwrap()[..4].try_into().unwrap();output.unmap();result
}
fn scenario(fast: bool,destroy_first: bool,replace_source: bool) {
    let mut g=gpu(fast);
    texture(&mut g,1,[255,0,0,255]);texture(&mut g,2,[0,0,0,255]);texture(&mut g,3,[0,0,255,255]);
    view(&mut g,11,1,false);view(&mut g,12,2,true);view(&mut g,13,3,false);
    g.s.srv[2][0]=11;g.s.uav[2][0]=12;
    let p=program(&mut g);let bgl=g.bgl_for(&p,2,0);
    let layout=g.device.create_pipeline_layout(&PipelineLayoutDescriptor {label:None,bind_group_layouts:&[Some(&bgl.0)],immediate_size:0});
    let pipeline=g.device.create_compute_pipeline(&ComputePipelineDescriptor {label:None,layout:Some(&layout),module:&p.module,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
    let source_binding=p.info.bindings.iter().find(|b| b.binding==32).unwrap();
    let unrelated=g.srv_view(13,source_binding).unwrap();
    let first_bg=copy(&mut g,&p,&bgl,&pipeline);assert_eq!(pixel(&mut g,2),[255,0,0,255]);
    let target=if replace_source {1} else {2};
    if destroy_first {g.destroy(target);}
    texture(&mut g,target,if replace_source {[0,255,0,255]} else {[0,0,0,255]});
    let second_bg=copy(&mut g,&p,&bgl,&pipeline);
    assert_eq!(pixel(&mut g,2),if replace_source {[0,255,0,255]} else {[255,0,0,255]},"fast={fast} destroy={destroy_first} source={replace_source}");
    assert_ne!(first_bg,second_bg,"texture allocation changed, so bind group must be rebuilt");
    assert!(g.srv_view(13,source_binding).unwrap()==unrelated,"unrelated cached view must survive");
    g.s.srv[2][0]=13;copy(&mut g,&p,&bgl,&pipeline);assert_eq!(pixel(&mut g,2),[0,0,255,255]);
    assert_eq!(g.errors.load(Ordering::Relaxed),0);
}
#[test] fn metal_srv_replacement_follows_new_texture_fast_off() {scenario(false,false,true);}
#[test] fn metal_srv_replacement_follows_new_texture_fast_on() {scenario(true,false,true);}
#[test] fn metal_srv_destroy_recreate_follows_new_texture_fast_off() {scenario(false,true,true);}
#[test] fn metal_srv_destroy_recreate_follows_new_texture_fast_on() {scenario(true,true,true);}
#[test] fn metal_uav_replacement_follows_new_texture_fast_off() {scenario(false,false,false);}
#[test] fn metal_uav_replacement_follows_new_texture_fast_on() {scenario(true,false,false);}
#[test] fn metal_uav_destroy_recreate_follows_new_texture_fast_off() {scenario(false,true,false);}
#[test] fn metal_uav_destroy_recreate_follows_new_texture_fast_on() {scenario(true,true,false);}

fn render_target(g: &mut Gpu,id: u32) {
    // No upload/flush: the tests must retain the existing open pass/clear.
    g.create_texture(id,2,1,1,1,1,1,28,BIND_RT|BIND_SRV,1);
}
fn green_pipeline(g: &Gpu) -> RenderPipeline {
    let module=g.device.create_shader_module(ShaderModuleDescriptor {label:Some("RT reuse fixture"),source:ShaderSource::Wgsl("@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4f { var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0,1); } @fragment fn fs()->@location(0) vec4f {return vec4f(0,1,0,1);}".into())});
    g.device.create_render_pipeline(&RenderPipelineDescriptor {label:None,layout:None,
        vertex:VertexState {module:&module,entry_point:Some("vs"),compilation_options:Default::default(),buffers:&[]},
        primitive:PrimitiveState::default(),depth_stencil:None,multisample:MultisampleState::default(),
        fragment:Some(FragmentState {module:&module,entry_point:Some("fs"),compilation_options:Default::default(),targets:&[Some(ColorTargetState {format:TextureFormat::Rgba8Unorm,blend:None,write_mask:ColorWrites::ALL})]}),multiview_mask:None,cache:None})
}
#[test]
fn metal_rt_replacement_rebuilds_only_its_open_attachment() {
    let mut g=gpu(false);render_target(&mut g,1);g.s.rt[0]=Rt {tex:1,mip:0,slice:0,fmt:28};
    g.clear_rt(1,0,0,[1.,0.,0.,1.]);assert!(g.ensure_pass());let old=g.tex(1).unwrap().tex.clone();
    let first_passes=g.passes;
    // Unrelated material texture creation must not close this render pass.
    g.create_texture(9,2,1,1,1,1,1,28,BIND_SRV,1);
    // Zero appears in unbound attachment slots and is never an active target.
    g.create_texture(0,2,1,1,1,1,1,28,BIND_SRV,1);
    assert!(g.in_open_pass(1));assert_eq!(g.passes,first_passes);
    g.pdirty=false;render_target(&mut g,1);
    let old_pass_retired=g.pass.is_none() && g.pass_key.is_none() && g.pass_info.is_none();
    assert!(g.ensure_pass());let green=green_pipeline(&g);
    {let pass=g.pass.as_mut().unwrap();pass.set_pipeline(&green);pass.draw(0..3,0..1);}
    assert_eq!(pixel(&mut g,1),[0,255,0,255],"green draw must target new allocation");
    assert!(old_pass_retired);assert!(g.pdirty);assert_eq!(g.passes,first_passes+1);
    // The old encoded clear stays valid even though its guest slot was replaced.
    // Reinsert a separate reference only for readback, never to render into it.
    g.set_obj(20,Obj::Tex(Tex {tex:old,trace_native_identity:None,desc:TexDesc {w:1,h:1,d:1,mips:1,samples:1,dim:TextureDimension::D2},format:TextureFormat::Rgba8Unorm,bc_source:None,views:HashMap::new(),a8:false,enc:0,in_bg:false,proxy_depth:None,proxy_stencil:None,proxy_fdepth:None,blit_bg:None,mip_passes:None}));
    assert_eq!(pixel(&mut g,20),[255,0,0,255]);assert_eq!(g.errors.load(Ordering::Relaxed),0);
}
#[test]
fn metal_rt_replacement_discards_only_old_lifetime_deferred_clear() {
    let mut g=gpu(false);render_target(&mut g,1);render_target(&mut g,3);
    g.clear_rt(1,0,0,[1.,0.,0.,1.]);g.clear_rt(3,0,0,[0.,0.,1.,1.]);
    render_target(&mut g,1);
    let t=g.tex(1).unwrap().tex.clone();
    // Seed new storage without the production upload helper, which would
    // materialize stale clears before upload and hide the bug under test.
    g.queue.write_texture(TexelCopyTextureInfo {texture:&t,mip_level:0,origin:Origin3d::ZERO,aspect:TextureAspect::All},&[0,255,0,255],TexelCopyBufferLayout {offset:0,bytes_per_row:Some(4),rows_per_image:Some(1)},Extent3d {width:1,height:1,depth_or_array_layers:1});
    g.s.rt[0]=Rt {tex:1,mip:0,slice:0,fmt:28};assert!(g.ensure_pass());
    assert_eq!(pixel(&mut g,1),[0,255,0,255],"old red clear must not affect new green allocation");
    assert!(g.pending_clears.contains_key(&3));g.materialize_clears(3);
    assert_eq!(pixel(&mut g,3),[0,0,255,255]);assert_eq!(g.errors.load(Ordering::Relaxed),0);
}
