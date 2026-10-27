//! Exact successful native module sources for private scene replay.
//!
//! Keys are actual wgpu module handles, never a guest shader ID or a
//! reconstructed variant name. Failed variants are not registered: a pipeline
//! that falls back to its base module therefore resolves the base source.
use super::{json, sha, Value};
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

const MAX_MODULES: usize = 4096;
const MAX_SOURCE_BYTES: usize = 32 << 20;
const MAX_EXPORT_BYTES: usize = 8 << 20;

/// A missing stage cannot disappear through filter_map and leave an empty,
/// apparently complete source export. The actual pipeline declares both fields.
pub(super) fn selected_pipeline_hashes(draws: &[Value]) -> (Vec<String>, Option<String>) {
    let mut hashes=Vec::new();let mut failure=None;
    for (index,draw) in draws.iter().enumerate() {
        let stages=draw.get("pipeline").and_then(|p|p.get("effective_shaders")).and_then(Value::as_object);
        for stage in ["vertex","fragment"] {
            match stages.and_then(|s|s.get(stage)) {
                Some(Value::Null) if stage=="fragment"=>{},
                Some(value) if value.get("status").and_then(Value::as_str)==Some("exact")=>{
                    match value.get("sha256").and_then(Value::as_str).filter(|h|h.len()==64 && h.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
                        Some(hash)=>hashes.push(hash.into()),
                        None=>{failure.get_or_insert_with(||format!("captured draw {index} {stage} source SHA is missing or invalid"));}
                    }
                }
                _=>{failure.get_or_insert_with(||format!("captured draw {index} {stage} module source is missing or unresolved"));}
            }
        }
    }
    (hashes,failure)
}

struct Source { text: String, origin: String }
pub(super) struct Registry<K> {
    modules: HashMap<K, String>,
    sources: BTreeMap<String, Source>,
    bytes: usize,
    failure: Option<&'static str>,
    max_modules: usize,
    max_bytes: usize,
}
impl<K: Eq + Hash + Clone> Registry<K> {
    pub(super) fn new() -> Self {
        Self { modules: HashMap::new(), sources: BTreeMap::new(), bytes: 0,
            failure: None, max_modules: MAX_MODULES, max_bytes: MAX_SOURCE_BYTES }
    }
    pub(super) fn remember(&mut self, module: &K, text: &str, origin: &str) {
        if self.failure.is_some() { return; }
        if text.len() > self.max_bytes || origin.len() > 4096 {
            self.failure = Some("shader source or origin exceeds bound"); return;
        }
        let hash = sha(text.as_bytes());
        if let Some(prior) = self.modules.get(module) {
            if prior != &hash { self.failure = Some("one native module has conflicting source"); }
            return;
        }
        if self.modules.len() >= self.max_modules {
            self.failure = Some("native module count bound exceeded"); return;
        }
        if let Some(prior) = self.sources.get(&hash) {
            if prior.text != text { self.failure = Some("shader source hash collision"); return; }
        } else {
            let Some(bytes) = self.bytes.checked_add(text.len()).and_then(|n| n.checked_add(origin.len()))
                .filter(|n| *n <= self.max_bytes) else {
                self.failure = Some("shader source byte bound exceeded"); return;
            };
            self.sources.insert(hash.clone(), Source { text: text.into(), origin: origin.into() });
            self.bytes = bytes;
        }
        self.modules.insert(module.clone(), hash);
    }
    pub(super) fn module(&self, module: &K) -> Value {
        match (self.failure, self.modules.get(module)) {
            (None, Some(hash)) => json!({"status":"exact", "sha256":hash, "entry_point":"main",
                "compilation_options":"wgpu default"}),
            (failure, _) => json!({"status":"unresolved", "reason":failure.unwrap_or("native module not registered")}),
        }
    }
    pub(super) fn snapshot(&self, selected: &[String]) -> Value {
        let mut failure = self.failure;
        let mut sources = BTreeMap::new();
        for hash in selected {
            if let Some(source) = self.sources.get(hash) {
                sources.insert(hash, json!({"wgsl":source.text, "origin":source.origin}));
            } else { failure = Some("selected shader source is missing"); }
        }
        let mut result = json!({"schema":1, "method":"successful actual ShaderModule creation",
            "complete":failure.is_none(), "failure":failure,
            "retained_module_count":self.modules.len(), "retained_source_bytes":self.bytes,
            "max_modules":self.max_modules, "max_source_bytes":self.max_bytes,
            "export_scope":"only modules referenced by captured draw pipelines", "sources":sources});
        if result.to_string().len() > MAX_EXPORT_BYTES {
            result["complete"] = false.into(); result["failure"] = "shader source JSON export bound exceeded".into();
            result["sources"] = json!({});
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_module_selects_final_variant_and_fallback() {
        let mut r = Registry::new();
        r.remember(&1, "base", "packed guest source");
        r.remember(&2, "flat remapped", "successful vertex variant");
        r.remember(&3, "alpha discard and hud", "successful fragment variant");
        for (module, text) in [(1,"base"),(2,"flat remapped"),(3,"alpha discard and hud")] {
            let hash = sha(text.as_bytes());
            assert_eq!(r.module(&module)["sha256"], hash);
            assert_eq!(r.snapshot(&[hash.clone()])["sources"][&hash]["wgsl"], text);
        }
        // A failed variant produces no module. The actual fallback handle
        // still resolves base bytes rather than the requested variant text.
        assert_eq!(r.module(&1)["sha256"], sha(b"base"));
        assert_eq!(r.module(&4)["status"], "unresolved");
    }
    #[test]
    fn native_handles_with_same_source_share_bytes_but_remain_distinct() {
        let mut r = Registry::new();
        r.remember(&1, "same", "first"); r.remember(&2, "same", "second");
        assert_eq!(r.modules.len(), 2); assert_eq!(r.sources.len(), 1);
        assert_eq!(r.bytes, 9); assert_eq!(r.module(&1), r.module(&2));
        r.remember(&1, "same", "cached"); assert_eq!(r.modules.len(), 2);
    }
    #[test]
    fn count_byte_and_identity_failures_are_latched_not_silent_truncation() {
        let mut r = Registry::new(); r.max_modules = 1;
        r.remember(&1, "a", ""); r.remember(&2, "a", "");
        assert_eq!(r.modules.len(), 1); assert_eq!(r.module(&1)["status"], "unresolved");
        assert_eq!(r.snapshot(&[])["complete"], false);
        let mut r = Registry::new(); r.max_bytes = 3;
        r.remember(&1, "ab", "c"); r.remember(&2, "d", "");
        assert_eq!(r.bytes, 3); assert!(r.failure.is_some());
        let mut r = Registry::new(); r.remember(&1, "a", ""); r.remember(&1, "b", "");
        assert_eq!(r.failure, Some("one native module has conflicting source"));
        r.remember(&2, "c", ""); assert_eq!(r.modules.len(), 1);
    }
    #[test]
    fn export_keeps_only_selected_sources_and_reports_missing_dependencies() {
        let mut r = Registry::new(); r.remember(&1, "a", ""); r.remember(&2, "b", "");
        let export = r.snapshot(&[sha(b"a")]);
        assert_eq!(export["sources"].as_object().unwrap().len(), 1);
        assert_eq!(export["complete"], true);
        assert_eq!(r.snapshot(&[sha(b"missing")])["complete"], false);
    }
    #[test]
    fn required_pipeline_stage_cannot_be_silently_omitted() {
        let exact=json!({"status":"exact","sha256":sha(b"vertex")});
        let draw=|vertex:Value,fragment:Value|json!({"pipeline":{"effective_shaders":{"vertex":vertex,"fragment":fragment}}});
        let (hashes,failure)=selected_pipeline_hashes(&[draw(exact.clone(),Value::Null)]);
        assert_eq!(hashes,vec![sha(b"vertex")]);assert!(failure.is_none());
        for bad in [json!({}),json!({"pipeline":{"effective_shaders":{"vertex":exact.clone()}}}),draw(json!({"status":"unresolved"}),Value::Null),draw(exact.clone(),json!({"status":"unresolved"})),draw(json!({"status":"exact","sha256":"bad"}),Value::Null)] {
            assert!(selected_pipeline_hashes(&[bad]).1.is_some());
        }
        let (hashes,failure)=selected_pipeline_hashes(&[draw(exact.clone(),json!({"status":"unresolved"}))]);
        assert_eq!(hashes,vec![sha(b"vertex")]);assert!(failure.unwrap().contains("fragment"));
    }

}
