//! Inactive producer packet transport prerequisite. No production publish,
//! consume or registration adapter is installed; no flag can enable it yet.
//! Packet identity proves owned byte correlation only, never scene/family,
//! shader consumption, native replay or same-simulation stereo authority.
use std::collections::{HashMap, VecDeque};

pub(crate) const LIVE_ADAPTERS_ENABLED: bool = false;
pub(crate) const MAX_PACKETS: usize = 4096;
pub(crate) const MAX_OWNED_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_PACKET_BYTES: usize = 64 * 1024;
const MAX_PRODUCERS: usize = 256;
const DATA_START: u64 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Registration {
    pub process_id: u32,
    /// Opaque runtime lifetime supplied by registration; not an Instance/TLS certificate.
    pub runtime_lifetime: u64,
    pub memory_identity: u64,
    pub memory_bytes: u64,
    pub ring_address: u64,
    pub ring_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Session { pub registration: Registration, pub generation: u64 }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct Producer {
    /// HostState/Store lifetime nonce. Current architecture has one Instance per Store.
    pub store_lifetime: u64,
    pub observed_pthread: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Rejection {
    Disabled, Unregistered, InvalidRegistration, GenerationExhausted, StaleSession,
    InvalidProducer, ProducerCap, SerialExhausted, PacketCap, ByteCap,
    InvalidRange, InvalidOperands, DuplicatePendingAddress, MissingPacket,
    OutOfOrderAddress, CursorConflict, HeaderConflict, OperandConflict, InvalidWrap, Invalidated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Correlation { FullOwnedBytesEqual, HeaderOnlyRangeCapped }

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Metadata {
    pub session: Session,
    pub producer: Producer,
    pub serial: u64,
    pub previous_producer_serial: Option<u64>,
    pub raw_command_address: u64,
    pub raw_cursor: u64,
    pub opcode: u32,
    pub operand_words: u32,
    pub operand_byte_extent: u64,
    /// Noncryptographic correlation summary; equality uses all owned bytes.
    pub operand_fingerprint: Option<u64>,
    pub correlation: Correlation,
    pub observed_camera_epoch: Option<u64>,
    /// None is permanently enforced until separately certified ancestry hooks exist.
    pub family_ancestry: Option<()>,
    pub producer_tls_identity: Option<u64>,
    pub publication_success_certified: bool,
}

struct Packet { metadata: Metadata, operands: Vec<u8> }

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Consumption {
    Matched(Metadata),
    /// Original Reserve551 emits [1,0] outside Emit550's hook.
    UninstrumentedReserveWrap { session: Session, raw_cursor: u64 },
    Rejected(Rejection),
}

#[allow(dead_code)]
pub(crate) struct Transport {
    enabled: bool,
    session: Option<Session>,
    generation: u64,
    next_serial: u64,
    packets: VecDeque<Packet>,
    producers: HashMap<Producer, u64>,
    owned_bytes: usize,
    taint: Option<Rejection>,
    expected_consumer_cursor: u64,
}

#[allow(dead_code)]
impl Transport {
    pub(crate) fn new(enabled: bool) -> Self {
        Self { enabled,session:None,generation:0,next_serial:1,packets:VecDeque::new(),
            producers:HashMap::new(),owned_bytes:0,taint:None,expected_consumer_cursor:DATA_START }
    }

    /// Future live registration must happen synchronously in gpu::start BEFORE
    /// worker spawn, after matching the exact shared-memory/ring lifetime.
    pub(crate) fn register(&mut self, registration: Registration) -> Result<Session,Rejection> {
        if !self.enabled { return Err(Rejection::Disabled); }
        let valid=registration.runtime_lifetime!=0 && registration.memory_identity!=0
            && registration.process_id!=0 && registration.ring_bytes>=DATA_START+8
            && registration.ring_address%4==0 && registration.ring_bytes%4==0
            && registration.ring_bytes<=u32::MAX as u64
            && registration.ring_address.checked_add(registration.ring_bytes).is_some_and(|end|end<=registration.memory_bytes);
        if !valid { self.invalidate(Rejection::InvalidRegistration); return Err(Rejection::InvalidRegistration); }
        let Some(generation)=self.generation.checked_add(1) else {
            self.invalidate(Rejection::GenerationExhausted); return Err(Rejection::GenerationExhausted);
        };
        self.generation=generation;
        self.packets.clear(); self.producers.clear(); self.owned_bytes=0; self.taint=None;
        self.expected_consumer_cursor=DATA_START;
        let session=Session{registration,generation}; self.session=Some(session); Ok(session)
    }

    fn check(&self, session:Session) -> Result<(),Rejection> {
        if !self.enabled { return Err(Rejection::Disabled); }
        if self.session.is_none() { return Err(Rejection::Unregistered); }
        if self.session!=Some(session) { return Err(Rejection::StaleSession); }
        if let Some(reason)=self.taint { return Err(reason); }
        Ok(())
    }

    fn extent(session:Session,address:u64,words:u32) -> Option<(u64,u64)> {
        let cursor=address.checked_sub(session.registration.ring_address)?;
        let bytes=(words as u64).checked_mul(4)?;
        if cursor<DATA_START || cursor%4!=0 || cursor.checked_add(8)?.checked_add(bytes)?>session.registration.ring_bytes { return None; }
        Some((cursor,bytes))
    }

    /// `operands` MUST already be an owned snapshot from the certified producer
    /// midpoint, after checked header/ring/memory extents and before publication.
    /// Large ranges retain no prefix pretending to be the full payload.
    pub(crate) fn publish(&mut self, session:Session, producer:Producer, address:u64,
        header:[u32;2], operands:Option<Vec<u8>>, camera_epoch:Option<u64>) -> Result<u64,Rejection> {
        self.check(session)?;
        if producer.store_lifetime==0 { return self.fail(Rejection::InvalidProducer); }
        if !self.producers.contains_key(&producer) && self.producers.len()>=MAX_PRODUCERS { return self.fail(Rejection::ProducerCap); }
        let Some((cursor,bytes))=Self::extent(session,address,header[1]) else { return self.fail(Rejection::InvalidRange); };
        // Emit's ordinary packets are distinct from Reserve's raw wrap sentinel.
        if header[0]==1 { return self.fail(Rejection::InvalidWrap); }
        if self.packets.iter().any(|packet|packet.metadata.raw_command_address==address) { return self.fail(Rejection::DuplicatePendingAddress); }
        let correlation=if bytes as usize>MAX_PACKET_BYTES {
            if operands.is_some() { return self.fail(Rejection::InvalidOperands); }
            Correlation::HeaderOnlyRangeCapped
        } else {
            if operands.as_ref().map(Vec::len)!=Some(bytes as usize) { return self.fail(Rejection::InvalidOperands); }
            Correlation::FullOwnedBytesEqual
        };
        let operands=operands.unwrap_or_default();
        if self.packets.len()>=MAX_PACKETS { return self.fail(Rejection::PacketCap); }
        if operands.len()>MAX_OWNED_BYTES.saturating_sub(self.owned_bytes) { return self.fail(Rejection::ByteCap); }
        let serial=self.next_serial;
        let Some(next)=serial.checked_add(1) else { return self.fail(Rejection::SerialExhausted); };
        self.next_serial=next;
        let metadata=Metadata { session,producer,serial,previous_producer_serial:self.producers.insert(producer,serial),
            raw_command_address:address,raw_cursor:cursor,opcode:header[0],operand_words:header[1],operand_byte_extent:bytes,
            operand_fingerprint:(correlation==Correlation::FullOwnedBytesEqual).then(||fingerprint(&operands)),
            correlation,observed_camera_epoch:camera_epoch.filter(|epoch|*epoch!=0),family_ancestry:None,
            producer_tls_identity:None,publication_success_certified:false };
        self.owned_bytes+=operands.len(); self.packets.push_back(Packet{metadata,operands}); Ok(serial)
    }

    /// The consumer supplies the SAME registered session and a pre-ack owned
    /// operand snapshot. It never reads producer TLS or falls back to current view.
    pub(crate) fn consume(&mut self,session:Session,address:u64,header:[u32;2],operands:&[u8])->Consumption {
        if let Err(reason)=self.check(session) { return Consumption::Rejected(reason); }
        let Some((cursor,bytes))=Self::extent(session,address,header[1]) else { return self.reject(Rejection::InvalidRange); };
        if header[0]==1 {
            if header[1]!=0 || !operands.is_empty() { return self.reject(Rejection::InvalidWrap); }
            if cursor!=self.expected_consumer_cursor { return self.reject(Rejection::CursorConflict); }
            // No producer or serial is fabricated for this unhooked packet.
            // This checks cursor progression, not why Reserve needed to wrap.
            self.expected_consumer_cursor=DATA_START;
            return Consumption::UninstrumentedReserveWrap{session,raw_cursor:cursor};
        }
        let Some(front)=self.packets.front() else { return self.reject(Rejection::MissingPacket); };
        if front.metadata.raw_command_address!=address { return self.reject(Rejection::OutOfOrderAddress); }
        if cursor!=self.expected_consumer_cursor { return self.reject(Rejection::CursorConflict); }
        if front.metadata.opcode!=header[0] || front.metadata.operand_words!=header[1] { return self.reject(Rejection::HeaderConflict); }
        if front.metadata.correlation==Correlation::FullOwnedBytesEqual {
            if operands.len()!=bytes as usize || front.operands!=operands { return self.reject(Rejection::OperandConflict); }
        } else if !operands.is_empty() { return self.reject(Rejection::InvalidOperands); }
        let packet=self.packets.pop_front().unwrap(); self.owned_bytes-=packet.operands.len();
        self.expected_consumer_cursor=cursor+8+bytes;
        Consumption::Matched(packet.metadata)
    }

    fn fail<T>(&mut self,reason:Rejection)->Result<T,Rejection> { self.invalidate(reason); Err(reason) }
    fn reject(&mut self,reason:Rejection)->Consumption { self.invalidate(reason); Consumption::Rejected(reason) }
    pub(crate) fn invalidate(&mut self,reason:Rejection) {
        self.packets.clear(); self.producers.clear(); self.owned_bytes=0; self.taint=Some(reason);
    }
}

fn fingerprint(bytes:&[u8])->u64 {
    bytes.iter().fold(0xcbf29ce484222325,|hash,byte|(hash^*byte as u64).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registration()->Registration { Registration{process_id:1,runtime_lifetime:2,memory_identity:3,memory_bytes:1024,
        ring_address:0,ring_bytes:1024} }
    fn producer(id:u64)->Producer { Producer{store_lifetime:id,observed_pthread:7} }
    fn active()->(Transport,Session) { let mut transport=Transport::new(true); let session=transport.register(registration()).unwrap(); (transport,session) }
    fn matched(value:Consumption)->Metadata { match value { Consumption::Matched(metadata)=>metadata,other=>panic!("{other:?}") } }

    #[test] fn default_off_has_no_registration_or_packet() {
        assert!(!LIVE_ADAPTERS_ENABLED);
        let mut transport=Transport::new(false);
        assert_eq!(transport.register(registration()),Err(Rejection::Disabled)); assert!(transport.packets.is_empty());
    }

    #[test] fn owned_bytes_not_fingerprint_or_pointer_define_match() {
        let (mut t,s)=active(); let mut source=vec![1,2,3,4];
        t.publish(s,producer(1),64,[32,1],Some(source.clone()),Some(9)).unwrap(); source[0]=99;
        let packet=matched(t.consume(s,64,[32,1],&[1,2,3,4]));
        assert_eq!(packet.observed_camera_epoch,Some(9)); assert_eq!(packet.family_ancestry,None);
        assert_eq!(packet.producer_tls_identity,None); assert!(!packet.publication_success_certified);
        assert_eq!(t.consume(s,64,[32,1],&source),Consumption::Rejected(Rejection::MissingPacket));
    }

    #[test] fn repeated_address_after_consumption_has_new_serial_and_producer_lifetime() {
        let (mut t,s)=active();
        t.publish(s,producer(1),64,[2,0],Some(vec![]),None).unwrap(); let first=matched(t.consume(s,64,[2,0],&[]));
        assert!(matches!(t.consume(s,72,[1,0],&[]),Consumption::UninstrumentedReserveWrap{..}));
        t.publish(s,producer(2),64,[2,0],Some(vec![]),None).unwrap(); let second=matched(t.consume(s,64,[2,0],&[]));
        assert!(second.serial>first.serial); assert_ne!(first.producer,second.producer);
        assert_eq!(second.previous_producer_serial,None);
        assert!(matches!(t.consume(s,72,[1,0],&[]),Consumption::UninstrumentedReserveWrap{..}));
        t.publish(s,producer(2),64,[2,0],Some(vec![]),None).unwrap();
        assert_eq!(matched(t.consume(s,64,[2,0],&[])).previous_producer_serial,Some(second.serial));
    }

    #[test] fn abandoned_duplicate_missing_conflicting_and_out_of_order_packets_taint() {
        for reason in [Rejection::DuplicatePendingAddress,Rejection::HeaderConflict,Rejection::OperandConflict,Rejection::OutOfOrderAddress] {
            let (mut t,s)=active(); t.publish(s,producer(1),64,[32,1],Some(vec![1,2,3,4]),None).unwrap();
            let observed=match reason {
                Rejection::DuplicatePendingAddress=>t.publish(s,producer(1),64,[32,1],Some(vec![1,2,3,4]),None).unwrap_err(),
                Rejection::HeaderConflict=>match t.consume(s,64,[39,1],&[1,2,3,4]) {Consumption::Rejected(r)=>r,_=>panic!()},
                Rejection::OperandConflict=>match t.consume(s,64,[32,1],&[9,2,3,4]) {Consumption::Rejected(r)=>r,_=>panic!()},
                _=>match t.consume(s,80,[32,1],&[1,2,3,4]) {Consumption::Rejected(r)=>r,_=>panic!()},
            };
            assert_eq!(observed,reason); assert_eq!(t.consume(s,64,[32,1],&[1,2,3,4]),Consumption::Rejected(reason));
            assert!(t.packets.is_empty()); assert_eq!(t.owned_bytes,0);
        }
    }

    #[test] fn reserve_wrap_has_no_producer_and_does_not_consume_next_emit_packet() {
        let (mut t,s)=active(); t.publish(s,producer(1),64,[7,0],Some(vec![]),None).unwrap();
        matched(t.consume(s,64,[7,0],&[]));
        t.publish(s,producer(1),64,[7,0],Some(vec![]),None).unwrap();
        assert_eq!(t.consume(s,72,[1,0],&[]),Consumption::UninstrumentedReserveWrap{session:s,raw_cursor:72});
        assert_eq!(matched(t.consume(s,64,[7,0],&[])).serial,2);
    }

    #[test] fn stale_registration_cannot_consume_new_pointer_reuse() {
        let (mut t,old)=active();t.publish(old,producer(1),64,[2,0],Some(vec![]),None).unwrap();
        let new=t.register(registration()).unwrap(); assert_ne!(old.generation,new.generation);
        t.publish(new,producer(1),64,[2,0],Some(vec![]),None).unwrap();
        assert_eq!(t.consume(old,64,[2,0],&[]),Consumption::Rejected(Rejection::StaleSession));
        assert_eq!(matched(t.consume(new,64,[2,0],&[])).session,new);
    }

    #[test] fn cursor_gaps_and_explicit_lifecycle_invalidation_cannot_reuse_packets() {
        let (mut t,s)=active();t.publish(s,producer(1),80,[2,0],Some(vec![]),None).unwrap();
        assert_eq!(t.consume(s,80,[2,0],&[]),Consumption::Rejected(Rejection::CursorConflict));
        let (mut t,s)=active();t.publish(s,producer(1),64,[2,0],Some(vec![]),None).unwrap();
        t.invalidate(Rejection::Invalidated);
        assert_eq!(t.consume(s,64,[2,0],&[]),Consumption::Rejected(Rejection::Invalidated));
        let new=t.register(registration()).unwrap();
        assert_eq!(t.consume(s,64,[2,0],&[]),Consumption::Rejected(Rejection::StaleSession));
        assert!(t.packets.is_empty());assert_eq!(t.owned_bytes,0);
        t.publish(new,producer(1),64,[2,0],Some(vec![]),None).unwrap();
        assert_eq!(matched(t.consume(new,64,[2,0],&[])).serial,2);
    }

    #[test] fn packet_byte_producer_and_serial_caps_fail_closed() {
        let (mut t,s)=active(); t.next_serial=u64::MAX;
        assert_eq!(t.publish(s,producer(1),64,[2,0],Some(vec![]),None),Err(Rejection::SerialExhausted));
        let (mut t,s)=active(); t.owned_bytes=MAX_OWNED_BYTES;
        assert_eq!(t.publish(s,producer(1),64,[2,1],Some(vec![0;4]),None),Err(Rejection::ByteCap));
        let (mut t,s)=active();for id in 1..=MAX_PRODUCERS as u64 {t.producers.insert(producer(id),id);}
        assert_eq!(t.publish(s,producer(999),64,[2,0],Some(vec![]),None),Err(Rejection::ProducerCap));
        let (mut t,s)=active();for i in 0..MAX_PACKETS { t.packets.push_back(Packet{metadata:Metadata {session:s,producer:producer(1),serial:i as u64,previous_producer_serial:None,
            raw_command_address:800,raw_cursor:800,opcode:2,operand_words:0,operand_byte_extent:0,operand_fingerprint:None,correlation:Correlation::FullOwnedBytesEqual,
            observed_camera_epoch:None,family_ancestry:None,producer_tls_identity:None,publication_success_certified:false},operands:vec![]}); }
        assert_eq!(t.publish(s,producer(1),64,[2,0],Some(vec![]),None),Err(Rejection::PacketCap));
    }

    #[test] fn tiny_memory_checked_ranges_and_large_headers_never_claim_full_operands() {
        let (mut t,s)=active();assert_eq!(t.publish(s,producer(1),1020,[2,1],Some(vec![0;4]),None),Err(Rejection::InvalidRange));
        let mut t=Transport::new(true); let mut reg=registration(); reg.memory_bytes=2*MAX_PACKET_BYTES as u64;reg.ring_bytes=reg.memory_bytes;
        let s=t.register(reg).unwrap();let words=(MAX_PACKET_BYTES/4+1) as u32;
        t.publish(s,producer(1),64,[13,words],None,None).unwrap();
        let meta=matched(t.consume(s,64,[13,words],&[]));
        assert_eq!(meta.correlation,Correlation::HeaderOnlyRangeCapped);assert_eq!(meta.operand_fingerprint,None);
        assert_eq!(meta.family_ancestry,None);
    }

    #[test] fn concurrent_producer_transport_and_consumer_keep_serial_order() {
        use std::sync::{Arc,Mutex};
        let (t,s)=active();let shared=Arc::new(Mutex::new((t,DATA_START)));
        let mut joins=Vec::new();
        for id in 1..=8 {let shared=shared.clone();joins.push(std::thread::spawn(move||{
            let mut state=shared.lock().unwrap();let address=state.1;state.1+=12;
            state.0.publish(s,producer(id),address,[32,1],Some(vec![id as u8;4]),None).unwrap();
        }));}
        for join in joins {join.join().unwrap();}
        let order:Vec<_>=shared.lock().unwrap().0.packets.iter().map(|p|(p.metadata.raw_command_address,p.metadata.producer.store_lifetime)).collect();
        let consumer=shared.clone();let metadata=std::thread::spawn(move||{
            order.into_iter().map(|(address,id)|matched(consumer.lock().unwrap().0.consume(s,address,[32,1],&[id as u8;4]))).collect::<Vec<_>>()
        }).join().unwrap();
        assert_eq!(metadata.iter().map(|m|m.serial).collect::<Vec<_>>(),(1..=8).collect::<Vec<_>>());
        assert!(metadata.iter().all(|m|m.family_ancestry.is_none()));
    }

    #[test] fn interleaved_producer_consumer_owns_tiny_packet_until_ack() {
        use std::sync::{Arc,Mutex,mpsc};
        let (t,s)=active();let shared=Arc::new(Mutex::new(t));
        let (tx,rx)=mpsc::sync_channel(1);let (ack_tx,ack_rx)=mpsc::sync_channel(1);
        let producer_state=shared.clone();
        let producer_thread=std::thread::spawn(move||{
            for i in 0..32u8 {
                let address=64+i as u64*12;let mut memory=vec![i;4];
                producer_state.lock().unwrap().publish(s,producer(1),address,[32,1],Some(memory.clone()),None).unwrap();
                memory.fill(255);tx.send((address,i)).unwrap();ack_rx.recv().unwrap();
            }
        });
        let consumer_thread=std::thread::spawn(move||{
            for (address,i) in rx {
                let packet=matched(shared.lock().unwrap().consume(s,address,[32,1],&[i;4]));
                assert_eq!(packet.serial,i as u64+1);ack_tx.send(()).unwrap();
            }
        });
        producer_thread.join().unwrap();consumer_thread.join().unwrap();
    }
}
