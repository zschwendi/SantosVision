//! Executed-image replay may record this already-omitted original command.
//! It never supplies the absent tessellation stages or certifies their effect.
use super::*;

const KNOWN_CONTROL_POINT_VS:&str="6edc9adb5e47f1e0";
const MAX_OMISSIONS:usize=1024;

fn qualified(reason:&str,hash:&str,stage:u32,failed:bool,index_absent:bool,source_absent:bool,
    topology:u32,errors:u64,jobs:usize,pending:usize,context:Option<([u32;6],Option<[u32;6]>)>)->Option<[u32;6]> {
    let (raw,Some(resolved))=context? else{return None;};
    (reason=="vertex_shader_unavailable" && hash==KNOWN_CONTROL_POINT_VS && stage==0 && failed
        && index_absent && source_absent && topology==35 && errors==0 && jobs==0 && pending==0
        && raw==resolved && raw[0]==1 && raw[1]>0 && raw[2]>0).then_some(raw)
}

impl Capture {
    pub(super) fn record_known_original_omission(&mut self,gpu:&Gpu,reason:&str,
        context:Option<([u32;6],Option<[u32;6]>)>)->bool {
        if self.kind!=CaptureKind::ExecutedFinished || !self.captures_finished_graph()
            || self.graph.anchor.is_none() || self.graph.present!=gpu.presents.checked_add(1)
            || self.finished.original_omission_count>=MAX_OMISSIONS || self.graph.event>=8190 {return false;}
        let id=gpu.s.shader[0];
        let Some(Obj::Shader(shader))=gpu.obj(id) else{return false;};
        // The same shader_program/load_program invocation has already cached
        // this failure. An absent index entry cannot create a shader, uniform
        // upload, proxy, render pass, clear, query or draw at this early seam.
        let index_absent=!gpu.shader_index.contains_key(&shader.hash);
        let source_absent=index_absent && matches!(gpu.shader_dir.join(format!("{}.wgsl",shader.hash)).try_exists(),Ok(false));
        let errors=gpu.errors.load(Ordering::SeqCst);
        let jobs=gpu.pipe_pending.len();
        let pending=gpu.pipelines.values().filter(|p|matches!(p,PipeEntry::Pending)).count();
        let Some(raw)=qualified(reason,&shader.hash,shader.stage,matches!(&shader.program,Some(None)),
            index_absent,source_absent,gpu.s.topo,errors,jobs,pending,context) else{return false;};
        let operation=json!({"event_index":self.graph_next_event(),"command_sequence":self.sequence,
            "skip_reason":reason,"omission_seam":"vertex_shader_program_before_draw_preparation",
            "requested_raw_words":raw,"resolved_raw_words":raw,
            "vertex_shader":{"guest_id":id,"hash":shader.hash,"stage":shader.stage,
                "index_entry_present":false,"translated_source_present":false,"program_state":"failed"},
            "topology":gpu.s.topo,"gpu_errors":errors,"pipeline_jobs_pending":jobs,"pipeline_cache_pending":pending,
            "original_draw_was_not_emitted":true,"gpu_commands_emitted":0,"native_behavior_changed":false,
            "requested_geometry_validated":false,"pixel_visibility_known":false});
        let record=json!({"kind":"original_omission","capture_id":self.graph.capture_id,
            "native_present":self.graph.present,"operation":operation});
        if record.to_string().len()>65536 || self.finished_record_operation(record).is_err(){return false;}
        self.finished.original_omission_count+=1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_original_early_absent_control_shader_is_recordable() {
        let raw=[1,5742,1,0,0,0];let context=Some((raw,Some(raw)));
        let check=|reason,hash,stage,failed,index,source,topo,errors,jobs,pending,context|
            qualified(reason,hash,stage,failed,index,source,topo,errors,jobs,pending,context);
        assert_eq!(check("vertex_shader_unavailable",KNOWN_CONTROL_POINT_VS,0,true,true,true,35,0,0,0,context),Some(raw));
        for reason in ["pixel_shader_unavailable","pipeline_failed_cached","capture_pass_reopen_failed"] {
            assert!(check(reason,KNOWN_CONTROL_POINT_VS,0,true,true,true,35,0,0,0,context).is_none());
        }
        for raw in [[1,0,1,0,0,0],[1,5742,0,0,0,0],[4,123,64,0,0,0],[0,3,1,0,0,0]] {
            assert!(check("vertex_shader_unavailable",KNOWN_CONTROL_POINT_VS,0,true,true,true,35,0,0,0,Some((raw,Some(raw)))).is_none());
        }
        for changed in [None,Some((raw,None)),Some((raw,Some([1,3,1,0,0,0])))] {
            assert!(check("vertex_shader_unavailable",KNOWN_CONTROL_POINT_VS,0,true,true,true,35,0,0,0,changed).is_none());
        }
        for (hash,stage,failed,index,source,topo,errors,jobs,pending) in [
            ("unknown",0,true,true,true,35,0,0,0),(KNOWN_CONTROL_POINT_VS,1,true,true,true,35,0,0,0),
            (KNOWN_CONTROL_POINT_VS,0,false,true,true,35,0,0,0),(KNOWN_CONTROL_POINT_VS,0,true,false,true,35,0,0,0),
            (KNOWN_CONTROL_POINT_VS,0,true,true,false,35,0,0,0),(KNOWN_CONTROL_POINT_VS,0,true,true,true,4,0,0,0),
            (KNOWN_CONTROL_POINT_VS,0,true,true,true,35,1,0,0),(KNOWN_CONTROL_POINT_VS,0,true,true,true,35,0,1,0),
            (KNOWN_CONTROL_POINT_VS,0,true,true,true,35,0,0,1)] {
            assert!(check("vertex_shader_unavailable",hash,stage,failed,index,source,topo,errors,jobs,pending,context).is_none());
        }
    }
}
