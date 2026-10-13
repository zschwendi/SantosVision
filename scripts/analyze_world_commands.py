#!/usr/bin/env python3
"""Offline schema-1 host command timeline; potential accesses are not GPU proof."""
import argparse
import collections
import csv
import hashlib
import json
import math
from pathlib import Path
import struct
import sys

MAX_INPUT = 17 * 1024 * 1024
MAX_RECORDS = 32768
MAX_EDGES = 200000
MAX_RESOURCES = 32768
MAX_REFS = 128
MAX_OUTPUT = 64 * 1024 * 1024
NAMES = dict(enumerate([
    "nop", "wrap", "nop2", "create_texture", "destroy", "clear_rt", "clear_ds", "present",
    "wait", "nop9", "text", "create_buffer", "upload_buffer", "upload_texture", "create_shader",
    "create_layout", "create_state", "create_srv", "set_shader", "set_layout", "set_topology",
    "set_vertex_buffers", "set_index_buffer", "set_constant_buffers", "set_srv", "set_samplers",
    "set_targets", "set_blend", "set_depth_state", "set_raster_state", "set_viewports", "set_scissors",
    "draw", "reset_state", "copy_region", "copy_resource", "resolve", "create_uav", "set_uav",
    "dispatch", "clear_uav", "generate_mips", "readback", "copy_counter", "create_query",
    "begin_query", "end_query", "upload_buffer_no_overwrite",
]))


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def uint(value, label, maximum=(1 << 64) - 1):
    require(type(value) is int and 0 <= value <= maximum, "invalid " + label)
    return value


def words(value, label):
    require(type(value) is list and len(value) <= 65536, "invalid " + label)
    for item in value:
        uint(item, label, (1 << 32) - 1)
    return value


def native_allocation(value):
    if value is None:
        return None
    require(type(value) is dict and value.get("kind") in ("texture","texture_proxy","buffer"),"invalid native allocation kind")
    require(uint(value.get("tag"),"native allocation tag")>0,"zero native allocation tag")
    require(value.get("scope") in ("this_process","this_gpu_instance"),"invalid native allocation scope")
    require(type(value.get("basis")) is str and len(value["basis"])<=192,"invalid native allocation basis")
    require(value.get("backend_id") is None,"unexpected backend identity assertion")
    require(value.get("backend_allocation_validated") is None or value.get("backend_allocation_validated") is False,"unexpected backend allocation validation assertion")
    return {key:value.get(key) for key in ("kind","tag","scope","basis","backend_id","identity_kind","backend_allocation_validated")}


def retained_parent(value):
    if value is None:
        return None
    require(type(value) is dict and type(value.get("resolved")) is bool,"invalid retained parent evidence")
    allocation=native_allocation(value.get("native_allocation"))
    if value["resolved"]:
        require(value.get("parent_eq") is True and allocation is not None,"resolved native parent lacks exact Eq and allocation")
    else:
        require(allocation is None,"unresolved parent asserts native allocation")
    return {"resolved":value["resolved"],"parent_eq":value.get("parent_eq"),"native_allocation":allocation,
            "reason":str(value.get("reason",""))[:192]}


def native_context(value):
    if value is None:
        return None
    require(type(value) is dict and value.get("capture_phase")=="pre_exec","invalid native context")
    require(type(value.get("pass_active")) is bool,"invalid native pass state")
    key=value.get("pass_key")
    if key is not None:
        require(len(words(key,"pass key"))==37,"invalid retained pass key")
    attachments=value.get("attachments")
    require(type(attachments) is list and len(attachments)<=9,"native attachment cap invalid")
    copied=[]
    for row in attachments:
        require(type(row) is dict and key is not None,"attachment lacks retained pass key")
        ident=uint(row.get("guest_id"),"attachment id",(1<<32)-1)
        aspect=row.get("aspect")
        require(aspect in ("color","depth_stencil"),"invalid attachment aspect")
        slot=uint(row.get("slot"),"attachment slot",7) if aspect=="color" else None
        offset=slot*4 if slot is not None else 32
        uint(row.get("mip"),"attachment mip",(1<<32)-1); uint(row.get("slice"),"attachment slice",(1<<32)-1)
        require(ident==key[offset] and row.get("mip")==key[offset+1] and row.get("slice")==key[offset+2],"attachment conflicts with retained pass key")
        copied.append({"aspect":aspect,"slot":slot,"guest_id":ident,"mip":key[offset+1],"slice":key[offset+2],
                       "retained_parent":retained_parent(row.get("retained_parent")),"format":str(row.get("format",""))[:80]})
    pending=value.get("pending_clears")
    require(type(pending) is list and len(pending)<=64,"pending clear cap invalid")
    observed=uint(value.get("pending_clears_observed"),"pending clear count")
    omitted=uint(value.get("pending_clears_omitted"),"pending clear omissions")
    require(observed==len(pending)+omitted,"pending clear counts conflict")
    clears=[]
    for row in pending:
        require(type(row) is dict,"invalid pending clear")
        for name in ("guest_id","mip","slice","queue_index"):
            uint(row.get(name),"pending clear "+name)
        color=row.get("color_f64_bits")
        if color is not None:
            require(type(color) is list and len(color)==4,"invalid clear color bits")
            for bit in color:uint(bit,"clear color bits")
        for name in ("depth_f32_bits","stencil"):
            if row.get(name) is not None:uint(row[name],name,(1<<32)-1)
        clears.append({**{k:row.get(k) for k in ("guest_id","mip","slice","queue_index","color_f64_bits","depth_f32_bits","stencil")},
                       "current_target_native_allocation":native_allocation(row.get("current_target_native_allocation"))})
    return {"capture_phase":"pre_exec","pass_active":value["pass_active"],"pass_key":key,"attachments":copied,
            "pending_clears":clears,"pending_clears_omitted":omitted,"native_graph_complete":False,
            "applied_state":{k:value.get(k) for k in ("encoder_epoch","pipeline_host_uid","bind_group_host_uids","applied_viewport","applied_scissor","applied_blend_constant","applied_viewport_f32_bits","applied_blend_constant_f64_bits","applied_stencil_reference")}}


def no_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key: " + key)
        result[key] = value
    return result


def load_trace(path):
    with path.open("rb") as stream:
        raw = stream.read(MAX_INPUT + 1)
    require(len(raw) <= MAX_INPUT, "input exceeds17MiB")
    trace = json.loads(raw, object_pairs_hook=no_duplicates,
                       parse_constant=lambda value: (_ for _ in ()).throw(ValueError("nonfinite JSON " + value)))
    validate(trace)
    return trace, hashlib.sha256(raw).hexdigest()


def validate(trace):
    require(type(trace) is dict and type(trace.get("schema")) is int and trace.get("schema") == 1 and trace.get("state") == "captured", "requires schema1 captured trace")
    require(trace.get("snapshot_complete") is False, "trace cannot assert immutable snapshot")
    records = trace.get("records")
    require(type(records) is list and 2 <= len(records) <= MAX_RECORDS, "invalid record count")
    require(trace.get("command_sequence_complete") is True and uint(trace.get("records_omitted"),"omitted records") == 0
            and trace.get("first_omitted_ordinal") is None, "truncated command interval rejected")
    require(uint(trace.get("commands_observed"), "observed count") == len(records)
            and uint(trace.get("records_retained"), "retained count") == len(records), "record counters conflict")
    require(trace.get("all_operands_complete") is True and uint(trace.get("records_with_incomplete_operands"),"incomplete records") == 0,
            "incomplete operands rejected before lifetime reconstruction")
    start = uint(trace.get("start_game_frame"), "start frame")
    require(start > 0 and uint(trace.get("end_game_frame"), "end frame") == start + 1, "not consecutive present frames")
    require(type(trace.get("start_view")) is dict and trace["start_view"].get("headset_applied") is True,
            "opening present lacks headset camera marker")
    for name in ("epoch","camera","request","head"):
        require(uint(trace["start_view"].get(name),"start "+name)>0,"zero start "+name)
    require(type(trace.get("end_immersive_qualified")) is bool and type(trace.get("endpoint_epoch_matches")) is bool,
            "endpoint qualification missing")
    presents = []
    for ordinal, row in enumerate(records, 1):
        require(type(row) is dict and uint(row.get("ordinal"), "ordinal") == ordinal, "ordinal gap or duplicate")
        op = uint(row.get("opcode"), "opcode", (1 << 32) - 1)
        n = uint(row.get("declared_operand_words"), "operand count", (1 << 32) - 1)
        w = words(row.get("operands"), "operands")
        require(row.get("operands_complete") is True and row.get("operand_range_valid") is True
                and row.get("operand_words_omitted") == 0, "truncated/malformed operand record")
        require(len(w) == (0 if op == 1 else n), "operand extent conflicts with header")
        if op == 1:
            require(row.get("wrap_declared_length_not_interpreted") is True, "wrap length interpreted as operands")
        else:
            validate_layout(op, w)
        if op == 7:
            presents.append(ordinal)
    require(presents == [1, len(records)], "requires exactly two inclusive present endpoints")
    require(type(records[0].get("inherited_state")) is dict, "opening inherited state missing")
    require(records[0].get("present_view") == trace["start_view"], "opening present marker conflicts")


def validate_layout(op, w):
    fixed = {3:10,4:1,5:7,6:6,7:1,8:3,10:1,11:5,12:3,13:12,14:5,15:2,16:2,17:9,18:2,
             19:1,20:1,21:2,22:3,23:3,24:3,25:3,26:37,27:5,28:2,29:1,30:1,31:1,32:6,
             34:14,35:2,36:4,37:9,38:3,39:3,40:6,41:1,42:17,43:3,44:4,45:1,46:2,47:3}
    require(len(w) >= fixed.get(op, 0), "opcode%d short header" % op)
    needed = 0
    if op in (10,12,13,47):
        header, length_index = {10:(1,0),12:(3,2),13:(12,11),47:(3,2)}[op]
        needed = header + (w[length_index] + 3) // 4
    elif op == 14:
        needed = 8 + (w[4]-1)*4 if w[4] else 5
    elif op == 15:
        needed = 8 + (w[1]-1)*7 if w[1] else 2
    elif op == 21:
        needed = 2 + min(w[1], max(0,16-w[0])) * 3
    elif op in (23,24,25,38):
        maximum = {23:16,24:128,25:16,38:8}[op]
        count = min(w[2], max(0,maximum-w[1])) if w[0] < 3 else 0
        needed = (5 + (count-1)*3 if count else 3) if op==23 else 3 + count
        if op == 38 and count:
            needed = 3 + w[2] + count
    elif op in (30,31):
        needed = 1 + w[0] * (6 if op == 30 else 4)
    require(len(w) >= needed, "opcode%d variable operands/payload exceed declared extent" % op)


def state_from(raw=None):
    state = {"shader":[0]*3,"layout":0,"topology":4,"vertex_buffers":[[0,0,0] for _ in range(16)],
             "index_buffer":[0,0,0],"constant_buffers":[[[0,0] for _ in range(16)] for _ in range(3)],
             "srv":[[0]*128 for _ in range(3)],"uav":[[0]*8 for _ in range(3)],"samplers":[[0]*16 for _ in range(3)],
             "render_targets":[[0]*4 for _ in range(8)],"depth_target":[0]*5,
             "blend_state":[0,[1.]*4],"depth_state":[0,0],"raster_state":0}
    if raw is None:
        return state
    for name, default in state.items():
        value = raw.get(name)
        require(value is not None, "missing inherited " + name)
        if name in ("layout","topology","raster_state"):
            uint(value, name, (1 << 32)-1)
        elif name == "blend_state":
            require(type(value) is list and len(value)==2 and type(value[1]) is list and len(value[1])==4, "bad inherited blend")
            uint(value[0],name,(1<<32)-1)
            require(all(type(x) in (int,float) and math.isfinite(x) for x in value[1]), "bad blend constant")
        else:
            def shape(actual, expected):
                require(type(actual) is list and len(actual)==len(expected), "bad inherited " + name)
                for a,e in zip(actual,expected):
                    shape(a,e) if type(e) is list else uint(a,name,(1<<32)-1)
            shape(value,default)
        state[name] = json.loads(json.dumps(value))
    return state


class Analyzer:
    def __init__(self, trace):
        validate(trace)
        self.trace = trace
        self.state = state_from(trace["records"][0]["inherited_state"])
        self.live, self.generations, self.nodes = {}, collections.Counter(), []
        self.history = collections.defaultdict(list)
        self.timeline, self.edges = [], []
        self.edge_omissions = 0
        self.issues = []
        self.issue_count = 0
        self.current = None
        self.ordinal = 0

    def issue(self, kind, **detail):
        self.issue_count += 1
        if len(self.issues) < 256:
            self.issues.append({"ordinal":self.ordinal,"kind":kind,**detail})

    def object(self, ident):
        if not ident:
            return None
        if ident not in self.live:
            require(len(self.nodes) < MAX_RESOURCES, "resource lifetime limit exceeded")
            node = {"key":"%d:unknown_initial"%ident,"id":ident,"local_generation":None,
                    "generation_known":False,"kind":"unknown","alive":True,"created_ordinal":None,
                    "potential_write_version":0,"last_potential_writer":None,"contents_known":False}
            self.live[ident] = node
            self.nodes.append(node)
            self.history[ident].append(node)
        node = self.live[ident]
        if not node["alive"]:
            self.issue("use_after_destroy",id=ident,key=node["key"])
        return node

    def create(self, ident, kind, descriptor):
        old = self.live.get(ident)
        if old and old["alive"]:
            old["alive"] = False
            old["retired_ordinal"] = self.ordinal
            old["retirement"] = "replacement_by_create"
        self.generations[ident] += 1
        require(len(self.nodes) < MAX_RESOURCES, "resource lifetime limit exceeded")
        node = {"key":"%d:local%d"%(ident,self.generations[ident]),"id":ident,
                "local_generation":self.generations[ident],"generation_known":True,"kind":kind,"alive":True,
                "created_ordinal":self.ordinal,"potential_write_version":0,"last_potential_writer":None,
                "contents_known":False,"descriptor":descriptor}
        self.live[ident] = node
        self.nodes.append(node)
        self.history[ident].append(node)

    def metadata(self, rows):
        require(type(rows) is list and len(rows)<=64, "invalid captured resource rows")
        for row in rows:
            require(type(row) is dict and type(row.get("pre_exec")) is dict, "invalid resource metadata")
            ident = uint(row.get("id"), "resource id", (1<<32)-1)
            node = self.object(ident)
            if node is None:
                continue
            pre = row["pre_exec"]
            allocation=native_allocation(pre.get("native_allocation"))
            if allocation is not None:
                if node.get("native_allocation") not in (None,allocation):
                    self.issue("native_allocation_conflict",id=ident)
                    node["native_identity_conflict"]=True
                node["native_allocation"]=allocation
                node["native_allocation_observed_ordinal"]=self.ordinal
            cached=pre.get("cached_native_views",[])
            require(type(cached) is list and len(cached)<=8,"invalid cached native view evidence")
            require(all(type(item) is dict for item in cached),"invalid cached native view row")
            if "current_resource_native_allocation" in pre:
                node["current_resource_native_allocation"]=native_allocation(pre["current_resource_native_allocation"])
            proxies=pre.get("proxies",[])
            require(type(proxies) is list and len(proxies)<=3,"invalid native proxy evidence")
            if "proxies" in pre:
                require(all(type(proxy) is dict for proxy in proxies),"invalid proxy evidence row")
                node["native_proxy_allocations"]=[{"kind":str(proxy.get("kind",""))[:80],"dirty":proxy.get("dirty"),
                    "native_allocation":native_allocation(proxy.get("native_allocation"))} for proxy in proxies]
            if "cached_native_views" in pre:
                node["cached_native_view_parents"]=[retained_parent(item.get("retained_parent_allocation")) for item in cached]
                node["cached_native_view_observed_ordinal"]=self.ordinal
                node["cached_native_views_omitted"]=uint(pre.get("cached_native_views_omitted",0),"cached native views omitted")
                if node["cached_native_views_omitted"]:
                    self.issue("native_cached_view_cap",omitted=node["cached_native_views_omitted"])
            kind = pre.get("kind","unknown")
            if kind == "missing":
                node["observed_missing_ordinal"] = self.ordinal
                self.issue("host_object_missing",id=ident)
            elif node["kind"] not in ("unknown",kind) and not (node["kind"] in ("srv","uav") and kind=="view"):
                self.issue("kind_conflict",id=ident,local=node["kind"],captured=str(kind)[:80])
            elif node["kind"]=="unknown":
                node["kind"] = str(kind)[:80]
            if "resource" in pre:
                resource_id = uint(pre["resource"],"view resource",(1<<32)-1)
                previous = node.get("resource_id",node.get("descriptor",{}).get("resource_id"))
                if previous is not None and previous != resource_id:
                    self.issue("view_resource_conflict",id=ident,local_resource=previous,captured_resource=resource_id)
                node["resource_id"] = resource_id
            if "is_buffer" in pre:
                require(type(pre["is_buffer"]) is bool,"invalid view type")
                node["view_is_buffer"] = pre["is_buffer"]
            if "words" in pre:
                node["state_words"] = words(pre["words"],"state words")[:128]
            if "identity" in pre:
                identity = uint(pre["identity"],"buffer identity")
                if "observed_native_identity" in node and node["observed_native_identity"] != identity:
                    self.issue("unobserved_native_lifetime_change",id=ident)
                    node["native_identity_conflict"] = True
                node["observed_native_identity"] = identity

    def reference(self, ident, mode, through_view=False):
        node = self.object(ident)
        if node is None:
            return None
        view_key = None
        cached_parents=[]
        cached_observed=None
        native_resolution = "unknown_native_execution"
        old_candidates = []
        unresolved_view = False
        if through_view:
            view_key = node["key"]
            cached_parents=node.get("cached_native_view_parents",[])
            cached_observed=node.get("cached_native_view_observed_ordinal")
            is_buffer = node.get("view_is_buffer",node.get("descriptor",{}).get("is_buffer"))
            resource_id = node.get("resource_id",node.get("descriptor",{}).get("resource_id"))
            if resource_id is None:
                self.issue("unresolved_view_resource",id=ident)
                unresolved_view = True
            else:
                view_created = node["created_ordinal"] or 0
                old_candidates = [old["key"] for old in self.history[resource_id]
                                  if old.get("retired_ordinal",0)>=view_created]
                node = self.object(resource_id)
                if node is None:
                    self.issue("null_view_resource",id=ident)
                    return None
            native_resolution = "raw_buffer_id_candidate_native_binding_unknown" if is_buffer is True else "unknown_cached_native_texture_view_allocation"
        result = {"resource":node["key"],"id":node["id"],"mode":mode,
                  "generation_known":node["generation_known"],"alive":node["alive"],
                  "potential_version":node["potential_write_version"],"through_view":view_key,
                  "native_allocation_resolution":native_resolution,"native_allocation_known":False,
                  "cached_prior_lifetime_candidates":old_candidates[:4],"prior_candidates_omitted":max(0,len(old_candidates)-4),
                  "unresolved_view_resource":unresolved_view,"native_identity_conflict":node.get("native_identity_conflict",False)}
        result["candidate_native_allocation"]=node.get("native_allocation")
        result["candidate_native_allocation_observed_ordinal"]=node.get("native_allocation_observed_ordinal")
        result["cached_native_view_parent_evidence"]=cached_parents
        result["cached_native_view_observed_ordinal"]=cached_observed
        result["cached_native_view_evidence_current"]=cached_observed==self.ordinal
        if mode == "possible_write" and not unresolved_view:
            if node["last_potential_writer"] != self.ordinal:
                node["version_before_current_write"] = node["potential_write_version"]
                node["writer_before_current_write"] = node["last_potential_writer"]
                node["potential_write_version"] += 1
            result["previous_potential_version"] = node["version_before_current_write"]
            result["previous_potential_writer"] = node["writer_before_current_write"]
            result["prior_contents_may_survive"] = True
            result["potential_version"] = node["potential_write_version"]
            node["last_potential_writer"] = self.ordinal
        elif mode == "possible_read":
            result["depends_on_potential_writer"] = node["last_potential_writer"]
            if node["last_potential_writer"] is None:
                result["unresolved_initial_contents"] = True
        if unresolved_view:
            result["potential_version"] = None
        if len(self.edges)<MAX_EDGES:
            self.edges.append({"command":self.ordinal,**result})
        else:
            self.edge_omissions += 1
        return result

    def blend_requested(self):
        ident = self.state["blend_state"][0]
        node = self.object(ident)
        if node is None:
            return False
        values = node.get("state_words",node.get("descriptor",{}).get("words"))
        if values is None:
            return None
        slots=[slot for slot,target in enumerate(self.state["render_targets"]) if target[0]] or [0]
        return any(len(values)>2+(slot if len(values)>1 and values[1] else 0)*8
                   and values[2+(slot if len(values)>1 and values[1] else 0)*8]!=0 for slot in slots)

    def process(self, row):
        self.ordinal = row["ordinal"]
        op,w = row["opcode"],row["operands"]
        self.metadata(row.get("resources",[]))
        resource_metadata_omitted=uint(row.get("resource_access_rows_omitted",0),"resource metadata omitted")
        context=native_context(row.get("native_context"))
        if context and context["pending_clears_omitted"]:
            self.issue("native_pending_clear_cap",omitted=context["pending_clears_omitted"])
        reads,writes,configuration = [],[],[]
        details = {}
        candidates = []
        before = (tuple(self.state["shader"]),tuple(tuple(x) for x in self.state["render_targets"]),tuple(self.state["depth_target"]))
        if op in (3,11,14,15,16,17,37,44):
            kind = {3:"texture",11:"buffer",14:"shader",15:"layout",16:"state",17:"srv",37:"uav",44:"query"}[op]
            descriptor = {"header_words":w[:min(len(w),12)]}
            if op in (17,37): descriptor.update(resource_id=w[2],is_buffer=w[1]==1)
            if op==16: descriptor["words"] = w[2:130]; descriptor["words_omitted"] = max(0,len(w)-130)
            if op==14: descriptor["hash"] = "%08x%08x"%(w[3],w[2]); descriptor["stage"] = w[1]
            self.create(w[0],kind,descriptor)
        elif op==4:
            node = self.object(w[0])
            if node: node.update(alive=False,retired_ordinal=self.ordinal,retirement="destroy")
        elif op in (5,6,12,13,47):
            writes.append((w[0],False))
            if op in (12,47): details.update(offset=w[1],byte_count=w[2])
            elif op==13: details.update(upload_header=w[:12])
            else: details.update(clear_header=w)
        elif op in (7,42): reads.append((w[0],False))
        elif op==18:
            if w[0]<3: self.state["shader"][w[0]]=w[1]
        elif op in (19,20,29): self.state[{19:"layout",20:"topology",29:"raster_state"}[op]]=w[0]
        elif op==22: self.state["index_buffer"]=w[:3]
        elif op==21:
            for i in range(min(w[1],max(0,16-w[0]))): self.state["vertex_buffers"][w[0]+i]=w[2+i*3:5+i*3]
        elif op in (23,24,25,38):
            name={23:"constant_buffers",24:"srv",25:"samplers",38:"uav"}[op]
            maximum={23:16,24:128,25:16,38:8}[op]
            if w[0]<3:
                for i in range(min(w[2],max(0,maximum-w[1]))):
                    self.state[name][w[0]][w[1]+i] = w[3+i*3:5+i*3] if op==23 else w[3+i]
                    if op==38 and w[3+w[2]+i]!=0xffffffff: writes.append((w[3+i],True))
        elif op==26:
            self.state["render_targets"]=[w[i*4:i*4+4] for i in range(8)]
            self.state["depth_target"]=w[32:37]
        elif op==27:
            self.state["blend_state"]=[w[0],[1.]*4]
            details["blend_constant_raw_words"] = w[1:5]
        elif op==28: self.state["depth_state"]=w[:2]
        elif op in (30,31): details["count"] = w[0]
        elif op in (32,39):
            stages=(0,1) if op==32 else (2,)
            for stage in stages:
                reads.append((self.state["shader"][stage],False))
                configuration.extend((ident,False) for ident in self.state["samplers"][stage])
                reads.extend((x[0],False) for x in self.state["constant_buffers"][stage])
                reads.extend((ident,True) for ident in self.state["srv"][stage])
                reads.extend((ident,True) for ident in self.state["uav"][stage])
                writes.extend((ident,True) for ident in self.state["uav"][stage])
            if op==32:
                reads.extend((x[0],False) for x in self.state["vertex_buffers"])
                reads.append((self.state["index_buffer"][0],False))
                if w[0]>=4: reads.append((w[1],False))
                writes.extend((x[0],False) for x in self.state["render_targets"])
                reads.extend((x[0],False) for x in self.state["render_targets"])
                configuration.extend((ident,False) for ident in (self.state["layout"],self.state["blend_state"][0],
                    self.state["depth_state"][0],self.state["raster_state"]))
                reads.append((self.state["depth_target"][0],False))
                writes.append((self.state["depth_target"][0],False))
                details.update(draw_words=w[:6],shader=self.state["shader"][:],targets=[x[:] for x in self.state["render_targets"]],
                               depth_target=self.state["depth_target"][:],blend_requested=self.blend_requested(),
                               hud_certified=False,foreground_certified=False)
                if self.state["depth_target"][0]==0: candidates.append("depthless_draw_not_HUD_proof")
                if details["blend_requested"]: candidates.append("blend_requested_not_HUD_or_transparency_proof")
            else: details["dispatch_groups"]=w[:3]
        elif op==33: self.state=state_from()
        elif op in (34,35,36):
            source = w[{34:5,35:1,36:2}[op]]
            reads.append((source,False)); writes.append((w[0],False)); details["copy_header"]=w
        elif op in (40,41): reads.append((w[0],True)); writes.append((w[0],True))
        elif op==43: reads.append((w[2],True)); writes.append((w[0],False))
        elif op in (45,46): reads.append((w[0],False))
        elif op not in NAMES: self.issue("unknown_opcode",opcode=op)
        after = (tuple(self.state["shader"]),tuple(tuple(x) for x in self.state["render_targets"]),tuple(self.state["depth_target"]))
        if before!=after: candidates.append("shader_or_target_binding_transition")
        world = row.get("world")
        require(world is None or type(world) is dict, "invalid command camera marker")
        if world != self.current: candidates.append("camera_marker_transition"); self.current=world
        if world:
            for name in ("epoch","camera","request","head","eye","views"):
                uint(world.get(name),"camera marker "+name)
        refs=[]
        # Dedupe within each mode; source reads are resolved before writes,
        # preserving source version for copy and read-modify-write commands.
        unique = list(dict.fromkeys((ident,view,"possible_read") for ident,view in reads if ident))
        unique += list(dict.fromkeys((ident,view,"possible_host_configuration_read") for ident,view in configuration if ident))
        unique += list(dict.fromkeys((ident,view,"possible_write") for ident,view in writes if ident))
        for index,(ident,view,mode) in enumerate(unique):
            ref=self.reference(ident,mode,view)
            if ref and index<MAX_REFS: refs.append(ref)
        if len(unique)>MAX_REFS: self.issue("per_command_access_cap",omitted=len(unique)-MAX_REFS)
        payload=row.get("payload")
        if payload is not None:
            require(type(payload) is dict, "invalid payload metadata")
            declared=uint(payload.get("declared_bytes"),"payload bytes",(1<<32)-1)
            hashed=uint(payload.get("hash_bytes"),"hashed bytes",declared)
            require(op in (10,12,13,47),"payload on non-payload opcode")
            length_index={10:0,12:2,13:11,47:2}[op]
            require(declared==w[length_index],"payload length conflicts with operands")
            require(type(payload.get("hash_complete")) is bool and payload["hash_complete"]==(payload.get("inline_range_valid") is True and hashed==declared), "conflicting payload completeness")
            if "raw_payload_bytes_retained_in_operand_words" in payload:
                require(uint(payload["raw_payload_bytes_retained_in_operand_words"],"raw payload bytes")==declared
                        and payload.get("raw_payload_complete_in_operand_words") is True,"raw payload retention conflicts with complete operands")
            if not payload["hash_complete"]: self.issue("incomplete_payload_fingerprint",bytes_missing=declared-hashed)
            details["payload"]={key:payload.get(key) for key in ("declared_bytes","hash_bytes","hash_complete","raw_payload_bytes_retained_in_operand_words","raw_payload_complete_in_operand_words")}
        elif op in (10,12,13,47):
            self.issue("payload_fingerprint_metadata_unavailable")
        self.timeline.append({"ordinal":self.ordinal,"opcode":op,"operation":NAMES.get(op,"unknown"),"world":world,
                              "accesses":refs,"accesses_omitted":max(0,len(unique)-MAX_REFS),"details":details,
                              "native_context":context,
                              "resource_metadata_rows_omitted":resource_metadata_omitted,
                              "boundary_candidates":candidates,"boundary_certified":False})

    def run(self):
        for row in self.trace["records"]: self.process(row)
        return {"schema":1,"analysis_scope":"observed host opcodes and potential accesses; never successful GPU execution or shader consumption",
                "input_command_sequence_complete":True,"snapshot_complete":False,"hud_certified":False,"foreground_certified":False,
                "start_view":self.trace["start_view"],"end_view":self.trace.get("end_view"),
                "end_immersive_qualified":self.trace.get("end_immersive_qualified"),"endpoint_epoch_matches":self.trace.get("endpoint_epoch_matches"),
                "generation_basis":"analyzer-local observed create/replace/destroy; pre-existing generations unknown, not native immutable identities",
                "version_basis":"potential write command ordinal; not GPU contents or successful write generation",
                "edge_omissions":self.edge_omissions,"issues_observed":self.issue_count,"issues_retained":self.issues,
                "retained_potential_edge_list_complete":self.edge_omissions==0,
                "timeline_access_lists_complete":not any(row["accesses_omitted"] for row in self.timeline),
                "native_resource_graph_complete":False,
                "native_evidence_untruncated":all(row["native_context"] is not None and row["native_context"]["pending_clears_omitted"]==0 for row in self.timeline)
                    and all(row["resource_metadata_rows_omitted"]==0 for row in self.timeline)
                    and not any(issue["kind"]=="native_cached_view_cap" for issue in self.issues) and self.issue_count==len(self.issues),
                "host_candidate_access_reconstruction_complete":self.edge_omissions==0 and not any(
                    issue["kind"] in {"unknown_opcode","unresolved_view_resource","null_view_resource","kind_conflict",
                        "view_resource_conflict","unobserved_native_lifetime_change","native_allocation_conflict","per_command_access_cap"} for issue in self.issues)
                    and self.issue_count==len(self.issues),
                "limits":{"edges":MAX_EDGES,"resource_lifetimes":MAX_RESOURCES,"accesses_per_command":MAX_REFS,"output_bytes":MAX_OUTPUT},
                "missing":["native derived work and actual binding consumption","GPU contents and async effects","subresource alias/coverage and confirmed execution","foreground/HUD semantics and coherent stereo epoch"]}


class BoundedText:
    def __init__(self, stream): self.stream,self.remaining=stream,MAX_OUTPUT
    def write(self, text):
        self.remaining-=len(text.encode("utf-8"))
        require(self.remaining>=0, "output byte cap exceeded; output may be incomplete")
        return self.stream.write(text)


def analyze(path, output):
    trace,digest=load_trace(path)
    analyzer=Analyzer(trace)
    summary=analyzer.run()
    summary["input_sha256"]=digest
    output.mkdir(parents=False,exist_ok=False)
    for filename,data in (("timeline.json",{"summary":summary,"commands":analyzer.timeline}),
                          ("graph.json",{"summary":summary,"resources":analyzer.nodes,"potential_edges":analyzer.edges})):
        with (output/filename).open("x") as stream:
            json.dump(data,BoundedText(stream),separators=(",",":"),allow_nan=False)
            stream.write("\n")
    with (output/"timeline.csv").open("x",newline="") as stream:
        writer=csv.writer(BoundedText(stream))
        writer.writerow(["ordinal","opcode","operation","epoch","camera","request","head","eye","accesses","omitted","candidate_boundaries","certified"])
        for row in analyzer.timeline:
            marker=row["world"] or {}
            writer.writerow([row["ordinal"],row["opcode"],row["operation"],*[marker.get(k,"") for k in ("epoch","camera","request","head","eye")],
                             len(row["accesses"]),row["accesses_omitted"],";".join(row["boundary_candidates"]),False])
    return summary


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trace",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True,help="New directory; never overwrite analysis")
    args=parser.parse_args()
    try:
        summary=analyze(args.trace,args.output)
    except (OSError,ValueError,TypeError,KeyError,RecursionError) as error:
        print("Command analysis rejected:",error,file=sys.stderr)
        return 2
    print(json.dumps({"output":str(args.output),"issues":summary["issues_observed"],"edge_omissions":summary["edge_omissions"],"snapshot_complete":False}))
    return 0


if __name__=="__main__":
    sys.exit(main())
