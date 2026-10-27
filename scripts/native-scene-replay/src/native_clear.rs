//! Promote direct native clear roots only after checking their original bytes.
//! Uniform pixels, prior clear ancestors, and incomplete producer history are not proof.
use crate::{model::*, require, sha, validate::aspects, Result};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::PathBuf};
use wgpu_types::{TextureDimension, TextureFormat};

fn number(v: &Value, key: &str) -> Result<u64> {
    v.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("clear proof invalid integer {key}").into())
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("clear proof invalid array {key}").into())
}
fn exact_ref(a: &Value, b: &Value) -> bool {
    a == b
}

/// CPU verification deliberately supports only unambiguous 8-bit UNORM endpoints,
/// Float32 depth and 8-bit stencil. Other formats/fractional UNORM remain captured.
pub(crate) fn clear_pattern(
    content: &Value,
    format: TextureFormat,
    aspect: &Aspect,
) -> Result<Option<(ClearValue, Vec<u8>)>> {
    let value = content.get("value").ok_or("clear proof missing value")?;
    match aspect {
        Aspect::Color => {
            require(
                value["kind"] == "color",
                "clear proof value/aspect mismatch",
            )?;
            let entries = array(value, "bits")?;
            require(entries.len() == 4, "clear proof color arity")?;
            let mut bits = [0u64; 4];
            for (out, entry) in bits.iter_mut().zip(entries) {
                *out = entry.as_u64().ok_or("clear proof invalid color bits")?;
                require(
                    f64::from_bits(*out).is_finite(),
                    "clear proof nonfinite color",
                )?;
            }
            if !matches!(
                format,
                TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm
            ) {
                return Ok(None);
            }
            let decoded = bits.map(f64::from_bits);
            if !decoded.iter().all(|v| *v == 0. || *v == 1.) {
                return Ok(None);
            }
            let mut pixel = decoded.map(|v| if v == 1. { 255 } else { 0 });
            if format == TextureFormat::Bgra8Unorm {
                pixel.swap(0, 2);
            }
            Ok(Some((
                ClearValue::Color { rgba_bits: bits },
                pixel.to_vec(),
            )))
        }
        Aspect::Depth => {
            require(
                value["kind"] == "depth",
                "clear proof value/aspect mismatch",
            )?;
            let bits: u32 = number(value, "bits")?.try_into()?;
            require(
                (0.0..=1.0).contains(&f32::from_bits(bits)),
                "clear proof invalid depth",
            )?;
            if !matches!(
                format,
                TextureFormat::Depth32Float | TextureFormat::Depth32FloatStencil8
            ) {
                return Ok(None);
            }
            Ok(Some((
                ClearValue::Depth { value_bits: bits },
                bits.to_le_bytes().to_vec(),
            )))
        }
        Aspect::Stencil => {
            require(
                value["kind"] == "stencil",
                "clear proof value/aspect mismatch",
            )?;
            let value: u32 = number(value, "value")?.try_into()?;
            require(value <= 255, "clear proof stencil range")?;
            if format != TextureFormat::Depth32FloatStencil8 {
                return Ok(None);
            }
            Ok(Some((ClearValue::Stencil { value }, vec![value as u8])))
        }
    }
}

pub(crate) fn promote(
    manifest: &Value,
    scene: &mut Scene,
    payload: &[(PathBuf, Vec<u8>)],
) -> Result<Vec<Value>> {
    let Some(ledger) = manifest.get("initialization_ledger") else {
        return Ok(vec![]);
    };
    let Some(snapshot) = ledger.get("capture_start") else {
        return Ok(vec![]);
    };
    // Missing/disabled provenance preserves captured-only replay; it never becomes a clear.
    if ledger["capture_versions_valid"] != true
        || snapshot["schema"] != 2
        || snapshot.get("disabled_reason") != Some(&Value::Null)
    {
        return Ok(vec![]);
    }
    require(
        snapshot["identity_basis"]
            == "process-local returned wgpu Texture handle lifetime; not a backend allocation ID",
        "clear proof native identity basis",
    )?;
    let attachments = array(manifest, "attachments")?;
    let roots = array(snapshot, "targets")?;
    let records = array(snapshot, "records")?;
    let allocations = array(snapshot, "allocations")?;
    let missing = array(snapshot, "missing_predecessors")?;
    let frontier = array(snapshot, "unexpanded_frontier")?;
    let missing_allocations = array(snapshot, "missing_allocations")?;
    require(
        roots.len() == attachments.len(),
        "clear proof target root count",
    )?;
    let mut unique = BTreeSet::new();
    for root in roots {
        require(
            unique.insert(serde_json::to_string(root)?),
            "clear proof duplicate target root",
        )?;
        require(
            attachments
                .iter()
                .any(|a| exact_ref(root, &a["initial_version"])),
            "clear proof unrelated target root",
        )?;
    }
    let mut record_versions = BTreeSet::new();
    for record in records {
        let version = number(&record["reference"], "content_version")?;
        require(
            version > 0 && record_versions.insert(version),
            "clear proof duplicate/absent global content version",
        )?;
    }
    let mut allocation_ids = BTreeSet::new();
    for allocation in allocations {
        let id = number(allocation, "allocation_id")?;
        require(
            id > 0 && allocation_ids.insert(id),
            "clear proof duplicate/absent allocation identity",
        )?;
    }
    let operation = number(snapshot, "root_operation_ordinal")?;
    let first = array(manifest, "draws")?
        .first()
        .ok_or("clear proof first draw absent")?;
    require(
        operation == number(first, "command_sequence")? && operation > 0,
        "clear proof snapshot chronology",
    )?;
    let mut proofs = vec![];
    for attachment in attachments {
        let reference = &attachment["initial_version"];
        require(
            roots.iter().filter(|r| exact_ref(r, reference)).count() == 1,
            "clear proof target root mismatch",
        )?;
        let target_version = number(reference, "content_version")?;
        require(
            !records.iter().any(|r| {
                r["reference"]["content_version"].as_u64() == Some(target_version)
                    && !exact_ref(&r["reference"], reference)
            }),
            "clear proof version identifies a different subresource",
        )?;
        let matches = records
            .iter()
            .filter(|r| exact_ref(&r["reference"], reference))
            .collect::<Vec<_>>();
        require(matches.len() <= 1, "clear proof duplicate record")?;
        let Some(record) = matches.first() else {
            continue;
        };
        let content = &record["content"];
        if content["kind"] != "uniform_clear" {
            continue;
        }
        let allocation = number(reference, "allocation_id")?;
        let content_version = number(reference, "content_version")?;
        let sub = &reference["subresource"];
        let mip: u32 = number(sub, "mip")?.try_into()?;
        let layer: u32 = number(sub, "layer")?.try_into()?;
        let aspect: Aspect = serde_json::from_value(sub["aspect"].clone())?;
        require(
            !missing.iter().any(|r| exact_ref(r, reference))
                && !frontier
                    .iter()
                    .any(|r| exact_ref(&r["reference"], reference))
                && !missing_allocations
                    .iter()
                    .any(|id| id.as_u64() == Some(allocation)),
            "clear proof has contradictory missing provenance",
        )?;
        let record_operation = number(record, "operation_ordinal")?;
        require(
            record_operation > 0 && record_operation <= operation,
            "clear proof future/absent operation",
        )?;
        let parents = allocations
            .iter()
            .filter(|a| a["allocation_id"].as_u64() == Some(allocation))
            .collect::<Vec<_>>();
        require(
            parents.len() == 1,
            "clear proof native allocation missing/duplicated",
        )?;
        let parent = parents[0];
        let texture = scene
            .textures
            .iter_mut()
            .find(|t| t.native_allocation_id == Some(allocation))
            .ok_or("clear proof target allocation absent")?;
        let d = &texture.descriptor;
        require(
            texture.mutable_target
                && d.dimension == TextureDimension::D2
                && parent["dimension"] == "d2"
                && parent["format"] == format!("{:?}", d.format)
                && number(parent, "width")? == d.width as u64
                && number(parent, "height")? == d.height as u64
                && number(parent, "layers")? == d.layers as u64
                && number(parent, "mips")? == d.mips as u64,
            "clear proof native allocation descriptor mismatch",
        )?;
        let parent_aspects: Vec<Aspect> = serde_json::from_value(parent["aspects"].clone())?;
        require(
            parent_aspects.iter().cloned().collect::<BTreeSet<_>>()
                == aspects(d.format).into_iter().collect()
                && parent_aspects.len() == aspects(d.format).len(),
            "clear proof allocation aspect mismatch",
        )?;
        let format: TextureFormat = serde_json::from_value(attachment["format"].clone())?;
        require(
            content["view_format"] == format!("{format:?}"),
            "clear proof view format mismatch",
        )?;
        let initial = texture
            .initial
            .iter_mut()
            .find(|i| i.mip == mip && i.layer == layer && i.aspect == aspect)
            .ok_or("clear proof initial subresource absent")?;
        require(
            initial.content_version == content_version,
            "clear proof stale native version",
        )?;
        let blob = match &initial.origin {
            Initial::Captured { blob } => blob.clone(),
            _ => return Err("clear proof initial origin already changed".into()),
        };
        require(
            attachment["initial_blob"] == blob,
            "clear proof original blob mismatch",
        )?;
        let Some((value, pixel)) = clear_pattern(content, format, &aspect)? else {
            continue;
        };
        let raw = &payload
            .iter()
            .find(|(name, _)| name.to_str() == Some(blob.as_str()))
            .ok_or("clear proof verified initial bytes absent")?
            .1;
        let (_, _, row, rows) = crate::validate::slice_bytes(d, mip, &aspect)?;
        require(
            raw.len() as u64 == row as u64 * rows as u64
                && raw.chunks_exact(pixel.len()).remainder().is_empty()
                && raw.chunks_exact(pixel.len()).all(|p| p == pixel.as_slice()),
            "clear proof disagrees with complete original bytes",
        )?;
        initial.origin = Initial::UniformClear {
            value,
            view_format: format,
            operation: record_operation,
            reference_blob: Some(blob.clone()),
        };
        proofs.push(json!({"reference":reference,"operation_ordinal":record_operation,
            "view_format":format,"content":content,"original_blob":blob,"original_blob_sha256":sha(raw),
            "whole_original_image_byte_verified":true}));
    }
    Ok(proofs)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, Scene, Vec<(PathBuf, Vec<u8>)>) {
        let mut scene = Scene {
            schema: 1,
            provenance: Provenance::Authored {
                description: "clear proof test".into(),
            },
            eye: None,
            blobs: vec![],
            textures: vec![],
            buffers: vec![],
            pipelines: vec![],
            compute_pipelines: vec![],
            commands: vec![],
            expected: vec![],
            original_submission_completed: true,
            original_gpu_errors: Some(0),
            omitted_commands: 0,
        };
        let mut attachments = vec![];
        let mut roots = vec![];
        let mut records = vec![];
        let mut allocations = vec![];
        let mut payload = vec![];
        for (index, role) in ["color0", "color1", "color2", "color3", "depth", "stencil"]
            .into_iter()
            .enumerate()
        {
            let allocation = if index < 4 { 100 + index as u64 } else { 104 };
            let content_version = 10 + index as u64;
            let aspect = match role {
                "depth" => Aspect::Depth,
                "stencil" => Aspect::Stencil,
                _ => Aspect::Color,
            };
            let format = if index < 4 {
                TextureFormat::Bgra8Unorm
            } else {
                TextureFormat::Depth32FloatStencil8
            };
            let reference = json!({"allocation_id":allocation,"content_version":content_version,"subresource":{"mip":0,"layer":0,"aspect":aspect}});
            let blob = format!("{role}.bin");
            let bytes = match index {
                0 | 4 => vec![0; 16],
                5 => vec![7; 4],
                _ => vec![index as u8; 16],
            };
            payload.push((PathBuf::from(&blob), bytes));
            let initial = SliceInitial {
                mip: 0,
                layer: 0,
                aspect: aspect.clone(),
                content_version,
                origin: Initial::Captured { blob: blob.clone() },
            };
            if index == 5 {
                scene.textures.last_mut().unwrap().initial.push(initial);
            } else {
                scene.textures.push(Texture {
                    id: format!("native-texture:{allocation}"),
                    native_allocation_id: Some(allocation),
                    descriptor: TextureDesc {
                        dimension: TextureDimension::D2,
                        width: 2,
                        height: 2,
                        layers: 1,
                        mips: 1,
                        format,
                        view_formats: vec![],
                    },
                    mutable_target: true,
                    initial: vec![initial],
                });
                allocations.push(json!({"allocation_id":allocation,"dimension":"d2","format":format!("{format:?}"),"width":2,"height":2,"layers":1,"mips":1,"aspects":aspects(format)}));
            }
            let value = match index {
                0 => json!({"kind":"color","bits":[0,0,0,0]}),
                4 => json!({"kind":"depth","bits":0}),
                5 => json!({"kind":"stencil","value":7}),
                _ => Value::Null,
            };
            let content = if value.is_null() {
                json!({"kind":"produced","previous":reference,"missing_inputs":["unretained draw inputs"]})
            } else {
                json!({"kind":"uniform_clear","view_format":format!("{format:?}"),"value":value})
            };
            records.push(json!({"reference":reference,"operation_ordinal":100,"content":content}));
            attachments.push(json!({"role":role,"initial_version":reference,"initial_blob":blob,"format":format}));
            roots.push(reference);
        }
        let manifest = json!({"draws":[{"command_sequence":100}],"attachments":attachments,"initialization_ledger":{
            "capture_versions_valid":true,"capture_start":{"schema":2,"identity_basis":"process-local returned wgpu Texture handle lifetime; not a backend allocation ID",
                "disabled_reason":null,"root_operation_ordinal":100,"targets":roots,"records":records,"allocations":allocations,
                "missing_predecessors":[],"unexpanded_frontier":[],"missing_allocations":[],"dependency_closure_complete":false}}});
        (manifest, scene, payload)
    }
    #[test]
    fn direct_clear_roots_preserve_inherited_contents_and_raw_references() -> Result<()> {
        let (m, mut s, p) = fixture();
        let proofs = promote(&m, &mut s, &p)?;
        assert_eq!(proofs.len(), 3);
        for index in [0, 4] {
            assert!(matches!(
                s.textures[index].initial[0].origin,
                Initial::UniformClear {
                    reference_blob: Some(_),
                    ..
                }
            ));
        }
        assert!(matches!(
            s.textures[4].initial[1].origin,
            Initial::UniformClear {
                reference_blob: Some(_),
                ..
            }
        ));
        for index in 1..4 {
            assert!(matches!(
                s.textures[index].initial[0].origin,
                Initial::Captured { .. }
            ));
        }
        // Uniform inherited bytes themselves are insufficient, even with a prior clear reference.
        assert_eq!(p[1].1, vec![1; 16]);
        let (mut m, mut s, p) = fixture();
        m["initialization_ledger"]["capture_start"]["records"][0]["content"] =
            json!({"kind":"produced","previous":{"content_version":9}});
        assert_eq!(promote(&m, &mut s, &p)?.len(), 2);
        assert!(matches!(
            s.textures[0].initial[0].origin,
            Initial::Captured { .. }
        ));
        // Fractional UNORM and unqualified formats retain captured bytes without guessing rounding or transfer.
        let (mut m, mut s, p) = fixture();
        m["initialization_ledger"]["capture_start"]["records"][0]["content"]["value"]["bits"][0] =
            json!(0.5f64.to_bits());
        assert_eq!(promote(&m, &mut s, &p)?.len(), 2);
        // Explicit RGBA clear components must match native BGRA storage order.
        let (mut m, mut s, mut p) = fixture();
        m["initialization_ledger"]["capture_start"]["records"][0]["content"]["value"]["bits"] =
            json!([1f64.to_bits(), 0, 0, 1f64.to_bits()]);
        p[0].1 = [0, 0, 255, 255].repeat(4);
        assert_eq!(promote(&m, &mut s, &p)?.len(), 3);
        Ok(())
    }
    #[test]
    fn missing_or_disabled_clear_provenance_never_promotes() -> Result<()> {
        for mode in 0..4 {
            let (mut m, mut s, p) = fixture();
            match mode {
                0 => {
                    m.as_object_mut().unwrap().remove("initialization_ledger");
                }
                1 => m["initialization_ledger"]["capture_versions_valid"] = json!(false),
                2 => {
                    m["initialization_ledger"]["capture_start"]["disabled_reason"] =
                        json!("ledger exhausted")
                }
                _ => m["initialization_ledger"]["capture_start"]["schema"] = json!(99),
            }
            assert!(promote(&m, &mut s, &p)?.is_empty());
            assert!(s.textures.iter().all(|t| t
                .initial
                .iter()
                .all(|i| matches!(i.origin, Initial::Captured { .. }))));
        }
        let (mut m, mut s, p) = fixture();
        m["initialization_ledger"]["capture_start"]["records"][0]["reference"]["content_version"] =
            json!(9);
        assert_eq!(promote(&m, &mut s, &p)?.len(), 2);
        assert!(matches!(
            s.textures[0].initial[0].origin,
            Initial::Captured { .. }
        ));
        Ok(())
    }
    #[test]
    fn contradictory_clear_proof_or_pixels_fail_closed() -> Result<()> {
        let reject =
            |change: &dyn Fn(&mut Value, &mut Scene, &mut Vec<(PathBuf, Vec<u8>)>)| -> Result<()> {
                let (mut m, mut s, mut p) = fixture();
                change(&mut m, &mut s, &mut p);
                require(
                    promote(&m, &mut s, &p).is_err(),
                    "contradictory native clear proof admitted",
                )
            };
        reject(&|m, _, _| {
            let r = m["initialization_ledger"]["capture_start"]["records"][0].clone();
            m["initialization_ledger"]["capture_start"]["records"]
                .as_array_mut()
                .unwrap()
                .push(r);
        })?;
        reject(&|m, _, _| {
            let mut r = m["initialization_ledger"]["capture_start"]["records"][0].clone();
            r["reference"]["allocation_id"] = json!(999);
            m["initialization_ledger"]["capture_start"]["records"]
                .as_array_mut()
                .unwrap()
                .push(r);
        })?;
        reject(&|m, _, _| {
            let r = m["initialization_ledger"]["capture_start"]["allocations"][0].clone();
            m["initialization_ledger"]["capture_start"]["allocations"]
                .as_array_mut()
                .unwrap()
                .push(r);
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["targets"][0]["subresource"]["layer"] =
                json!(1)
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["records"][0]["operation_ordinal"] =
                json!(101)
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["root_operation_ordinal"] = json!(99)
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["allocations"][0]["width"] = json!(4)
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["allocations"][0]["format"] =
                json!("Rgba8Unorm")
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["records"][0]["content"]["view_format"] =
                json!("Bgra8UnormSrgb")
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["records"][0]["content"]["value"]["kind"] =
                json!("depth")
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["records"][0]["content"]["value"]["bits"]
                [0] = json!(f64::NAN.to_bits())
        })?;
        reject(&|m, _, _| {
            let r = m["attachments"][0]["initial_version"].clone();
            m["initialization_ledger"]["capture_start"]["missing_predecessors"] = json!([r]);
        })?;
        reject(&|m, _, _| {
            let r = m["attachments"][0]["initial_version"].clone();
            m["initialization_ledger"]["capture_start"]["unexpanded_frontier"] =
                json!([{"reference":r,"reason":"missing"}]);
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["missing_allocations"] = json!([100])
        })?;
        reject(&|m, _, _| {
            m["initialization_ledger"]["capture_start"]["records"][0]["reference"]
                ["allocation_id"] = json!(999);
        })?;
        reject(&|_, s, _| s.textures[0].initial[0].content_version = 9)?;
        for index in [0, 4, 5] {
            reject(&|_, _, p| p[index].1[0] ^= 1)?;
            reject(&|_, _, p| {
                p[index].1.pop();
            })?;
        }
        Ok(())
    }
}
