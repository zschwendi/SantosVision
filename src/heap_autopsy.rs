//! One-shot, read-only post-mortem of the guest dlmalloc state after an engine
//! thread traps. Nothing here runs while the game is healthy.
//!
//! Scope is diagnosis only. The observed streaming crash traps on the
//! chunk-header load in dlmalloc's large-bin tree walk: the immediate fault is
//! an invalid allocator pointer, and what wrote it is unknown. This report
//! names inconsistent free-list records, shows the bytes found there, and
//! walks the heap in address order for the first inconsistent chunk header.
//!
//! It is a best-effort snapshot of memory that other guest and host threads
//! may still be writing. It runs on any engine trap, so the allocator lock is
//! not necessarily held; the lock word is reported, and even a held lock only
//! excludes malloc/free, not writes into payloads. Each finding is therefore
//! checked again in a second pass and labelled as reproduced or not. Nothing
//! here repairs or masks a fault. Anything that could not be inspected (an
//! exhausted budget, an unreadable record, a segment list that does not end)
//! makes the pass incomplete, and an incomplete pass is never reported clean.
//! Only allocator records are checked, not the contents of allocations.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// Static guest addresses, read from emscripten_builtin_malloc/free/dlmallinfo
/// in game.wasm 11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0
/// and valid for that binary only. `recognized` is a sanity gate on the
/// allocator's magic, page size and lock flag; it is not a binary identity
/// check and cannot prove another build does not share those values.
#[derive(Clone, Copy)]
pub(crate) struct Layout { pub mparams: u64, pub gm: u64 }
pub(crate) const GAME: Layout = Layout { mparams: 29_600_768, gm: 29_600_816 };

// malloc_state field offsets for 64-bit pointers, 8-byte chunk alignment.
const DVSIZE: u64 = 8;
const TOPSIZE: u64 = 16;
const LEAST_ADDR: u64 = 24;
const DV: u64 = 32;
const TOP: u64 = 40;
const MAGIC: u64 = 64;
const SMALLBINS: u64 = 72;
const TREEBINS: u64 = 600;
const FOOTPRINT: u64 = 856;
const MFLAGS: u64 = 880;
const MUTEX: u64 = 888;
const SEGMENT: u64 = 928;

const PINUSE: u64 = 1;
const CINUSE: u64 = 2;
const FLAGS: u64 = 7;
const MIN_CHUNK: u64 = 32;
const FENCEPOST_HEAD: u64 = 11;
const MAX_FINDINGS: usize = 16;
const MAX_SEGMENTS: u32 = 64;
const MAX_REASONS: usize = 16;
/// Bounds the visited-set memory of the free-tree pass.
const MAX_TREE_CHUNKS: usize = 4_000_000;

/// One wall-clock and step allowance shared by every traversal of a pass.
pub(crate) struct Budget { deadline: Instant, steps: u64, used: u64, exhausted: bool }
impl Budget {
    pub(crate) fn new(wall: Duration, steps: u64) -> Self {
        Budget { deadline: Instant::now() + wall, steps, used: 0, exhausted: false }
    }
    /// Charges one record about to be inspected; false once the allowance is
    /// spent. The clock is sampled every 1024 records.
    fn spend(&mut self) -> bool {
        if self.exhausted { return false; }
        if self.used >= self.steps || (self.used % 1024 == 1023 && Instant::now() >= self.deadline) {
            self.exhausted = true;
            return false;
        }
        self.used += 1;
        true
    }
}

/// Bounds-checked little-endian reads of a guest memory image.
pub(crate) trait Guest {
    fn len(&self) -> u64;
    fn read(&self, address: u64, out: &mut [u8]) -> bool;
    fn u64(&self, address: u64) -> Option<u64> {
        let mut bytes = [0u8; 8];
        self.read(address, &mut bytes).then(|| u64::from_le_bytes(bytes))
    }
    fn u32(&self, address: u64) -> Option<u32> {
        let mut bytes = [0u8; 4];
        self.read(address, &mut bytes).then(|| u32::from_le_bytes(bytes))
    }
}

impl Guest for wasmtime::SharedMemory {
    fn len(&self) -> u64 { self.data().len() as u64 }
    fn read(&self, address: u64, out: &mut [u8]) -> bool {
        let data = self.data();
        let cells = usize::try_from(address).ok()
            .and_then(|start| data.get(start..start.checked_add(out.len())?));
        let Some(cells) = cells else { return false };
        for (cell, byte) in cells.iter().zip(out.iter_mut()) {
            // Guest threads may be writing this memory. Each byte is one atomic
            // load, so a multi-byte value can be torn; callers treat every
            // value as unverified and bounds-check it before following it.
            *byte = unsafe { AtomicU8::from_ptr(cell.get()) }.load(Ordering::Relaxed);
        }
        true
    }
}

fn hex(guest: &dyn Guest, address: u64, count: u64) -> String {
    let mut bytes = vec![0u8; count as usize];
    if !guest.read(address, &mut bytes) { return format!("0x{address:x}: unreadable"); }
    let text: String = bytes.iter().map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' }).collect();
    let digits: Vec<String> = bytes.chunks(8).map(|c| c.iter().map(|b| format!("{b:02x}")).collect()).collect();
    format!("0x{address:x}: {} |{text}|", digits.join(" "))
}

fn tree_bin_minimum(index: u64) -> u64 {
    (1u64 << ((index >> 1) + 8)) | ((index & 1) << ((index >> 1) + 7))
}

struct Report<'a> {
    guest: &'a dyn Guest,
    gm: u64,
    least: u64,
    lines: Vec<String>,
    findings: usize,
    found: Vec<String>,
    first_holder: Option<u64>,
    budget: Budget,
    /// Why part of the state was not inspected; never empty for a pass that stopped early.
    unfinished: Vec<String>,
    unfinished_total: usize,
}

/// Result of one pass over the snapshot.
pub(crate) struct Examination {
    pub lines: Vec<String>,
    /// Finding texts without numbering, for comparison between passes.
    pub findings: Vec<String>,
    /// Some allocator state was not inspected; the pass cannot be called clean.
    pub incomplete: bool,
}

/// How the address-order walk of one segment ended.
#[derive(PartialEq)]
enum SegmentEnd { Top, Fencepost, Stopped }

impl Report<'_> {
    fn note(&mut self, line: String) { if self.lines.len() < 400 { self.lines.push(line); } }

    /// Records that something was not inspected.
    fn unfinished(&mut self, reason: String) {
        if self.unfinished.contains(&reason) { return; }
        self.unfinished_total += 1;
        if self.unfinished.len() < MAX_REASONS {
            self.note(format!("NOT INSPECTED: {reason}"));
            self.unfinished.push(reason);
        }
    }

    /// Charges the shared budget; an exhausted budget is an unfinished pass.
    fn spend(&mut self, pass: &'static str) -> bool {
        if self.budget.spend() { return true; }
        self.unfinished(format!("{pass}: budget exhausted after {} inspected records", self.budget.used));
        false
    }

    /// Reads a word the inspection depends on. A failed read is recorded, so
    /// it can never be mistaken for an empty list or a clean record.
    fn need(&mut self, address: u64, what: &str) -> Option<u64> {
        let value = self.guest.u64(address);
        if value.is_none() { self.unfinished(format!("{what} at 0x{address:x} is unreadable")); }
        value
    }
    fn need32(&mut self, address: u64, what: &str) -> Option<u32> {
        let value = self.guest.u32(address);
        if value.is_none() { self.unfinished(format!("{what} at 0x{address:x} is unreadable")); }
        value
    }

    /// `holder` is the chunk (or allocator-state slot) whose stored field is wrong.
    fn finding(&mut self, holder: u64, what: String) {
        self.findings += 1;
        if self.findings > MAX_FINDINGS { return; }
        self.note(format!("FINDING {}: {what}", self.findings));
        self.found.push(what);
        let in_state = holder >= self.gm && holder < self.gm + SEGMENT + 32;
        if !in_state {
            self.note(format!("  holder chunk  {}", hex(self.guest, holder, 64)));
            self.note(format!("  before holder {}", hex(self.guest, holder.saturating_sub(64), 64)));
            self.first_holder.get_or_insert(holder);
        }
    }

    fn chunk_ok(&self, chunk: u64) -> bool {
        chunk >= self.least && chunk % 8 == 0 && chunk.checked_add(MIN_CHUNK).is_some_and(|end| end <= self.guest.len())
    }

    /// Checks one free chunk's own header and footer; returns its size.
    fn free_chunk(&mut self, chunk: u64, place: &str) -> Option<u64> {
        let head = self.need(chunk + 8, "free chunk header")?;
        let size = head & !FLAGS;
        if head & CINUSE != 0 || head & PINUSE == 0 || size < MIN_CHUNK {
            self.finding(chunk, format!("{place}: free chunk 0x{chunk:x} has head 0x{head:x} (expected free, previous in use, size >= 32)"));
            return None;
        }
        let next = chunk.checked_add(size).filter(|next| next.checked_add(16).is_some_and(|end| end <= self.guest.len()));
        let Some(next) = next else {
            self.finding(chunk, format!("{place}: free chunk 0x{chunk:x} size 0x{size:x} runs past guest memory"));
            return None;
        };
        let (foot, next_head) = (self.need(next, "free chunk footer")?, self.need(next + 8, "next chunk header")?);
        if foot != size || next_head & PINUSE != 0 {
            self.finding(chunk, format!("{place}: free chunk 0x{chunk:x} size 0x{size:x} disagrees with the next chunk (footer 0x{foot:x}, next head 0x{next_head:x})"));
        }
        Some(size)
    }

    fn tree_bins(&mut self, treemap: u32) {
        let mut seen = HashSet::new();
        'bins: for index in 0..32u64 {
            let slot = self.gm + TREEBINS + index * 8;
            let Some(root) = self.need(slot, "tree bin root slot") else { continue };
            if (root != 0) != (treemap >> index & 1 != 0) {
                self.finding(slot, format!("tree bin {index}: root 0x{root:x} disagrees with treemap bit"));
            }
            // (node, holder of the pointer to it, field in the holder, expected parent)
            let mut pending = vec![(root, slot, "root", slot)];
            while let Some((node, holder, field, parent)) = pending.pop() {
                if node == 0 { continue; }
                if self.findings > MAX_FINDINGS {
                    self.unfinished(format!("tree bins: stopped after {MAX_FINDINGS} findings"));
                    break 'bins;
                }
                if seen.len() >= MAX_TREE_CHUNKS {
                    self.unfinished(format!("tree bins: more than {MAX_TREE_CHUNKS} free chunks"));
                    break 'bins;
                }
                if !self.spend("tree bins") { break 'bins; }
                if !self.chunk_ok(node) {
                    self.finding(holder, format!("tree bin {index}: {field} of 0x{holder:x} is 0x{node:x}, not a chunk inside guest memory (0x{:x} bytes)", self.guest.len()));
                    continue;
                }
                if !seen.insert(node) {
                    self.finding(holder, format!("tree bin {index}: {field} of 0x{holder:x} revisits 0x{node:x} (cycle)"));
                    continue;
                }
                let place = format!("tree bin {index}");
                let Some(size) = self.free_chunk(node, &place) else { continue };
                let upper = if index == 31 { u64::MAX } else { tree_bin_minimum(index + 1) };
                if size < tree_bin_minimum(index) || size >= upper {
                    self.finding(node, format!("tree bin {index}: chunk 0x{node:x} size 0x{size:x} is outside this bin's range"));
                }
                let (stored_parent, stored_index) = (self.need(node + 48, "tree chunk parent"), self.need32(node + 56, "tree chunk index"));
                if let (Some(stored_parent), Some(stored_index)) = (stored_parent, stored_index) {
                    if stored_parent != parent || stored_index != index as u32 {
                        self.finding(node, format!("tree bin {index}: chunk 0x{node:x} has parent 0x{stored_parent:x} index {stored_index} (expected parent 0x{parent:x})"));
                    }
                }
                // Equal-size chunks hang off the tree node in a ring through fd/bk.
                let (mut previous, mut ring) = (node, self.need(node + 16, "tree chunk fd").unwrap_or(node));
                while ring != node {
                    if seen.len() >= MAX_TREE_CHUNKS {
                        self.unfinished(format!("tree bins: more than {MAX_TREE_CHUNKS} free chunks"));
                        break 'bins;
                    }
                    if !self.spend("tree bins") { break 'bins; }
                    if !self.chunk_ok(ring) {
                        self.finding(previous, format!("tree bin {index}: fd of 0x{previous:x} is 0x{ring:x}, not a chunk inside guest memory"));
                        break;
                    }
                    if !seen.insert(ring) {
                        self.finding(previous, format!("tree bin {index}: fd of 0x{previous:x} revisits 0x{ring:x}"));
                        break;
                    }
                    if self.need(ring + 24, "ring chunk bk").is_some_and(|bk| bk != previous) {
                        self.finding(ring, format!("tree bin {index}: bk of 0x{ring:x} does not point back to 0x{previous:x}"));
                    }
                    if self.free_chunk(ring, &place).is_some_and(|ring_size| ring_size != size) {
                        self.finding(ring, format!("tree bin {index}: ring chunk 0x{ring:x} differs in size from 0x{node:x}"));
                    }
                    previous = ring;
                    let Some(next) = self.need(ring + 16, "ring chunk fd") else { break };
                    ring = next;
                }
                for (child, field) in [(node + 32, "child[0]"), (node + 40, "child[1]")] {
                    if let Some(next) = self.need(child, "tree chunk child") { pending.push((next, node, field, node)); }
                }
            }
        }
        self.note(format!("tree bins: {} free chunks visited", seen.len()));
    }

    fn small_bins(&mut self, smallmap: u32) {
        let mut visited = 0usize;
        'bins: for index in 1..32u64 {
            let bin = self.gm + SMALLBINS + index * 16;
            let Some(first) = self.need(bin + 16, "small bin head") else { continue };
            let (mut previous, mut chunk) = (bin, first);
            if (chunk != bin) != (smallmap >> index & 1 != 0) {
                self.finding(bin, format!("small bin {index}: list state disagrees with smallmap bit"));
            }
            while chunk != bin {
                if self.findings > MAX_FINDINGS {
                    self.unfinished(format!("small bins: stopped after {MAX_FINDINGS} findings"));
                    break 'bins;
                }
                if !self.spend("small bins") { break 'bins; }
                visited += 1;
                if !self.chunk_ok(chunk) {
                    self.finding(previous, format!("small bin {index}: fd of 0x{previous:x} is 0x{chunk:x}, not a chunk inside guest memory"));
                    break;
                }
                match self.need(chunk + 24, "small chunk bk") {
                    Some(bk) if bk == previous => {}
                    Some(_) => {
                        self.finding(chunk, format!("small bin {index}: bk of 0x{chunk:x} does not point back to 0x{previous:x}"));
                        break;
                    }
                    None => break,
                }
                if self.free_chunk(chunk, &format!("small bin {index}")).is_some_and(|size| size != index * 8) {
                    self.finding(chunk, format!("small bin {index}: chunk 0x{chunk:x} has the wrong size for this bin"));
                }
                previous = chunk;
                let Some(next) = self.need(chunk + 16, "small chunk fd") else { break };
                chunk = next;
            }
        }
        self.note(format!("small bins: {visited} free chunks visited"));
    }

    /// Walks one segment in address order. Only `Top` and `Fencepost` mean
    /// the segment was inspected to its end.
    fn walk_segment(&mut self, index: u32, base: u64, end: u64, top: u64, chunks: &mut u64) -> SegmentEnd {
        let (mut chunk, mut previous, mut previous_inuse) = ((base + 7) & !7, 0u64, true);
        loop {
            if chunk == top { return SegmentEnd::Top; }
            if chunk.checked_add(16).is_none_or(|header_end| header_end > end) {
                self.finding(if previous != 0 { previous } else { self.gm + SEGMENT },
                    format!("segment {index}: chunks end at 0x{chunk:x} (segment end 0x{end:x}) without reaching the top chunk or a fencepost"));
                return SegmentEnd::Stopped;
            }
            if !self.spend("address-order walk") {
                self.note(format!("address-order walk stopped by the budget at 0x{chunk:x} after {chunks} chunks; the rest of the heap was not inspected"));
                return SegmentEnd::Stopped;
            }
            *chunks += 1;
            let Some(head) = self.need(chunk + 8, "chunk header") else { return SegmentEnd::Stopped };
            if head == FENCEPOST_HEAD { return SegmentEnd::Fencepost; }
            let size = head & !FLAGS;
            let next = chunk.saturating_add(size);
            if size < MIN_CHUNK || next > end || (head & PINUSE != 0) != previous_inuse {
                self.finding(chunk, format!("PHYSICAL BREAK: chunk 0x{chunk:x} has head 0x{head:x}; segment ends 0x{end:x}; previous chunk 0x{previous:x} was {}", if previous_inuse { "in use" } else { "free" }));
                if previous != 0 {
                    let previous_size = chunk - previous;
                    self.note(format!("  previous chunk size 0x{previous_size:x} ({previous_size} bytes, user pointer 0x{:x})", previous + 16));
                    self.note(format!("  previous start {}", hex(self.guest, previous, 64)));
                    self.note("  (the previous chunk is a candidate source of an overrun, not proof of the writer)".into());
                }
                self.note(format!("  before break   {}", hex(self.guest, chunk.saturating_sub(64), 64)));
                self.note(format!("  at break       {}", hex(self.guest, chunk, 96)));
                self.unfinished(format!("segment {index} after the break at 0x{chunk:x}"));
                return SegmentEnd::Stopped;
            }
            if let Some(holder) = self.first_holder.filter(|holder| *holder >= chunk && *holder < next) {
                self.note(format!("first finding's holder 0x{holder:x} lies in chunk 0x{chunk:x} size 0x{size:x} ({}); physically previous chunk 0x{previous:x} size 0x{:x} was {}",
                    if head & CINUSE != 0 { "in use" } else { "free" }, chunk - previous.min(chunk), if previous_inuse { "in use" } else { "free" }));
                if previous != 0 { self.note(format!("  previous start {}", hex(self.guest, previous, 64))); }
                self.first_holder = None;
            }
            previous = chunk;
            previous_inuse = head & CINUSE != 0;
            chunk = next;
        }
    }

    /// Address-order walk of every segment. An overflow damages headers in
    /// address order, so the first inconsistent header and the chunk in front
    /// of it are where to look. The previous chunk is a candidate source, not
    /// proof of the writer. Every early stop is recorded.
    fn physical(&mut self, top: u64) {
        let started = Instant::now();
        let (mut chunks, mut segments, mut reached_top, mut stopped) = (0u64, 0u32, false, false);
        let mut visited = HashSet::new();
        // The first record is part of malloc_state; later ones live in the heap.
        let (mut record, mut holder) = (self.gm + SEGMENT, self.gm + SEGMENT);
        while record != 0 {
            if segments == MAX_SEGMENTS {
                self.unfinished(format!("segment list continues past {MAX_SEGMENTS} records (next record 0x{record:x})"));
                stopped = true;
                break;
            }
            if !visited.insert(record) {
                self.finding(holder, format!("segment list: next pointer of record 0x{holder:x} returns to record 0x{record:x} (cycle)"));
                break;
            }
            let mut words = [0u8; 32];
            if !self.guest.read(record, &mut words) {
                self.finding(holder, format!("segment list: next pointer of record 0x{holder:x} is 0x{record:x}, not readable guest memory (0x{:x} bytes)", self.guest.len()));
                self.unfinished(format!("segments after record 0x{holder:x} (record 0x{record:x} is unreadable)"));
                stopped = true;
                break;
            }
            segments += 1;
            let field = |at: usize| u64::from_le_bytes(words[at..at + 8].try_into().expect("eight bytes"));
            let (base, size, next) = (field(0), field(8), field(16));
            self.note(format!("segment {segments}: record 0x{record:x} base 0x{base:x} size 0x{size:x} flags 0x{:x}", field(24) as u32));
            let end = base.checked_add(size).filter(|end| base >= self.least && size >= MIN_CHUNK && *end <= self.guest.len());
            match end {
                Some(end) => match self.walk_segment(segments, base, end, top, &mut chunks) {
                    SegmentEnd::Top => reached_top = true,
                    SegmentEnd::Fencepost => {}
                    SegmentEnd::Stopped => stopped = true,
                },
                None => {
                    self.finding(record, format!("segment {segments}: base 0x{base:x} size 0x{size:x} is not a region inside guest memory at or above least_addr 0x{:x}", self.least));
                    self.unfinished(format!("chunks of segment {segments} (bounds invalid)"));
                    stopped = true;
                }
            }
            (holder, record) = (record, next);
        }
        if !stopped && !reached_top {
            self.finding(self.gm + TOP, format!("top chunk 0x{top:x} was not reached in any of the {segments} segment(s)"));
        }
        self.note(format!("address-order walk: {chunks} chunks in {segments} segment(s), {}, {:.3} s",
            if stopped { "stopped early" } else { "every segment walked to its end" }, started.elapsed().as_secs_f64()));
    }
}

/// One pass of pure analysis. `budget` bounds every traversal together.
pub(crate) fn examine(guest: &dyn Guest, layout: Layout, budget: Budget) -> Examination {
    let word = |address| guest.u64(address).unwrap_or(0);
    let (gm, mparams) = (layout.gm, layout.mparams);
    // Every malloc_state field read below lies inside this readable range.
    let readable = gm.checked_add(SEGMENT + 32).is_some_and(|end| end <= guest.len());
    let recognized = readable && word(mparams) != 0 && word(mparams) == word(gm + MAGIC) && word(mparams + 8) == 4096
        && guest.u32(gm + MFLAGS).is_some_and(|flags| flags & 2 != 0);
    if !recognized {
        let line = format!("allocator state not recognized at 0x{gm:x} (magic 0x{:x}/0x{:x}, page 0x{:x}); nothing interpreted, so this is not a clean result", word(mparams), word(gm + MAGIC), word(mparams + 8));
        return Examination { lines: vec![line], findings: Vec::new(), incomplete: true };
    }
    let (smallmap, treemap) = (guest.u32(gm).unwrap_or(0), guest.u32(gm + 4).unwrap_or(0));
    let (top, topsize) = (word(gm + TOP), word(gm + TOPSIZE));
    let lock = guest.u32(gm + MUTEX + 4);
    let mut report = Report { guest, gm, least: word(gm + LEAST_ADDR), lines: Vec::new(), findings: 0, found: Vec::new(),
        first_holder: None, budget, unfinished: Vec::new(), unfinished_total: 0 };
    report.note(format!("guest memory 0x{:x} bytes; footprint 0x{:x}; least_addr 0x{:x}; top 0x{top:x} size 0x{topsize:x}; dv 0x{:x} size 0x{:x}; smallmap 0x{smallmap:08x} treemap 0x{treemap:08x}",
        guest.len(), word(gm + FOOTPRINT), report.least, word(gm + DV), word(gm + DVSIZE)));
    report.note(match lock {
        Some(0) => "allocator lock word 0: not held, so malloc/free may be changing bins during this snapshot".into(),
        Some(value) => format!("allocator lock word {value}: held, so malloc/free on other threads are excluded; writes into chunk payloads are not"),
        None => "allocator lock word unreadable".into(),
    });
    if top == 0 {
        report.unfinished("address-order walk: the allocator has no top chunk".into());
    } else if !report.chunk_ok(top) || top.saturating_add(topsize) > guest.len() || word(top + 8) != topsize | PINUSE {
        report.finding(top, format!("top chunk 0x{top:x}: head 0x{:x} does not match topsize 0x{topsize:x} or leaves guest memory", word(top + 8)));
    }
    report.tree_bins(treemap);
    report.small_bins(smallmap);
    if top != 0 { report.physical(top); }
    let (findings, incomplete) = (report.findings, report.unfinished_total != 0);
    if incomplete {
        report.note(format!("INSPECTION INCOMPLETE: {} reason(s), first: {}; uninspected state is unknown, not clean", report.unfinished_total, report.unfinished[0]));
    }
    report.note(match (findings, incomplete) {
        (0, false) => "no inconsistent allocator record in this pass".into(),
        (0, true) => "no inconsistent allocator record in the part that was inspected".into(),
        _ => format!("{findings} inconsistent allocator record(s) in this pass; the first {} are listed above", findings.min(MAX_FINDINGS)),
    });
    Examination { lines: report.lines, findings: report.found, incomplete }
}

/// The closing line of a report: never "clean" unless both passes were complete.
fn verdict(first: &Examination, second: &Examination) -> String {
    let reproduced = second.findings.iter().filter(|finding| first.findings.contains(finding)).count();
    let summary = format!("two passes 100 ms apart: {reproduced} of {} listed finding(s) identical in both; first pass listed {}", second.findings.len(), first.findings.len());
    let result = if first.incomplete || second.incomplete {
        "RESULT: INCOMPLETE. Part of the allocator state was not inspected; this is not a clean result"
    } else if first.findings.is_empty() && second.findings.is_empty() {
        "RESULT: no inconsistent allocator record in two complete passes. Allocation contents are not checked, so this does not show the heap is intact"
    } else {
        "RESULT: inconsistent allocator records listed above"
    };
    format!("{summary}. {result}")
}

/// Called from the engine trap paths. Runs at most once per process and is
/// bounded to about five seconds across both passes.
pub(crate) fn report_once(memory: &wasmtime::SharedMemory, reason: &str) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) { return; }
    eprintln!("[heap-autopsy] begin: {reason}. Best-effort snapshot of memory other threads may still write; findings are evidence to confirm, not proof of corruption.");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let pass = || examine(memory, GAME, Budget::new(Duration::from_millis(2400), 40_000_000));
        let first = pass();
        std::thread::sleep(Duration::from_millis(100));
        (first, pass())
    }));
    match outcome {
        Ok((first, second)) => {
            for line in &second.lines { eprintln!("[heap-autopsy] {line}"); }
            for finding in first.findings.iter().filter(|finding| !second.findings.contains(finding)) {
                eprintln!("[heap-autopsy] first pass only (state changed between passes): {finding}");
            }
            eprintln!("[heap-autopsy] {}", verdict(&first, &second));
        }
        Err(_) => eprintln!("[heap-autopsy] analysis panicked; RESULT: INCOMPLETE, no conclusion"),
    }
    eprintln!("[heap-autopsy] end");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Image(Vec<u8>);
    impl Guest for Image {
        fn len(&self) -> u64 { self.0.len() as u64 }
        fn read(&self, address: u64, out: &mut [u8]) -> bool {
            let Some(end) = (address as usize).checked_add(out.len()) else { return false };
            if end > self.0.len() { return false; }
            out.copy_from_slice(&self.0[address as usize..end]);
            true
        }
    }
    impl Image {
        fn put(&mut self, address: u64, value: u64) { self.0[address as usize..address as usize + 8].copy_from_slice(&value.to_le_bytes()); }
        fn put32(&mut self, address: u64, value: u32) { self.0[address as usize..address as usize + 4].copy_from_slice(&value.to_le_bytes()); }
    }

    const LAYOUT: Layout = Layout { mparams: 0x100, gm: 0x130 };
    const BASE: u64 = 0x1000;
    // In-use A, free tree chunk F (0x200, bin 2), in-use B, free tree chunk G
    // (0x208, child of F), in-use C, free small chunk S (0x40), in-use D, top.
    const A: u64 = BASE;
    const F: u64 = A + 0x60;
    const B: u64 = F + 0x200;
    const G: u64 = B + 0x80;
    const C: u64 = G + 0x208;
    const S: u64 = C + 0x40;
    const D: u64 = S + 0x40;
    const TOP_CHUNK: u64 = D + 0x30;
    const END: u64 = 0x2000;

    fn heap() -> Image {
        let mut m = Image(vec![0u8; 0x3000]);
        let gm = LAYOUT.gm;
        m.put(LAYOUT.mparams, 0x5555_1234); m.put(LAYOUT.mparams + 8, 4096);
        m.put(gm + MAGIC, 0x5555_1234); m.put32(gm + MFLAGS, 2);
        m.put(gm + LEAST_ADDR, BASE); m.put(gm + SEGMENT, BASE); m.put(gm + SEGMENT + 8, END - BASE);
        m.put(A + 8, 0x60 | PINUSE | CINUSE);
        m.put(F + 8, 0x200 | PINUSE); m.put(B, 0x200);
        m.put(B + 8, 0x80 | CINUSE);
        m.put(G + 8, 0x208 | PINUSE); m.put(C, 0x208);
        m.put(C + 8, 0x40 | CINUSE);
        m.put(S + 8, 0x40 | PINUSE); m.put(D, 0x40);
        m.put(D + 8, 0x30 | CINUSE);
        m.put(TOP_CHUNK + 8, (END - TOP_CHUNK) | PINUSE);
        m.put(gm + TOP, TOP_CHUNK); m.put(gm + TOPSIZE, END - TOP_CHUNK);
        // Tree bin 2 holds sizes 0x200..0x300: F is the root, G its child.
        let slot = gm + TREEBINS + 2 * 8;
        m.put32(gm + 4, 1 << 2); m.put(slot, F);
        for (node, parent, child) in [(F, slot, G), (G, F, 0)] {
            m.put(node + 16, node); m.put(node + 24, node);
            m.put(node + 32, child); m.put(node + 48, parent); m.put32(node + 56, 2);
        }
        // Empty small bins point at themselves; bin 8 (size 0x40) holds S.
        for index in 1..32u64 { let bin = gm + SMALLBINS + index * 16; m.put(bin + 16, bin); m.put(bin + 24, bin); }
        let bin = gm + SMALLBINS + 8 * 16;
        m.put32(gm, 1 << 8); m.put(bin + 16, S); m.put(bin + 24, S); m.put(S + 16, bin); m.put(S + 24, bin);
        m
    }
    fn budget() -> Budget { Budget::new(Duration::from_secs(5), 1_000_000) }
    fn run(image: &Image) -> String { examine(image, LAYOUT, budget()).lines.join("\n") }

    #[test]
    fn intact_heap_reports_no_findings_and_walks_every_chunk() {
        let text = run(&heap());
        assert!(text.contains("no inconsistent allocator record in this pass"), "{text}");
        assert!(text.contains("tree bins: 2 free chunks visited") && text.contains("small bins: 1 free chunks visited"), "{text}");
        assert!(text.contains("address-order walk: 7 chunks in 1 segment(s), every segment walked to its end"), "{text}");
        assert!(!text.contains("FINDING") && !text.contains("INCOMPLETE") && !text.contains("NOT INSPECTED"), "{text}");
        assert!(!examine(&heap(), LAYOUT, budget()).incomplete);
        assert!(text.contains("allocator lock word 0: not held"), "{text}");
    }

    #[test]
    fn wild_tree_child_pointer_names_holder_field_value_and_physical_neighbour() {
        // The crash signature: a child pointer that is not inside guest memory.
        let mut image = heap();
        image.put(F + 32, 0xffff_ffff_0000_0010);
        let text = run(&image);
        assert!(text.contains(&format!("FINDING 1: tree bin 2: child[0] of 0x{F:x} is 0xffffffff00000010, not a chunk inside guest memory")), "{text}");
        assert!(text.contains(&format!("first finding's holder 0x{F:x} lies in chunk 0x{F:x} size 0x200 (free); physically previous chunk 0x{A:x} size 0x60 was in use")), "{text}");
        assert!(text.contains("1 inconsistent allocator record(s)") && !text.contains("PHYSICAL BREAK"), "{text}");
    }

    #[test]
    fn overflow_from_an_in_use_chunk_is_located_by_the_first_broken_header() {
        // B overruns into G: header and tree links replaced by payload bytes.
        let mut image = heap();
        for offset in (0..64).step_by(8) { image.put(G + offset, 0x4141_4141_4141_4141); }
        let text = run(&image);
        assert!(text.contains(&format!("FINDING 2: PHYSICAL BREAK: chunk 0x{G:x} has head 0x4141414141414141")), "{text}");
        assert!(text.contains(&format!("previous chunk size 0x80 (128 bytes, user pointer 0x{:x})", B + 16)), "{text}");
        assert!(text.contains("FINDING 1: tree bin 2") && text.contains("|AAAAAAAA"), "{text}");
    }

    #[test]
    fn small_bin_and_top_damage_are_reported_without_reading_out_of_bounds() {
        let mut image = heap();
        image.put(S + 16, u64::MAX - 7);
        image.put(TOP_CHUNK + 8, 0x10);
        let text = run(&image);
        assert!(text.contains("top chunk") && text.contains("small bin 8: fd of"), "{text}");
        let mut cyclic = heap();
        cyclic.put(G + 32, F);
        assert!(run(&cyclic).contains("revisits"), "cycle must terminate");
    }

    #[test]
    fn unknown_layout_is_not_interpreted() {
        let mut image = heap();
        image.put(LAYOUT.gm + MAGIC, 7);
        let unknown = examine(&image, LAYOUT, budget());
        assert!(unknown.lines.len() == 1 && unknown.lines[0].contains("not recognized") && unknown.incomplete, "{:?}", unknown.lines);
        assert!(verdict(&unknown, &unknown).contains("RESULT: INCOMPLETE"));
        assert!(examine(&Image(vec![0u8; 64]), GAME, budget()).lines[0].contains("not recognized"));
        // malloc_state cut off by the end of memory is not interpreted either.
        let mut short = heap();
        short.0.truncate((LAYOUT.gm + SEGMENT + 31) as usize);
        assert!(examine(&short, LAYOUT, budget()).lines[0].contains("not recognized"));
    }

    #[test]
    fn exhausted_budget_is_reported_as_incomplete_never_as_clean() {
        // Three records: both tree chunks and the small chunk, then nothing left for the walk.
        let outcome = examine(&heap(), LAYOUT, Budget::new(Duration::from_secs(5), 3));
        let text = outcome.lines.join("\n");
        assert!(outcome.incomplete && outcome.findings.is_empty(), "{text}");
        assert!(text.contains("NOT INSPECTED: address-order walk: budget exhausted after 3 inspected records"), "{text}");
        assert!(text.contains("INSPECTION INCOMPLETE: 1 reason(s)") && text.contains("address-order walk: 0 chunks in 1 segment(s), stopped early"), "{text}");
        assert!(text.contains("in the part that was inspected") && !text.contains("in this pass"), "{text}");
        assert!(verdict(&outcome, &outcome).contains("RESULT: INCOMPLETE"));
        // Exhaustion inside the bin passes is also incomplete, and leaves the walk undone.
        let early = examine(&heap(), LAYOUT, Budget::new(Duration::from_secs(5), 1));
        assert!(early.incomplete && early.lines.join("\n").contains("NOT INSPECTED: tree bins: budget exhausted after 1 inspected records"));
        // A ring that never closes is cut off by the same budget.
        let mut endless = heap();
        endless.put(F + 16, G); endless.put(G + 24, F); endless.put(G + 16, G);
        let outcome = examine(&endless, LAYOUT, Budget::new(Duration::from_secs(5), 50));
        assert!(outcome.incomplete || !outcome.findings.is_empty());
        let expired = examine(&heap(), LAYOUT, Budget::new(Duration::ZERO, 1_000_000));
        assert!(!expired.incomplete, "the clock is sampled every 1024 records, so a tiny heap completes");
    }

    // Segment records after the first live in the heap. These fixtures put
    // them at RECORDS, outside the first segment, with tiny valid segments at
    // EXTRA: one 32-byte in-use chunk followed by a fencepost.
    const RECORDS: u64 = 0x2100;
    const EXTRA: u64 = 0x4000;
    fn chained(count: u64) -> Image {
        let mut image = heap();
        image.0.resize(0x10000, 0);
        let mut previous = LAYOUT.gm + SEGMENT;
        for index in 0..count {
            let (record, base) = (RECORDS + index * 32, EXTRA + index * 0x40);
            image.put(previous + 16, record);
            image.put(record, base); image.put(record + 8, 0x40);
            image.put(base + 8, 0x20 | PINUSE | CINUSE); image.put(base + 0x28, FENCEPOST_HEAD);
            previous = record;
        }
        image
    }
    fn outcome(image: &Image) -> (Examination, String) {
        let examination = examine(image, LAYOUT, budget());
        let text = examination.lines.join("\n");
        (examination, text)
    }

    #[test]
    fn additional_segments_are_walked_to_their_fenceposts() {
        let (examination, text) = outcome(&chained(3));
        assert!(!examination.incomplete && examination.findings.is_empty(), "{text}");
        assert!(text.contains("address-order walk: 13 chunks in 4 segment(s), every segment walked to its end"), "{text}");
        assert!(verdict(&examination, &examination).contains("RESULT: no inconsistent allocator record in two complete passes"));
    }

    #[test]
    fn unreadable_segment_record_is_a_finding_and_never_clean() {
        // Sol's case: a torn or corrupted next pointer used to end the walk silently.
        let mut image = chained(1);
        image.put(RECORDS + 16, 0x10_0000);
        let (examination, text) = outcome(&image);
        assert!(examination.incomplete, "{text}");
        assert!(text.contains(&format!("FINDING 1: segment list: next pointer of record 0x{RECORDS:x} is 0x100000, not readable guest memory")), "{text}");
        assert!(text.contains("NOT INSPECTED: segments after record") && text.contains("stopped early"), "{text}");
        assert!(!text.contains("every segment walked to its end") && !text.contains("no inconsistent allocator record"), "{text}");
        assert!(verdict(&examination, &examination).contains("RESULT: INCOMPLETE"));
        // The same for the pointer held in malloc_state itself.
        let mut first = heap();
        first.put(LAYOUT.gm + SEGMENT + 16, u64::MAX - 31);
        let (examination, text) = outcome(&first);
        assert!(examination.incomplete && examination.findings.len() == 1, "{text}");
    }

    #[test]
    fn out_of_range_segment_bounds_are_a_finding_and_never_clean() {
        for (base, size) in [(0xffff_0000u64, 0x1000u64), (EXTRA, 0x100_0000), (u64::MAX - 8, 0x40), (EXTRA, 8), (0x10, 0x40)] {
            let mut image = chained(2);
            image.put(RECORDS, base); image.put(RECORDS + 8, size);
            let (examination, text) = outcome(&image);
            assert!(examination.incomplete, "{base:x} {size:x}\n{text}");
            assert!(text.contains(&format!("FINDING 1: segment 2: base 0x{base:x} size 0x{size:x} is not a region inside guest memory")), "{text}");
            assert!(text.contains("NOT INSPECTED: chunks of segment 2 (bounds invalid)"), "{text}");
            // The following segment is still walked, but the result is not clean.
            assert!(text.contains("in 3 segment(s), stopped early") && !text.contains("no inconsistent allocator record"), "{text}");
        }
        // The first segment's own bounds are checked the same way.
        let mut image = heap();
        image.put(LAYOUT.gm + SEGMENT + 8, 0x100_0000);
        let (examination, text) = outcome(&image);
        assert!(examination.incomplete && text.contains("FINDING 1: segment 1:"), "{text}");
        // Sol's case: an otherwise intact heap whose first segment base lies past guest memory.
        let mut image = heap();
        let past = image.0.len() as u64 + 64;
        image.put(LAYOUT.gm + SEGMENT, past); image.put(LAYOUT.gm + SEGMENT + 8, 64);
        let (examination, text) = outcome(&image);
        assert!(examination.incomplete && examination.findings.len() == 1, "{text}");
        assert!(text.contains(&format!("FINDING 1: segment 1: base 0x{past:x} size 0x40 is not a region inside guest memory")), "{text}");
        assert!(!text.contains("every segment walked to its end") && !text.contains("no inconsistent allocator record"), "{text}");
        assert!(verdict(&examination, &examination).contains("RESULT: INCOMPLETE"));
    }

    #[test]
    fn segment_list_cap_and_cycle_are_explicit() {
        // One static record plus 63 chained ones is exactly the cap and completes.
        let (at_cap, text) = outcome(&chained(MAX_SEGMENTS as u64 - 1));
        assert!(!at_cap.incomplete && at_cap.findings.is_empty(), "{text}");
        assert!(text.contains("in 64 segment(s), every segment walked to its end"), "{text}");
        // One more is reported as uninspected, not as the end of the list.
        let (over, text) = outcome(&chained(MAX_SEGMENTS as u64));
        assert!(over.incomplete && over.findings.is_empty(), "{text}");
        assert!(text.contains("NOT INSPECTED: segment list continues past 64 records"), "{text}");
        assert!(text.contains("in 64 segment(s), stopped early") && text.contains("in the part that was inspected"), "{text}");
        assert!(verdict(&over, &over).contains("RESULT: INCOMPLETE"));
        // A list that loops back is an inconsistent record.
        let mut cycle = chained(3);
        cycle.put(RECORDS + 2 * 32 + 16, RECORDS);
        let (examination, text) = outcome(&cycle);
        assert!(text.contains(&format!("FINDING 1: segment list: next pointer of record 0x{:x} returns to record 0x{RECORDS:x} (cycle)", RECORDS + 64)), "{text}");
        assert!(!examination.findings.is_empty() && verdict(&examination, &examination).contains("RESULT: inconsistent allocator records"));
        let mut own = heap();
        own.put(LAYOUT.gm + SEGMENT + 16, LAYOUT.gm + SEGMENT);
        assert!(outcome(&own).1.contains("(cycle)"));
    }

    #[test]
    fn segment_without_a_terminator_or_readable_header_is_not_clean() {
        // Chunks that run to the segment end with neither top nor fencepost.
        let mut open = chained(1);
        open.put(EXTRA + 0x28, 0x20 | PINUSE | CINUSE);
        let (examination, text) = outcome(&open);
        assert!(text.contains("FINDING 1: segment 2: chunks end at") && text.contains("without reaching the top chunk or a fencepost"), "{text}");
        assert!(text.contains("stopped early") && !examination.findings.is_empty(), "{text}");
        // A segment flush against the end of guest memory: the last header would lie outside it.
        let mut edge = chained(1);
        let last = edge.0.len() as u64 - 0x28;
        edge.put(RECORDS, last); edge.put(RECORDS + 8, 0x28);
        edge.put(last + 8, 0x20 | PINUSE | CINUSE);
        let (examination, text) = outcome(&edge);
        assert!(!examination.findings.is_empty() && text.contains("without reaching the top chunk or a fencepost"), "{text}");
        // Top missing from every segment.
        let mut lost = heap();
        lost.put(TOP_CHUNK + 8, FENCEPOST_HEAD);
        lost.put(LAYOUT.gm + TOP, EXTRA); lost.put(LAYOUT.gm + TOPSIZE, 0);
        lost.0.resize(0x10000, 0);
        lost.put(EXTRA + 8, PINUSE);
        let (examination, text) = outcome(&lost);
        assert!(text.contains("was not reached in any of the 1 segment(s)") && !examination.findings.is_empty(), "{text}");
        // An allocator with no top chunk is not walked and is not called clean.
        let mut empty = heap();
        empty.put(LAYOUT.gm + TOP, 0);
        let (examination, text) = outcome(&empty);
        assert!(examination.incomplete && text.contains("NOT INSPECTED: address-order walk: the allocator has no top chunk"), "{text}");
    }

    #[test]
    fn locked_state_and_reproduced_findings_are_distinguished() {
        let mut image = heap();
        image.put32(LAYOUT.gm + MUTEX + 4, 16);
        image.put(F + 32, 0xffff_ffff_0000_0010);
        let (first, second) = (examine(&image, LAYOUT, budget()), examine(&image, LAYOUT, budget()));
        assert!(first.lines.join("\n").contains("allocator lock word 16: held"));
        assert_eq!(first.findings, second.findings);
        assert_eq!(first.findings.len(), 1);
        // A record that was only momentarily inconsistent differs between passes.
        image.put(F + 32, G);
        assert!(examine(&image, LAYOUT, budget()).findings.is_empty());
    }

    #[test]
    fn shared_memory_reads_are_bytewise_and_bounds_checked() {
        let mut config = wasmtime::Config::new(); config.wasm_threads(true); config.shared_memory(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let memory = wasmtime::SharedMemory::new(&engine, wasmtime::MemoryType::shared(1, 1)).unwrap();
        for (index, cell) in memory.data()[65528..].iter().enumerate() { unsafe { *cell.get() = index as u8 + 1; } }
        assert_eq!(Guest::len(&memory), 65536);
        assert_eq!(memory.u64(65528), Some(u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8])));
        assert_eq!(memory.u32(65532), Some(u32::from_le_bytes([5, 6, 7, 8])));
        assert_eq!((memory.u64(65529), memory.u32(65533), memory.u64(u64::MAX - 3)), (None, None, None));
        let mut empty = [0u8; 0];
        assert!(memory.read(65536, &mut empty) && !memory.read(65537, &mut empty));
        assert!(examine(&memory, GAME, budget()).lines[0].contains("not recognized"));
    }

    #[test]
    fn tree_bin_ranges_match_dlmalloc() {
        assert_eq!((tree_bin_minimum(0), tree_bin_minimum(1), tree_bin_minimum(2), tree_bin_minimum(31)), (0x100, 0x180, 0x200, 0xc0_0000));
    }
}
