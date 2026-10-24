//! Actual offscreen Metal: optional MRT output must preserve flat target bytes,
//! and transparent coverage must follow the very same discard/scissor/stencil.
use super::*;
use std::time::Duration;
fn half(bits:u16)->f32 {let sign=if bits&0x8000!=0{-1.}else{1.};let e=(bits>>10)&31;let m=bits&1023;if e==0{sign*(m as f32)*2f32.powi(-24)}else{sign*(1.+m as f32/1024.)*2f32.powi(e as i32-15)}}
fn bytes(d:&Device,q:&Queue,t:&Texture,bpp:u32)->Vec<u8>{
 let b=d.create_buffer(&BufferDescriptor{label:None,size:1024,usage:BufferUsages::COPY_DST|BufferUsages::MAP_READ,mapped_at_creation:false});let mut e=d.create_command_encoder(&CommandEncoderDescriptor::default());
 e.copy_texture_to_buffer(TexelCopyTextureInfo{texture:t,mip_level:0,origin:Origin3d::ZERO,aspect:TextureAspect::All},TexelCopyBufferInfo{buffer:&b,layout:TexelCopyBufferLayout{offset:0,bytes_per_row:Some(256),rows_per_image:Some(4)}},Extent3d{width:4,height:4,depth_or_array_layers:1});
 let(tx,rx)=std::sync::mpsc::channel();e.map_buffer_on_submit(&b,MapMode::Read,..,move|r|{let _=tx.send(r);});let id=q.submit([e.finish()]);d.poll(PollType::Wait{submission_index:Some(id),timeout:Some(Duration::from_secs(10))}).unwrap();rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();let v=b.slice(..).get_mapped_range().unwrap();let out=(0..4).flat_map(|r|v[r*256..r*256+4*bpp as usize].to_vec()).collect();drop(v);b.unmap();out
}
const VS:&str="@vertex fn vs(@builtin(vertex_index) n:u32)->@builtin(position) vec4<f32>{let p=array<vec2<f32>,3>(vec2<f32>(-1.,-1.),vec2<f32>(3.,-1.),vec2<f32>(-1.,3.));return vec4<f32>(p[n],0.,1.);}";
fn pipeline(d:&Device,source:&str,targets:&[Option<ColorTargetState>],stencil:bool)->RenderPipeline {
 let m=d.create_shader_module(ShaderModuleDescriptor{label:Some("actual HUD MRT fixture"),source:ShaderSource::Wgsl(source.into())});
 d.create_render_pipeline(&RenderPipelineDescriptor{label:None,layout:None,vertex:VertexState{module:&m,entry_point:Some("vs"),compilation_options:Default::default(),buffers:&[]},fragment:Some(FragmentState{module:&m,entry_point:Some("main"),compilation_options:Default::default(),targets}),primitive:PrimitiveState::default(),depth_stencil:Some(DepthStencilState{format:TextureFormat::Depth24PlusStencil8,depth_write_enabled:Some(false),depth_compare:Some(CompareFunction::Always),stencil:StencilState{front:StencilFaceState{compare:if stencil{CompareFunction::Equal}else{CompareFunction::Always},..Default::default()},back:StencilFaceState{compare:if stencil{CompareFunction::Equal}else{CompareFunction::Always},..Default::default()},read_mask:255,write_mask:0},bias:Default::default()}),multisample:MultisampleState::default(),multiview_mask:None,cache:None})
}
#[test]
fn metal_hud_mrt_preserves_flat_bytes_and_real_coverage(){
 let mut desc=InstanceDescriptor::new_without_display_handle();desc.backends=Backends::METAL;let inst=Instance::new(desc);let adapter=pollster::block_on(inst.request_adapter(&RequestAdapterOptions::default())).unwrap();let(d,q)=pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).unwrap();
 let make=|format|d.create_texture(&TextureDescriptor{label:None,size:Extent3d{width:4,height:4,depth_or_array_layers:1},mip_level_count:1,sample_count:1,dimension:TextureDimension::D2,format,usage:TextureUsages::RENDER_ATTACHMENT|TextureUsages::COPY_SRC,view_formats:&[]});
 for mode in [HudBlend::StraightOver,HudBlend::PremultipliedOver,HudBlend::Opaque,HudBlend::StencilOnly] {for stencil_pass in [false,true]{for source_case in 0..4{
  let mut results=Vec::new();let mut hud_bytes=Vec::new();
  for augmented in [false,true]{
   let original=make(TextureFormat::Rgba8Unorm);let hud=make(TextureFormat::Rgba16Float);let depth=make(TextureFormat::Depth24PlusStencil8);let ov=original.create_view(&Default::default());let hv=hud.create_view(&Default::default());let dv=depth.create_view(&Default::default());
   let original_blend=match mode{HudBlend::StraightOver=>Some(BlendState{color:BlendComponent{src_factor:BlendFactor::SrcAlpha,dst_factor:BlendFactor::OneMinusSrcAlpha,operation:BlendOperation::Add},alpha:BlendComponent{src_factor:BlendFactor::SrcAlpha,dst_factor:BlendFactor::OneMinusSrcAlpha,operation:BlendOperation::Add}}),HudBlend::PremultipliedOver=>Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),_=>None};
   let flat=ColorTargetState{format:TextureFormat::Rgba8Unorm,blend:original_blend,write_mask:if mode==HudBlend::StencilOnly{ColorWrites::empty()}else{ColorWrites::ALL}};
   let colors=match source_case{0=>[[0.8f32,0.2,0.1,0.25],[0.1,0.7,0.3,0.5]],1=>[[1.2,-0.2,0.6,0.2],[0.1,0.7,0.3,0.5]],2=>[[0.8,0.2,0.1,-0.5],[0.1,0.7,0.3,0.5]],_=>[[0.8,0.2,0.1,1.5],[0.1,0.7,0.3,0.5]]};
   let mut pipes=Vec::new();for mut c in colors{if mode==HudBlend::PremultipliedOver{for i in 0..3{c[i]*=c[3];}}
    let ps=format!("var<private> o0:vec4<f32>; @fragment fn main(@builtin(position) pos:vec4<f32>)-> @location(0u) vec4<f32>{{if(pos.y<1.){{discard;}}o0=vec4<f32>({},{},{},{});return o0;}}",c[0],c[1],c[2],c[3]);let ps=if augmented{augment_fragment(&ps,mode).unwrap()}else{ps};let source=VS.to_owned()+&ps;let targets=if augmented{vec![Some(flat.clone()),Some(target(mode))]}else{vec![Some(flat.clone())]};pipes.push(pipeline(&d,&source,&targets,true));
   }
   let mut e=d.create_command_encoder(&Default::default());{
    let mut colors=vec![Some(RenderPassColorAttachment{view:&ov,depth_slice:None,resolve_target:None,ops:Operations{load:LoadOp::Clear(Color{r:0.12,g:0.37,b:0.81,a:1.}),store:StoreOp::Store}})];if augmented{colors.push(Some(RenderPassColorAttachment{view:&hv,depth_slice:None,resolve_target:None,ops:Operations{load:LoadOp::Clear(Color::TRANSPARENT),store:StoreOp::Store}}));}
    let mut p=e.begin_render_pass(&RenderPassDescriptor{label:None,color_attachments:&colors,depth_stencil_attachment:Some(RenderPassDepthStencilAttachment{view:&dv,depth_ops:Some(Operations{load:LoadOp::Clear(1.),store:StoreOp::Store}),stencil_ops:Some(Operations{load:LoadOp::Clear(if stencil_pass{1}else{0}),store:StoreOp::Store})}),timestamp_writes:None,occlusion_query_set:None,multiview_mask:None});p.set_stencil_reference(1);p.set_scissor_rect(0,0,3,4);for pipe in &pipes{p.set_pipeline(pipe);p.draw(0..3,0..1);}
   }q.submit([e.finish()]);results.push(bytes(&d,&q,&original,4));if augmented{hud_bytes=bytes(&d,&q,&hud,8);}
  }
  assert_eq!(results[0],results[1],"flat image changed: {mode:?} stencil={stencil_pass}");
  for y in 0..4{for x in 0..4{let at=(y*4+x)*8;let values:Vec<f32>=hud_bytes[at..at+8].chunks_exact(2).map(|v|half(u16::from_le_bytes([v[0],v[1]]))).collect();let covered=stencil_pass&&y>=1&&x<3&&mode!=HudBlend::StencilOnly;
   let alpha=if !covered{0.}else if mode==HudBlend::Opaque{1.}else{match source_case{0=>0.625,1=>0.6,2=>0.5,_=>1.}};assert!((values[3]-alpha).abs()<0.001,"coverage {mode:?}({x},{y}) {values:?}");
   if !covered{assert_eq!(values,vec![0.;4]);}else{for c in 0..3{let base=[0.12,0.37,0.81][c];let recomposed=values[c]+base*(1.-values[3]);let original=results[0][(y*4+x)*4+c]as f32/255.;assert!((recomposed-original).abs()<0.01,"composition differs {mode:?}: {recomposed} {original}");}}
  }}
 }}}
}
