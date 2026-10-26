//! Metadata only. Published at construction and the existing ten-second log
//! tick; status never enters Gpu or retains this lock while creating JSON.
use std::sync::{Mutex,atomic::{AtomicU64,Ordering}};

#[derive(Clone,Copy,Default)]
pub(super) struct Counters {
    pub calls:u64,pub lookups:u64,pub hits:u64,pub fallbacks:u64,pub avoided:u64,
    pub mix:[u64;4],pub hit_mix:[u64;4],
}
#[derive(Clone,Copy)]
struct Snapshot {publisher:u64,ordinal:u64,counters:Counters}
static NEXT_PUBLISHER:AtomicU64=AtomicU64::new(1);
static PUBLISHED:Mutex<Option<Snapshot>>=Mutex::new(None);

pub(super) fn begin(enabled:bool)->u64 {
    // A new renderer retires the prior publisher. Exhaustion fails closed for
    // telemetry only; it cannot alias an older publisher or affect rendering.
    let mut slot=PUBLISHED.lock().unwrap_or_else(|p|p.into_inner());
    let id=if enabled {NEXT_PUBLISHER.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|id|id.checked_add(1)).unwrap_or(0)}else{0};
    *slot=(id!=0).then_some(Snapshot{publisher:id,ordinal:0,counters:Counters::default()});
    id
}
pub(super) fn publish(publisher:u64,counters:Counters) {
    publish_to(&PUBLISHED,publisher,counters);
}
fn publish_to(slot:&Mutex<Option<Snapshot>>,publisher:u64,counters:Counters) {
    if publisher==0 {return;}
    let mut current=slot.lock().unwrap_or_else(|p|p.into_inner());
    if let Some(old)=current.as_ref().filter(|old|old.publisher==publisher) {
        *current=Some(Snapshot{publisher,ordinal:old.ordinal.saturating_add(1),counters});
    }
}
fn copy_from(slot:&Mutex<Option<Snapshot>>)->Option<Snapshot> {
    *slot.lock().unwrap_or_else(|p|p.into_inner())
}
fn json(snapshot:Option<Snapshot>)->serde_json::Value {
    let Some(s)=snapshot else{return serde_json::json!({"enabled":false});};let c=s.counters;
    serde_json::json!({"enabled":true,"publisher_id":s.publisher,"publication_ordinal":s.ordinal,
        "periodic_not_current_frame":true,"publication_interval_seconds":10,"may_stale_while_idle":true,
        "calls":c.calls,"eligible_lookups":c.lookups,"hits":c.hits,"fallbacks":c.fallbacks,"owned_items_avoided":c.avoided,
        "binding_items":{"uniform":c.mix[0],"sampler":c.mix[1],"texture":c.mix[2],"storage_buffer":c.mix[3]},
        "hit_binding_items":{"uniform":c.hit_mix[0],"sampler":c.hit_mix[1],"texture":c.hit_mix[2],"storage_buffer":c.hit_mix[3]}})
}
pub(crate) fn status_json()->serde_json::Value {
    let snapshot=copy_from(&PUBLISHED); // the metadata guard is dropped here
    json(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_publisher_cannot_overwrite_new_renderer_snapshot() {
        let slot=Mutex::new(Some(Snapshot{publisher:8,ordinal:0,counters:Counters::default()}));
        publish_to(&slot,7,Counters{calls:999,..Default::default()});assert_eq!(copy_from(&slot).unwrap().counters.calls,0);
        publish_to(&slot,8,Counters{calls:23,..Default::default()});let s=copy_from(&slot).unwrap();assert_eq!((s.publisher,s.ordinal,s.counters.calls),(8,1,23));
        publish_to(&slot,0,Counters::default());assert_eq!(copy_from(&slot).unwrap().ordinal,1);
    }
    #[test]
    fn metadata_copy_is_consistent_and_unlocks_before_json() {
        let slot=std::sync::Arc::new(Mutex::new(None));let writer=slot.clone();
        let thread=std::thread::spawn(move||{for ordinal in 1..=10_000 {
            *writer.lock().unwrap()=Some(Snapshot{publisher:7,ordinal,counters:Counters{calls:ordinal,mix:[ordinal;4],..Default::default()}});
        }});
        for _ in 0..10_000 {
            let copied=copy_from(&slot);
            // Lock availability may race the separate writer; a second status
            // read is nevertheless bounded to one consistent metadata copy.
            let value=json(copied);
            if let Some(s)=copied {assert_eq!(s.ordinal,s.counters.calls);assert_eq!(s.counters.mix,[s.ordinal;4]);assert_eq!(value["calls"],s.ordinal);}
        }
        thread.join().unwrap();let copied=copy_from(&slot);assert!(slot.try_lock().is_ok());
        let value=json(copied);assert_eq!(value["publication_ordinal"],10_000);
        assert!(value.to_string().len()<1024);assert_eq!(json(None),serde_json::json!({"enabled":false}));
    }
}
