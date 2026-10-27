//! One connected original raster/proxy/compute graph. No alternate rendering,
//! submission, camera change, or WorldFrame publication occurs here.
use super::*;
use texture_ledger::{Aspect, Subresource, VersionRef};

const MAX_STAGES: usize = 16;
const MAX_PROXIES: usize = 8;
const MAX_EVENTS: u64 = 1 << 20;

#[derive(Default)]
pub(super) struct GraphState {
    pub(super) stages: Vec<Pending>,
    pub(super) operations: Vec<Value>,
    pub(super) present: Option<u64>,
    pub(super) anchor: Option<Value>,
    pub(super) event: u64,
    pub(super) proxy_pending: Option<Value>,
    pub(super) dispatch_pending: Option<u64>,
    pub(super) proxy_readbacks: Vec<Readback>,
    pub(super) proxy_bytes: u64,
    pub(super) capture_id: String,
    proxy_parents: Option<(Texture, Texture)>,
}

fn anchor_matches(
    anchor: Option<&Value>,
    present: Option<u64>,
    view: &Value,
    actual_present: u64,
) -> Result<(), String> {
    if view["view_count"] != 2
        || view["headset_applied"] != true
        || !view["eye_index"].as_u64().is_some_and(|n| n < 2)
        || ![
            "provenance_epoch",
            "camera_sequence",
            "request_frame",
            "head_pose_frame",
        ]
        .iter()
        .all(|key| view[*key].as_u64().is_some_and(|n| n > 0))
    {
        return Err("graph raster anchor lacks an exact individual world identity".into());
    }
    if present.is_some_and(|p| p != actual_present) || anchor.is_some_and(|a| a != view) {
        return Err("graph crossed its original present or individual world identity".into());
    }
    Ok(())
}
fn descriptor(t: &Texture) -> Value {
    json!({"width":t.width(),"height":t.height(),"depth_or_array_layers":t.depth_or_array_layers(),
        "mip_level_count":t.mip_level_count(),"sample_count":t.sample_count(),
        "dimension":t.dimension(),"format":t.format()})
}
fn ref_key(v: VersionRef) -> (u64, u32, u32, Aspect, u64) {
    (
        v.allocation_id,
        v.subresource.mip,
        v.subresource.layer,
        v.subresource.aspect,
        v.content_version,
    )
}
fn reference(v: &Value, field: &str) -> Result<VersionRef, String> {
    serde_json::from_value(v[field].clone()).map_err(|e| format!("graph {field} version: {e}"))
}
fn dispatch_consumes(dispatch: &Value, reference: &Value) -> bool {
    dispatch["bindings"].as_array().is_some_and(|bindings| {
        bindings.iter().any(|binding| {
            binding["kind"] == "texture"
                && binding["subresources"]
                    .as_array()
                    .is_some_and(|subs| subs.iter().any(|sub| &sub["version"] == reference))
        })
    })
}
fn event(v: &Value) -> Result<u64, String> {
    v["event_index"]
        .as_u64()
        .filter(|n| *n > 0 && *n < MAX_EVENTS)
        .ok_or("graph event index absent or excessive".into())
}
fn check_events(events: &[(u64, u64)]) -> Result<(), String> {
    if events.is_empty() || events.len() as u64 >= MAX_EVENTS {
        return Err("graph event count bound".into());
    }
    for (i, &(index, command)) in events.iter().enumerate() {
        if index == 0
            || index >= MAX_EVENTS
            || command == 0
            || (i > 0 && (events[i - 1].0 >= index || events[i - 1].1 > command))
        {
            return Err("graph native event or guest command ordering is not strict".into());
        }
    }
    Ok(())
}

impl Capture {
    pub(super) fn graph_next_event(&mut self) -> u64 {
        self.graph.event = self.graph.event.saturating_add(1);
        self.graph.event
    }
    pub(super) fn graph_start_or_check(&mut self, gpu: &Gpu, view: &Value) -> Result<(), String> {
        let present = gpu
            .presents
            .checked_add(1)
            .ok_or("graph present overflow")?;
        anchor_matches(
            self.graph.anchor.as_ref(),
            self.graph.present,
            view,
            present,
        )?;
        if self.graph.anchor.is_none() {
            let pass = gpu.pass_info.as_ref().ok_or("graph main pass absent")?;
            if pass.rt_formats != vec![Some(TextureFormat::Bgra8Unorm); 4]
                || pass.ds_format != Some(TextureFormat::Depth32FloatStencil8)
            {
                return Err("graph must start at the original full main-world target".into());
            }
            self.graph.present = Some(present);
            self.graph.anchor = Some(view.clone());
            self.graph.capture_id =
                format!("native-graph-{}-{}", std::process::id(), self.sequence);
        }
        Ok(())
    }
    pub(super) fn graph_retain_stage(&mut self, mut p: Pending) -> Result<(), String> {
        if self.graph.stages.len() >= MAX_STAGES
            || p.kind != CaptureKind::TargetSpan
            || !p.finished
            || p.draws.is_empty()
        {
            return Err("graph requires a bounded complete raster stage".into());
        }
        anchor_matches(
            self.graph.anchor.as_ref(),
            self.graph.present,
            &p.view,
            self.graph.present.ok_or("graph present absent")?,
        )?;
        let checkpoint = p
            .alias
            .as_ref()
            .and_then(|v| v["checkpoint_event_index"].as_u64())
            .filter(|n| *n > 0 && *n < MAX_EVENTS)
            .ok_or("graph raster checkpoint event absent")?;
        let mut last = 0;
        for draw in &p.draws {
            let current = event(draw)?;
            if current <= last || current >= checkpoint {
                return Err("graph raster event order invalid".into());
            }
            last = current;
        }
        let registry = self
            .shader_sources
            .as_ref()
            .ok_or("graph shader registry missing")?
            .borrow();
        let (hashes, failure) = shader_sources::selected_pipeline_hashes(&p.draws);
        if let Some(reason) = failure {
            return Err(format!("graph raster shader stage unresolved: {reason}"));
        }
        let snapshot = registry.snapshot(&hashes);
        if snapshot["complete"] != true {
            return Err("graph raster final shader source incomplete".into());
        }
        p.shader_sources = Some(snapshot);
        self.graph.stages.push(p);
        Ok(())
    }
    pub(in super::super) fn graph_before_proxy(
        &mut self,
        gpu: &Gpu,
        id: u32,
        stencil: bool,
    ) -> Result<(), String> {
        // Pre-anchor helper outputs remain ordinary immutable external inputs.
        // Once a complete main interval exists, every dirty proxy is an edge.
        if self.kind != CaptureKind::Graph || self.done || self.graph.stages.is_empty() {
            return Ok(());
        }
        if stencil
            || self.graph.operations.len() >= MAX_PROXIES
            || self.graph.proxy_pending.is_some()
            || self
                .pending
                .as_ref()
                .is_some_and(|p| p.kind != CaptureKind::TargetSpan || p.finished)
            || self.graph.present != gpu.presents.checked_add(1)
            || gpu.errors.load(Ordering::SeqCst) != 0
            || gpu.occlusion_active.is_some()
            || gpu.occlusion_pass_slot.is_some()
            || !gpu.pending_resolve.is_empty()
        {
            return Err(
                "graph dirty proxy has unsupported kind, stage, present, or query state".into(),
            );
        }
        let native = gpu.tex(id).ok_or("graph depth source allocation absent")?;
        let proxy = native
            .proxy_depth
            .as_ref()
            .ok_or("graph actual depth proxy absent")?;
        let source = &native.tex;
        let destination = &proxy.tex;
        if source == destination
            || source.dimension() != TextureDimension::D2
            || !matches!(
                source.format(),
                TextureFormat::Depth32Float | TextureFormat::Depth32FloatStencil8
            )
            || destination.dimension() != TextureDimension::D2
            || destination.format() != TextureFormat::R32Float
            || [source, destination].iter().any(|t| {
                t.sample_count() != 1 || t.mip_level_count() != 1 || t.depth_or_array_layers() != 1
            })
            || source.width() != destination.width()
            || source.height() != destination.height()
        {
            return Err(
                "graph proxy requires exact single-mip/layer D2 Float32 depth to R32Float".into(),
            );
        }
        let ledger = self
            .texture_ledger
            .as_ref()
            .ok_or("graph proxy ledger absent")?;
        if !ledger.active() {
            return Err(format!(
                "graph proxy ledger disabled: {}",
                ledger.disabled_reason().unwrap_or("unknown")
            ));
        }
        let source_id = native
            .trace_native_identity
            .ok_or("graph source native identity absent")?;
        let destination_id = proxy
            .trace_native_identity
            .ok_or("graph proxy native identity absent")?;
        if source_id == destination_id {
            return Err("graph proxy aliases source native identity".into());
        }
        let source_version = ledger
            .current(
                source_id,
                Subresource {
                    mip: 0,
                    layer: 0,
                    aspect: Aspect::Depth,
                },
            )
            .ok_or("graph source depth version absent")?;
        let before = ledger
            .current(
                destination_id,
                Subresource {
                    mip: 0,
                    layer: 0,
                    aspect: Aspect::Color,
                },
            )
            .ok_or("graph proxy before version absent")?;
        if !self.graph.stages.iter().any(|stage| {
            stage
                .attachments
                .iter()
                .any(|a| a["final_version"] == json!(source_version))
        }) {
            return Err("graph proxy source is not an actual closed raster output".into());
        }
        let source_blob = self
            .span
            .texture_blobs
            .get(&ref_key(source_version))
            .ok_or("graph proxy depth source has no earlier exact snapshot")?
            .clone();
        let index = self.graph_next_event();
        if index >= MAX_EVENTS {
            return Err("graph event limit".into());
        }
        let final_blob = format!("graph-proxy{index}-texture{destination_id}.bin");
        let plan = target_span::subresource_plan(
            final_blob.clone(),
            destination,
            0,
            0,
            TextureAspect::All,
        )?;
        let reserved = self.pending.as_ref().map_or(0, |p| p.final_bytes);
        if !reservation_fits(self.live_bytes.load(Ordering::SeqCst), plan.bytes, reserved) {
            return Err("graph proxy checkpoint exceeds aggregate live snapshot bound".into());
        }
        let shader = super::super::DEPTH_PROXY_WGSL;
        self.graph.proxy_pending = Some(
            json!({"event_index":index,"command_sequence":self.sequence,
            "source":source_version,"before":before,"source_descriptor":descriptor(source),
            "destination_descriptor":descriptor(destination),"source_blob":source_blob,"final_blob":final_blob,
            "shader":{"wgsl":shader,"sha256":sha(shader.as_bytes()),"vertex_entry":"vs","fragment_entry":"fs","compilation_options":"wgpu default"},
            "clear_rgba_bits":[0u64,0,0,1.0f64.to_bits()],"vertices":3,"instances":1}),
        );
        self.graph.proxy_parents = Some((source.clone(), destination.clone()));
        Ok(())
    }
    pub(in super::super) fn graph_after_proxy(
        &mut self,
        gpu: &mut Gpu,
        id: u32,
        stencil: bool,
    ) -> Result<(), String> {
        if self.kind != CaptureKind::Graph || self.done {
            return Ok(());
        }
        let Some(mut operation) = self.graph.proxy_pending.take() else {
            return Ok(());
        };
        let (source, destination) = self
            .graph
            .proxy_parents
            .take()
            .ok_or("graph retained proxy parents absent")?;
        if stencil || self.graph.present != gpu.presents.checked_add(1) {
            return Err("graph proxy completion changed scope".into());
        }
        let native = gpu
            .tex(id)
            .ok_or("graph depth source replaced during proxy")?;
        let proxy = native
            .proxy_depth
            .as_ref()
            .ok_or("graph depth proxy replaced")?;
        let source_version = reference(&operation, "source")?;
        let before = reference(&operation, "before")?;
        if native.tex != source
            || proxy.tex != destination
            || native.trace_native_identity != Some(source_version.allocation_id)
            || proxy.trace_native_identity != Some(before.allocation_id)
        {
            return Err("graph depth proxy changed its actual native parents".into());
        }
        let ledger = self
            .texture_ledger
            .as_ref()
            .ok_or("graph proxy ledger absent after draw")?;
        if ledger.current(source_version.allocation_id, source_version.subresource)
            != Some(source_version)
        {
            return Err("graph source changed during internal proxy operation".into());
        }
        let after = ledger
            .current(before.allocation_id, before.subresource)
            .ok_or("graph proxy after version absent")?;
        if after.content_version <= before.content_version {
            return Err("graph proxy lacks an actual new output version".into());
        }
        let blob = operation["final_blob"]
            .as_str()
            .ok_or("graph proxy final blob absent")?
            .to_owned();
        let plan =
            target_span::subresource_plan(blob.clone(), &destination, 0, 0, TextureAspect::All)?;
        let reserved = self.pending.as_ref().map_or(0, |p| p.final_bytes);
        if !reservation_fits(self.live_bytes.load(Ordering::SeqCst), plan.bytes, reserved) {
            return Err("graph proxy checkpoint exceeded live snapshot bound".into());
        }
        let bytes = plan.bytes;
        let proxy_bytes = add(self.graph.proxy_bytes, bytes)?;
        let epoch_bytes = add(self.epoch_bytes, bytes)?;
        operation["after"] = json!(after);
        let readbacks = self.enqueue_span(gpu, vec![plan]);
        self.graph.proxy_readbacks.extend(readbacks);
        let checkpoint = self.graph_next_event();
        if checkpoint >= MAX_EVENTS {
            return Err("graph proxy checkpoint event limit".into());
        }
        operation["checkpoint_event_index"] = checkpoint.into();
        self.graph.proxy_bytes = proxy_bytes;
        self.epoch_bytes = epoch_bytes;
        self.span.texture_blobs.insert(ref_key(after), blob);
        self.graph.operations.push(operation);
        Ok(())
    }
    pub(super) fn graph_finish_compute(&mut self, gpu: &Gpu) -> Result<(), String> {
        if self.graph.stages.is_empty()
            || self.graph.operations.is_empty()
            || self.graph.proxy_pending.is_some()
            || self.graph.present != gpu.presents.checked_add(1)
            || gpu.errors.load(Ordering::SeqCst) != 0
            || gpu.occlusion_active.is_some()
            || gpu.occlusion_pass_slot.is_some()
            || !gpu.pending_resolve.is_empty()
        {
            return Err("graph completion lacks closed original raster/proxy stages".into());
        }
        let compute = self
            .pending
            .take()
            .ok_or("graph completed compute absent")?;
        if compute.kind != CaptureKind::Compute
            || !compute.finished
            || compute.draws.len() != 1
            || compute.domain["preceding_main_world"]
                != *self.graph.anchor.as_ref().ok_or("graph anchor absent")?
            || compute.domain["native_present"].as_u64() != self.graph.present
        {
            return Err("graph final compute changed original context or is incomplete".into());
        }
        if !self
            .graph
            .operations
            .iter()
            .any(|operation| dispatch_consumes(&compute.draws[0], &operation["after"]))
        {
            return Err("graph compute does not consume its actual produced depth proxy".into());
        }
        let epoch = compute.epoch;
        let backend = format!("{:?}", gpu.adapter.get_info().backend);
        let mut readbacks = std::mem::take(&mut self.graph.proxy_readbacks);
        let mut bytes = self.graph.proxy_bytes;
        let mut stages = Vec::<(u64, Value)>::new();
        let mut ordered_events = Vec::new();
        let mut errors = compute.errors;
        for mut raster in std::mem::take(&mut self.graph.stages) {
            errors = errors.max(raster.errors);
            let first = event(&raster.draws[0])?;
            for d in &raster.draws {
                ordered_events.push((
                    event(d)?,
                    d["command_sequence"]
                        .as_u64()
                        .ok_or("graph draw command sequence absent")?,
                ));
            }
            let closure = raster
                .alias
                .as_ref()
                .ok_or("graph raster interval closure absent")?;
            ordered_events.push((
                closure["checkpoint_event_index"]
                    .as_u64()
                    .ok_or("graph raster checkpoint absent")?,
                closure["interval_end_command"]
                    .as_u64()
                    .ok_or("graph raster end command absent")?,
            ));
            bytes = add(bytes, raster.bytes)?;
            readbacks.append(&mut raster.readbacks);
            let capture = json!({"schema":4,"kind":"native_target_span","status":"captured",
                "source":{"method":"Gpu::do_draw resolved bindings","backend":backend,"encoder_epoch":raster.epoch},
                "world_view":raster.view,"target_domain":raster.domain,"draws":raster.draws,
                "attachments":raster.attachments,"allocated_readback_bytes":raster.bytes,
                "intervening_commands":raster.intervening_commands,"initialization_ledger":raster.initialization_ledger,
                "effective_shader_sources":raster.shader_sources,"normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,
                "closure":{"complete_target_interval":true,"first_executed_draw_captured":true,"last_executed_draw_captured":true,
                    "interval_start_command":closure["interval_start_command"],"interval_end_command":closure["interval_end_command"],
                    "checkpoint_event_index":closure["checkpoint_event_index"],"unmodelled_commands":0,"initial_contents_captured":true,
                    "original_final_captured":true,"natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0}});
            stages.push((first, json!({"kind":"raster","capture_id":self.graph.capture_id,"native_present":self.graph.present,"capture":capture})));
        }
        for operation in std::mem::take(&mut self.graph.operations) {
            let index = event(&operation)?;
            let command = operation["command_sequence"]
                .as_u64()
                .ok_or("graph proxy command sequence absent")?;
            ordered_events.push((index, command));
            let checkpoint = operation["checkpoint_event_index"]
                .as_u64()
                .filter(|e| *e > index && *e < MAX_EVENTS)
                .ok_or("graph proxy checkpoint event absent")?;
            ordered_events.push((checkpoint, command));
            stages.push((index, json!({"kind":"depth_to_color","capture_id":self.graph.capture_id,"native_present":self.graph.present,"operation":operation})));
        }
        let mut compute = compute;
        let dispatch = compute.draws.pop().unwrap();
        let index = event(&dispatch)?;
        ordered_events.push((
            index,
            dispatch["command_sequence"]
                .as_u64()
                .ok_or("graph dispatch command sequence absent")?,
        ));
        if ordered_events.iter().any(|&(e, _)| e > index) {
            return Err("graph compute is not its final operation".into());
        }
        let compute_checkpoint = self.graph_next_event();
        if compute_checkpoint <= index || compute_checkpoint >= MAX_EVENTS {
            return Err("graph compute checkpoint event invalid".into());
        }
        ordered_events.push((
            compute_checkpoint,
            dispatch["command_sequence"].as_u64().unwrap(),
        ));
        ordered_events.sort_unstable_by_key(|x| x.0);
        check_events(&ordered_events)?;
        bytes = add(bytes, compute.bytes)?;
        readbacks.append(&mut compute.readbacks);
        let capture = json!({"schema":5,"kind":"native_compute_dispatch","status":"captured",
            "source":{"method":"Gpu::do_dispatch actual final pipeline and resolved bindings","backend":backend,"encoder_epoch":compute.epoch},
            "context":compute.domain,"dispatch":dispatch,"attachments":compute.attachments,"allocated_readback_bytes":compute.bytes,
            "initialization_ledger":compute.initialization_ledger,"effective_shader_sources":compute.shader_sources,
            "closure":{"single_original_dispatch":true,"checkpoint_event_index":compute_checkpoint,"unmodelled_commands":0,"initial_contents_captured":true,"original_final_captured":true,
                "natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0},
            "normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false});
        stages.push((index, json!({"kind":"compute","capture_id":self.graph.capture_id,"native_present":self.graph.present,"capture":capture})));
        stages.sort_by_key(|x| x.0);
        let mut names = std::collections::HashSet::new();
        let actual_bytes = readbacks.iter().try_fold(0, |n, r| {
            if !names.insert(r.plan.name.clone()) {
                return Err("graph duplicate immutable payload name".to_string());
            }
            add(n, r.plan.bytes)
        })?;
        if actual_bytes != bytes || errors != 0 {
            return Err("graph aggregate byte or original error accounting mismatch".into());
        }
        let manifest = json!({"schema":6,"kind":"native_connected_graph","status":"captured",
            "source":{"method":"single native raster/proxy/compute graph","backend":backend,"capture_id":self.graph.capture_id,
                "native_namespace":self.graph.capture_id,"native_present":self.graph.present,"encoder_epoch":epoch},
            "world_view":self.graph.anchor,"stages":stages.into_iter().map(|(_,s)|s).collect::<Vec<_>>(),
            "closure":{"same_capture":true,"same_native_namespace":true,"same_present":true,"continuous_command_interval":true,
                "unmodelled_commands":0,"initial_contents_captured":true,"original_final_captured":true,
                "natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0},
            "normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false});
        json_size_bounded(&manifest, 128 << 20)?;
        self.pending = Some(Pending {
            kind: CaptureKind::Graph,
            alias: Some(manifest),
            initialization_ledger: None,
            shader_sources: None,
            epoch,
            errors,
            view: Value::Null,
            domain: Value::Null,
            draws: Vec::new(),
            attachments: Vec::new(),
            targets: Vec::new(),
            readbacks,
            bytes,
            final_bytes: 0,
            finished: true,
            intervening_commands: Vec::new(),
            candidate_key: Vec::new(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_anchor_is_exact_and_cannot_cross_eye_camera_or_present() {
        let a = json!({"view_count":2,"headset_applied":true,"eye_index":1,"provenance_epoch":2,
            "camera_sequence":3,"request_frame":4,"head_pose_frame":5,"projection_from_eye":[1.0,2.0]});
        assert!(anchor_matches(None, None, &a, 7).is_ok());
        assert!(anchor_matches(Some(&a), Some(7), &a, 7).is_ok());
        assert!(anchor_matches(Some(&a), Some(7), &a, 8).is_err());
        for key in ["eye_index", "camera_sequence", "projection_from_eye"] {
            let mut b = a.clone();
            b[key] = json!(0);
            assert!(anchor_matches(Some(&a), Some(7), &b, 7).is_err(), "{key}");
        }
    }
    #[test]
    fn native_events_disambiguate_internal_operations_in_one_guest_command() {
        assert!(check_events(&[(1, 10), (2, 10), (3, 11)]).is_ok());
        for invalid in [
            vec![],
            vec![(0, 10)],
            vec![(1, 0)],
            vec![(1, 10), (1, 10)],
            vec![(2, 10), (1, 10)],
            vec![(1, 11), (2, 10)],
            vec![(MAX_EVENTS, 10)],
        ] {
            assert!(check_events(&invalid).is_err());
        }
    }
    #[test]
    fn compute_connection_requires_exact_produced_sampled_version() {
        let v = json!({"allocation_id":9,"content_version":23,"subresource":{"mip":0,"layer":0,"aspect":"color"}});
        let mut d = json!({"bindings":[{"kind":"texture","subresources":[{"version":v}]}]});
        assert!(dispatch_consumes(&d, &v));
        d["bindings"][0]["subresources"][0]["version"]["content_version"] = 22.into();
        assert!(!dispatch_consumes(&d, &v));
        d["bindings"][0]["subresources"][0]["version"] = v.clone();
        d["bindings"][0]["kind"] = "storage_texture".into();
        assert!(!dispatch_consumes(&d, &v));
    }
    #[test]
    fn texture_snapshot_keys_preserve_allocation_aspect_and_version() {
        let a = VersionRef {
            allocation_id: 1,
            content_version: 3,
            subresource: Subresource {
                mip: 0,
                layer: 0,
                aspect: Aspect::Depth,
            },
        };
        assert_ne!(
            ref_key(a),
            ref_key(VersionRef {
                content_version: 4,
                ..a
            })
        );
        assert_ne!(
            ref_key(a),
            ref_key(VersionRef {
                allocation_id: 2,
                ..a
            })
        );
        assert_ne!(
            ref_key(a),
            ref_key(VersionRef {
                subresource: Subresource {
                    aspect: Aspect::Stencil,
                    ..a.subresource
                },
                ..a
            })
        );
    }
}
