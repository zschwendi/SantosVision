//! Authored exact depth-proxy and ordered-checkpoint oracle, never game imagery.
use crate::{model::*, sha, validate::Input, Result};
use std::{collections::BTreeMap, fs, path::Path};
use wgpu_types::*;
const W: u32 = 7;
const H: u32 = 5;
const COMPUTE: &str = "@group(0) @binding(0) var src:texture_2d<f32>; @group(0) @binding(1) var dst:texture_storage_2d<r32float,write>; @compute @workgroup_size(4,3) fn main(@builtin(global_invocation_id)p:vec3u){if(p.x<7u&&p.y<5u){textureStore(dst,vec2i(p.xy),textureLoad(src,vec2i(p.xy),0));}}";
fn slice(id: &str, mip: u32, layer: u32, aspect: Aspect) -> Slice { Slice { resource:id.into(),mip,layer,aspect } }
fn reference(s: Slice, n: u64) -> Version { Version { slice:s,content_version:n } }
fn transition(s: Slice, before: u64, after: u64) -> Transition { Transition { slice:s,before,after } }
fn add_blob(scene:&mut Scene, files:&mut BTreeMap<String,Vec<u8>>, name:String, bytes:Vec<u8>) -> String {
    scene.blobs.push(Blob { path:name.clone(),bytes:bytes.len() as u64,sha256:sha(&bytes) });
    files.insert(name.clone(),bytes); name
}
fn depth_bytes(w:u32,h:u32,mip:u32,layer:u32) -> Vec<u8> {
    // Exact authored Float32 encodings, including adjacent representable values.
    let bits=[0x00000000u32,0x3f800000,0x3f7fffff,0x3eaaaaab,0x3f000000,0x3f000001,0x00800000,0x3e000000];
    (0..w*h).flat_map(|i|bits[((i+3*mip+5*layer)as usize)%bits.len()].to_le_bytes()).collect()
}
fn selected_view(format:TextureFormat,mip:u32,layer:u32)->TextureViewDescriptor<Option<String>> {
    TextureViewDescriptor { format:Some(format),dimension:Some(TextureViewDimension::D2),base_mip_level:mip,
        mip_level_count:Some(1),base_array_layer:layer,array_layer_count:Some(1),..Default::default() }
}
pub fn make(out:&Path)->Result<()> {
    let mut scene=Scene { schema:1,provenance:Provenance::Authored { description:
        "Exact native depth-to-R32 operation on mip1/layer1 with CPU Float32 bit oracle, untouched depth/stencil and neighbor subresources, ordered checkpoint before overwrite, and downstream compute consumption. No native image or density claim.".into() },
        eye:None,blobs:vec![],textures:vec![],buffers:vec![],pipelines:vec![],compute_pipelines:vec![],commands:vec![],expected:vec![],
        original_submission_completed:true,original_gpu_errors:Some(0),omitted_commands:0 };
    let mut files=BTreeMap::new();
    for (id,format,mips,layers,mutable) in [("depth",TextureFormat::Depth32FloatStencil8,2,2,false),
        ("proxy",TextureFormat::R32Float,2,2,true),("sink",TextureFormat::R32Float,1,1,true)] {
        let width=if id=="sink" {W}else{2*W}; let height=if id=="sink" {H}else{2*H};
        let mut initial=vec![];
        for mip in 0..mips {for layer in 0..layers {for aspect in crate::validate::aspects(format) {
            let w=width>>mip;let h=height>>mip;
            let bytes=match aspect {
                Aspect::Depth=>depth_bytes(w,h,mip,layer),
                Aspect::Stencil=>(0..w*h).map(|i|(i+31*mip+67*layer)as u8).collect(),
                Aspect::Color=>std::iter::repeat_n((-19.0f32-mip as f32-layer as f32).to_le_bytes(),(w*h)as usize).flatten().collect(),
            };
            let blob=add_blob(&mut scene,&mut files,format!("{id}-m{mip}-l{layer}-{aspect:?}-initial.bin"),bytes);
            initial.push(SliceInitial {mip,layer,aspect:aspect.clone(),content_version:1,origin:Initial::Captured {blob:blob.clone()} });
            if !(id=="proxy"&&mip==1&&layer==1) && id!="sink" {
                scene.expected.push(Expected { version:reference(slice(id,mip,layer,aspect),1),blob });
            }
        }}}
        scene.textures.push(Texture {id:id.into(),native_allocation_id:None,mutable_target:mutable,
            descriptor:TextureDesc {dimension:TextureDimension::D2,width,height,layers,mips,format,view_formats:vec![]},initial});
    }
    let depth=slice("depth",1,1,Aspect::Depth);let proxy=slice("proxy",1,1,Aspect::Color);let sink=slice("sink",0,0,Aspect::Color);
    let copied=add_blob(&mut scene,&mut files,"selected-depth-exact.bin".into(),depth_bytes(W,H,1,1));
    let cleared=add_blob(&mut scene,&mut files,"selected-proxy-after-overwrite.bin".into(),vec![0;(W*H*4)as usize]);
    scene.commands.push(Command::DepthToColor {source:reference(depth,1),destination:transition(proxy.clone(),1,2)});
    scene.commands.push(Command::Checkpoint {expected:vec![Expected {version:reference(proxy.clone(),2),blob:copied.clone()}]});
    scene.compute_pipelines.push(ComputePipeline {id:"consume-depth-proxy".into(),shader:Shader {wgsl:COMPUTE.into(),sha256:sha(COMPUTE.as_bytes()),entry:"main".into(),exact_final_variant:true},
        layouts:vec![vec![BindGroupLayoutEntry {binding:0,visibility:ShaderStages::COMPUTE,ty:BindingType::Texture {sample_type:TextureSampleType::Float {filterable:false},view_dimension:TextureViewDimension::D2,multisampled:false},count:None},
            BindGroupLayoutEntry {binding:1,visibility:ShaderStages::COMPUTE,ty:BindingType::StorageTexture {access:StorageTextureAccess::WriteOnly,format:TextureFormat::R32Float,view_dimension:TextureViewDimension::D2},count:None}]]});
    scene.commands.push(Command::Dispatch {dispatch:Dispatch {eye:None,pipeline:"consume-depth-proxy".into(),workgroups:[W.div_ceil(4),H.div_ceil(3),1],
        bindings:vec![Binding {group:0,binding:0,value:BindingValue::Texture {texture:"proxy".into(),view:selected_view(TextureFormat::R32Float,1,1),versions:vec![reference(proxy.clone(),2)]}},
            Binding {group:0,binding:1,value:BindingValue::Texture {texture:"sink".into(),view:selected_view(TextureFormat::R32Float,0,0),versions:vec![reference(sink.clone(),1)]}}],
        writes:vec![transition(sink.clone(),1,3)]}});
    scene.commands.push(Command::Clear {transition:transition(proxy.clone(),2,4),value:ClearValue::Color {rgba_bits:[0;4]},view_format:TextureFormat::R32Float});
    scene.commands.push(Command::Checkpoint {expected:vec![Expected {version:reference(proxy.clone(),4),blob:cleared.clone()}]});
    scene.expected.push(Expected {version:reference(proxy,4),blob:cleared});
    scene.expected.push(Expected {version:reference(sink,3),blob:copied});
    fs::create_dir(out)?;for (name,bytes) in files {fs::write(out.join(name),bytes)?;}
    fs::write(out.join("scene.json"),serde_json::to_vec_pretty(&scene)?)?;
    let i=Input::load(&out.join("scene.json"))?;assert!(!i.targets_have_resolution_independent_initialization);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_depth_shader_and_independent_float_bits_are_exact() {
        assert!(include_str!("../../../src/gpu.rs").contains(crate::execute::DEPTH_TO_COLOR_WGSL));
        let bytes=depth_bytes(W,H,1,1);
        assert_eq!(bytes.len(),(W*H*4)as usize);
        assert_eq!(&bytes[0..4],&0u32.to_le_bytes());
        assert!(bytes.chunks_exact(4).any(|p|p==0x3f000001u32.to_le_bytes()));
        for p in bytes.chunks_exact(4) {assert!((0.0..=1.0).contains(&f32::from_le_bytes(p.try_into().unwrap())));}
    }
    #[test]
    fn depth_and_checkpoint_admission_fail_closed() -> Result<()> {
        let dir=std::env::temp_dir().join(format!("depth-proxy-oracle-{}",std::process::id()));
        make(&dir)?;let base=Input::load(&dir.join("scene.json"))?.scene;
        let reject=|name:&str,change:&dyn Fn(&mut Scene)|->Result<()> {
            let mut s=base.clone();change(&mut s);fs::write(dir.join("negative.json"),serde_json::to_vec(&s)?)?;
            assert!(Input::load(&dir.join("negative.json")).is_err(),"accepted {name}");Ok(())
        };
        reject("stale source",&|s|if let Command::DepthToColor {source,..}=&mut s.commands[0] {source.content_version=0;})?;
        reject("stencil source",&|s|if let Command::DepthToColor {source,..}=&mut s.commands[0] {source.slice.aspect=Aspect::Stencil;})?;
        reject("wrong source layer",&|s|if let Command::DepthToColor {source,..}=&mut s.commands[0] {source.slice.layer=2;})?;
        reject("mismatched extent",&|s|if let Command::DepthToColor {source,..}=&mut s.commands[0] {source.slice.mip=0;})?;
        reject("wrong destination format",&|s|s.textures[1].descriptor.format=TextureFormat::Rgba8Unorm)?;
        reject("immutable destination",&|s|s.textures[1].mutable_target=false)?;
        reject("stale destination",&|s|if let Command::DepthToColor {destination,..}=&mut s.commands[0] {destination.before=0;})?;
        reject("nonadvancing destination",&|s|if let Command::DepthToColor {destination,..}=&mut s.commands[0] {destination.after=1;})?;
        reject("wrong destination aspect",&|s|if let Command::DepthToColor {destination,..}=&mut s.commands[0] {destination.slice.aspect=Aspect::Depth;})?;
        reject("future checkpoint",&|s|if let Command::Checkpoint {expected}=&mut s.commands[1] {expected[0].version.content_version=4;})?;
        reject("checkpoint after overwrite",&|s|s.commands.swap(1,4))?;
        reject("missing checkpoint bytes",&|s|if let Command::Checkpoint {expected}=&mut s.commands[1] {expected[0].blob="absent.bin".into();})?;
        reject("empty checkpoint",&|s|if let Command::Checkpoint {expected}=&mut s.commands[1] {expected.clear();})?;
        reject("duplicate checkpoint",&|s|if let Command::Checkpoint {expected}=&mut s.commands[1] {expected.push(expected[0].clone());})?;
        // Full native proxy overwrite initializes an unknown destination, but
        // does not establish resolution independence of its captured source.
        let mut unknown=base.clone();unknown.textures[1].initial.iter_mut().find(|i|i.mip==1&&i.layer==1).unwrap().origin=Initial::Unknown {reason:"original untouched proxy".into()};
        fs::write(dir.join("unknown.json"),serde_json::to_vec(&unknown)?)?;
        assert!(!Input::load(&dir.join("unknown.json"))?.targets_have_resolution_independent_initialization);
        fs::remove_dir_all(dir)?;Ok(())
    }
}
