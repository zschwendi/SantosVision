//! Private native texture provenance. Metadata only: no Texture handles, submits,
//! readbacks, replacement graphics commands, or inferred initialization.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Aspect {
    Color,
    Depth,
    Stencil,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct Subresource {
    pub mip: u32,
    pub layer: u32,
    pub aspect: Aspect,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct VersionRef {
    pub allocation_id: u64,
    pub content_version: u64,
    pub subresource: Subresource,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Allocation {
    pub allocation_id: u64,
    pub guest_id: u32,
    pub origin: String,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub mips: u32,
    pub dimension: String,
    pub format: String,
    pub aspects: Vec<Aspect>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ClearValue {
    Color { bits: [u64; 4] },
    Depth { bits: u32 },
    Stencil { value: u32 },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Content {
    Unknown {
        reason: String,
    },
    UniformClear {
        view_format: String,
        value: ClearValue,
    },
    Produced {
        operation: String,
        previous: Option<VersionRef>,
        sources: Vec<VersionRef>,
        missing_inputs: Vec<String>,
        full_subresource_write: bool,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Record {
    pub reference: VersionRef,
    pub operation_ordinal: u64,
    pub content: Content,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Frontier {
    pub reference: VersionRef,
    pub reason: String,
}
#[derive(Clone, Debug)]
pub(super) struct PassRoots {
    pub operation_ordinal: u64,
    pub targets: Vec<VersionRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub schema: u32,
    pub identity_basis: String,
    pub gpu_completion_required: bool,
    pub disabled_reason: Option<String>,
    pub targets: Vec<VersionRef>,
    pub root_operation_ordinal: u64,
    pub dependency_closure_complete: bool,
    pub record_limit: usize,
    pub serialized_byte_limit: usize,
    pub records: Vec<Record>,
    pub unexpanded_frontier: Vec<Frontier>,
    pub missing_predecessors: Vec<VersionRef>,
    pub missing_allocations: Vec<u64>,
    pub allocations: Vec<Allocation>,
    pub evicted_records: u64,
}

// No record embeds payload bytes or native resource handles. Exhaustion disables
// admission rather than letting an untracked write inherit an old clear proof.
pub(super) struct Ledger {
    allocations: BTreeMap<u64, Allocation>,
    current: BTreeMap<(u64, Subresource), VersionRef>,
    records: BTreeMap<u64, Record>,
    order: VecDeque<u64>,
    next: u64,
    operation: u64,
    max_allocations: usize,
    max_subresources: usize,
    max_records: usize,
    disabled: Option<String>,
    evicted: u64,
    pub last_pass_start: Option<PassRoots>,
}
impl Ledger {
    pub fn new() -> Self {
        Self::bounded(8192, 65536, 65536)
    }
    fn bounded(a: usize, s: usize, r: usize) -> Self {
        Self {
            allocations: BTreeMap::new(),
            current: BTreeMap::new(),
            records: BTreeMap::new(),
            order: VecDeque::new(),
            next: 1,
            operation: 0,
            max_allocations: a,
            max_subresources: s,
            max_records: r,
            disabled: None,
            evicted: 0,
            last_pass_start: None,
        }
    }
    pub fn active(&self) -> bool {
        self.disabled.is_none()
    }
    pub fn command(&mut self) {
        self.operation = self.operation.saturating_add(1);
    }
    fn fail(&mut self, reason: &str) {
        self.disabled.get_or_insert_with(|| reason.to_owned());
        // Keep the last known allocation roots for diagnosis. disabled_reason
        // makes their possibly stale contents inadmissible after failure.
    }
    pub fn register(&mut self, allocation: Allocation) {
        if !self.active() || self.allocations.contains_key(&allocation.allocation_id) {
            return;
        }
        if allocation.origin.len() > 128
            || allocation.format.len() > 128
            || allocation.dimension.len() > 8
            || allocation.aspects.len() > 3
        {
            self.fail("allocation metadata field exceeds bound");
            return;
        }
        let layers = if allocation.dimension == "d3" {
            1
        } else {
            allocation.layers
        };
        let count = (allocation.mips as usize)
            .checked_mul(layers as usize)
            .and_then(|n| n.checked_mul(allocation.aspects.len()));
        if self.allocations.len() >= self.max_allocations
            || count
                .and_then(|n| self.current.len().checked_add(n))
                .is_none_or(|n| n > self.max_subresources)
        {
            self.fail("native texture metadata capacity exceeded");
            return;
        }
        let id = allocation.allocation_id;
        self.allocations.insert(id, allocation.clone());
        for mip in 0..allocation.mips {
            for layer in 0..layers {
                for &aspect in &allocation.aspects {
                    self.write(
                        id,
                        Subresource { mip, layer, aspect },
                        Content::Unknown {
                            reason: "allocation has no observed original initialization".into(),
                        },
                    );
                }
            }
        }
    }
    pub fn retire(&mut self, allocation: u64) {
        self.allocations.remove(&allocation);
        for subresource in self.subresources(allocation) {
            self.current.remove(&(allocation, subresource));
        }
        // Bounded historical records deliberately remain available to immutable
        // predecessor references. Guest-ID reuse cannot resolve to a new object.
    }
    pub fn current(&self, allocation: u64, subresource: Subresource) -> Option<VersionRef> {
        self.active()
            .then(|| self.current.get(&(allocation, subresource)).copied())
            .flatten()
    }
    fn write(
        &mut self,
        allocation: u64,
        subresource: Subresource,
        content: Content,
    ) -> Option<VersionRef> {
        if !self.active() {
            return None;
        }
        if self.next == u64::MAX || self.max_records == 0 {
            self.fail("native texture version capacity exhausted");
            return None;
        }
        let reference = VersionRef {
            allocation_id: allocation,
            content_version: self.next,
            subresource,
        };
        self.next += 1;
        while self.records.len() >= self.max_records {
            if let Some(id) = self.order.pop_front() {
                self.records.remove(&id);
                self.evicted += 1;
            } else {
                break;
            }
        }
        self.current.insert((allocation, subresource), reference);
        self.records.insert(
            reference.content_version,
            Record {
                reference,
                operation_ordinal: self.operation,
                content,
            },
        );
        self.order.push_back(reference.content_version);
        Some(reference)
    }
    fn valid(&self, id: u64, s: Subresource) -> bool {
        self.allocations.get(&id).is_some_and(|a| {
            s.mip < a.mips
                && s.layer < if a.dimension == "d3" { 1 } else { a.layers }
                && a.aspects.contains(&s.aspect)
        })
    }
    pub fn clear(&mut self, id: u64, s: Subresource, format: String, value: ClearValue) {
        if format.len() > 128 {
            self.fail("clear format exceeds bound");
            return;
        }
        if !self.valid(id, s) {
            self.fail("clear references an untracked native subresource");
            return;
        }
        let matches = matches!(
            (&value, s.aspect),
            (ClearValue::Color { .. }, Aspect::Color)
                | (ClearValue::Depth { .. }, Aspect::Depth)
                | (ClearValue::Stencil { .. }, Aspect::Stencil)
        );
        if !matches {
            self.fail("clear aspect mismatch");
            return;
        }
        self.write(
            id,
            s,
            Content::UniformClear {
                view_format: format,
                value,
            },
        );
    }
    pub fn produced(
        &mut self,
        id: u64,
        s: Subresource,
        operation: &str,
        sources: Vec<VersionRef>,
        missing_inputs: Vec<String>,
        full: bool,
    ) {
        if operation.len() > 128
            || sources.len() > 16
            || missing_inputs.len() > 8
            || missing_inputs.iter().any(|s| s.len() > 256)
        {
            self.fail("producer metadata field exceeds bound");
            return;
        }
        if !self.valid(id, s) {
            self.fail("write references an untracked native subresource");
            return;
        }
        let previous = if full { None } else { self.current(id, s) };
        self.write(
            id,
            s,
            Content::Produced {
                operation: operation.into(),
                previous,
                sources,
                missing_inputs,
                full_subresource_write: full,
            },
        );
    }
    pub fn subresources(&self, id: u64) -> Vec<Subresource> {
        let first = Subresource {
            mip: 0,
            layer: 0,
            aspect: Aspect::Color,
        };
        let last = Subresource {
            mip: u32::MAX,
            layer: u32::MAX,
            aspect: Aspect::Stencil,
        };
        self.current
            .range((id, first)..=(id, last))
            .map(|((_, s), _)| *s)
            .collect()
    }
    pub fn remember_pass_start(&mut self, targets: Vec<VersionRef>) {
        if !self.active() {
            return;
        }
        // Called on every native pass: retain at most8colors+depth+stencil.
        // Never walk dependency history, clone records, or serialize here.
        if targets.len() > 10 {
            self.fail("pass root count exceeds bound");
            return;
        }
        self.last_pass_start = Some(PassRoots {
            operation_ordinal: self.operation,
            targets,
        });
    }
    pub fn capture_evidence(&self) -> serde_json::Value {
        const RECORDS_PER_SNAPSHOT: usize = 256;
        const BYTES_PER_SNAPSHOT: usize = 448 << 10;
        let roots = self.last_pass_start.as_ref();
        let targets = roots
            .map(|p| {
                p.targets
                    .iter()
                    .map(|r| {
                        self.current
                            .get(&(r.allocation_id, r.subresource))
                            .copied()
                            .unwrap_or(*r)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let pass = roots.map(|p| {
            self.snapshot_bounded(
                p.targets.clone(),
                p.operation_ordinal,
                RECORDS_PER_SNAPSHOT,
                BYTES_PER_SNAPSHOT,
            )
        });
        let capture = self.snapshot_bounded(
            targets,
            self.operation,
            RECORDS_PER_SNAPSHOT,
            BYTES_PER_SNAPSHOT,
        );
        let value = serde_json::json!({"pass_start":pass,"capture_start":capture,"not_a_complete_scene_certificate":true,
            "capture_versions_valid":self.active(),"maximum_expanded_records":512,"maximum_serialized_bytes":1<<20});
        // Individual projections reserve ample space for wrapper indentation.
        // This final safety path still retains roots and descriptors, never a
        // size-only receipt that loses the very allocation IDs being audited.
        if serde_json::to_vec_pretty(&value)
            .map(|v| v.len())
            .unwrap_or(usize::MAX)
            > 1 << 20
        {
            let pass = roots.map(|p| {
                self.snapshot_bounded(
                    p.targets.clone(),
                    p.operation_ordinal,
                    0,
                    BYTES_PER_SNAPSHOT,
                )
            });
            let targets = roots
                .map(|p| {
                    p.targets
                        .iter()
                        .map(|r| {
                            self.current
                                .get(&(r.allocation_id, r.subresource))
                                .copied()
                                .unwrap_or(*r)
                        })
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!({"pass_start":pass,"capture_start":self.snapshot_bounded(targets,self.operation,0,BYTES_PER_SNAPSHOT),
                "disabled_reason":"combined ledger export byte budget","not_a_complete_scene_certificate":true,"capture_versions_valid":false})
        } else {
            value
        }
    }
    pub fn snapshot(&self, targets: Vec<VersionRef>) -> Snapshot {
        self.snapshot_bounded(targets, self.operation, 256, 448 << 10)
    }
    fn projection(
        &self,
        targets: &[VersionRef],
        ordinal: u64,
        records: &[Record],
        frontier: &BTreeMap<VersionRef, String>,
        record_limit: usize,
        byte_limit: usize,
    ) -> Snapshot {
        let ids = targets
            .iter()
            .map(|r| r.allocation_id)
            .chain(records.iter().map(|r| r.reference.allocation_id))
            .chain(frontier.keys().map(|r| r.allocation_id))
            .collect::<std::collections::BTreeSet<_>>();
        let missing_allocations = ids
            .iter()
            .copied()
            .filter(|id| !self.allocations.contains_key(id))
            .collect::<Vec<_>>();
        let complete = self.disabled.is_none()
            && frontier.is_empty()
            && missing_allocations.is_empty()
            && !targets.is_empty()
            && records.iter().all(|r| match &r.content {
                Content::Unknown { .. } => false,
                Content::Produced { missing_inputs, .. } => missing_inputs.is_empty(),
                Content::UniformClear { .. } => true,
            });
        Snapshot {
            schema: 2,
            identity_basis:
                "process-local returned wgpu Texture handle lifetime; not a backend allocation ID"
                    .into(),
            gpu_completion_required: true,
            disabled_reason: self.disabled.clone(),
            targets: targets.to_vec(),
            root_operation_ordinal: ordinal,
            dependency_closure_complete: complete,
            record_limit,
            serialized_byte_limit: byte_limit,
            records: records.to_vec(),
            unexpanded_frontier: frontier
                .iter()
                .map(|(&reference, reason)| Frontier {
                    reference,
                    reason: reason.clone(),
                })
                .collect(),
            missing_predecessors: frontier.keys().copied().collect(),
            missing_allocations,
            allocations: ids
                .into_iter()
                .filter_map(|id| self.allocations.get(&id).cloned())
                .collect(),
            evicted_records: self.evicted,
        }
    }
    fn snapshot_bounded(
        &self,
        targets: Vec<VersionRef>,
        ordinal: u64,
        record_limit: usize,
        byte_limit: usize,
    ) -> Snapshot {
        let mut pending = VecDeque::from(targets.clone());
        let mut seen = std::collections::BTreeSet::new();
        let mut records = Vec::new();
        let mut frontier = targets
            .iter()
            .copied()
            .map(|r| (r, "unexpanded_budget_reservation".into()))
            .collect::<BTreeMap<_, String>>();
        while let Some(reference) = pending.pop_front() {
            if !seen.insert(reference) {
                continue;
            }
            if records.len() >= record_limit {
                for reason in frontier.values_mut() {
                    if reason == "unexpanded_budget_reservation" {
                        *reason = "record_limit".into();
                    }
                }
                break;
            }
            let Some(record) = self
                .records
                .get(&reference.content_version)
                .filter(|r| r.reference == reference)
            else {
                frontier.insert(reference, "record_evicted_or_unavailable".into());
                continue;
            };
            let mut candidate = frontier.clone();
            candidate.remove(&reference);
            let mut children = Vec::new();
            if let Content::Produced {
                previous, sources, ..
            } = &record.content
            {
                children.extend(previous);
                children.extend(sources);
            }
            for child in &children {
                if !seen.contains(child) {
                    candidate
                        .entry(*child)
                        .or_insert_with(|| "unexpanded_budget_reservation".into());
                }
            }
            records.push(record.clone());
            let projected = self.projection(
                &targets,
                ordinal,
                &records,
                &candidate,
                record_limit,
                byte_limit,
            );
            if serde_json::to_vec_pretty(&projected)
                .map(|v| v.len())
                .unwrap_or(usize::MAX)
                > byte_limit
            {
                records.pop();
                for reason in frontier.values_mut() {
                    if reason == "unexpanded_budget_reservation" {
                        *reason = "byte_budget".into();
                    }
                }
                break;
            }
            frontier = candidate;
            pending.extend(children);
        }
        self.projection(
            &targets,
            ordinal,
            &records,
            &frontier,
            record_limit,
            byte_limit,
        )
    }
}

use super::*;
impl Gpu {
    pub(super) fn texture_ledger_enabled(&self) -> bool {
        self.draw_capture
            .as_ref()
            .is_some_and(draw_capture::Capture::texture_ledger_active)
    }
    fn with_texture_ledger(&mut self, f: impl FnOnce(&mut Ledger)) {
        if let Some(l) = self
            .draw_capture
            .as_mut()
            .filter(|c| c.texture_ledger_active())
            .and_then(|c| c.texture_ledger.as_mut())
        {
            f(l);
        }
    }
    fn ledger_allocation(id: u32, tag: u64, t: &Tex, origin: &str) -> Allocation {
        Allocation {
            allocation_id: tag,
            guest_id: id,
            origin: origin.into(),
            width: t.desc.w,
            height: t.desc.h,
            layers: t.desc.d,
            mips: t.desc.mips,
            dimension: match t.desc.dim {
                TextureDimension::D1 => "d1",
                TextureDimension::D2 => "d2",
                TextureDimension::D3 => "d3",
            }
            .into(),
            format: format!("{:?}", t.format),
            aspects: if is_depth(t.format) {
                if has_stencil(t.format) {
                    vec![Aspect::Depth, Aspect::Stencil]
                } else {
                    vec![Aspect::Depth]
                }
            } else {
                vec![Aspect::Color]
            },
        }
    }
    /// The caller supplies the actual resolved view's normalized subresources.
    /// A stale view, separate proxy or dummy cannot be certified by guest ID.
    pub(super) fn texture_ledger_view_versions(
        &self,
        id: u32,
        view: &TextureView,
        subresources: &[Subresource],
    ) -> Result<Vec<VersionRef>, &'static str> {
        let ledger = self
            .draw_capture
            .as_ref()
            .and_then(|c| c.texture_ledger.as_ref())
            .ok_or("ledger disabled")?;
        let texture = self.tex(id).ok_or("current texture missing")?;
        let tag = if view.texture() == &texture.tex {
            texture.trace_native_identity
        } else {
            [
                &texture.proxy_depth,
                &texture.proxy_stencil,
                &texture.proxy_fdepth,
            ]
            .into_iter()
            .flatten()
            .find(|p| view.texture() == &p.tex)
            .and_then(|p| p.trace_native_identity)
        }
        .ok_or("resolved native view parent is not current texture or proxy")?;
        subresources
            .iter()
            .map(|&s| {
                ledger
                    .current(tag, s)
                    .ok_or("native subresource version is unavailable")
            })
            .collect()
    }
    pub(super) fn texture_ledger_replace(&mut self, id: u32, new: Option<&Obj>) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let old = self.tex(id).map(|t| {
            [
                t.trace_native_identity,
                t.proxy_depth.as_ref().and_then(|p| p.trace_native_identity),
                t.proxy_stencil
                    .as_ref()
                    .and_then(|p| p.trace_native_identity),
                t.proxy_fdepth
                    .as_ref()
                    .and_then(|p| p.trace_native_identity),
            ]
        });
        let new = match new {
            Some(Obj::Tex(t)) => t
                .trace_native_identity
                .map(|tag| Self::ledger_allocation(id, tag, t, "guest_texture")),
            _ => None,
        };
        self.with_texture_ledger(|l| {
            if let Some(tags) = old {
                for tag in tags.into_iter().flatten() {
                    l.retire(tag);
                }
            }
            if let Some(a) = new {
                l.register(a);
            }
        });
    }
    fn ledger_target(
        &self,
        id: u32,
        mip: u32,
        layer: u32,
        aspect: Aspect,
    ) -> Option<(u64, Subresource)> {
        let t = self.tex(id)?;
        Some((
            t.trace_native_identity?,
            Subresource {
                mip,
                layer: if t.desc.dim == TextureDimension::D3 {
                    0
                } else {
                    layer
                },
                aspect,
            },
        ))
    }
    pub(super) fn texture_ledger_clear(
        &mut self,
        id: u32,
        mip: u32,
        layer: u32,
        format: TextureFormat,
        value: ClearValue,
    ) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let aspect = match value {
            ClearValue::Color { .. } => Aspect::Color,
            ClearValue::Depth { .. } => Aspect::Depth,
            ClearValue::Stencil { .. } => Aspect::Stencil,
        };
        let key = self.ledger_target(id, mip, layer, aspect);
        let three = self
            .tex(id)
            .is_some_and(|t| t.desc.dim == TextureDimension::D3);
        self.with_texture_ledger(|l| {
            if let Some((id, s)) = key {
                // A 3D depth-slice clear does not establish its complete D3D mip.
                if three {
                    l.produced(id, s, "3d_slice_clear", vec![], vec![], false);
                } else {
                    l.clear(id, s, format!("{format:?}"), value);
                }
            } else {
                l.fail("clear has no retained native allocation");
            }
        });
    }
    pub(super) fn texture_ledger_write(
        &mut self,
        id: u32,
        mip: Option<u32>,
        layer: Option<u32>,
        operation: &str,
    ) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let tag = self.tex(id).and_then(|t| t.trace_native_identity);
        self.with_texture_ledger(|l| {
            if let Some(tag) = tag {
                for s in l.subresources(tag) {
                    if mip.is_none_or(|m| s.mip == m) && layer.is_none_or(|a| s.layer == a) {
                        l.produced(
                            tag,
                            s,
                            operation,
                            vec![],
                            vec![
                                "producer inputs are not retained by the initialization ledger"
                                    .into(),
                            ],
                            false,
                        );
                    }
                }
            } else {
                l.fail("write has no retained native allocation");
            }
        });
    }
    pub(super) fn texture_ledger_proxy(&mut self, id: u32, kind: &str, written: bool) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let Some(t) = self.tex(id) else {
            return;
        };
        let proxy = match kind {
            "depth" => t.proxy_depth.as_ref(),
            "stencil" => t.proxy_stencil.as_ref(),
            _ => t.proxy_fdepth.as_ref(),
        };
        let Some(p) = proxy else {
            return;
        };
        let Some(tag) = p.trace_native_identity else {
            return;
        };
        let mut allocation = Self::ledger_allocation(id, tag, t, kind);
        allocation.format = format!("{:?}", p.format);
        allocation.dimension = "d2".into();
        allocation.aspects = if is_depth(p.format) {
            vec![Aspect::Depth]
        } else {
            vec![Aspect::Color]
        };
        let source = t.trace_native_identity;
        let source_aspect = match kind {
            "depth" => Aspect::Depth,
            "stencil" => Aspect::Stencil,
            _ => Aspect::Color,
        };
        self.with_texture_ledger(|l| {
            l.register(allocation);
            if written {
                for sub in l.subresources(tag) {
                    let predecessor = source.and_then(|a| {
                        l.current(
                            a,
                            Subresource {
                                aspect: source_aspect,
                                ..sub
                            },
                        )
                    });
                    l.produced(
                        tag,
                        sub,
                        "native_proxy_conversion",
                        predecessor.into_iter().collect(),
                        if predecessor.is_none() {
                            vec!["proxy source predecessor unavailable".into()]
                        } else {
                            vec![]
                        },
                        true,
                    );
                }
            }
        });
    }
    pub(super) fn texture_ledger_discard(&mut self, id: u32, mip: u32, layer: u32) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let target = self.ledger_target(id, mip, layer, Aspect::Color);
        self.with_texture_ledger(|ledger| {
            if let Some((id, subresource)) = target {
                if ledger.valid(id, subresource) {
                    ledger.write(
                        id,
                        subresource,
                        Content::Unknown {
                            reason: "native attachment StoreOp::Discard".into(),
                        },
                    );
                } else {
                    ledger.fail("discard references untracked native subresource");
                }
            } else {
                ledger.fail("discard native allocation unavailable");
            }
        });
    }
    pub(super) fn texture_ledger_copy(
        &mut self,
        dst: u32,
        src: u32,
        dst_sub: Option<(u32, u32)>,
        src_sub: Option<(u32, u32)>,
        full: bool,
        operation: &str,
    ) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let dest = self.tex(dst).and_then(|t| t.trace_native_identity);
        let source = self.tex(src).and_then(|t| t.trace_native_identity);
        self.with_texture_ledger(|l| {
            if let Some(dest) = dest {
                // Collect every source version before any destination mutation; a
                // same-allocation alias must reference the old source generation.
                let writes = l
                    .subresources(dest)
                    .into_iter()
                    .filter(|s| dst_sub.is_none_or(|(m, a)| s.mip == m && s.layer == a))
                    .map(|sub| {
                        let input = source.and_then(|a| {
                            l.current(
                                a,
                                src_sub
                                    .map(|(m, layer)| Subresource {
                                        mip: m,
                                        layer,
                                        aspect: sub.aspect,
                                    })
                                    .unwrap_or(sub),
                            )
                        });
                        (sub, input)
                    })
                    .collect::<Vec<_>>();
                for (sub, input) in writes {
                    l.produced(
                        dest,
                        sub,
                        operation,
                        input.into_iter().collect(),
                        if input.is_none() {
                            vec!["copy source predecessor unavailable".into()]
                        } else {
                            vec![]
                        },
                        full,
                    );
                }
            } else {
                l.fail("copy destination native allocation unavailable");
            }
        });
    }
    pub(super) fn texture_ledger_pass_start(&mut self) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let mut keys = Vec::new();
        for rt in &self.s.rt {
            if rt.tex != 0 {
                if let Some(key) = self.ledger_target(rt.tex, rt.mip, rt.slice, Aspect::Color) {
                    keys.push(key);
                }
            }
        }
        let ds = &self.s.ds;
        if let Some(t) = self.tex(ds.tex) {
            for aspect in [Aspect::Depth, Aspect::Stencil] {
                if aspect != Aspect::Stencil || has_stencil(t.format) {
                    if let Some(key) = self.ledger_target(ds.tex, ds.mip, ds.slice, aspect) {
                        keys.push(key);
                    }
                }
            }
        }
        self.with_texture_ledger(|l| {
            let targets = keys
                .iter()
                .filter_map(|&(id, s)| l.current(id, s))
                .collect::<Vec<_>>();
            if targets.len() != keys.len() {
                l.fail("pass attachment has no current version");
            }
            l.remember_pass_start(targets);
        });
    }
    pub(super) fn texture_ledger_draw(&mut self) {
        if !self.texture_ledger_enabled() {
            return;
        }
        // Conservative until effective write-mask/state certificates are joined:
        // every writable attachment becomes produced; never keep a stale clear.
        let rt = self.s.rt.clone();
        let ds = self.s.ds.clone();
        for r in rt {
            if r.tex != 0 {
                self.texture_ledger_write(r.tex, Some(r.mip), Some(r.slice), "resolved_draw");
            }
        }
        if ds.tex != 0 {
            let keys = [
                (Aspect::Depth, ds.flags & 1 == 0),
                (Aspect::Stencil, ds.flags & 2 == 0),
            ]
            .into_iter()
            .filter_map(|(a, w)| {
                w.then(|| self.ledger_target(ds.tex, ds.mip, ds.slice, a))
                    .flatten()
            })
            .collect::<Vec<_>>();
            self.with_texture_ledger(|l| {
                for (id, s) in keys {
                    if l.valid(id, s) {
                        l.produced(
                            id,
                            s,
                            "resolved_draw",
                            vec![],
                            vec!["draw inputs and coverage require complete replay".into()],
                            false,
                        );
                    }
                }
            });
        }
        let storage = self.s.uav[..2]
            .iter()
            .flatten()
            .filter_map(|&id| {
                self.view_obj(id)
                    .filter(|v| !v.is_buf)
                    .map(|v| (v.res, v.a))
            })
            .collect::<Vec<_>>();
        for (id, mip) in storage {
            self.texture_ledger_write(id, Some(mip), None, "draw_storage_binding");
        }
    }
    pub(super) fn texture_ledger_dispatch(&mut self) {
        if !self.texture_ledger_enabled() {
            return;
        }
        let writes = self.s.uav[2]
            .iter()
            .filter_map(|&id| {
                self.view_obj(id)
                    .filter(|v| !v.is_buf)
                    .map(|v| (v.res, v.a))
            })
            .collect::<Vec<_>>();
        for (id, mip) in writes {
            self.texture_ledger_write(id, Some(mip), None, "compute_dispatch");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn allocation(id: u64) -> Allocation {
        Allocation {
            allocation_id: id,
            guest_id: 7,
            origin: "test".into(),
            width: 8,
            height: 8,
            layers: 2,
            mips: 2,
            dimension: "d2".into(),
            format: "Bgra8Unorm".into(),
            aspects: vec![Aspect::Color],
        }
    }
    fn sub(mip: u32, layer: u32) -> Subresource {
        Subresource {
            mip,
            layer,
            aspect: Aspect::Color,
        }
    }
    fn clear(l: &mut Ledger, id: u64, s: Subresource) {
        l.clear(
            id,
            s,
            "Bgra8Unorm".into(),
            ClearValue::Color { bits: [0; 4] },
        );
    }
    #[test]
    fn replacement_never_inherits_guest_id_clear() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        clear(&mut l, 1, sub(0, 0));
        let old = l.current(1, sub(0, 0)).unwrap();
        l.retire(1);
        l.register(allocation(2));
        assert!(l.current(1, sub(0, 0)).is_none());
        let new = l.current(2, sub(0, 0)).unwrap();
        assert_ne!(old, new);
        assert!(matches!(
            l.records[&new.content_version].content,
            Content::Unknown { .. }
        ));
        assert!(matches!(
            l.records[&old.content_version].content,
            Content::UniformClear { .. }
        ));
    }
    #[test]
    fn partial_write_preserves_exact_predecessor_and_other_subresources() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        clear(&mut l, 1, sub(0, 0));
        let old = l.current(1, sub(0, 0)).unwrap();
        let untouched = l.current(1, sub(0, 1));
        l.produced(
            1,
            sub(0, 0),
            "partial_upload",
            vec![],
            vec!["bytes missing".into()],
            false,
        );
        let now = l.current(1, sub(0, 0)).unwrap();
        assert!(
            matches!(&l.records[&now.content_version].content,Content::Produced{previous:Some(p),..}if *p==old)
        );
        assert_eq!(untouched, l.current(1, sub(0, 1)));
    }
    #[test]
    fn queued_clear_is_not_proof_until_materialized_before_load() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        let queued = ClearValue::Color { bits: [0; 4] };
        let before = l.current(1, sub(0, 0)).unwrap();
        assert!(matches!(
            l.records[&before.content_version].content,
            Content::Unknown { .. }
        ));
        l.clear(1, sub(0, 0), "Bgra8Unorm".into(), queued);
        let after = l.current(1, sub(0, 0)).unwrap();
        assert!(matches!(
            l.records[&after.content_version].content,
            Content::UniformClear { .. }
        ));
        assert!(l.snapshot(vec![after]).missing_predecessors.is_empty());
        assert_eq!(l.snapshot(vec![after]).records.len(), 1);
    }
    #[test]
    fn alias_resolves_same_native_subresource_not_guest_id() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        l.register(allocation(2));
        clear(&mut l, 1, sub(1, 1));
        assert_eq!(l.current(1, sub(1, 1)), l.current(1, sub(1, 1)));
        assert_ne!(l.current(1, sub(1, 1)), l.current(2, sub(1, 1)));
        assert!(matches!(
            l.records[&l.current(1, sub(0, 1)).unwrap().content_version].content,
            Content::Unknown { .. }
        ));
    }
    #[test]
    fn eviction_is_explicit_and_never_reconnects_to_current_version() {
        let mut l = Ledger::bounded(4, 32, 5);
        l.register(allocation(1));
        let old = l.current(1, sub(0, 0)).unwrap();
        for _ in 0..7 {
            l.produced(1, sub(0, 0), "draw", vec![], vec![], false);
        }
        let s = l.snapshot(vec![l.current(1, sub(0, 0)).unwrap()]);
        assert!(!s.missing_predecessors.is_empty());
        assert!(l.records.len() <= 5);
        assert!(!l.records.contains_key(&old.content_version));
        clear(&mut l, 1, sub(0, 0));
        assert!(l
            .snapshot(vec![l.current(1, sub(0, 0)).unwrap()])
            .missing_predecessors
            .is_empty());
    }
    #[test]
    fn capacity_disables_admission_before_untracked_lifetime() {
        let mut l = Ledger::bounded(1, 4, 8);
        l.register(allocation(1));
        l.register(allocation(2));
        assert!(!l.active());
        assert!(l.current(1, sub(0, 0)).is_none());
        assert!(l.snapshot(vec![]).disabled_reason.is_some());
    }
    #[test]
    fn depth_stencil_clear_versions_are_independent() {
        let mut l = Ledger::new();
        let mut a = allocation(1);
        a.aspects = vec![Aspect::Depth, Aspect::Stencil];
        l.register(a);
        let d = Subresource {
            mip: 0,
            layer: 0,
            aspect: Aspect::Depth,
        };
        let s = Subresource {
            aspect: Aspect::Stencil,
            ..d
        };
        let before = l.current(1, s);
        l.clear(
            1,
            d,
            "Depth32FloatStencil8".into(),
            ClearValue::Depth { bits: 0 },
        );
        assert_eq!(before, l.current(1, s));
        l.clear(
            1,
            s,
            "Depth32FloatStencil8".into(),
            ClearValue::Stencil { value: 7 },
        );
        assert_ne!(before, l.current(1, s));
    }
    #[test]
    fn copied_version_is_immutable_and_missing_allocation_is_explicit() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        l.register(allocation(2));
        clear(&mut l, 1, sub(0, 0));
        let source = l.current(1, sub(0, 0)).unwrap();
        l.produced(2, sub(0, 0), "full_copy", vec![source], vec![], true);
        let destination = l.current(2, sub(0, 0)).unwrap();
        l.produced(
            1,
            sub(0, 0),
            "later_upload",
            vec![],
            vec!["bytes".into()],
            false,
        );
        let snapshot = l.snapshot(vec![destination]);
        assert!(snapshot.records.iter().any(|r| r.reference == source));
        assert!(!snapshot
            .records
            .iter()
            .any(|r| r.reference == l.current(1, sub(0, 0)).unwrap()));
        l.retire(1);
        assert_eq!(l.snapshot(vec![destination]).missing_allocations, vec![1]);
    }
    #[test]
    fn same_allocation_copy_keeps_old_source_and_partial_destination() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        clear(&mut l, 1, sub(0, 0));
        let source = l.current(1, sub(0, 0)).unwrap();
        let dest = l.current(1, sub(1, 1)).unwrap();
        l.produced(1, sub(1, 1), "partial_copy", vec![source], vec![], false);
        let now = l.current(1, sub(1, 1)).unwrap();
        let Content::Produced {
            previous, sources, ..
        } = &l.records[&now.content_version].content
        else {
            panic!("producer missing")
        };
        assert_eq!(*previous, Some(dest));
        assert_eq!(sources, &[source]);
        assert_eq!(l.current(1, sub(0, 0)), Some(source));
    }
    #[test]
    fn operation_ordinal_and_raw_clear_bits_survive_serialization() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        l.command();
        l.command();
        let value = ClearValue::Color {
            bits: [
                (-0.0f64).to_bits(),
                0.25f64.to_bits(),
                1f64.to_bits(),
                2f64.to_bits(),
            ],
        };
        l.clear(1, sub(0, 0), "Bgra8UnormSrgb".into(), value.clone());
        let snapshot = l.snapshot(vec![l.current(1, sub(0, 0)).unwrap()]);
        assert_eq!(snapshot.records[0].operation_ordinal, 2);
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let decoded: Snapshot = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            decoded.records[0].content,
            Content::UniformClear {
                view_format: "Bgra8UnormSrgb".into(),
                value
            }
        );
    }
    #[test]
    fn metadata_and_export_bounds_fail_closed() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        l.produced(1, sub(0, 0), "draw", vec![], vec!["x".repeat(257)], false);
        assert!(!l.active());
        let mut l = Ledger::new();
        l.register(allocation(1));
        for _ in 0..1800 {
            l.produced(1, sub(0, 0), "draw", vec![], vec!["x".repeat(256)], false);
        }
        l.remember_pass_start(vec![l.current(1, sub(0, 0)).unwrap()]);
        let export = l.capture_evidence();
        assert_eq!(export["capture_start"]["targets"][0]["allocation_id"], 1);
        assert_eq!(
            export["capture_start"]["dependency_closure_complete"],
            false
        );
        assert!(!export["capture_start"]["unexpanded_frontier"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(serde_json::to_vec_pretty(&export).unwrap().len() <= 1 << 20);
        assert_eq!(export["not_a_complete_scene_certificate"], true);
    }
    #[test]
    fn pass_start_retains_only_immutable_roots_until_export() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        clear(&mut l, 1, sub(0, 0));
        let root = l.current(1, sub(0, 0)).unwrap();
        let records = l.records.len();
        for _ in 0..10000 {
            l.remember_pass_start(vec![root]);
        }
        assert_eq!(l.records.len(), records);
        assert_eq!(l.last_pass_start.as_ref().unwrap().targets, vec![root]);
        l.produced(
            1,
            sub(0, 0),
            "later_draw",
            vec![],
            vec!["inputs".into()],
            false,
        );
        let export = l.capture_evidence();
        assert_eq!(
            export["pass_start"]["targets"][0]["content_version"],
            root.content_version
        );
        assert_ne!(
            export["capture_start"]["targets"][0]["content_version"],
            root.content_version
        );
        assert_eq!(export["pass_start"]["dependency_closure_complete"], true);
        assert_eq!(
            export["capture_start"]["dependency_closure_complete"],
            false
        );
    }
    #[test]
    fn bounded_export_keeps_current_roots_and_explicit_frontier() {
        let mut l = Ledger::new();
        l.register(allocation(1));
        clear(&mut l, 1, sub(0, 0));
        for _ in 0..1000 {
            l.produced(1, sub(0, 0), "draw", vec![], vec![], false);
        }
        let root = l.current(1, sub(0, 0)).unwrap();
        let bounded = l.snapshot_bounded(vec![root], l.operation, 8, 448 << 10);
        assert_eq!(bounded.targets, vec![root]);
        assert_eq!(bounded.records.len(), 8);
        assert_eq!(bounded.unexpanded_frontier.len(), 1);
        assert_eq!(bounded.unexpanded_frontier[0].reason, "record_limit");
        assert!(!bounded.dependency_closure_complete);
        let byte_bounded = l.snapshot_bounded(vec![root], l.operation, 256, 4096);
        assert_eq!(byte_bounded.targets, vec![root]);
        assert!(byte_bounded
            .unexpanded_frontier
            .iter()
            .any(|f| f.reason == "byte_budget"));
        assert!(serde_json::to_vec_pretty(&byte_bounded).unwrap().len() <= 4096);
    }
    #[test]
    fn range_lookup_and_retirement_touch_only_one_native_allocation() {
        let mut l = Ledger::new();
        for id in 1..=1000 {
            l.register(allocation(id));
        }
        assert_eq!(
            l.subresources(500),
            vec![sub(0, 0), sub(0, 1), sub(1, 0), sub(1, 1)]
        );
        let prior = l.current(501, sub(0, 0));
        let count = l.current.len();
        l.retire(500);
        assert_eq!(l.current.len(), count - 4);
        assert_eq!(l.current(501, sub(0, 0)), prior);
        assert!(l.subresources(500).is_empty());
    }
}
