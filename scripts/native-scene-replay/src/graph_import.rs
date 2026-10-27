//! One native capture/namespace; downstream resources are references, never reuploads.
use crate::{model::*, require, sha, validate::{aspects, Input, MAX_BYTES}, Result};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Component, Path, PathBuf}};
use wgpu_types::*;

type Key = (Slice, u64);
fn key(v: &Version) -> Key { (v.slice.clone(), v.content_version) }
fn get<'a>(v: &'a Value, k: &str) -> Result<&'a Value> { v.get(k).ok_or_else(|| format!("graph missing {k}").into()) }
fn num(v: &Value, k: &str) -> Result<u64> { get(v,k)?.as_u64().ok_or_else(|| format!("graph invalid integer {k}").into()) }
fn txt<'a>(v: &'a Value,k:&str)->Result<&'a str>{get(v,k)?.as_str().ok_or_else(||format!("graph invalid string {k}").into())}
fn array<'a>(v:&'a Value,k:&str)->Result<&'a Vec<Value>>{get(v,k)?.as_array().ok_or_else(||format!("graph invalid array {k}").into())}
fn shape(v:&Value,keys:&[&str])->Result<()> {
    require(v.as_object().is_some_and(|o|o.keys().all(|k|keys.contains(&k.as_str()))),"unmodelled graph field")
}
fn version(v:&Value)->Result<Version>{
    let a=num(v,"allocation_id")?;let n=num(v,"content_version")?;let s=get(v,"subresource")?;
    require(a>0&&n>0,"graph native version identity absent")?;
    Ok(Version{slice:Slice{resource:format!("native-texture:{a}"),mip:num(s,"mip")?.try_into()?,layer:num(s,"layer")?.try_into()?,aspect:serde_json::from_value(get(s,"aspect")?.clone())?},content_version:n})
}
fn descriptor(v:&Value)->Result<TextureDesc>{
    shape(v,&["width","height","depth_or_array_layers","mip_level_count","sample_count","dimension","format"])?;
    require(num(v,"sample_count")?==1,"graph proxy sample count")?;
    Ok(TextureDesc{dimension:serde_json::from_value(get(v,"dimension")?.clone())?,width:num(v,"width")?.try_into()?,height:num(v,"height")?.try_into()?,layers:num(v,"depth_or_array_layers")?.try_into()?,mips:num(v,"mip_level_count")?.try_into()?,format:serde_json::from_value(get(v,"format")?.clone())?,view_formats:vec![]})
}
fn same_descriptor(a:&TextureDesc,b:&TextureDesc)->bool{
    a.dimension==b.dimension&&a.width==b.width&&a.height==b.height&&a.layers==b.layers&&a.mips==b.mips&&a.format==b.format
}
fn origin_blob(o:&Initial)->Option<&str>{match o{Initial::Captured{blob}=>Some(blob),Initial::UniformClear{reference_blob:Some(blob),..}=>Some(blob),_=>None}}
fn same_blob(a:&str,b:&str,blobs:&BTreeMap<String,Blob>)->Result<bool>{
    let a=blobs.get(a).ok_or("graph blob absent")?;let b=blobs.get(b).ok_or("graph blob absent")?;
    Ok(a.bytes==b.bytes&&a.sha256==b.sha256)
}
struct Event { ordinal:u64, command:Command }
struct Assembly {
    scene:Scene,
    events:BTreeMap<u64,Event>,
    candidates:BTreeMap<Slice,Vec<SliceInitial>>,
    blobs:BTreeMap<String,Blob>,
    elided:Vec<Value>,
    declared_loads:BTreeMap<u64,Vec<Version>>,
    proxy_sources:Vec<Key>,
}
impl Assembly {
    fn new(source_hash:String,blobs:BTreeMap<String,Blob>)->Self {
        Self {scene:Scene{schema:1,provenance:Provenance::NativeGraph{source_sha256:source_hash},eye:None,blobs:blobs.values().cloned().collect(),textures:vec![],buffers:vec![],pipelines:vec![],compute_pipelines:vec![],commands:vec![],expected:vec![],original_submission_completed:true,original_gpu_errors:Some(0),omitted_commands:0},events:BTreeMap::new(),candidates:BTreeMap::new(),blobs,elided:vec![],declared_loads:BTreeMap::new(),proxy_sources:vec![]}
    }
    fn event(&mut self,index:u64,ordinal:u64,command:Command)->Result<()> {
        require(index>0&&index<1<<20&&ordinal>0&&self.events.len()<8192,"graph event bound")?;
        require(self.events.insert(index,Event{ordinal,command}).is_none(),"duplicate graph event index")
    }
    fn texture(&mut self,t:Texture)->Result<()> {
        let id=t.native_allocation_id.ok_or("graph texture missing native identity")?;
        require(id>0&&t.id==format!("native-texture:{id}"),"graph texture identity changed")?;
        for i in &t.initial {
            let s=Slice{resource:t.id.clone(),mip:i.mip,layer:i.layer,aspect:i.aspect.clone()};
            self.candidates.entry(s).or_default().push(i.clone());
        }
        if let Some(old)=self.scene.textures.iter_mut().find(|o|o.id==t.id) {
            require(old.native_allocation_id==t.native_allocation_id&&same_descriptor(&old.descriptor,&t.descriptor),"graph allocation descriptor changed")?;
            old.mutable_target|=t.mutable_target;
            for f in t.descriptor.view_formats {if !old.descriptor.view_formats.contains(&f){old.descriptor.view_formats.push(f);}}
        } else {let mut t=t;t.initial.clear();self.scene.textures.push(t);}
        Ok(())
    }
    fn record_loads(&mut self,stage:&Scene,events:&[u64])->Result<()> {
        let mut current=BTreeMap::new();
        for t in &stage.textures{for i in &t.initial{current.insert(Slice{resource:t.id.clone(),mip:i.mip,layer:i.layer,aspect:i.aspect.clone()},i.content_version);}}
        require(events.len()==stage.commands.len(),"graph stage event arity")?;
        for (command,event) in stage.commands.iter().zip(events){
            if let Command::Draw{draw}=command{
                let mut slices=vec![];
                for a in draw.colors.iter().flatten(){slices.push(Slice{resource:a.resource.clone(),mip:a.mip,layer:a.layer,aspect:Aspect::Color});}
                if let Some(ds)=&draw.depth{let a=&ds.attachment;let t=stage.textures.iter().find(|t|t.id==a.resource).ok_or("graph depth allocation absent")?;
                    for aspect in aspects(t.descriptor.format){slices.push(Slice{resource:a.resource.clone(),mip:a.mip,layer:a.layer,aspect});}}
                for slice in slices{let version=*current.get(&slice).ok_or("graph attachment initial version absent")?;self.declared_loads.entry(*event).or_default().push(Version{slice,content_version:version});}
                for t in &draw.writes{require(current.get(&t.slice)==Some(&t.before),"graph stage draw predecessor mismatch")?;current.insert(t.slice.clone(),t.after);}
            }
        }
        Ok(())
    }
    fn resources(&mut self,mut stage:Scene,index:usize)->Result<Vec<Command>> {
        for t in stage.textures {self.texture(t)?;}
        for b in stage.buffers {
            if let Some(old)=self.scene.buffers.iter().find(|v|v.id==b.id){
                require(old.native_allocation_id==b.native_allocation_id&&old.content_version==b.content_version
                    &&old.source_offset==b.source_offset&&same_blob(&old.blob,&b.blob,&self.blobs)?,"graph immutable buffer snapshot changed")?;
            } else {self.scene.buffers.push(b);}
        }
        let mut rename=BTreeMap::new();
        for mut p in stage.pipelines {let old=p.id.clone();p.id=format!("graph:{index}:{}",p.id);rename.insert(old,p.id.clone());self.scene.pipelines.push(p);}
        for mut p in stage.compute_pipelines {let old=p.id.clone();p.id=format!("graph:{index}:{}",p.id);rename.insert(old,p.id.clone());self.scene.compute_pipelines.push(p);}
        for c in &mut stage.commands {match c{
            Command::Draw{draw}=>draw.pipeline=rename.get(&draw.pipeline).ok_or("graph draw pipeline absent")?.clone(),
            Command::Dispatch{dispatch}=>dispatch.pipeline=rename.get(&dispatch.pipeline).ok_or("graph compute pipeline absent")?.clone(),
            _=>return Err("unexpected imported stage command".into()),
        }}
        Ok(stage.commands)
    }
    fn finish(mut self)->Result<(Scene,Vec<Value>)>{
        // The capture ledger has one positive content-version namespace for textures.
        // Reusing a number for a different allocation/aspect is contradictory evidence.
        let mut native_versions=BTreeMap::<u64,Slice>::new();
        let mut register=|v:&Version|->Result<()>{
            if v.content_version>0 {if let Some(old)=native_versions.insert(v.content_version,v.slice.clone()) {
                require(old==v.slice,"graph content version names different subresources")?;
            }} Ok(())
        };
        for (slice,candidates) in &self.candidates{for i in candidates{register(&Version{slice:slice.clone(),content_version:i.content_version})?;}}
        let mut producers=BTreeMap::<Key,u64>::new();let mut refs=BTreeMap::<Key,String>::new();let mut uses=BTreeMap::<Key,u64>::new();
        for (&event,loads) in &self.declared_loads{for v in loads{register(v)?;uses.entry(key(v)).and_modify(|old|*old=(*old).min(event)).or_insert(event);}}
        let mut ordinal=0;
        for (&event,e) in &self.events {
            require(e.ordinal>=ordinal,"graph guest command sequence reversed")?;ordinal=e.ordinal;
            let mut reads=vec![];
            let transitions:&[Transition]=match &e.command{
                Command::Draw{draw}=>{
                    for b in &draw.bindings {if let BindingValue::Texture{versions,..}=&b.value{reads.extend(versions.clone());}}
                    &draw.writes
                },
                Command::Dispatch{dispatch}=>{for b in &dispatch.bindings{if let BindingValue::Texture{versions,..}=&b.value{reads.extend(versions.clone());}} &dispatch.writes},
                Command::DepthToColor{source,destination}=>{reads.push(source.clone());std::slice::from_ref(destination)},
                Command::Checkpoint{expected}=>{
                    for x in expected {let k=key(&x.version);if let Some(old)=refs.get(&k){require(same_blob(old,&x.blob,&self.blobs)?,"graph checkpoint bytes contradict same version")?;}else{refs.insert(k,x.blob.clone());} reads.push(x.version.clone());}
                    &[]
                },
                _=>return Err("unmodelled graph command".into()),
            };
            for t in transitions {
                reads.push(Version{slice:t.slice.clone(),content_version:t.before});
                register(&Version{slice:t.slice.clone(),content_version:t.after})?;
                require(producers.insert((t.slice.clone(),t.after),event).is_none(),"duplicate graph content producer")?;
            }
            for r in reads {register(&r)?;uses.entry(key(&r)).and_modify(|old|*old=(*old).min(event)).or_insert(event);}
        }
        for source in &self.proxy_sources{require(producers.contains_key(source),"graph proxy source was not produced in captured raster")?;}
        for (slice,candidates) in &self.candidates {
            let mut selected:Option<SliceInitial>=None;
            for initial in candidates {
                if initial.content_version==0&&matches!(initial.origin,Initial::Unknown{..}){continue;}
                let k=(slice.clone(),initial.content_version);
                if let Some(producer)=producers.get(&k) {
                    require(uses.get(&k).is_some_and(|use_event|producer<use_event),"graph input consumed before its producer")?;
                    let reference=refs.get(&k).ok_or("graph produced input lacks original checkpoint")?;
                    let blob=origin_blob(&initial.origin).ok_or("graph produced input has no captured reference")?;
                    require(same_blob(reference,blob,&self.blobs)?,"graph produced input differs from preceding output")?;
                    self.elided.push(json!({"version":{"slice":slice,"content_version":initial.content_version},"original_input_blob":blob,"producer_reference_blob":reference,"producer_event":producer,"reuploaded":false}));
                    continue;
                }
                if let Some(old)=&selected {
                    require(old.content_version==initial.content_version,"unmodelled graph texture version change")?;
                    match (origin_blob(&old.origin),origin_blob(&initial.origin)) {
                        (Some(a),Some(b))=>require(same_blob(a,b,&self.blobs)?,"graph immutable initial bytes changed")?,
                        (None,Some(_))=>selected=Some(initial.clone()),
                        (Some(_),None)=>{},
                        (None,None)=>{},
                    }
                }else{selected=Some(initial.clone());}
            }
            let init=selected.unwrap_or(SliceInitial{mip:slice.mip,layer:slice.layer,aspect:slice.aspect.clone(),content_version:0,origin:Initial::Unknown{reason:"No pre-graph initialization captured for this unused subresource".into()}});
            self.scene.textures.iter_mut().find(|t|t.id==slice.resource).ok_or("graph initializer allocation absent")?.initial.push(init);
        }
        let mut last_refs=BTreeMap::<Slice,Expected>::new();
        let mut current=BTreeMap::<Slice,u64>::new();
        for t in &self.scene.textures{for i in &t.initial{current.insert(Slice{resource:t.id.clone(),mip:i.mip,layer:i.layer,aspect:i.aspect.clone()},i.content_version);}}
        for (index,e) in self.events {
            if let Some(loads)=self.declared_loads.get(&index){for v in loads{require(current.get(&v.slice)==Some(&v.content_version),"graph attachment load changed version")?;}}
            let writes:&[Transition]=match &e.command{Command::Draw{draw}=>&draw.writes,Command::Dispatch{dispatch}=>&dispatch.writes,Command::DepthToColor{destination,..}=>std::slice::from_ref(destination),_=>&[]};
            for w in writes{require(current.get(&w.slice)==Some(&w.before),"graph transition missing preceding owner")?;current.insert(w.slice.clone(),w.after);}
            if let Command::Checkpoint{expected}=&e.command{for x in expected{require(current.get(&x.version.slice)==Some(&x.version.content_version),"graph checkpoint is not current")?;last_refs.insert(x.version.slice.clone(),x.clone());}}
            self.scene.commands.push(e.command);
        }
        self.scene.expected=last_refs.into_values().filter(|e|current.get(&e.version.slice)==Some(&e.version.content_version)).collect();
        require(!self.scene.expected.is_empty(),"graph final checkpoint missing")?;
        Ok((self.scene,self.elided))
    }
}

fn validate_buffer_overlaps(scene:&Scene,payload:&[(PathBuf,Vec<u8>)])->Result<()> {
    let bytes=payload.iter().map(|(p,b)|(p.to_string_lossy().to_string(),b.as_slice())).collect::<BTreeMap<_,_>>();
    let mut groups=BTreeMap::<(u64,u64),Vec<&Buffer>>::new();
    for b in &scene.buffers {groups.entry((b.native_allocation_id.ok_or("graph buffer native identity")?,b.content_version)).or_default().push(b);}
    for group in groups.values_mut(){
        group.sort_by_key(|b|b.source_offset);
        for (i,a) in group.iter().enumerate(){
            let ab=bytes.get(&a.blob).ok_or("graph buffer payload missing")?;
            let end=a.source_offset.checked_add(ab.len() as u64).ok_or("graph buffer source range overflow")?;
            for b in &group[i+1..]{
                if b.source_offset>=end{break;}
                let bb=bytes.get(&b.blob).ok_or("graph buffer payload missing")?;
                let b_end=b.source_offset.checked_add(bb.len() as u64).ok_or("graph buffer source range overflow")?;
                let start=a.source_offset.max(b.source_offset);let stop=end.min(b_end);
                if start<stop{
                    let ao:usize=(start-a.source_offset).try_into()?;let bo:usize=(start-b.source_offset).try_into()?;let n:usize=(stop-start).try_into()?;
                    require(ab[ao..ao+n]==bb[bo..bo+n],"same native buffer version has contradictory overlapping bytes")?;
                }
            }
        }
    }Ok(())
}
fn referenced_blobs(v:&Value,out:&mut BTreeSet<String>)->Result<()> {
    match v{
        Value::Object(o)=>for (k,v) in o{if ["blob","initial_blob","final_blob"].contains(&k.as_str()){
            out.insert(v.as_str().ok_or("graph blob reference not a string")?.into());
        }else{referenced_blobs(v,out)?;}},
        Value::Array(a)=>for v in a{referenced_blobs(v,out)?;},_=>{},
    }Ok(())
}
fn stage_payload(c:&mut Value,rows:&BTreeMap<String,Value>)->Result<u64>{
    require(c.get("blobs").is_none(),"nested graph payload table forbidden")?;
    let mut names=BTreeSet::new();referenced_blobs(c,&mut names)?;
    let mut sum=0u64;let mut selected=vec![];
    for n in names{let b=rows.get(&n).ok_or("graph stage references absent outer blob")?;
        sum=sum.checked_add(num(b,"readback_pitch")?.checked_mul(num(b,"row_count")?).ok_or("graph reference footprint overflow")?).ok_or("graph reference footprint overflow")?;selected.push(b.clone());}
    require(sum<=MAX_BYTES,"graph stage reference footprint bound")?;
    c["blobs"]=selected.into();Ok(sum)
}
fn proxy_texture(id:&Version,d:TextureDesc,origin:Initial,mutable:bool)->Texture{
    let initial=aspects(d.format).into_iter().map(|aspect|SliceInitial{mip:0,layer:0,content_version:if aspect==id.slice.aspect{id.content_version}else{0},origin:if aspect==id.slice.aspect{origin.clone()}else{Initial::Unknown{reason:"Unselected native aspect".into()}},aspect}).collect();
    Texture{id:id.slice.resource.clone(),native_allocation_id:Some(id.slice.resource.strip_prefix("native-texture:").unwrap().parse().unwrap()),descriptor:d,mutable_target:mutable,initial}
}
fn proxy(a:&mut Assembly,o:&Value)->Result<Key>{
    shape(o,&["event_index","checkpoint_event_index","command_sequence","source","before","after","source_descriptor","destination_descriptor","source_blob","final_blob","shader","clear_rgba_bits","vertices","instances"])?;
    let index=num(o,"event_index")?;let checkpoint=num(o,"checkpoint_event_index")?;let ordinal=num(o,"command_sequence")?;
    let source=version(get(o,"source")?)?;let before=version(get(o,"before")?)?;let after=version(get(o,"after")?)?;
    require(checkpoint>index&&source.slice.resource!=before.slice.resource&&before.slice==after.slice&&after.content_version>before.content_version
        &&source.slice.aspect==Aspect::Depth&&before.slice.aspect==Aspect::Color
        &&[&source,&before,&after].iter().all(|v|v.slice.mip==0&&v.slice.layer==0),"graph proxy event/version scope")?;
    let sd=descriptor(get(o,"source_descriptor")?)?;let dd=descriptor(get(o,"destination_descriptor")?)?;
    require([&sd,&dd].iter().all(|d|d.dimension==TextureDimension::D2&&d.mips==1&&d.layers==1&&d.width>0&&d.height>0)
        &&sd.width==dd.width&&sd.height==dd.height&&matches!(sd.format,TextureFormat::Depth32Float|TextureFormat::Depth32FloatStencil8)
        &&dd.format==TextureFormat::R32Float,"graph proxy descriptor scope")?;
    let shader=get(o,"shader")?;shape(shader,&["wgsl","sha256","vertex_entry","fragment_entry","compilation_options"])?;
    require(txt(shader,"wgsl")?==crate::execute::DEPTH_TO_COLOR_WGSL&&txt(shader,"sha256")?==sha(crate::execute::DEPTH_TO_COLOR_WGSL.as_bytes())
        &&shader["vertex_entry"]=="vs"&&shader["fragment_entry"]=="fs"&&shader["compilation_options"]=="wgpu default"
        &&o["clear_rgba_bits"]==json!([0u64,0,0,1.0f64.to_bits()])&&num(o,"vertices")?==3&&num(o,"instances")?==1,"graph proxy actual implementation differs")?;
    let source_blob=txt(o,"source_blob")?.to_owned();let final_blob=txt(o,"final_blob")?.to_owned();
    require(a.blobs.contains_key(&source_blob)&&a.blobs.contains_key(&final_blob),"graph proxy payload absent")?;
    a.texture(proxy_texture(&source,sd,Initial::Captured{blob:source_blob.clone()},false))?;
    a.texture(proxy_texture(&before,dd,Initial::Unknown{reason:"Actual pre-proxy content version; full original overwrite follows".into()},true))?;
    a.proxy_sources.push(key(&source));
    a.event(index,ordinal,Command::DepthToColor{source:source.clone(),destination:Transition{slice:before.slice,before:before.content_version,after:after.content_version}})?;
    a.event(checkpoint,ordinal,Command::Checkpoint{expected:vec![Expected{version:source,blob:source_blob},Expected{version:after.clone(),blob:final_blob}]})?;
    Ok(key(&after))
}
struct Conversion{scene:Scene,elided:Vec<Value>,footprints:Vec<Value>}
fn convert(m:&Value,source_hash:String,payload:&[(PathBuf,Vec<u8>)])->Result<Conversion>{
    shape(m,&["schema","kind","status","source","world_view","stages","blobs","allocated_readback_bytes","prior_rejected_candidates","closure","normal_frame_publication_changed","finished_frame_or_density_accepted","coherent_stereo_accepted"])?;
    require(m["schema"]==6&&m["kind"]=="native_connected_graph"&&m["status"]=="captured"
        &&m["normal_frame_publication_changed"]==false&&m["finished_frame_or_density_accepted"]==false&&m["coherent_stereo_accepted"]==false,"requires bounded native connected graph")?;
    let source=get(m,"source")?;shape(source,&["method","backend","capture_id","native_namespace","native_present","encoder_epoch"])?;
    let capture_id=txt(source,"capture_id")?;let present=num(source,"native_present")?;
    require(!capture_id.is_empty()&&capture_id.len()<=256&&txt(source,"native_namespace")?==capture_id&&present>0&&num(source,"encoder_epoch")?>0
        &&source["backend"]=="Metal"&&source["method"]=="single native raster/proxy/compute graph","graph single native namespace absent")?;
    let closure=get(m,"closure")?;shape(closure,&["same_capture","same_native_namespace","same_present","continuous_command_interval","initial_contents_captured","original_final_captured","unmodelled_commands","natural_submission_completed","queries_inactive","gpu_errors"])?;
    for k in ["same_capture","same_native_namespace","same_present","continuous_command_interval","initial_contents_captured","original_final_captured","natural_submission_completed","queries_inactive"]{require(closure[k]==true,"graph original closure absent")?;}
    require(num(closure,"unmodelled_commands")?==0&&num(closure,"gpu_errors")?==0,"graph omitted commands/errors")?;
    let mut rows=BTreeMap::new();let mut blobs=BTreeMap::new();let mut packed=0u64;let mut allocated=0u64;
    for b in array(m,"blobs")?{
        shape(b,&["path","byte_length","sha256","packed_row_bytes","row_count","readback_pitch"])?;
        let name=txt(b,"path")?;let path=Path::new(name);let bytes=num(b,"byte_length")?;let row=num(b,"packed_row_bytes")?;let count=num(b,"row_count")?;let pitch=num(b,"readback_pitch")?;
        require(!name.is_empty()&&!path.is_absolute()&&path.components().all(|c|matches!(c,Component::Normal(_)))
            &&!["scene.json","native-capture.json","native-import.json"].contains(&name)
            &&bytes>0&&row>0&&count>0&&row.checked_mul(count)==Some(bytes)&&pitch>=row
            &&txt(b,"sha256")?.len()==64&&txt(b,"sha256")?.bytes().all(|b|b.is_ascii_hexdigit()),"graph payload shape/path")?;
        packed=packed.checked_add(bytes).ok_or("graph payload overflow")?;allocated=allocated.checked_add(pitch.checked_mul(count).ok_or("graph staging overflow")?).ok_or("graph staging overflow")?;
        require(packed<=MAX_BYTES&&allocated<=MAX_BYTES&&rows.insert(name.to_owned(),b.clone()).is_none(),"graph payload budget/duplicate")?;
        blobs.insert(name.into(),Blob{path:name.into(),bytes,sha256:txt(b,"sha256")?.into()});
    }
    require(!blobs.is_empty()&&blobs.len()<=16384&&allocated==num(m,"allocated_readback_bytes")?,"graph actual union staging accounting")?;
    let mut a=Assembly::new(source_hash.clone(),blobs);let mut footprints=vec![];let mut proxies=BTreeSet::new();let mut compute_inputs=BTreeSet::new();
    let stages=array(m,"stages")?;require(stages.len()>=3&&stages.len()<=25,"graph stage bound")?;
    let mut rasters=0;let mut prior_raster_checkpoint=0;let mut computes=0;let mut stage_order=0;let mut compute_event=0;let mut compute_checkpoint=0;let mut used_names=BTreeSet::new();let anchor=get(m,"world_view")?;
    for (stage_index,stage) in stages.iter().enumerate(){
        shape(stage,&["kind","capture_id","native_present","capture","operation"])?;
        require(txt(stage,"capture_id")?==capture_id&&num(stage,"native_present")?==present,"graph stage belongs to another capture/present")?;
        match txt(stage,"kind")?{
            "raster"|"compute"=>{
                require(stage.get("operation").is_none(),"mixed graph stage")?;
                let mut c=get(stage,"capture")?.clone();let raw_cost=num(&c,"allocated_readback_bytes")?;
                let footprint=stage_payload(&mut c,&rows)?;
                require(raw_cost<=footprint,"graph stage incremental cost exceeds referenced footprint")?;
                for b in array(&c,"blobs")?{used_names.insert(txt(b,"path")?.to_owned());}
                let raster=stage["kind"]=="raster";
                let checkpoint=num(get(&c,"closure")?,"checkpoint_event_index")?;
                c["closure"].as_object_mut().ok_or("graph stage closure")?.remove("checkpoint_event_index");
                let (mut part,indices,ordinals,checkpoint_ordinal)=if raster{
                    require(get(&c,"world_view")?==anchor,"graph raster world changed")?;
                    let ds=array(&c,"draws")?;let indices=ds.iter().map(|d|num(d,"event_index")).collect::<Result<Vec<_>>>()?;
                    let ordinals=ds.iter().map(|d|num(d,"command_sequence")).collect::<Result<Vec<_>>>()?;
                    for d in c["draws"].as_array_mut().unwrap(){d.as_object_mut().ok_or("graph draw object")?.remove("event_index");}
                    let mut part=crate::native_import::convert(&c,source_hash.clone())?;
                    crate::native_clear::promote(&c,&mut part,payload)?;
                    if let Some(eye)=&a.scene.eye{require(part.eye.as_ref()==Some(eye),"graph raster eye changed")?;}else{a.scene.eye=part.eye.clone();}
                    rasters+=1;let ordinal=num(get(&c,"closure")?,"interval_end_command")?;
                    (part,indices,ordinals,ordinal)
                }else{
                    require(computes==0&&stage_index+1==stages.len(),"graph requires one final compute stage")?;
                    let context=get(&c,"context")?;require(num(context,"native_present")?==present&&get(context,"preceding_main_world")?==anchor,"graph compute context changed")?;
                    let index=num(get(&c,"dispatch")?,"event_index")?;let ordinal=num(get(&c,"dispatch")?,"command_sequence")?;
                    c["dispatch"].as_object_mut().unwrap().remove("event_index");
                    // This is the footprint of referenced original payloads, not another allocation.
                    c["allocated_readback_bytes"]=footprint.into();
                    let part=crate::compute_import::convert(&c,source_hash.clone())?;
                    if let Command::Dispatch{dispatch}=&part.commands[0]{for b in &dispatch.bindings{if let BindingValue::Texture{versions,..}=&b.value{compute_inputs.extend(versions.iter().map(key));}}}
                    computes+=1;compute_event=index;compute_checkpoint=checkpoint;(part,vec![index],vec![ordinal],ordinal)
                };
                require(!indices.is_empty()&&indices.windows(2).all(|w|w[0]<w[1])&&indices.iter().all(|e|*e>0&&*e<checkpoint),"graph stage event/checkpoint order")?;
                require(indices[0]>stage_order,"graph stage ordering")?;stage_order=indices[0];
                // Guest ordinals may share a boundary, and proxy helpers may nest,
                // but two complete fixed-target raster intervals cannot interleave.
                if raster {require(indices[0]>prior_raster_checkpoint,"graph raster target intervals overlap")?;prior_raster_checkpoint=checkpoint;}
                footprints.push(json!({"stage":stage_index,"native_incremental_readback_bytes":raw_cost,"referenced_payload_footprint":footprint,"normalization":"strict stage decoder reference footprint only; outer actual allocation unchanged"}));
                a.record_loads(&part,&indices)?;
                let expected=std::mem::take(&mut part.expected);
                let commands=a.resources(part,stage_index)?;
                require(commands.len()==indices.len(),"graph event/command count")?;
                for ((command,index),ordinal) in commands.into_iter().zip(indices).zip(ordinals){a.event(index,ordinal,command)?;}
                a.event(checkpoint,checkpoint_ordinal,Command::Checkpoint{expected})?;
            },
            "depth_to_color"=>{
                require(stage.get("capture").is_none(),"mixed proxy stage")?;let operation=get(stage,"operation")?;let index=num(operation,"event_index")?;
                require(index>stage_order,"graph proxy stage order")?;stage_order=index;
                used_names.insert(txt(operation,"source_blob")?.to_owned());used_names.insert(txt(operation,"final_blob")?.to_owned());proxies.insert(proxy(&mut a,operation)?);
            },
            _=>return Err("unsupported graph stage".into()),
        }
    }
    require(rasters>0&&computes==1&&!proxies.is_empty()&&!proxies.is_disjoint(&compute_inputs),"graph compute does not consume its produced depth proxies")?;
    require(a.events.keys().all(|index|*index<=compute_event||*index==compute_checkpoint),"graph has events after final compute")?;
    require(used_names==rows.keys().cloned().collect(),"graph unused outer payload")?;
    let (scene,elided)=a.finish()?;
    validate_buffer_overlaps(&scene,payload)?;
    require(!elided.is_empty(),"graph has no produced-input references")?;
    Ok(Conversion{scene,elided,footprints})
}

struct UniqueValue(Value);
impl<'de> serde::Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unique-key JSON")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<UniqueValue, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite JSON"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<UniqueValue, E> {
                self.visit_unit()
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<UniqueValue, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = a.next_element::<UniqueValue>()? {
                    out.push(v.0);
                }
                Ok(UniqueValue(Value::Array(out)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<UniqueValue, A::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, v)) = a.next_entry::<String, UniqueValue>()? {
                    if out.insert(k, v.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(UniqueValue(Value::Object(out)))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn native(path:&Path,out:&Path)->Result<()> {
    require(fs::metadata(path)?.len()<=32<<20,"graph manifest bound")?;
    let raw=fs::read(path)?;require(raw.len()<=32<<20,"graph manifest bound")?;
    let manifest=serde_json::from_slice::<UniqueValue>(&raw)?.0;
    let root=path.parent().ok_or("graph manifest parent")?.canonicalize()?;
    let mut payload=vec![];let mut total=0u64;let mut names=BTreeSet::new();
    for b in array(&manifest,"blobs")?{
        let rel=Path::new(txt(b,"path")?);let size=num(b,"byte_length")?;
        require(!rel.is_absolute()&&rel.components().all(|c|matches!(c,Component::Normal(_)))&&names.insert(rel.to_owned()),"graph local unique payload")?;
        let p=root.join(rel).canonicalize()?;require(p.starts_with(&root),"graph payload escaped capture")?;
        total=total.checked_add(size).ok_or("graph payload overflow")?;require(total<=MAX_BYTES&&fs::metadata(&p)?.len()==size,"graph payload pre-read bound")?;
        let bytes=fs::read(p)?;require(bytes.len() as u64==size&&sha(&bytes)==txt(b,"sha256")?,"graph payload length/hash mismatch")?;
        payload.push((rel.to_owned(),bytes));
    }
    let conversion=convert(&manifest,sha(&raw),&payload)?;
    let scene_bytes=serde_json::to_vec_pretty(&conversion.scene)?;
    require(scene_bytes.len()<=32<<20,"expanded graph scene bound")?;
    fs::create_dir(out)?;
    for (rel,bytes) in payload{let dest=out.join(rel);if let Some(parent)=dest.parent(){fs::create_dir_all(parent)?;}fs::write(dest,bytes)?;}
    fs::write(out.join("native-capture.json"),&raw)?;
    fs::write(out.join("scene.json"),&scene_bytes)?;
    Input::load(&out.join("scene.json"))?;
    // Standard receipt is the last file; an incomplete import cannot pass run.py.
    fs::write(out.join("native-import.json"),serde_json::to_vec_pretty(&json!({
        "schema":1,"adapter":"schema6 single native connected graph v1","capture_sha256":sha(&raw),"scene_sha256":sha(&scene_bytes),
        "capture_id":manifest["source"]["capture_id"],"native_namespace":manifest["source"]["native_namespace"],"native_present":manifest["source"]["native_present"],
        "native_identifiers_preserved":true,"compute_eye_ownership_assigned":false,"produced_input_initializers_elided":conversion.elided,
        "stage_reference_footprints":conversion.footprints,"outer_actual_readback_bytes":manifest["allocated_readback_bytes"],
        "finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false
    }))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(allocation:u64,version:u64,mip:u32,layer:u32)->Value { json!({"allocation_id":allocation,"content_version":version,"subresource":{"mip":mip,"layer":layer,"aspect":"color"}}) }
    fn reference(id:u64,n:u64,mip:u32)->Value { v(id,n,mip,0) }
    fn raster_fixture() -> (Value, BTreeMap<String, Vec<u8>>) {
        let vs="@vertex fn main(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0,1);}";
        let ps="@group(0) @binding(0) var t:texture_2d_array<f32>; @fragment fn main()->@location(0)vec4f{return textureLoad(t,vec2i(0),0,0);}";
        let vh = sha(vs.as_bytes());
        let ph = sha(ps.as_bytes());
        let mut sources = serde_json::Map::new();
        sources.insert(vh.clone(), json!({"wgsl":vs}));
        sources.insert(ph.clone(), json!({"wgsl":ps}));
        let reference = |h: &str| json!({"status":"exact","sha256":h,"entry_point":"main","compilation_options":"wgpu default"});
        let mut matrix = [0f32; 16];
        for i in [0, 5, 10, 15] {
            matrix[i] = 1.;
        }
        let world = json!({"provenance_epoch":1,"camera_sequence":10,"request_frame":20,"head_pose_frame":20,"eye_index":0,"view_count":2,"headset_applied":true,"depth_reverse_z":false,"presentation_time_seconds":1.,"game_from_eye":matrix,"game_from_stage":matrix,"projection_from_eye":matrix,"optics":{"scope_sequence":0,"magnification":1.,"reference_fov_degrees":0.,"actual_fov_degrees":0.}});
        let domain = json!({"width":2,"height":2,"depth_flags":0,"attachments":[{"role":"color0","guest_id":1,"native_allocation_id":100,"format":TextureFormat::Rgba8Unorm,"mip":0,"slice":0}]});
        let descriptor = json!({"width":4,"height":4,"depth_or_array_layers":2,"mip_level_count":2,"sample_count":1,"dimension":"2d","format":TextureFormat::Rgba8Unorm});
        let view = TextureViewDescriptor::<Option<String>> {
            dimension: Some(TextureViewDimension::D2Array),
            format: Some(TextureFormat::Rgba8UnormSrgb),
            base_mip_level: 1,
            mip_level_count: Some(1),
            base_array_layer: 1,
            array_layer_count: Some(1),
            ..Default::default()
        };
        let layout = BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::FRAGMENT,
            ty: BindingType::Texture {
                sample_type: TextureSampleType::Float { filterable: true },
                view_dimension: TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        };
        let binding = json!({"group":0,"binding":0,"layout":layout,"kind":"texture","native_allocation_id":200,"descriptor":descriptor,"view":view,"subresources":[{"mip_level":1,"layer":1,"aspect":"color","blob":"material.bin","packed_row_bytes":8,"version":v(200,2,1,1)}]});
        let pipeline = json!({"effective_shaders":{"vertex":reference(&vh),"fragment":reference(&ph)},"vertex_layouts":[],"primitive":PrimitiveState::default(),"depth_stencil":null,"multisample":MultisampleState::default(),"targets":[ColorTargetState{format:TextureFormat::Rgba8Unorm,blend:None,write_mask:ColorWrites::ALL}]});
        let draw = json!({"command_sequence":101,"world_view":world,"target_domain":domain,"pipeline":pipeline,"bindings":[binding],"vertex_buffers":[],"index_buffer":null,"draw":{"kind":0,"count":3,"instances":1,"start_index":0,"first_vertex":0,"base_vertex":0,"start_instance":0},"viewport":[0.,0.,2.,2.,0.,1.],"scissor":[0,0,2,2],"blend_constant":[0.,0.,0.,0.],"stencil_reference":0,"writes":[{"before":v(100,1,0,0),"after":v(100,3,0,0)}]});
        let bytes = BTreeMap::from([
            ("initial.bin".into(), [0, 0, 0, 255].repeat(4)),
            ("final.bin".into(), [255, 0, 0, 255].repeat(4)),
            ("material.bin".into(), [255, 0, 0, 255].repeat(4)),
        ]);
        let blobs = bytes
            .iter()
            .map(|(path, b)| json!({"path":path,"byte_length":b.len(),"sha256":sha(b)}))
            .collect::<Vec<_>>();
        (
            json!({"schema":4,"kind":"native_target_span","status":"captured","source":{"backend":"Metal","method":"Gpu::do_draw resolved bindings","encoder_epoch":1},"normal_frame_publication_changed":false,"world_view":world,"target_domain":domain,"draws":[draw],"attachments":[{"role":"color0","native_allocation_id":100,"width":2,"height":2,"descriptor":{"width":2,"height":2,"depth_or_array_layers":1,"mip_level_count":1,"sample_count":1,"dimension":"2d","format":TextureFormat::Rgba8Unorm},"format":TextureFormat::Rgba8Unorm,"mip":0,"slice":0,"initial_blob":"initial.bin","final_blob":"final.bin","initial_version":v(100,1,0,0),"final_version":v(100,3,0,0)}],"blobs":blobs,"effective_shader_sources":{"complete":true,"sources":sources},"closure":{"complete_target_interval":true,"interval_start_command":100,"interval_end_command":102,"first_executed_draw_captured":true,"last_executed_draw_captured":true,"unmodelled_commands":0,"initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0}}),
            bytes,
        )
    }
    fn compute_fixture() -> (Value, BTreeMap<String, Vec<u8>>) {
        let text="@group(0) @binding(0) var s:texture_2d<f32>; @group(0) @binding(1) var o:texture_storage_2d<rgba32float,write>; @compute @workgroup_size(1) fn main(@builtin(global_invocation_id)p:vec3u){if(p.x<2u&&p.y<2u){textureStore(o,vec2i(p.xy),textureLoad(s,vec2i(p.xy),0));}}";
        let hash = sha(text.as_bytes());
        let d = json!({"width":4,"height":4,"depth_or_array_layers":1,"mip_level_count":2,"sample_count":1,"dimension":"2d","format":"rgba32float"});
        let view = TextureViewDescriptor::<Option<String>> {
            dimension: Some(TextureViewDimension::D2),
            base_mip_level: 1,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(1),
            ..Default::default()
        };
        let payload = BTreeMap::from([
            ("input.bin".into(), vec![0u8; 64]),
            ("initial.bin".into(), vec![1u8; 64]),
            ("final.bin".into(), vec![0u8; 64]),
        ]);
        let bindings=[(0,100,10,"texture","input.bin",BindingType::Texture{sample_type:TextureSampleType::Float{filterable:false},view_dimension:TextureViewDimension::D2,multisampled:false}),
            (1,200,20,"storage_texture","initial.bin",BindingType::StorageTexture{access:StorageTextureAccess::WriteOnly,format:TextureFormat::Rgba32Float,view_dimension:TextureViewDimension::D2})]
            .into_iter().map(|(binding,id,n,kind,name,ty)|json!({"group":0,"binding":binding,"kind":kind,"layout":BindGroupLayoutEntry{binding,visibility:ShaderStages::COMPUTE,ty,count:None},"native_allocation_id":id,"descriptor":d,"view":view,"immutable_native_dummy":false,
                "subresources":[{"mip_level":1,"layer":0,"aspect":"color","blob":name,"packed_row_bytes":32,"width":2,"height":2,"depth":1,"version":reference(id,n,1)}]})).collect::<Vec<_>>();
        let blobs=payload.iter().map(|(path,bytes)|json!({"path":path,"byte_length":bytes.len(),"sha256":sha(bytes),"packed_row_bytes":32,"row_count":2,"readback_pitch":256})).collect::<Vec<_>>();
        (
            json!({"schema":5,"kind":"native_compute_dispatch","status":"captured","source":{"method":"Gpu::do_dispatch actual final pipeline and resolved bindings","backend":"Metal","encoder_epoch":1},
            "context":{"native_present":1,"preceding_main_world":{},"qualification":"preceding actual main-target draw in this present; not a full dependency or stereo certificate"},
            "dispatch":{"workgroups":[2,2,1],"command_sequence":100,"bindings":bindings,"pipeline":{"effective_shader":{"status":"exact","sha256":hash,"entry_point":"main","compilation_options":"wgpu default"},"compilation_options":"wgpu default"},"writes":[{"before":reference(200,20,1),"after":reference(200,30,1)}]},
            "attachments":[{"role":"storage-b1","binding":1,"native_allocation_id":200,"format":"rgba32float","descriptor":d,"view":view,"width":2,"height":2,"mip":1,"slice":0,"initial_blob":"initial.bin","final_blob":"final.bin","initial_version":reference(200,20,1),"final_version":reference(200,30,1)}],
            "blobs":blobs,"allocated_readback_bytes":1536,"closure":{"single_original_dispatch":true,"unmodelled_commands":0,"initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0},
            "normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false,
            "effective_shader_sources":{"schema":1,"method":"successful actual ShaderModule creation","complete":true,"failure":null,"sources":{hash:{"wgsl":text}}}}),
            payload,
        )
    }
    fn fixture()->(Value,BTreeMap<String,Vec<u8>>){
        let (mut r,mut files)=raster_fixture();let (mut c,_)=compute_fixture();
        let depth_desc=json!({"width":2,"height":2,"depth_or_array_layers":1,"mip_level_count":1,"sample_count":1,"dimension":"2d","format":"depth32float"});
        let float_desc=json!({"width":2,"height":2,"depth_or_array_layers":1,"mip_level_count":1,"sample_count":1,"dimension":"2d","format":"r32float"});
        let output_desc=json!({"width":2,"height":2,"depth_or_array_layers":1,"mip_level_count":1,"sample_count":1,"dimension":"2d","format":"rgba32float"});
        let depth_ref=|n|json!({"allocation_id":300,"content_version":n,"subresource":{"mip":0,"layer":0,"aspect":"depth"}});
        files.insert("depth-initial.bin".into(),1.0f32.to_le_bytes().repeat(4));
        files.insert("depth-final.bin".into(),0.0f32.to_le_bytes().repeat(4));
        files.insert("proxy-final.bin".into(),0.0f32.to_le_bytes().repeat(4));
        files.insert("compute-initial.bin".into(),0.25f32.to_le_bytes().repeat(16));
        files.insert("compute-final.bin".into(),[0f32,0.,0.,1.].into_iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>().repeat(4));
        r["target_domain"]["attachments"].as_array_mut().unwrap().push(json!({"role":"depth_stencil","guest_id":3,"native_allocation_id":300,"format":"depth32float","mip":0,"slice":0}));
        r["attachments"].as_array_mut().unwrap().push(json!({"role":"depth","native_allocation_id":300,"width":2,"height":2,"descriptor":depth_desc,"format":"depth32float","mip":0,"slice":0,"initial_blob":"depth-initial.bin","final_blob":"depth-final.bin","initial_version":depth_ref(4),"final_version":depth_ref(5)}));
        r["draws"][0]["target_domain"]=r["target_domain"].clone();
        r["draws"][0]["pipeline"]["depth_stencil"]=serde_json::to_value(DepthStencilState{format:TextureFormat::Depth32Float,depth_write_enabled:Some(true),depth_compare:Some(CompareFunction::Always),stencil:Default::default(),bias:Default::default()}).unwrap();
        r["draws"][0]["writes"].as_array_mut().unwrap().push(json!({"before":depth_ref(4),"after":depth_ref(5)}));
        r["draws"][0]["event_index"]=1.into();r["closure"]["checkpoint_event_index"]=2.into();
        r["allocated_readback_bytes"]=2560.into();r.as_object_mut().unwrap().remove("blobs");
        let view=TextureViewDescriptor::<Option<String>>{dimension:Some(TextureViewDimension::D2),mip_level_count:Some(1),array_layer_count:Some(1),..Default::default()};
        for (i,id,n,blob,desc,row) in [(0,400,7,"proxy-final.bin",float_desc.clone(),8),(1,500,8,"compute-initial.bin",output_desc.clone(),32)]{
            let b=&mut c["dispatch"]["bindings"][i];b["native_allocation_id"]=id.into();b["descriptor"]=desc;b["view"]=serde_json::to_value(&view).unwrap();b["subresources"]=json!([{"mip_level":0,"layer":0,"aspect":"color","blob":blob,"packed_row_bytes":row,"width":2,"height":2,"depth":1,"version":v(id,n,0,0)}]);
        }
        c["context"]["preceding_main_world"]=r["world_view"].clone();c["dispatch"]["command_sequence"]=103.into();c["dispatch"]["event_index"]=5.into();
        c["dispatch"]["writes"]=json!([{"before":v(500,8,0,0),"after":v(500,9,0,0)}]);
        c["attachments"]=json!([{"role":"storage-b1","binding":1,"native_allocation_id":500,"format":"rgba32float","descriptor":output_desc,"view":view,"width":2,"height":2,"mip":0,"slice":0,"initial_blob":"compute-initial.bin","final_blob":"compute-final.bin","initial_version":v(500,8,0,0),"final_version":v(500,9,0,0)}]);
        c["closure"]["checkpoint_event_index"]=6.into();c["allocated_readback_bytes"]=1024.into();c.as_object_mut().unwrap().remove("blobs");
        let op=json!({"event_index":3,"checkpoint_event_index":4,"command_sequence":103,"source":depth_ref(5),"before":v(400,6,0,0),"after":v(400,7,0,0),"source_descriptor":depth_desc,"destination_descriptor":float_desc,"source_blob":"depth-final.bin","final_blob":"proxy-final.bin","shader":{"wgsl":crate::execute::DEPTH_TO_COLOR_WGSL,"sha256":sha(crate::execute::DEPTH_TO_COLOR_WGSL.as_bytes()),"vertex_entry":"vs","fragment_entry":"fs","compilation_options":"wgpu default"},"clear_rgba_bits":[0u64,0,0,1.0f64.to_bits()],"vertices":3,"instances":1});
        let rows=files.iter().map(|(name,bytes)|json!({"path":name,"byte_length":bytes.len(),"sha256":sha(bytes),"packed_row_bytes":bytes.len()/2,"row_count":2,"readback_pitch":256})).collect::<Vec<_>>();
        let m=json!({"schema":6,"kind":"native_connected_graph","status":"captured","source":{"method":"single native raster/proxy/compute graph","backend":"Metal","capture_id":"fixture-single-capture","native_namespace":"fixture-single-capture","native_present":1,"encoder_epoch":1},"world_view":r["world_view"],
            "stages":[{"kind":"raster","capture_id":"fixture-single-capture","native_present":1,"capture":r},{"kind":"depth_to_color","capture_id":"fixture-single-capture","native_present":1,"operation":op},{"kind":"compute","capture_id":"fixture-single-capture","native_present":1,"capture":c}],
            "blobs":rows,"allocated_readback_bytes":files.len()*512,"closure":{"same_capture":true,"same_native_namespace":true,"same_present":true,"continuous_command_interval":true,"initial_contents_captured":true,"original_final_captured":true,"unmodelled_commands":0,"natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0},"normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false});
        (m,files)
    }
    fn payload(files:&BTreeMap<String,Vec<u8>>)->Vec<(PathBuf,Vec<u8>)>{files.iter().map(|(n,b)|(PathBuf::from(n),b.clone())).collect()}
    #[test]
    fn connected_graph_preserves_allocations_and_elides_produced_inputs()->Result<()> {
        let (m,files)=fixture();let c=convert(&m,"1".repeat(64),&payload(&files))?;
        assert!(matches!(c.scene.provenance,Provenance::NativeGraph{..}));assert_eq!(c.scene.commands.len(),6);
        assert_eq!(c.elided.len(),2);assert!(c.elided.iter().all(|v|v["reuploaded"]==false));
        let proxy=c.scene.textures.iter().find(|t|t.id=="native-texture:400").unwrap();
        assert_eq!(proxy.initial[0].content_version,6);assert!(matches!(proxy.initial[0].origin,Initial::Unknown{..}));
        let depth=c.scene.textures.iter().find(|t|t.id=="native-texture:300").unwrap();assert_eq!(depth.initial[0].content_version,4);
        if let Command::Dispatch{dispatch}=&c.scene.commands[4]{assert!(dispatch.eye.is_none());if let BindingValue::Texture{texture,versions,..}=&dispatch.bindings[0].value{assert_eq!(texture,"native-texture:400");assert_eq!(versions[0].content_version,7);}else{panic!("missing produced input")}}else{panic!("dispatch order")}
        let dir=std::env::temp_dir().join(format!("native-graph-import-{}",std::process::id()));fs::create_dir(&dir)?;
        for (name,bytes) in &files{fs::write(dir.join(name),bytes)?;}fs::write(dir.join("capture.json"),serde_json::to_vec(&m)?)?;
        native(&dir.join("capture.json"),&dir.join("imported"))?;
        let receipt:Value=serde_json::from_slice(&fs::read(dir.join("imported/native-import.json"))?)?;
        assert_eq!(receipt["capture_sha256"],sha(&fs::read(dir.join("capture.json"))?));
        assert!(Input::load(&dir.join("imported/scene.json"))?.scene.eye.is_some());
        fs::remove_dir_all(dir)?;Ok(())
    }
    #[test]
    fn connected_graph_rejects_splicing_stale_edges_and_unmodelled_operations()->Result<()> {
        let (m,files)=fixture();let bytes=payload(&files);
        let reject=|name:&str,change:&dyn Fn(&mut Value)|{let mut v=m.clone();change(&mut v);assert!(convert(&v,"1".repeat(64),&bytes).is_err(),"accepted {name}");};
        reject("native content-version collision",&|v|v["stages"][0]["capture"]["draws"][0]["bindings"][0]["subresources"][0]["version"]["content_version"]=3.into());
        reject("cross capture",&|v|v["stages"][2]["capture_id"]="other".into());
        reject("namespace",&|v|v["source"]["native_namespace"]="other".into());
        reject("present",&|v|v["stages"][1]["native_present"]=2.into());
        reject("world",&|v|v["stages"][2]["capture"]["context"]["preceding_main_world"]["eye_index"]=1.into());
        reject("omission",&|v|v["closure"]["unmodelled_commands"]=1.into());
        reject("completion",&|v|v["closure"]["natural_submission_completed"]=false.into());
        reject("extra stage operation",&|v|v["stages"][1]["operation"]["hidden_copy"]=true.into());
        reject("duplicate event",&|v|v["stages"][1]["operation"]["event_index"]=1.into());
        reject("backward ordinal",&|v|v["stages"][1]["operation"]["command_sequence"]=99.into());
        reject("early checkpoint",&|v|v["stages"][1]["operation"]["checkpoint_event_index"]=2.into());
        reject("late raster checkpoint",&|v|v["stages"][0]["capture"]["closure"]["checkpoint_event_index"]=10.into());
        reject("proxy wrong source",&|v|v["stages"][1]["operation"]["source"]["allocation_id"]=301.into());
        reject("proxy stale source",&|v|v["stages"][1]["operation"]["source"]["content_version"]=4.into());
        reject("proxy shader",&|v|v["stages"][1]["operation"]["shader"]["wgsl"]="unmodelled shader".into());
        reject("proxy draw arguments",&|v|v["stages"][1]["operation"]["vertices"]=6.into());
        reject("proxy wrong descriptor",&|v|v["stages"][1]["operation"]["source_descriptor"]["width"]=3.into());
        reject("proxy multiple mips",&|v|v["stages"][1]["operation"]["destination_descriptor"]["mip_level_count"]=2.into());
        reject("proxy produced bytes differ",&|v|v["stages"][1]["operation"]["source_blob"]="depth-initial.bin".into());
        reject("missing proxy consumption",&|v|v["stages"][2]["capture"]["dispatch"]["bindings"][0]["subresources"][0]["version"]["content_version"]=6.into());
        reject("changed allocation",&|v|v["stages"][2]["capture"]["dispatch"]["bindings"][0]["descriptor"]["width"]=4.into());
        reject("payload duplicate",&|v|{let b=v["blobs"][0].clone();v["blobs"].as_array_mut().unwrap().push(b);});
        reject("independent nested payload",&|v|v["stages"][2]["capture"]["blobs"]=json!([]));
        reject("budget lie",&|v|v["allocated_readback_bytes"]=1.into());
        reject("not final compute",&|v|{v["stages"].as_array_mut().unwrap().swap(1,2);});
        assert!(serde_json::from_slice::<UniqueValue>(br#"{"schema":6,"schema":5}"#).is_err());
        Ok(())
    }
    #[test]
    fn graph_sorts_internal_proxy_between_raster_draws_and_retains_readonly_loads()->Result<()> {
        let (mut m,files)=fixture();
        let r=&mut m["stages"][0]["capture"];
        let mut second=r["draws"][0].clone();second["event_index"]=5.into();second["command_sequence"]=104.into();
        second["pipeline"]["depth_stencil"]["depth_write_enabled"]=false.into();
        second["writes"]=json!([{"before":v(100,3,0,0),"after":v(100,8,0,0)}]);
        r["draws"].as_array_mut().unwrap().push(second);r["attachments"][0]["final_version"]=v(100,8,0,0);
        r["closure"]["interval_end_command"]=105.into();r["closure"]["checkpoint_event_index"]=6.into();
        let c=&mut m["stages"][2]["capture"];c["dispatch"]["event_index"]=7.into();c["dispatch"]["command_sequence"]=106.into();c["closure"]["checkpoint_event_index"]=8.into();
        c["dispatch"]["bindings"][1]["subresources"][0]["version"]=v(500,10,0,0);
        c["dispatch"]["writes"]=json!([{"before":v(500,10,0,0),"after":v(500,11,0,0)}]);
        c["attachments"][0]["initial_version"]=v(500,10,0,0);c["attachments"][0]["final_version"]=v(500,11,0,0);
        let converted=convert(&m,"2".repeat(64),&payload(&files))?;
        assert_eq!(converted.scene.commands.len(),7);
        assert!(matches!(converted.scene.commands[0],Command::Draw{..}));
        assert!(matches!(converted.scene.commands[1],Command::DepthToColor{..}));
        assert!(matches!(converted.scene.commands[2],Command::Checkpoint{..}));
        assert!(matches!(converted.scene.commands[3],Command::Draw{..}));
        assert!(matches!(converted.scene.commands[4],Command::Checkpoint{..}));
        let dir=std::env::temp_dir().join(format!("nested-graph-import-{}",std::process::id()));fs::create_dir(&dir)?;
        for (name,bytes) in files{fs::write(dir.join(name),bytes)?;}
        fs::write(dir.join("scene.json"),serde_json::to_vec(&converted.scene)?)?;
        Input::load(&dir.join("scene.json"))?;fs::remove_dir_all(dir)?;Ok(())
    }

    #[test]
    fn native_buffer_overlaps_must_match_exact_bytes()->Result<()> {
        let (m,files)=fixture();let mut scene=convert(&m,"3".repeat(64),&payload(&files))?.scene;
        scene.buffers=vec![Buffer{id:"range-a".into(),native_allocation_id:Some(9),content_version:7,source_offset:256,blob:"a".into()},Buffer{id:"range-b".into(),native_allocation_id:Some(9),content_version:7,source_offset:260,blob:"b".into()}];
        let mut p=vec![(PathBuf::from("a"),(0..16u8).collect::<Vec<_>>()),(PathBuf::from("b"),(4..12u8).collect::<Vec<_>>())];
        validate_buffer_overlaps(&scene,&p)?;p[1].1[0]=99;
        assert!(validate_buffer_overlaps(&scene,&p).is_err());
        scene.buffers[1].content_version=8;validate_buffer_overlaps(&scene,&p)?;
        scene.buffers[1].content_version=7;scene.buffers[1].source_offset=272;validate_buffer_overlaps(&scene,&p)?;
        Ok(())
    }
    #[test]
    fn additional_recorded_proxy_need_not_directly_feed_final_compute()->Result<()> {
        let (mut m,mut files)=fixture();let mut extra=m["stages"][1].clone();
        let op=&mut extra["operation"];op["event_index"]=5.into();op["checkpoint_event_index"]=6.into();
        op["before"]=v(401,12,0,0);op["after"]=v(401,13,0,0);op["final_blob"]="earlier-proxy-final.bin".into();
        let c=&mut m["stages"][2]["capture"];c["dispatch"]["event_index"]=7.into();c["closure"]["checkpoint_event_index"]=8.into();
        c["dispatch"]["bindings"][1]["subresources"][0]["version"]=v(500,18,0,0);c["dispatch"]["writes"]=json!([{"before":v(500,18,0,0),"after":v(500,20,0,0)}]);
        c["attachments"][0]["initial_version"]=v(500,18,0,0);c["attachments"][0]["final_version"]=v(500,20,0,0);
        m["stages"].as_array_mut().unwrap().insert(2,extra);
        let raw=files["proxy-final.bin"].clone();files.insert("earlier-proxy-final.bin".into(),raw.clone());
        m["blobs"].as_array_mut().unwrap().push(json!({"path":"earlier-proxy-final.bin","byte_length":16,"sha256":sha(&raw),"packed_row_bytes":8,"row_count":2,"readback_pitch":256}));
        m["allocated_readback_bytes"]=json!(files.len()*512);
        let c=convert(&m,"4".repeat(64),&payload(&files))?;assert_eq!(c.scene.commands.len(),8);
        assert_eq!(c.scene.commands.iter().filter(|c|matches!(c,Command::DepthToColor{..})).count(),2);
        Ok(())
    }

    #[test]
    fn complete_raster_event_intervals_cannot_overlap()->Result<()> {
        let (mut m,files)=fixture();let mut b=m["stages"][0].clone();
        let a=&mut m["stages"][0]["capture"];a["draws"][0]["command_sequence"]=100.into();a["closure"]["interval_start_command"]=99.into();a["closure"]["interval_end_command"]=104.into();a["closure"]["checkpoint_event_index"]=6.into();
        let r=&mut b["capture"];r["target_domain"]["attachments"].as_array_mut().unwrap().truncate(1);r["target_domain"]["attachments"][0]["native_allocation_id"]=600.into();
        r["attachments"].as_array_mut().unwrap().truncate(1);r["attachments"][0]["native_allocation_id"]=600.into();r["attachments"][0]["initial_version"]=v(600,10,0,0);r["attachments"][0]["final_version"]=v(600,11,0,0);
        r["draws"][0]["target_domain"]=r["target_domain"].clone();r["draws"][0]["pipeline"]["depth_stencil"]=Value::Null;r["draws"][0]["writes"]=json!([{"before":v(600,10,0,0),"after":v(600,11,0,0)}]);r["draws"][0]["event_index"]=2.into();
        r["closure"]["interval_end_command"]=104.into();r["closure"]["checkpoint_event_index"]=5.into();r["allocated_readback_bytes"]=0.into();
        let p=&mut m["stages"][1]["operation"];p["before"]=v(400,12,0,0);p["after"]=v(400,13,0,0);
        let c=&mut m["stages"][2]["capture"];c["dispatch"]["event_index"]=7.into();c["dispatch"]["command_sequence"]=105.into();c["closure"]["checkpoint_event_index"]=8.into();
        c["dispatch"]["bindings"][0]["subresources"][0]["version"]=v(400,13,0,0);c["dispatch"]["bindings"][1]["subresources"][0]["version"]=v(500,14,0,0);
        c["dispatch"]["writes"]=json!([{"before":v(500,14,0,0),"after":v(500,15,0,0)}]);c["attachments"][0]["initial_version"]=v(500,14,0,0);c["attachments"][0]["final_version"]=v(500,15,0,0);
        m["stages"].as_array_mut().unwrap().insert(1,b);
        let error=match convert(&m,"5".repeat(64),&payload(&files)){Ok(_)=>panic!("accepted overlapping fixed-target intervals"),Err(e)=>e};
        assert!(error.to_string().contains("raster target intervals overlap"),"{error}");Ok(())
    }

}
