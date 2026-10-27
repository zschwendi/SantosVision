//! Independent authored operation oracle. These inputs are not game imagery.
use crate::{model::*, sha, validate::Input, Result};
use std::{collections::BTreeMap, fs, path::Path};
use wgpu_types::*;

fn slice(id: &str, mip: u32, layer: u32, aspect: Aspect) -> Slice {
    Slice { resource: id.into(), mip, layer, aspect }
}
fn version(s: Slice, n: u64) -> Version { Version { slice: s, content_version: n } }
fn transition(s: Slice, before: u64, after: u64) -> Transition { Transition { slice: s, before, after } }
fn shader(text:&str)->Shader {Shader{wgsl:text.into(),sha256:sha(text.as_bytes()),entry:"main".into(),exact_final_variant:true}}
fn selected(id:&str,mip:u32,layer:u32,n:u64,binding:u32)->Binding {
    Binding {group:0,binding,value:BindingValue::Texture{texture:id.into(),view:TextureViewDescriptor{
        format:Some(TextureFormat::Rgba32Float),dimension:Some(TextureViewDimension::D2),base_mip_level:mip,
        mip_level_count:Some(1),base_array_layer:layer,array_layer_count:Some(1),..Default::default()},
        versions:vec![version(slice(id,mip,layer,Aspect::Color),n)]}}
}
const COMPUTE:&str="@group(0) @binding(0) var src:texture_2d<f32>; @group(0) @binding(1) var dst:texture_storage_2d<rgba32float,write>; @compute @workgroup_size(2,2) fn main(@builtin(global_invocation_id)p:vec3u){if(p.x<4u&&p.y<2u){textureStore(dst,vec2i(p.xy),textureLoad(src,vec2i(0,0),0)+vec4f(f32(p.x),f32(p.y),0,0));}}";
const VERTEX:&str="@vertex fn main(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0.5,1);}";
const FRAGMENT:&str="@group(0) @binding(0) var src:texture_2d<f32>; @fragment fn main(@builtin(position)p:vec4f)->@location(0)vec4f{return textureLoad(src,vec2i(p.xy),0);}";

struct Oracle {
    scene: Scene,
    files: BTreeMap<String, Vec<u8>>,
    current: BTreeMap<Slice, (u64, Vec<u8>)>,
}
impl Oracle {
    fn new() -> Self {
        Self { scene: Scene { schema: 1, provenance: Provenance::Authored { description:
            "Independent exact clear, partial selected-mip/layer copy, whole D32S8 copy, integer stencil proxy, odd-extent native mip chain and sRGB transfer oracle. No original game or finished-frame claim.".into() },
            eye: None, blobs: vec![], textures: vec![], buffers: vec![], pipelines: vec![], compute_pipelines: vec![],
            commands: vec![], expected: vec![], original_submission_completed: true, original_gpu_errors: Some(0), omitted_commands: 0 },
            files: BTreeMap::new(), current: BTreeMap::new() }
    }
    fn blob(&mut self, name: String, bytes: Vec<u8>) -> String {
        assert!(!self.files.contains_key(&name));
        self.scene.blobs.push(Blob { path: name.clone(), bytes: bytes.len() as u64, sha256: sha(&bytes) });
        self.files.insert(name.clone(), bytes); name
    }
    fn texture(&mut self, id: &str, format: TextureFormat, width: u32, height: u32, mips: u32,
        mutable: bool, data: impl Fn(u32,u32,Aspect,u32,u32)->Vec<u8>) {
        let mut initial=vec![];
        for mip in 0..mips { for layer in 0..2 { for aspect in crate::validate::aspects(format) {
            let bytes=data(mip,layer,aspect.clone(),(width>>mip).max(1),(height>>mip).max(1));
            let s=slice(id,mip,layer,aspect.clone());
            let blob=self.blob(format!("{id}-m{mip}-l{layer}-{aspect:?}-initial.bin"),bytes.clone());
            initial.push(SliceInitial {mip,layer,aspect,content_version:1,origin:Initial::Captured{blob}});
            self.current.insert(s,(1,bytes));
        }}}
        self.scene.textures.push(Texture {id:id.into(),native_allocation_id:None,mutable_target:mutable,
            descriptor:TextureDesc {dimension:TextureDimension::D2,width,height,layers:2,mips,format,view_formats:vec![]},initial});
    }
    fn changed(&mut self, s:Slice, after:u64, bytes:Vec<u8>, label:&str) {
        self.current.insert(s.clone(),(after,bytes.clone()));
        let blob=self.blob(format!("{label}.bin"),bytes);
        self.scene.commands.push(Command::Checkpoint{expected:vec![Expected{version:version(s,after),blob}]});
    }
    fn finish(mut self,out:&Path)->Result<()> {
        for (index,(s,(n,bytes))) in self.current.clone().into_iter().enumerate() {
            let blob=self.blob(format!("final-{index}.bin"),bytes);
            self.scene.expected.push(Expected{version:version(s,n),blob});
        }
        fs::create_dir(out)?;
        for (name,bytes) in self.files {fs::write(out.join(name),bytes)?;}
        fs::write(out.join("scene.json"),serde_json::to_vec_pretty(&self.scene)?)?;
        let input=Input::load(&out.join("scene.json"))?;
        assert!(!input.targets_have_resolution_independent_initialization);
        Ok(())
    }
}

fn color_pattern(mip:u32,layer:u32,_:Aspect,w:u32,h:u32)->Vec<u8> {
    (0..h).flat_map(|y|(0..w).flat_map(move|x|[(17*x+31*y+7*mip+11*layer)as u8,
        (29*x+3*y+23*layer)as u8,(11*x+13*y+41*mip)as u8,(19*x+7*y+5*layer+63)as u8])).collect()
}
fn depth_pattern(mip:u32,layer:u32,aspect:Aspect,w:u32,h:u32)->Vec<u8> {
    match aspect {
        Aspect::Depth=>{
            let values=[0u32,0x3f800000,0x3f000000,0x3eaaaaab,0x3f000001,0x00800000];
            (0..w*h).flat_map(|i|values[((i+2*mip+3*layer)as usize)%values.len()].to_le_bytes()).collect()
        },
        Aspect::Stencil=>(0..w*h).map(|i|(17*i+31*mip+67*layer)as u8).collect(),
        Aspect::Color=>unreachable!(),
    }
}
fn float_pattern(mip:u32,layer:u32,_:Aspect,w:u32,h:u32)->Vec<u8> {
    (0..h).flat_map(|y|(0..w).flat_map(move|x|{
        let b=(mip*100+layer*1000)as f32;
        [b+3.*x as f32+11.*y as f32,b+7.*x as f32+5.*y as f32,b+x as f32*y as f32,b+9.]
            .into_iter().flat_map(f32::to_le_bytes)
    })).collect()
}
/// CPU center-of-pixel normalized bilinear sampling with clamp-to-edge.
/// The 9x5->4x2->2x1->1x1 fixture uses dyadic locations and small exact values.
/// This oracle does not call the native shader or reproduce its WGSL code.
fn cpu_mip(source:&[u8],w:u32,h:u32)->Vec<u8> {
    let dw=(w/2).max(1);let dh=(h/2).max(1);let mut out=Vec::new();
    let get=|x:i64,y:i64,c:usize|{
        let x=x.clamp(0,w as i64-1)as usize;let y=y.clamp(0,h as i64-1)as usize;
        let p=((y*w as usize+x)*4+c)*4;
        f32::from_le_bytes(source[p..p+4].try_into().unwrap())as f64
    };
    for y in 0..dh {for x in 0..dw {
        let sx=(x as f64+0.5)*w as f64/dw as f64-0.5;
        let sy=(y as f64+0.5)*h as f64/dh as f64-0.5;
        let ix=sx.floor()as i64;let iy=sy.floor()as i64;let fx=sx-ix as f64;let fy=sy-iy as f64;
        for c in 0..4 {
            let top=get(ix,iy,c)*(1.-fx)+get(ix+1,iy,c)*fx;
            let bottom=get(ix,iy+1,c)*(1.-fx)+get(ix+1,iy+1,c)*fx;
            out.extend_from_slice(&((top*(1.-fy)+bottom*fy)as f32).to_le_bytes());
        }
    }}out
}

/// Stencil is an integer byte; the native proxy duplicates it into both stored
/// channels. No normalization, transfer function, filtering, or rounding applies.
fn cpu_stencil_proxy(source:&[u8])->Vec<u8> {
    source.iter().flat_map(|s|[*s,*s]).collect()
}

pub fn make(out:&Path)->Result<()> {
    let mut o=Oracle::new();
    o.texture("copy-source",TextureFormat::Rgba8Unorm,14,10,2,false,color_pattern);
    o.texture("copy-target",TextureFormat::Rgba8Unorm,18,14,2,true,color_pattern);
    o.texture("depth-source",TextureFormat::Depth32FloatStencil8,14,10,2,false,depth_pattern);
    o.texture("depth-target",TextureFormat::Depth32FloatStencil8,14,10,2,true,depth_pattern);
    o.texture("mips",TextureFormat::Rgba32Float,9,5,4,true,float_pattern);
    o.texture("post-compute",TextureFormat::Rgba32Float,4,2,1,true,float_pattern);
    o.texture("post-context",TextureFormat::Rgba32Float,4,2,1,true,float_pattern);
    o.texture("srgb-mips",TextureFormat::Rgba8UnormSrgb,2,2,2,true,|m,l,_,w,h|{
        if m==0&&l==1 {vec![0,0,0,255,255,255,255,255,255,255,255,255,0,0,0,255]}
        else {vec![7+(l+m)as u8;(w*h*4)as usize]}
    });
    let target=slice("copy-target",1,1,Aspect::Color);
    let clear=[255u8,0,255,255].repeat(9*7);
    o.scene.commands.push(Command::Clear {transition:transition(target.clone(),1,2),
        value:ClearValue::Color {rgba_bits:[1f64.to_bits(),0,1f64.to_bits(),1f64.to_bits()]},view_format:TextureFormat::Rgba8Unorm});
    o.changed(target.clone(),2,clear.clone(),"clear-selected-target");
    let source=slice("copy-source",1,1,Aspect::Color);
    let source_bytes=&o.current[&source].1;
    let mut copied=clear;
    for y in 0..3usize {let a=((y+1)*7+2)*4;let b=((y+2)*9+3)*4;copied[b..b+16].copy_from_slice(&source_bytes[a..a+16]);}
    o.scene.commands.push(Command::CopyRegion {sources:vec![version(source,1)],destinations:vec![transition(target.clone(),2,3)],
        source_origin:[2,1,0],destination_origin:[3,2,0],extent:[4,3,1]});
    o.changed(target,3,copied,"copy-partial-with-borders");
    let sd=slice("depth-source",1,1,Aspect::Depth);let ss=slice("depth-source",1,1,Aspect::Stencil);
    let dd=slice("depth-target",1,0,Aspect::Depth);let ds=slice("depth-target",1,0,Aspect::Stencil);
    let dbytes=o.current[&sd].1.clone();let sbytes=o.current[&ss].1.clone();
    o.scene.commands.push(Command::CopyRegion {sources:vec![version(sd,1),version(ss,1)],
        destinations:vec![transition(dd.clone(),1,4),transition(ds.clone(),1,5)],
        source_origin:[0,0,0],destination_origin:[0,0,0],extent:[7,5,1]});
    o.changed(dd,4,dbytes,"depth-copy-exact");o.changed(ds,5,sbytes,"stencil-copy-exact");
    for mip in 1..4 {
        let source=slice("mips",mip-1,1,Aspect::Color);let target=slice("mips",mip,1,Aspect::Color);
        let (n,bytes)=&o.current[&source];let bytes=cpu_mip(bytes,(9>>(mip-1)).max(1),(5>>(mip-1)).max(1));
        let after=5+mip as u64;
        o.scene.commands.push(Command::GenerateMip {source:version(source,*n),destination:transition(target.clone(),1,after)});
        o.changed(target,after,bytes,&format!("native-mip-{mip}"));
    }
    let s=slice("srgb-mips",0,1,Aspect::Color);let d=slice("srgb-mips",1,1,Aspect::Color);
    o.scene.commands.push(Command::GenerateMip {source:version(s,1),destination:transition(d.clone(),1,9)});
    // Linear mean(.0,1.,1.,.0)=.5, IEC sRGB encoding=.73535698 -> byte188.
    // Filtering the encoded bytes first would produce128, a distinct negative reference.
    o.changed(d,9,vec![188,188,188,255],"srgb-decode-before-filter");
    let sampled=|visibility|BindGroupLayoutEntry{binding:0,visibility,ty:BindingType::Texture{
        sample_type:TextureSampleType::Float{filterable:false},view_dimension:TextureViewDimension::D2,multisampled:false},count:None};
    o.scene.compute_pipelines.push(ComputePipeline{id:"post-mip-compute".into(),shader:shader(COMPUTE),layouts:vec![vec![sampled(ShaderStages::COMPUTE),
        BindGroupLayoutEntry{binding:1,visibility:ShaderStages::COMPUTE,ty:BindingType::StorageTexture{access:StorageTextureAccess::WriteOnly,
            format:TextureFormat::Rgba32Float,view_dimension:TextureViewDimension::D2},count:None}]]});
    let compute=slice("post-compute",0,1,Aspect::Color);let context=slice("post-context",0,1,Aspect::Color);
    let last=&o.current[&slice("mips",3,1,Aspect::Color)].1;
    let pixel:Vec<f32>=last.chunks_exact(4).map(|b|f32::from_le_bytes(b.try_into().unwrap())).collect();
    let mut expected=Vec::new();for y in 0..2 {for x in 0..4 {for (c,v) in pixel.iter().enumerate(){
        expected.extend_from_slice(&(v+match c{0=>x as f32,1=>y as f32,_=>0.}).to_le_bytes());
    }}}
    o.scene.commands.push(Command::Dispatch{dispatch:Dispatch{eye:None,pipeline:"post-mip-compute".into(),
        bindings:vec![selected("mips",3,1,8,0),selected("post-compute",0,1,1,1)],workgroups:[2,1,1],writes:vec![transition(compute.clone(),1,10)]}});
    o.changed(compute,10,expected.clone(),"compute-consumes-generated-mip");
    o.scene.pipelines.push(Pipeline{id:"unattributed-context-copy".into(),vertex:shader(VERTEX),fragment:Some(shader(FRAGMENT)),
        layouts:vec![vec![sampled(ShaderStages::FRAGMENT)]],vertex_layouts:vec![],primitive:Default::default(),depth_stencil:None,
        multisample:Default::default(),targets:vec![Some(ColorTargetState{format:TextureFormat::Rgba32Float,blend:None,write_mask:ColorWrites::ALL})]});
    o.scene.commands.push(Command::ContextDraw{context:OriginalRasterContext{native_present:23,world_view:serde_json::Value::Null},draw:Draw{
        eye:None,pipeline:"unattributed-context-copy".into(),bindings:vec![selected("post-compute",0,1,10,0)],vertex_buffers:vec![],index_buffer:None,
        args:DrawArgs::Direct{first_vertex:0,vertices:3,first_instance:0,instances:1},colors:vec![Some(Attachment{resource:"post-context".into(),mip:0,layer:1,view_format:TextureFormat::Rgba32Float})],
        depth:None,viewport:[0.,0.,4.,2.,0.,1.],scissor:[0,0,4,2],blend:[0.;4],stencil:0,writes:vec![transition(context.clone(),1,11)]}});
    o.changed(context,11,expected,"unattributed-context-after-compute");
    // Separate original D32S8 stencil read at nonzero mip/layer. 17x17 texels
    // span every possible stencil byte and a repeated boundary, so a normalized
    // sample, stale layer, channel swap, or clipped high bit is observable.
    o.texture("stencil-source",TextureFormat::Depth32FloatStencil8,34,34,2,false,
        |m,l,a,w,h|match a {
            Aspect::Stencil=>(0..w*h).map(|i|(i+31*m+67*l)as u8).collect(),
            Aspect::Depth=>(0..w*h).flat_map(|i|(((i+7*m+11*l)%1025)as f32/1024.).to_le_bytes()).collect(),
            Aspect::Color=>unreachable!(),
        });
    o.texture("stencil-proxy",TextureFormat::Rg8Uint,34,34,2,true,
        |m,l,_,w,h|(0..w*h).flat_map(|i|[(i+7+13*l)as u8,(i+117+19*m)as u8]).collect());
    let source=slice("stencil-source",1,1,Aspect::Stencil);
    let destination=slice("stencil-proxy",1,1,Aspect::Color);
    let expected=cpu_stencil_proxy(&o.current[&source].1);
    o.scene.commands.push(Command::StencilToColor{source:version(source,1),destination:transition(destination.clone(),1,12)});
    o.changed(destination,12,expected,"stencil-integer-duplicate-selected-mip-layer");
    o.finish(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_stencil_reference_spans_every_integer_byte() {
        assert!(include_str!("../../../src/gpu.rs").contains(crate::execute::STENCIL_TO_COLOR_WGSL));
        let source:Vec<u8>=(0u16..=255).map(|n|n as u8).collect();
        let output=cpu_stencil_proxy(&source);
        assert_eq!(output.len(),512);
        for value in [0u8,1,2,7,63,127,128,254,255] {
            assert_eq!(&output[value as usize*2..value as usize*2+2],&[value,value]);
        }
        assert!(output.chunks_exact(2).enumerate().all(|(i,p)|p==[i as u8,i as u8]));
    }
    #[test]
    fn stencil_proxy_admission_rejects_wrong_aspect_extent_and_versions()->Result<()> {
        let dir=std::env::temp_dir().join(format!("finished-stencil-admission-{}",std::process::id()));
        make(&dir)?;
        let base:Scene=serde_json::from_slice(&fs::read(dir.join("scene.json"))?)?;
        let index=base.commands.iter().position(|c|matches!(c,Command::StencilToColor{..})).unwrap();
        let reject=|name:&str,change:&dyn Fn(&mut Scene)|->Result<()> {
            let mut s=base.clone();change(&mut s);fs::write(dir.join("scene.json"),serde_json::to_vec(&s)?)?;
            assert!(Input::load(&dir.join("scene.json")).is_err(),"accepted {name}");Ok(())
        };
        reject("depth input aspect",&|s|if let Command::StencilToColor{source,..}=&mut s.commands[index]{source.slice.aspect=Aspect::Depth;})?;
        reject("color input aspect",&|s|if let Command::StencilToColor{source,..}=&mut s.commands[index]{source.slice.aspect=Aspect::Color;})?;
        reject("stale source",&|s|if let Command::StencilToColor{source,..}=&mut s.commands[index]{source.content_version=0;})?;
        reject("stale destination",&|s|if let Command::StencilToColor{destination,..}=&mut s.commands[index]{destination.before=2;})?;
        reject("reused transition",&|s|if let Command::StencilToColor{destination,..}=&mut s.commands[index]{destination.after=1;})?;
        reject("out of range source mip",&|s|if let Command::StencilToColor{source,..}=&mut s.commands[index]{source.slice.mip=2;})?;
        reject("out of range source layer",&|s|if let Command::StencilToColor{source,..}=&mut s.commands[index]{source.slice.layer=2;})?;
        reject("out of range destination layer",&|s|if let Command::StencilToColor{destination,..}=&mut s.commands[index]{destination.slice.layer=2;})?;
        reject("unequal selected extent",&|s|if let Command::StencilToColor{source,..}=&mut s.commands[index]{source.slice.mip=0;})?;
        reject("depth destination aspect",&|s|if let Command::StencilToColor{destination,..}=&mut s.commands[index]{destination.slice.aspect=Aspect::Depth;})?;
        reject("same allocation destination",&|s|if let Command::StencilToColor{destination,..}=&mut s.commands[index]{destination.slice.resource="stencil-source".into();})?;
        reject("normalized destination",&|s|s.textures.iter_mut().find(|t|t.id=="stencil-proxy").unwrap().descriptor.format=TextureFormat::Rg8Unorm)?;
        reject("nonstencil source",&|s|s.textures.iter_mut().find(|t|t.id=="stencil-source").unwrap().descriptor.format=TextureFormat::Depth32Float)?;
        reject("immutable destination",&|s|s.textures.iter_mut().find(|t|t.id=="stencil-proxy").unwrap().mutable_target=false)?;
        reject("wrong input dimension",&|s|s.textures.iter_mut().find(|t|t.id=="stencil-source").unwrap().descriptor.dimension=TextureDimension::D3)?;
        reject("wrong output dimension",&|s|s.textures.iter_mut().find(|t|t.id=="stencil-proxy").unwrap().descriptor.dimension=TextureDimension::D3)?;
        fs::remove_dir_all(dir)?;Ok(())
    }
    fn endpoint_model()->(Scene,Command) {
        // This is a synthetic metadata parser test, never an exported native capture.
        let mut s=Oracle::new().scene;
        s.provenance=Provenance::NativeFinishedGraph{source_sha256:"a".repeat(64),capture_id:"authored-certificate-parser-test".into(),native_present:23};
        s.eye=Some(EyeIdentity{epoch:2,camera:3,request:4,head:5,eye:0});
        let identity=[1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.];
        let world=serde_json::json!({"provenance_epoch":2,"camera_sequence":3,"request_frame":4,"head_pose_frame":5,"eye_index":0,
            "view_count":2,"headset_applied":true,"depth_reverse_z":true,"presentation_time_seconds":1.,
            "game_from_eye":identity,"game_from_stage":identity,"projection_from_eye":identity,
            "optics":{"scope_sequence":0,"magnification":1.,"reference_fov_degrees":60.,"actual_fov_degrees":60.}});
        for (id,format) in [(101,TextureFormat::Bgra8Unorm),(102,TextureFormat::Bgra8Unorm),(103,TextureFormat::R32Float)] {
            s.textures.push(Texture{id:format!("native-texture:{id}"),native_allocation_id:Some(id),mutable_target:true,
                descriptor:TextureDesc{dimension:TextureDimension::D2,width:2,height:2,layers:1,mips:1,format,view_formats:vec![]},initial:vec![]});
        }
        let a=slice("native-texture:101",0,0,Aspect::Color);let b=slice("native-texture:102",0,0,Aspect::Color);let d=slice("native-texture:103",0,0,Aspect::Color);
        s.commands.push(Command::CopyRegion{sources:vec![version(a.clone(),4)],destinations:vec![transition(b.clone(),2,5)],
            source_origin:[0,0,0],destination_origin:[0,0,0],extent:[2,2,1]});
        let publication=serde_json::json!({"accepted":true,"capture_id":"authored-certificate-parser-test","native_present":23,
            "source_id":7,"source_generation":8,"provenance_epoch":2,"eye_index":0,"request_frame_sequence":4,"head_sequence":5,"camera_sequence":3,
            "world_view":world,"graph_end_before_hud":true,"postlude_original_only":true,"hud_world_is_ready_hud_world":true,
            "ready_hud_world_allocation_id":102,"final_color_parent_allocation_id":101,"hud_draw_count":1,"hud_rejected":false,
            "slot_color":{"kind":"original_copy","source_allocation_id":101,"destination_allocation_id":201,"source_version":null},
            "slot_depth":{"kind":"original_copy","source_allocation_id":103,"destination_allocation_id":202,
                "source_version":{"allocation_id":103,"content_version":6,"subresource":{"mip":0,"layer":0,"aspect":"color"}}}});
        let c=Command::FinishedEndpoint{hud_free_source:Expected{version:version(a,4),blob:"source.bin".into()},
            hud_world:Expected{version:version(b,5),blob:"source.bin".into()},depth_source:Expected{version:version(d,6),blob:"depth.bin".into()},depth_world:world,publication};
        (s,c)
    }
    #[test]
    fn synthetic_endpoint_certificate_requires_exact_callback_copy_and_depth()->Result<()> {
        let (base,command)=endpoint_model();
        let check=|s:&Scene,c:&Command|->Result<()> {let Command::FinishedEndpoint{depth_world,publication,..}=c else{unreachable!()};
            crate::validate::validate_finished_endpoint(s,c,depth_world,publication)};
        check(&base,&command)?;
        let reject=|name:&str,change:&dyn Fn(&mut Scene,&mut Command)|{
            let mut s=base.clone();let mut c=command.clone();change(&mut s,&mut c);
            assert!(check(&s,&c).is_err(),"accepted {name}");
        };
        for k in ["accepted","graph_end_before_hud","postlude_original_only","hud_world_is_ready_hud_world"] {
            reject(k,&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication[k]=false.into();});
        }
        for (k,n) in [("source_id",0),("hud_draw_count",0),("eye_index",1),("camera_sequence",99),("native_present",24),("ready_hud_world_allocation_id",101)] {
            reject(k,&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication[k]=n.into();});
        }
        reject("foreign capture",&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication["capture_id"]="foreign".into();});
        reject("HUD rejected",&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication["hud_rejected"]=true.into();});
        reject("stale depth version",&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication["slot_depth"]["source_version"]["content_version"]=99.into();});
        reject("missing depth version",&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication["slot_depth"]["source_version"]=serde_json::Value::Null;});
        reject("foreign depth allocation",&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication["slot_depth"]["source_version"]["allocation_id"]=104.into();});
        reject("foreign depth layer",&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication["slot_depth"]["source_version"]["subresource"]["layer"]=1.into();});
        reject("invented retained handle",&|_,c|if let Command::FinishedEndpoint{publication,..}=c{publication["slot_depth"]["kind"]="same_native_handle".into();});
        reject("partial HUD copy",&|s,_|if let Command::CopyRegion{extent,..}=&mut s.commands[0]{extent[0]=1;});
        reject("offset HUD copy",&|s,_|if let Command::CopyRegion{source_origin,..}=&mut s.commands[0]{source_origin[0]=1;});
        reject("missing original copy",&|s,_|s.commands.clear());
        reject("renamed allocation",&|s,_|s.textures[0].native_allocation_id=Some(777));
        reject("wrong HUD format",&|s,_|s.textures[1].descriptor.format=TextureFormat::Rgba8Unorm);
        reject("wrong HUD extent",&|s,_|s.textures[1].descriptor.width=3);
        reject("unsupported Lens format",&|s,_|{s.textures[0].descriptor.format=TextureFormat::Rgba32Float;s.textures[1].descriptor.format=TextureFormat::Rgba32Float;});
        reject("array final target",&|s,_|s.textures[0].descriptor.layers=2);
        reject("wrong depth format",&|s,_|s.textures[2].descriptor.format=TextureFormat::Bgra8Unorm);
        reject("wrong depth extent",&|s,_|s.textures[2].descriptor.height=3);
        reject("array published depth",&|s,_|s.textures[2].descriptor.layers=2);
        reject("wrong depth aspect",&|_,c|if let Command::FinishedEndpoint{depth_source,publication,..}=c{depth_source.version.slice.aspect=Aspect::Depth;
            publication["slot_depth"]["source_version"]["subresource"]["aspect"]="depth".into();});
        let mut retained=command.clone();if let Command::FinishedEndpoint{publication,..}=&mut retained{
            publication["slot_depth"]["kind"]="same_native_handle".into();publication["slot_depth"]["destination_allocation_id"]=103.into();}
        check(&base,&retained)?;
        Ok(())
    }
    fn endpoint_scene(width:u32)->Scene {
        let (mut s,c)=endpoint_model();let mut c=c;
        for (i,t) in s.textures.iter_mut().enumerate() {
            t.descriptor.width=width;t.descriptor.height=width;
            t.initial=vec![SliceInitial{mip:0,layer:0,aspect:Aspect::Color,content_version:[1,2,6][i],
                origin:Initial::Captured{blob:"source.bin".into()}}];
        }
        if let Command::FinishedEndpoint{depth_source,..}=&mut c{depth_source.blob="source.bin".into();}
        if let Command::CopyRegion{extent,..}=&mut s.commands[0]{*extent=[width,width,1];}
        s.pipelines.push(Pipeline{id:"synthetic-anchor".into(),vertex:shader(VERTEX),fragment:Some(shader("@fragment fn main()->@location(0)vec4f{return vec4f(0,0,0,0);}")),
            layouts:vec![],vertex_layouts:vec![],primitive:Default::default(),depth_stencil:None,multisample:Default::default(),
            targets:vec![Some(ColorTargetState{format:TextureFormat::Bgra8Unorm,blend:None,write_mask:ColorWrites::ALL})]});
        s.commands.insert(0,Command::Draw{draw:Draw{eye:s.eye.clone(),pipeline:"synthetic-anchor".into(),bindings:vec![],vertex_buffers:vec![],index_buffer:None,
            args:DrawArgs::Direct{first_vertex:0,vertices:3,first_instance:0,instances:1},colors:vec![Some(Attachment{resource:"native-texture:101".into(),mip:0,layer:0,view_format:TextureFormat::Bgra8Unorm})],
            depth:None,viewport:[0.,0.,width as f32,width as f32,0.,1.],scissor:[0,0,width,width],blend:[0.;4],stencil:0,
            writes:vec![transition(slice("native-texture:101",0,0,Aspect::Color),1,4)]}});
        s.commands.push(c);
        s.expected.push(Expected{version:version(slice("native-texture:101",0,0,Aspect::Color),4),blob:"source.bin".into()});s
    }
    #[test]
    fn synthetic_endpoint_reference_identity_and_aggregate_budget()->Result<()> {
        let dir=std::env::temp_dir().join(format!("finished-endpoint-parser-{}",std::process::id()));fs::create_dir(&dir)?;
        let save=|s:&mut Scene,bytes:Vec<u8>|->Result<()> {
            s.blobs=vec![Blob{path:"source.bin".into(),bytes:bytes.len()as u64,sha256:sha(&bytes)}];fs::write(dir.join("source.bin"),bytes)?;
            fs::write(dir.join("scene.json"),serde_json::to_vec(s)?)?;Ok(())
        };
        let mut s=endpoint_scene(2);save(&mut s,vec![0;16])?;Input::load(&dir.join("scene.json"))?;
        let different=vec![1;16];fs::write(dir.join("different.bin"),&different)?;
        s.blobs.push(Blob{path:"different.bin".into(),bytes:16,sha256:sha(&different)});
        if let Command::FinishedEndpoint{hud_world,..}=s.commands.last_mut().unwrap(){hud_world.blob="different.bin".into();}
        fs::write(dir.join("scene.json"),serde_json::to_vec(&s)?)?;
        let e=match Input::load(&dir.join("scene.json")){Ok(_)=>panic!("accepted divergent HUD-free copied reference"),Err(e)=>e};
        assert!(e.to_string().contains("reference bytes differ"),"{e}");
        // One16MiB raw blob is shared by three logical initialized textures.
        // Sixty-three historical reads plus one final reference fit the finished-only1GiB cumulative bound;
        // the endpoint's additional
        // reads must be charged before any GPU allocation or execution.
        let mut big=endpoint_scene(2048);let index=big.commands.len()-1;
        for _ in 0..63 {big.commands.insert(index,Command::Checkpoint{expected:vec![Expected{
            version:version(slice("native-texture:101",0,0,Aspect::Color),4),blob:"source.bin".into()}]});}
        save(&mut big,vec![0;2048*2048*4])?;
        let e=match Input::load(&dir.join("scene.json")){Ok(_)=>panic!("accepted endpoint beyond aggregate readback budget"),Err(e)=>e};
        assert!(e.to_string().contains("output readback bound"),"{e}");
        fs::remove_dir_all(dir)?;Ok(())
    }
    #[test]
    fn independent_bilinear_and_transfer_reference() {
        assert!(include_str!("../../../src/gpu.rs").contains(crate::execute::GENERATE_MIP_WGSL));
        let initial=float_pattern(0,0,Aspect::Color,9,5);
        let first=cpu_mip(&initial,9,5);
        let f=|n:usize|f32::from_le_bytes(first[n*4..n*4+4].try_into().unwrap());
        assert_eq!([f(0),f(1),f(2),f(3)],[10.125,8.125,0.46875,9.]);
        assert_eq!(first.len(),4*2*16);
        let encoded=(1.055*0.5f64.powf(1./2.4)-0.055)*255.;
        assert_eq!(encoded.round()as u8,188);
        assert_ne!(encoded.round()as u8,128);
        let second=cpu_mip(&first,4,2);let last=cpu_mip(&second,2,1);
        assert_eq!(last.len(),16);
    }
    #[test]
    fn finished_operations_reject_incomplete_aspects_and_stale_ranges()->Result<()> {
        let dir=std::env::temp_dir().join(format!("finished-ops-oracle-{}",std::process::id()));
        make(&dir)?;let base=Input::load(&dir.join("scene.json"))?.scene;
        let reject=|name:&str,change:&dyn Fn(&mut Scene)|->Result<()> {
            let mut s=base.clone();change(&mut s);fs::write(dir.join("negative.json"),serde_json::to_vec(&s)?)?;
            assert!(Input::load(&dir.join("negative.json")).is_err(),"accepted {name}");Ok(())
        };
        let copy=base.commands.iter().position(|c|matches!(c,Command::CopyRegion{..})).unwrap();
        let depth=base.commands.iter().position(|c|matches!(c,Command::CopyRegion{sources,..}if sources.len()==2)).unwrap();
        let mip=base.commands.iter().position(|c|matches!(c,Command::GenerateMip{..})).unwrap();
        reject("copy x overflow",&|s|if let Command::CopyRegion{source_origin,..}=&mut s.commands[copy]{source_origin[0]=u32::MAX;})?;
        reject("copy out of bounds",&|s|if let Command::CopyRegion{destination_origin,..}=&mut s.commands[copy]{destination_origin[1]=6;})?;
        reject("copy stale source",&|s|if let Command::CopyRegion{sources,..}=&mut s.commands[copy]{sources[0].content_version=2;})?;
        reject("copy stale destination",&|s|if let Command::CopyRegion{destinations,..}=&mut s.commands[copy]{destinations[0].before=1;})?;
        reject("copy source alias",&|s|if let Command::CopyRegion{sources,destinations,..}=&mut s.commands[copy]{sources[0]=version(destinations[0].slice.clone(),2);})?;
        reject("partial unknown destination",&|s|{s.commands.remove(0);s.commands.remove(0);s.textures[1].initial.iter_mut().find(|i|i.mip==1&&i.layer==1).unwrap().origin=Initial::Unknown{reason:"not initialized".into()};if let Command::CopyRegion{destinations,..}=&mut s.commands[0]{destinations[0].before=1;}})?;
        reject("missing stencil aspect",&|s|if let Command::CopyRegion{sources,destinations,..}=&mut s.commands[depth]{sources.pop();destinations.pop();})?;
        reject("duplicate depth aspect",&|s|if let Command::CopyRegion{sources,..}=&mut s.commands[depth]{sources[1]=sources[0].clone();})?;
        reject("partial depth stencil",&|s|if let Command::CopyRegion{extent,..}=&mut s.commands[depth]{extent[0]=6;})?;
        reject("copy zero extent",&|s|if let Command::CopyRegion{extent,..}=&mut s.commands[copy]{extent[0]=0;})?;
        reject("copy multiple layers",&|s|if let Command::CopyRegion{extent,..}=&mut s.commands[copy]{extent[2]=2;})?;
        reject("mip skips predecessor",&|s|if let Command::GenerateMip{destination,..}=&mut s.commands[mip]{destination.slice.mip=2;})?;
        reject("mip wrong layer",&|s|if let Command::GenerateMip{destination,..}=&mut s.commands[mip]{destination.slice.layer=0;})?;
        reject("mip stale source",&|s|if let Command::GenerateMip{source,..}=&mut s.commands[mip]{source.content_version=77;})?;
        reject("mip immutable target",&|s|s.textures.iter_mut().find(|t|t.id=="mips").unwrap().mutable_target=false)?;
        reject("checkpoint future output",&|s|if let Command::Checkpoint{expected}=&mut s.commands[1]{expected[0].version.content_version=3;})?;
        let context=base.commands.iter().position(|c|matches!(c,Command::ContextDraw{..})).unwrap();
        reject("context fabricated eye",&|s|if let Command::ContextDraw{draw,..}=&mut s.commands[context]{draw.eye=Some(EyeIdentity{epoch:1,camera:1,request:1,head:1,eye:0});})?;
        reject("context missing present",&|s|if let Command::ContextDraw{context,..}=&mut s.commands[context]{context.native_present=0;})?;
        reject("context invented metadata",&|s|if let Command::ContextDraw{context,..}=&mut s.commands[context]{context.world_view=serde_json::json!({"invented":true});})?;
        reject("legacy context scope",&|s|s.provenance=Provenance::LegacyCaptured{manifest_sha256:"a".repeat(64)})?;
        fs::remove_dir_all(dir)?;Ok(())
    }
    #[test]
    fn original_only_events_never_replay_guest_side_effects()->Result<()> {
        let dir=std::env::temp_dir().join(format!("finished-sideeffects-parser-{}",std::process::id()));
        make(&dir)?;let mut base=Input::load(&dir.join("scene.json"))?.scene;
        let texture=base.textures.iter_mut().find(|t|t.id=="copy-source").unwrap();texture.native_allocation_id=Some(701);
        let d=&texture.descriptor;
        let desc=serde_json::json!({"width":d.width,"height":d.height,"depth_or_array_layers":d.layers,"mip_level_count":d.mips,
            "sample_count":1,"dimension":d.dimension,"format":d.format});
        let blob="copy-source-m1-l1-Color-initial.bin";
        let query=base.commands.len();
        base.commands.push(Command::OriginalOnly{operation_kind:"query".into(),expected:vec![],metadata:serde_json::json!({
            "event_index":50,"command_sequence":51,"original_only":true,"replay_side_effects":false,"result_completion_recorded":false,
            "action":"end","native_query_identity":1,"qtype":1,"sequence":7,"slots":[3,4],"live":true})});
        let readback=base.commands.len();
        base.commands.push(Command::OriginalOnly{operation_kind:"readback".into(),
            expected:vec![Expected{version:version(slice("copy-source",1,1,Aspect::Color),1),blob:blob.into()}],metadata:serde_json::json!({
                "event_index":52,"command_sequence":53,"original_only":true,"replay_side_effects":false,"result_completion_recorded":false,
                "sources":[{"allocation_id":701,"content_version":1,"subresource":{"mip":1,"layer":1,"aspect":"color"}}],
                "source_blobs":[blob],"source_descriptors":[desc],"source_origin":[2,1,0],"extent":[3,2,1],
                "row_bytes":12,"rows":2,"destination_row_pitch":16,"destination_depth_pitch":0,"destination_size":28})});
        let save=|s:&Scene|->Result<()> {fs::write(dir.join("sideeffects.json"),serde_json::to_vec(s)?)?;Ok(())};
        save(&base)?;Input::load(&dir.join("sideeffects.json"))?;
        let reject=|name:&str,change:&dyn Fn(&mut Scene)|->Result<()> {
            let mut s=base.clone();change(&mut s);save(&s)?;
            assert!(Input::load(&dir.join("sideeffects.json")).is_err(),"accepted {name}");Ok(())
        };
        for index in [query,readback] {for field in ["replay_side_effects","result_completion_recorded"] {
            reject(field,&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[index]{metadata[field]=true.into();})?;
        }}
        reject("duplicate query slots",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[query]{metadata["slots"]=serde_json::json!([3,3]);})?;
        reject("query slot beyond original capacity",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[query]{metadata["slots"]=serde_json::json!([2048]);})?;
        reject("nonocclusion slots",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[query]{metadata["qtype"]=2.into();})?;
        reject("invented query action",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[query]{metadata["action"]="replay_completion".into();})?;
        reject("wrong captured readback version",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["sources"][0]["content_version"]=2.into();})?;
        reject("stale actual source",&|s|if let Command::OriginalOnly{metadata,expected,..}=&mut s.commands[readback]{metadata["sources"][0]["content_version"]=2.into();expected[0].version.content_version=2;})?;
        reject("wrong readback source blob",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["source_blobs"][0]="other.bin".into();})?;
        reject("wrong readback descriptor",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["source_descriptors"][0]["width"]=15.into();})?;
        reject("readback range outside source",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["source_origin"]=serde_json::json!([6,1,0]);})?;
        reject("readback row truncated",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["destination_size"]=27.into();})?;
        reject("readback pitch shorter than row",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["destination_row_pitch"]=11.into();})?;
        reject("readback wrong pixel byte count",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["row_bytes"]=13.into();})?;
        reject("readback extra layer",&|s|if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["extent"]=serde_json::json!([3,2,2]);})?;
        reject("missing native source identity",&|s|{s.textures.iter_mut().find(|t|t.id=="copy-source").unwrap().native_allocation_id=None;
            if let Command::OriginalOnly{metadata,..}=&mut s.commands[readback]{metadata["sources"][0]["allocation_id"]=serde_json::Value::Null;}})?;
        reject("unknown original-only operation",&|s|if let Command::OriginalOnly{operation_kind,..}=&mut s.commands[readback]{*operation_kind="skip_unknown".into();})?;
        fs::remove_dir_all(dir)?;Ok(())
    }
}
