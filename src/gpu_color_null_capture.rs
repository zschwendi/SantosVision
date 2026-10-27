//! One private original draw, with real material and the actual native dummy.
//! This is a pixel-proof capture, never a world-image publication candidate.
use super::*;

pub(super) const VS:&str="4e1d5d361e0ed0bc";
pub(super) const PS:&str="7f531ca66548aefe";
const VS_TEXT:&str="6153fb6404c740ac049805cd05964a8e1ae36860f3f3f0c3e344e51335b8e538";
const PS_TEXT:&str="21bd6330cd32748471b4f917ff7e3a3ea99838978391a2eafd096eb58f093e2d";

pub(super) fn dummy_copyable(dimension:TextureViewDimension,sample:Sample,multisampled:bool)->bool {
    !multisampled && dimension==TextureViewDimension::D2 && sample==Sample::Float
}

// Copy exactly the resolved nonindexed vertex range, with four-byte copy
// alignment. A large shared streaming buffer is not itself a draw input range.
fn vertex_range(offset:u64,size:u64,stride:u64,last:u64,vertices:u64)->Result<(u64,u64,u64),String> {
    if last==0 || vertices==0 {return Err("empty resolved vertex range".into());}
    let needed=(vertices-1).checked_mul(stride).and_then(|n|n.checked_add(last)).ok_or("vertex range overflow")?;
    let end=offset.checked_add(needed).ok_or("vertex end overflow")?;
    let first=offset&!3;
    let end=end.checked_add(3).ok_or("vertex alignment overflow")?&!3;
    if end>size || end<=first {return Err("resolved vertex range exceeds buffer".into());}
    Ok((first,end-first,offset-first))
}

fn texture_binding(name:&str,texture:&Texture,view:TextureViewDescriptor<'_>,plans:&mut Vec<Plan>)->Result<Value,String> {
    if texture.dimension()!=TextureDimension::D2 || texture.sample_count()!=1 || texture.depth_or_array_layers()!=1
        || texture.mip_level_count()==0 || texture.mip_level_count()>16 || is_depth(texture.format()) {
        return Err("color-null sampled texture shape unsupported".into());
    }
    let mut subresources=Vec::new();
    for mip in 0..texture.mip_level_count() {
        let blob=format!("{name}-mip{mip}.bin");let plan=texture_plan(blob.clone(),texture,mip,TextureAspect::All)?;
        subresources.push(json!({"mip_level":mip,"layer":0,"width":(texture.width()>>mip).max(1),
            "height":(texture.height()>>mip).max(1),"packed_row_bytes":plan.row,"blob":blob}));plans.push(plan);
    }
    Ok(json!({"kind":"texture","descriptor":{"width":texture.width(),"height":texture.height(),
        "depth_or_array_layers":1,"mip_level_count":texture.mip_level_count(),"sample_count":1,
        "dimension":texture.dimension(),"format":texture.format()},"view":view,"subresources":subresources}))
}

impl Capture {
    pub(super) fn prepare_color_null(&self,gpu:&Gpu,d:&Resolved<'_>)->Result<Prepared,String> {
        let ps=d.ps.ok_or("color-null pixel program absent")?;
        if !gpu.bind_group_prepare.enabled || gpu.errors.load(Ordering::SeqCst)!=0
            || gpu.occlusion_active.is_some() || gpu.occlusion_pass_slot.is_some() || !gpu.pending_resolve.is_empty() {
            return Err("color-null capture requires clean resolved bindings and inactive queries".into());
        }
        if d.kind!=0 || d.count!=6 || d.instances!=1 || d.start!=0 || d.base_vertex!=0 || d.start_instance!=0
            || !d.unclamped || gpu.s.topo!=4 || d.index.is_some() || d.vs.vid || d.vs.iid || d.vs.clip
            || !d.vs.info.bindings.is_empty() || d.groups[0].is_some() || ps.info.writes_fragment_depth
            || !ps.flat.is_empty() || gpu.inter_stage_remap(d.vs,Some(ps)).is_some() || gpu.hud_draw.is_some()
            || sha(d.vs.text.as_bytes())!=VS_TEXT || sha(ps.text.as_bytes())!=PS_TEXT {
            return Err("exact color-null original shader/draw contract not qualified".into());
        }
        let pi=gpu.pass_info.as_ref().ok_or("color-null native pass missing")?;
        if pi.width==0 || pi.height==0 || pi.width>4096 || pi.height>4096 || pi.samples!=1
            || pi.rt_formats!=vec![Some(TextureFormat::Bgra8Unorm)] || pi.ds_format!=Some(TextureFormat::Depth32FloatStencil8)
            || d.viewport!=[0.,0.,pi.width as f32,pi.height as f32,0.,1.] || d.scissor!=[0,0,pi.width,pi.height] {
            return Err("color-null full BGRA/D32S8 pass not qualified".into());
        }
        let target=&gpu.s.rt[0];let color=gpu.tex(target.tex).ok_or("color-null target absent")?;
        let ds=&gpu.s.ds;let depth=gpu.tex(ds.tex).ok_or("color-null bound depth absent")?;
        if target.mip!=0 || target.slice!=0 || ds.mip!=0 || ds.slice!=0 || color.tex==depth.tex
            || color.format!=TextureFormat::Bgra8Unorm || depth.format!=TextureFormat::Depth32FloatStencil8
            || [color,depth].iter().any(|t|t.desc.w!=pi.width || t.desc.h!=pi.height || t.desc.d!=1 || t.desc.samples!=1 || t.desc.dim!=TextureDimension::D2) {
            return Err("color-null attachment identity/shape unsupported".into());
        }
        let attached=color.views.get(&(TextureFormat::Bgra8Unorm,0,0)).ok_or("retained color attachment view absent")?;
        let cause=self.color_null_binding.borrow();let cause=cause.as_ref().ok_or("actual color-null binding cause absent")?;
        if gpu.s.srv[1][5]!=0 || cause.parent!=color.tex || attached.texture()!=&cause.parent
            || !contains_subresource(cause.mips,cause.layers,0,0) || cause.raw!=[87,4,0,1,0,0] {
            return Err("color-null draw no longer matches retained native alias cause".into());
        }
        let actual=ps.group_bindings.get(&1).ok_or("color-null pixel group absent")?;
        let expected=[(0,Kind::Uniform),(18,Kind::Sampler),(21,Kind::Sampler),(34,Kind::Texture),(37,Kind::Texture)];
        if ps.info.bindings.len()!=expected.len() || actual.len()!=expected.len()
            || !actual.iter().zip(expected).all(|(b,(binding,kind))|b.binding==binding&&b.kind==kind) {
            return Err("color-null resolved shader binding contract changed".into());
        }
        let group=d.groups[1].ok_or("color-null actual group absent")?;
        let (key,_)=gpu.bind_groups.iter().find(|(_,(_,uid))|*uid==group.uid).ok_or("color-null group cache missing")?;
        let material_id=gpu.s.srv[1][2];
        if key.len()!=9 || key[1]!=ps.uid || key[2]!=1 || material_id==0 || key[6]!=material_id as u64 || key[7]!=0 || key[8]!=0 {
            return Err("color-null real t2 and dummy t5 resolved key not exact".into());
        }
        let (pipeline_key,_)=gpu.pipelines.iter().find(|(_,entry)|matches!(entry,PipeEntry::Done(Some(pl)) if pl.uid==d.pipeline.uid))
            .ok_or("color-null resolved pipeline cache missing")?;
        let pipeline=self.pipelines.get(pipeline_key).ok_or("color-null actual pipeline descriptor missing")?.clone();
        let mut plans=Vec::new();let mut vertices=Vec::new();
        for (slot,buffer,offset,id) in d.vertices {
            let requirement=d.pipeline.vb_req.iter().find(|r|r.slot==*slot).ok_or("color-null resolved VB requirement absent")?;
            let count=if requirement.instance {1}else{6};
            let (first,size,normalized)=vertex_range(*offset,buffer.size(),requirement.stride,requirement.last,count)?;
            let blob=format!("draw0-vb{slot}.bin");plans.push(buffer_plan(blob.clone(),buffer,first,size)?);
            vertices.push(json!({"slot":slot,"offset":normalized,"blob":blob,"guest_id":id,"size":size,
                "original_binding_offset":offset,"original_buffer_size":buffer.size(),"copied_source_offset":first,
                "renderer_zero_buffer":*id==u64::MAX}));
        }
        if vertices.is_empty() {return Err("color-null resolved geometry absent".into());}
        let mut bindings=Vec::new();
        for b in actual {
            let mut layout=Gpu::layout_entry(b,1);
            let mut entry=match b.kind {
                Kind::Uniform=>{
                    let pack=ps.pack.as_ref().ok_or("color-null packed constants missing")?;
                    let offset=group.dyn_off.ok_or("color-null resolved uniform offset missing")? as u64;
                    let (buffer,_)=gpu.ubo_shared.iter().find(|(_,id)|*id==key[3]).ok_or("color-null resolved uniform ring missing")?;
                    let blob="draw0-group1-ubo.bin".to_owned();plans.push(buffer_plan(blob.clone(),buffer,offset,pack.size)?);
                    if let BindingType::Buffer{has_dynamic_offset,..}=&mut layout.ty {*has_dynamic_offset=false;}
                    json!({"kind":"uniform","blob":blob,"offset":0,"size":pack.size,
                        "original_dynamic_offset":offset,"original_ring_id":key[3],
                        "pack_members":pack.members.iter().map(|m|json!({"binding":m.binding,"offset":m.offset,"size":m.size})).collect::<Vec<_>>()})
                }
                Kind::Sampler=>{
                    let slot=(b.binding-16)as usize;let id=gpu.s.smp[1][slot];
                    let state=gpu.state(id).ok_or("color-null actual sampler state missing")?;
                    let position=if b.binding==18 {4}else{5};
                    if key[position]!=id as u64 || state.sampler.is_none() || state.words.first().copied().unwrap_or(0)&0x80!=0 {
                        return Err("color-null fallback/comparison sampler unsupported".into());
                    }
                    json!({"kind":"sampler","descriptor":Gpu::sampler_descriptor(&state.words),"guest_words":state.words})
                }
                Kind::Texture=>{
                    if b.dim!=TextureViewDimension::D2 || b.ms || b.sample!=Sample::Float {return Err("color-null texture declaration unsupported".into());}
                    if b.binding==37 {
                        let key=format!("{:?}|{:?}|{}",b.dim,b.sample,b.ms);
                        let dummy=gpu.dummy_tex.get(&key).ok_or("actual color-null dummy view missing")?;
                        let texture=dummy.texture();
                        if texture==&color.tex || texture==&depth.tex || texture.width()!=1 || texture.height()!=1
                            || texture.format()!=TextureFormat::Rgba8Unorm || texture.mip_level_count()!=1 {
                            return Err("actual color-null dummy allocation unsupported".into());
                        }
                        let mut entry=texture_binding("draw0-dummy5",texture,TextureViewDescriptor{dimension:Some(TextureViewDimension::D2),..Default::default()},&mut plans)?;
                        entry["native_dummy"]=true.into();entry["native_parent_differs_from_output"]=true.into();entry
                    }else{
                        let view=gpu.view_obj(material_id).filter(|v|!v.is_buf).ok_or("color-null actual material view missing")?;
                        let texture=gpu.tex(view.res).ok_or("color-null material allocation missing")?;
                        let cached=view.views.get("D2").ok_or("color-null resolved material view missing")?;
                        if cached.texture()!=&texture.tex || texture.tex==color.tex || texture.tex==depth.tex || view.c!=0
                            || !matches!(view.vdim,4|6) || texture.desc.mips==0 {return Err("color-null material parent/slice unsupported".into());}
                        let total=texture.desc.mips;let count=if view.b==0||view.b==u32::MAX {total.saturating_sub(view.a)}else{view.b};
                        let mut vd=TextureViewDescriptor{dimension:Some(TextureViewDimension::D2),base_mip_level:view.a.min(total-1),
                            mip_level_count:Some(count.min(total.saturating_sub(view.a)).max(1)),base_array_layer:0,array_layer_count:Some(1),..Default::default()};
                        if view.fmt!=0 {
                            let original=map_format(view.fmt,0).format;
                            let mapped=if texture.bc_source.is_some(){crate::bc::decoded_format(original).unwrap_or(original)}else{original};
                            if mapped!=texture.format && mapped.is_srgb()!=texture.format.is_srgb() && mapped.remove_srgb_suffix()==texture.format.remove_srgb_suffix() {vd.format=Some(mapped);}
                        }
                        let mut entry=texture_binding("draw0-material2",&texture.tex,vd,&mut plans)?;
                        entry["guest_id"]=view.res.into();entry["native_dummy"]=false.into();entry
                    }
                }
                _=>return Err("color-null resource unsupported".into()),
            };
            entry["group"]=1.into();entry["binding"]=b.binding.into();entry["layout"]=json!(layout);bindings.push(entry);
        }
        let targets=vec![(color.tex.clone(),TextureAspect::All,"color0".into()),(depth.tex.clone(),TextureAspect::DepthOnly,"depth".into()),(depth.tex.clone(),TextureAspect::StencilOnly,"stencil".into())];
        let domain=json!({"width":pi.width,"height":pi.height,"depth_flags":ds.flags,
            "attachments":[{"role":"color0","guest_id":target.tex,"format":color.format,"mip":0,"slice":0},
                {"role":"depth_stencil","guest_id":ds.tex,"format":depth.format,"mip":0,"slice":0}],
            "retained_native_parents_compared":true});
        let view=gpu.world_view.map(|v|json!({"provenance_epoch":v.provenance_epoch,"camera_sequence":v.camera_sequence,
            "request_frame":v.request_frame,"head_pose_frame":v.head_pose_frame,"eye_index":v.eye_index,"view_count":v.view_count,
            "presentation_time_seconds":v.presentation_time_seconds,"game_from_eye":v.game_from_eye,
            "projection_from_eye":v.projection_from_eye,"headset_applied":v.headset_applied,"depth_reverse_z":v.depth_reverse_z})).unwrap_or(Value::Null);
        let alias=json!({"group":1,"binding":37,"target_role":"color0","mip":0,"layer":0,
            "native_parent_matches_retained_null_cause":true,"dummy_parent_differs_from_output":true,
            "retained_cause":{"command_sequence":cause.sequence,"opcode":cause.opcode,"view":cause.view,"resource":cause.resource,
                "raw_view":cause.raw,"mips":cause.mips,"layers":cause.layers},"resolved_group_uid":group.uid,"resolved_group_key":key,
            "view":TextureViewDescriptor{format:Some(TextureFormat::Bgra8Unorm),dimension:Some(TextureViewDimension::D2),mip_level_count:Some(1),array_layer_count:Some(1),..Default::default()}});
        let draw=json!({"draw":{"kind":0,"count":6,"instances":1,"start_vertex":0,"start_instance":0},
            "command_sequence":self.sequence,"vertex_hash":VS,"pixel_hash":PS,"vertex_wgsl":d.vs.text,"pixel_wgsl":ps.text,
            "vertex_wgsl_sha256":VS_TEXT,"pixel_wgsl_sha256":PS_TEXT,"pipeline":pipeline,"bindings":bindings,"vertex_buffers":vertices,
            "viewport":d.viewport,"scissor":d.scissor,"blend_constant":d.blend,"stencil_reference":d.stencil,
            "world_view":view,"target_domain":domain,"native_source_phase":if view.is_null(){"unattributed native draw"}else{"retained native world view"}});
        Ok(Prepared{draw,view,domain,targets,plans,alias:Some(alias)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_vertex_bytes_preserve_alignment_and_original_binding_offset() {
        assert_eq!(vertex_range(14_625_256,16<<20,36,36,6).unwrap(),(14_625_256,216,0));
        assert_eq!(vertex_range(3,40,5,3,6).unwrap(),(0,32,3));
        assert_eq!(vertex_range(4,8,0,4,6).unwrap(),(4,4,0));
        for args in [(0,4,4,4,2),(u64::MAX,64,1,1,1),(0,64,u64::MAX,1,6),(0,64,1,0,6)] {
            assert!(vertex_range(args.0,args.1,args.2,args.3,args.4).is_err());
        }
    }
    #[test]
    fn explicit_capture_kind_preserves_original_default_and_rejects_unknown() {
        assert_eq!(CaptureKind::parse(None),Some(CaptureKind::TwoDraw));
        assert_eq!(CaptureKind::parse(Some("two-draw")),Some(CaptureKind::TwoDraw));
        assert_eq!(CaptureKind::parse(Some("color-null")),Some(CaptureKind::ColorNull));
        for value in ["", "color_null", "unknown"] {assert_eq!(CaptureKind::parse(Some(value)),None);}
        assert_eq!(CaptureKind::TwoDraw.draw_count(),2);assert_eq!(CaptureKind::ColorNull.draw_count(),1);
    }
    #[test]
    fn only_exact_single_sample_float_2d_dummy_gets_copy_usage() {
        assert!(dummy_copyable(TextureViewDimension::D2,Sample::Float,false));
        for dimension in [TextureViewDimension::D1,TextureViewDimension::D2,TextureViewDimension::D2Array,
            TextureViewDimension::D3,TextureViewDimension::Cube,TextureViewDimension::CubeArray] {
            assert!(!dummy_copyable(dimension,Sample::Float,true));
            for sample in [Sample::Uint,Sample::Sint,Sample::Depth] {assert!(!dummy_copyable(dimension,sample,false));}
            if dimension!=TextureViewDimension::D2 {assert!(!dummy_copyable(dimension,Sample::Float,false));}
        }
    }
}
