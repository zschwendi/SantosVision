//! Authored float-to-depth API oracle. These pixels are not game imagery.
use crate::{model::*, sha, validate::Input, Result};
use std::{collections::BTreeMap, fs, path::Path};
use wgpu_types::*;

const WIDTH:u32=34;
const HEIGHT:u32=22;
const MIPS:u32=3;
const LAYERS:u32=2;

fn slice(id:&str,mip:u32,layer:u32,aspect:Aspect)->Slice {
    Slice{resource:id.into(),mip,layer,aspect}
}
fn version(slice:Slice,n:u64)->Version {Version{slice,content_version:n}}
fn transition(slice:Slice,before:u64,after:u64)->Transition {Transition{slice,before,after}}
fn blob(scene:&mut Scene,files:&mut BTreeMap<String,Vec<u8>>,name:String,bytes:Vec<u8>)->String {
    scene.blobs.push(Blob{path:name.clone(),bytes:bytes.len()as u64,sha256:sha(&bytes)});
    files.insert(name.clone(),bytes);name
}

fn source_bytes(mip:u32,layer:u32)->Vec<u8> {
    // Literal finite IEEE754 values. Adjacent representable in-range values
    // discriminate a depth conversion from quantization or a constant clear.
    // No NaN, infinity, negative zero or subnormal portability claim is made.
    const BITS:[u32;12]=[0,0x3f800000,0x3f7fffff,0x3f000001,0x3eaaaaab,
        0x3e800000,0x3f400000,0xbf800000,0x40000000,0xc2f70000,0x43000000,0x3a800000];
    (0..(WIDTH>>mip)*(HEIGHT>>mip)).flat_map(|i| {
        BITS[((i+3*mip+5*layer)as usize)%BITS.len()].to_le_bytes()
    }).collect()
}

fn expected_depth(mip:u32,layer:u32)->Vec<u8> {
    // The CPU oracle preserves exact in-range bits and independently specifies
    // finite frag_depth clamping. The GPU test must establish this behavior on
    // the actual backend; it is not inferred from the replay's implementation.
    source_bytes(mip,layer).chunks_exact(4).flat_map(|word| {
        let bits=u32::from_le_bytes(word.try_into().unwrap());
        let v=f32::from_bits(bits);assert!(v.is_finite());
        if v<0.0 {0u32.to_le_bytes()} else if v>1.0 {1.0f32.to_le_bytes()} else {bits.to_le_bytes()}
    }).collect()
}

pub fn make(out:&Path)->Result<()> {
    let mut scene=Scene{schema:1,provenance:Provenance::Authored{description:
        "Independent finite Float32-to-Depth32 oracle: selected mip1/layer1 isolation, exact in-range bits and finite clamping, neighboring subresources retained and depth-to-color consumption, then six exact helper passes in native mip-major/layer-minor order with each checkpointed. This is authored replay API evidence; original native helper outputs require actual capture. NaN/infinity/subnormal behavior is outside this oracle; no native image, eye, finished scene or density claim.".into()},
        eye:None,blobs:vec![],textures:vec![],buffers:vec![],pipelines:vec![],compute_pipelines:vec![],
        commands:vec![],expected:vec![],original_submission_completed:true,original_gpu_errors:Some(0),omitted_commands:0};
    let mut files=BTreeMap::new();
    let mut unchanged=vec![];
    for (id,format,mutable) in [("source",TextureFormat::R32Float,false),
        ("depth",TextureFormat::Depth32Float,true),("sink",TextureFormat::R32Float,true)] {
        let aspect=if id=="depth" {Aspect::Depth}else{Aspect::Color};
        let mut initial=vec![];
        for mip in 0..MIPS {for layer in 0..LAYERS {
            let n=((WIDTH>>mip)*(HEIGHT>>mip))as usize;
            let bytes=if id=="source" {source_bytes(mip,layer)}else{
                let v=if id=="depth" {0.125+0.125*mip as f32+0.03125*layer as f32}else{-17.0-mip as f32-layer as f32};
                std::iter::repeat_n(v.to_le_bytes(),n).flatten().collect()
            };
            let name=blob(&mut scene,&mut files,format!("{id}-m{mip}-l{layer}-initial.bin"),bytes);
            initial.push(SliceInitial{mip,layer,aspect:aspect.clone(),content_version:1,origin:Initial::Captured{blob:name.clone()}});
            if id=="source" || mip!=1 || layer!=1 {
                let e=Expected{version:version(slice(id,mip,layer,aspect.clone()),1),blob:name};
                unchanged.push(e.clone());scene.expected.push(e);
            }
        }}
        scene.textures.push(Texture{id:id.into(),native_allocation_id:None,mutable_target:mutable,
            descriptor:TextureDesc{dimension:TextureDimension::D2,width:WIDTH,height:HEIGHT,layers:LAYERS,mips:MIPS,format,view_formats:vec![]},initial});
    }
    let source=slice("source",1,1,Aspect::Color);let depth=slice("depth",1,1,Aspect::Depth);let sink=slice("sink",1,1,Aspect::Color);
    let exact=blob(&mut scene,&mut files,"selected-finite-depth.bin".into(),expected_depth(1,1));
    scene.commands.push(Command::FloatToDepth{source:version(source,1),destination:transition(depth.clone(),1,2)});
    let depth_expected=Expected{version:version(depth.clone(),2),blob:exact.clone()};
    let mut checkpoint=unchanged;checkpoint.push(depth_expected.clone());
    scene.commands.push(Command::Checkpoint{expected:checkpoint});
    scene.commands.push(Command::DepthToColor{source:version(depth,2),destination:transition(sink.clone(),1,3)});
    let sink_expected=Expected{version:version(sink,3),blob:exact};
    scene.commands.push(Command::Checkpoint{expected:vec![depth_expected.clone(),sink_expected.clone()]});
    scene.expected.extend([depth_expected,sink_expected]);
    // After the isolation check, replay the entire dirty-helper loop in its
    // original mip-outer/layer-inner order. Each pass has its own retained
    // output reference. This qualifies API ordering, not original game output.
    scene.expected.retain(|e|e.version.slice.resource!="depth");
    for mip in 0..MIPS {for layer in 0..LAYERS {
        let before=if mip==1&&layer==1 {2}else{1};let after=4+(mip*LAYERS+layer)as u64;
        let d=slice("depth",mip,layer,Aspect::Depth);
        let name=blob(&mut scene,&mut files,format!("whole-loop-m{mip}-l{layer}-expected.bin"),expected_depth(mip,layer));
        scene.commands.push(Command::FloatToDepth{source:version(slice("source",mip,layer,Aspect::Color),1),
            destination:transition(d.clone(),before,after)});
        let e=Expected{version:version(d,after),blob:name};
        scene.commands.push(Command::Checkpoint{expected:vec![e.clone()]});scene.expected.push(e);
    }}
    fs::create_dir(out)?;
    for (name,bytes) in files {fs::write(out.join(name),bytes)?;}
    fs::write(out.join("scene.json"),serde_json::to_vec_pretty(&scene)?)?;
    let input=Input::load(&out.join("scene.json"))?;
    assert!(!input.targets_have_resolution_independent_initialization);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_depth_oracle_preserves_bits_and_clamps_only_outside_range() {
        let input=source_bytes(1,1);let expected=expected_depth(1,1);
        assert_eq!(input.len(),17*11*4);assert_eq!(input.len(),expected.len());
        let(mut low,mut high,mut exact)=(0,0,0);
        for (a,b) in input.chunks_exact(4).zip(expected.chunks_exact(4)) {
            let f=f32::from_le_bytes(a.try_into().unwrap());assert!(f.is_finite());
            if f<0.0 {low+=1;assert_eq!(b,&0u32.to_le_bytes());}
            else if f>1.0 {high+=1;assert_eq!(b,&1f32.to_le_bytes());}
            else {exact+=1;assert_eq!(a,b);}
        }
        assert!(low>0&&high>0&&exact>0);
        assert!(expected.chunks_exact(4).any(|v|v==0x3f000001u32.to_le_bytes()));
        assert!(include_str!("../../../src/gpu.rs").contains(crate::execute::FLOAT_TO_DEPTH_WGSL));
    }
    #[test]
    fn float_depth_rejects_false_aspects_ranges_versions_and_provenance()->Result<()> {
        let dir=std::env::temp_dir().join(format!("float-depth-oracle-{}",std::process::id()));
        make(&dir)?;let base=Input::load(&dir.join("scene.json"))?.scene;
        let reject=|name:&str,change:&dyn Fn(&mut Scene)|->Result<()> {
            let mut s=base.clone();change(&mut s);fs::write(dir.join("negative.json"),serde_json::to_vec(&s)?)?;
            assert!(Input::load(&dir.join("negative.json")).is_err(),"accepted {name}");Ok(())
        };
        reject("stale source",&|s|if let Command::FloatToDepth{source,..}=&mut s.commands[0]{source.content_version=0;})?;
        reject("source depth aspect",&|s|if let Command::FloatToDepth{source,..}=&mut s.commands[0]{source.slice.aspect=Aspect::Depth;})?;
        reject("source stencil aspect",&|s|if let Command::FloatToDepth{source,..}=&mut s.commands[0]{source.slice.aspect=Aspect::Stencil;})?;
        reject("source mip overflow",&|s|if let Command::FloatToDepth{source,..}=&mut s.commands[0]{source.slice.mip=MIPS;})?;
        reject("source layer overflow",&|s|if let Command::FloatToDepth{source,..}=&mut s.commands[0]{source.slice.layer=LAYERS;})?;
        reject("source uint format",&|s|s.textures[0].descriptor.format=TextureFormat::R32Uint)?;
        reject("source wrong dimension",&|s|s.textures[0].descriptor.dimension=TextureDimension::D3)?;
        reject("destination color aspect",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.slice.aspect=Aspect::Color;})?;
        reject("destination stencil aspect",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.slice.aspect=Aspect::Stencil;})?;
        reject("destination format has stencil",&|s|s.textures[1].descriptor.format=TextureFormat::Depth32FloatStencil8)?;
        reject("destination immutable",&|s|s.textures[1].mutable_target=false)?;
        reject("destination stale",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.before=0;})?;
        reject("destination not advancing",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.after=1;})?;
        reject("destination extent differs",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.slice.mip=0;})?;
        reject("destination layer differs from native loop",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.slice.layer=0;})?;
        reject("destination layer overflow",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.slice.layer=LAYERS;})?;
        reject("read/write allocation alias",&|s|if let Command::FloatToDepth{destination,..}=&mut s.commands[0]{destination.slice.resource="source".into();})?;
        reject("reordered consuming depth",&|s|s.commands.swap(0,2))?;
        reject("plain native provenance",&|s|s.provenance=Provenance::Native{source_sha256:"f".repeat(64)})?;
        reject("legacy provenance",&|s|s.provenance=Provenance::LegacyCaptured{manifest_sha256:"f".repeat(64)})?;
        let mut unknown=base;
        unknown.textures[1].initial.iter_mut().find(|i|i.mip==1&&i.layer==1).unwrap().origin=Initial::Unknown{reason:"uninitialized native proxy overwritten by full pass".into()};
        fs::write(dir.join("unknown.json"),serde_json::to_vec(&unknown)?)?;
        assert!(!Input::load(&dir.join("unknown.json"))?.targets_have_resolution_independent_initialization);
        fs::remove_dir_all(dir)?;Ok(())
    }
}
