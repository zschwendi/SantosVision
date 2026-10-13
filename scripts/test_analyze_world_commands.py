#!/usr/bin/env python3
"""Synthetic tests; no game assets or native runtime needed."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec=importlib.util.spec_from_file_location("commands",Path(__file__).with_name("analyze_world_commands.py"))
m=importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def record(op,operands):
    return {"opcode":op,"declared_operand_words":len(operands),"operands":operands,
            "operands_complete":True,"operand_range_valid":True,"operand_words_omitted":0,
            "wrap_declared_length_not_interpreted":op==1,"world":None,"resources":[],"payload":None}


def trace(commands, inherited=None):
    marker={"epoch":7,"camera":8,"request":9,"head":10,"eye":0,"views":2,"time":1.,"headset_applied":True}
    first=record(7,[99]); first.update(inherited_state=inherited or m.state_from(),present_view=marker)
    rows=[first,*[record(op,w) for op,w in commands],record(7,[99])]
    for i,row in enumerate(rows,1): row["ordinal"]=i
    return {"schema":1,"state":"captured","snapshot_complete":False,"start_game_frame":10,"end_game_frame":11,
            "start_view":marker,"end_view":marker,"end_immersive_qualified":True,"endpoint_epoch_matches":True,
            "records":rows,"records_retained":len(rows),"commands_observed":len(rows),"records_omitted":0,
            "first_omitted_ordinal":None,"command_sequence_complete":True,"all_operands_complete":True,
            "records_with_incomplete_operands":0}


class Tests(unittest.TestCase):
    def test_id_reuse_retires_lifetime_and_resets_potential_version(self):
        value=trace([(11,[1,64,0,0,0]),(12,[1,0,4,123]),(4,[1]),(11,[1,64,0,0,0]),(12,[1,0,4,456])])
        analyzer=m.Analyzer(value); analyzer.run()
        nodes=[n for n in analyzer.nodes if n["id"]==1]
        self.assertEqual([n["local_generation"] for n in nodes],[1,2])
        self.assertFalse(nodes[0]["alive"])
        self.assertEqual([n["potential_write_version"] for n in nodes],[1,1])

    def test_copy_reads_old_version_before_destination_write(self):
        value=trace([(11,[1,64,0,0,0]),(11,[2,64,0,0,0]),(12,[1,0,4,123]),(35,[2,1]),(35,[2,2])])
        analyzer=m.Analyzer(value); analyzer.run()
        copy=analyzer.timeline[-3]["accesses"]
        self.assertEqual(copy[0]["resource"],"1:local1")
        self.assertEqual(copy[0]["depends_on_potential_writer"],4)
        self.assertEqual(copy[1]["potential_version"],1)
        last=analyzer.timeline[-2]["accesses"]
        self.assertEqual([x["potential_version"] for x in last],[1,2])
        self.assertEqual(last[1]["previous_potential_version"],1)
        self.assertEqual(last[1]["previous_potential_writer"],5)
        self.assertTrue(last[1]["prior_contents_may_survive"])

    def test_unknown_inherited_resource_stays_unknown(self):
        inherited=m.state_from(); inherited["vertex_buffers"][0]=[55,12,0]
        value=trace([(32,[0,3,1,0,0,0])],inherited)
        analyzer=m.Analyzer(value); analyzer.run()
        ref=next(x for x in analyzer.timeline[1]["accesses"] if x["id"]==55)
        self.assertFalse(ref["generation_known"])
        self.assertTrue(ref["unresolved_initial_contents"])
        self.assertFalse(ref["native_allocation_known"])

    def test_truncated_prefix_gaps_and_conflicting_counters_rejected(self):
        original=trace([(1,[]),(35,[1,2])])
        for mutate in (lambda x:x.update(records_omitted=1,command_sequence_complete=False),
                       lambda x:x["records"][1].update(ordinal=3),
                       lambda x:x.update(commands_observed=99),
                       lambda x:x["records"][1].update(operands_complete=False),
                       lambda x:x["records"].pop()):
            value=copy.deepcopy(original); mutate(value)
            with self.assertRaises(ValueError): m.Analyzer(value)

    def test_payload_extent_rejected_but_missing_fingerprint_not_complete_claim(self):
        with self.assertRaises(ValueError): m.Analyzer(trace([(12,[1,0,8,123])]))
        value=trace([(12,[1,0,4,123])])
        value["records"][1]["payload"]={"declared_bytes":4,"hash_bytes":2,"hash_complete":False,"inline_range_valid":True}
        analyzer=m.Analyzer(value); result=analyzer.run()
        self.assertIn("incomplete_payload_fingerprint",[x["kind"] for x in result["issues_retained"]])
        self.assertFalse(result["snapshot_complete"])
        value["records"][1]["payload"]["hash_complete"]=True
        with self.assertRaises(ValueError): m.Analyzer(value).run()

    def test_blend_and_depthless_candidates_never_certify_hud(self):
        values=[0,0,1,5,6,1,1,0,1,15]
        value=trace([(16,[7,0,*values]),(27,[7,0,0,0,0]),(32,[0,3,1,0,0,0])])
        analyzer=m.Analyzer(value); result=analyzer.run()
        draw=analyzer.timeline[-2]
        self.assertTrue(draw["details"]["blend_requested"])
        self.assertFalse(draw["details"]["hud_certified"])
        self.assertFalse(draw["details"]["foreground_certified"])
        self.assertFalse(result["hud_certified"])

    def test_view_cache_allocation_unresolved_across_texture_reuse(self):
        tex=[1,2,64,64,1,1,1,28,8,1]
        value=trace([(3,tex),(17,[2,2,1,28,4,0,1,0,1]),(4,[1]),(3,tex),
                     (24,[1,0,1,2]),(32,[0,3,1,0,0,0])])
        analyzer=m.Analyzer(value); analyzer.run()
        ref=next(x for x in analyzer.timeline[-2]["accesses"] if x["through_view"]=="2:local1")
        self.assertEqual(ref["resource"],"1:local2")
        self.assertEqual(ref["native_allocation_resolution"],"unknown_cached_native_texture_view_allocation")
        self.assertIn("1:local1",ref["cached_prior_lifetime_candidates"])
        self.assertFalse(ref["native_allocation_known"])

    def test_unqualified_epoch_endpoint_is_explicit(self):
        value=trace([]); value.update(end_view=None,end_immersive_qualified=False,endpoint_epoch_matches=False)
        result=m.Analyzer(value).run()
        self.assertFalse(result["end_immersive_qualified"])
        self.assertFalse(result["endpoint_epoch_matches"])

    def test_render_target_load_and_host_configuration_dependencies(self):
        targets=[1,0,0,0]+[0]*33
        value=trace([(3,[1,2,64,64,1,1,1,28,32,1]),(5,[1,0,0,0,0,0,0]),
                     (16,[2,3,0]),(25,[0,0,1,2]),(15,[3,0]),(19,[3]),(26,targets),(32,[0,3,1,0,0,0])])
        analyzer=m.Analyzer(value); analyzer.run()
        refs=analyzer.timeline[-2]["accesses"]
        target=[r for r in refs if r["id"]==1]
        self.assertEqual([r["mode"] for r in target],["possible_read","possible_write"])
        self.assertEqual(target[0]["depends_on_potential_writer"],3)
        self.assertTrue(any(r["id"]==2 and r["mode"]=="possible_host_configuration_read" for r in refs))
        self.assertTrue(any(r["id"]==3 and r["mode"]=="possible_host_configuration_read" for r in refs))

    def test_unknown_opcode_keeps_record_but_access_reconstruction_incomplete(self):
        analyzer=m.Analyzer(trace([(999,[])])); result=analyzer.run()
        self.assertEqual(analyzer.timeline[1]["opcode"],999)
        self.assertFalse(result["host_candidate_access_reconstruction_complete"])
        self.assertTrue(result["retained_potential_edge_list_complete"])

    def test_native_handle_evidence_does_not_certify_selected_binding_or_contents(self):
        allocation={"kind":"texture","tag":123,"scope":"this_process","basis":"returned wgpu handle allocation site", "backend_id":None,
                    "identity_kind":"wgpu_texture_handle_lifetime","backend_allocation_validated":False}
        value=trace([(24,[1,0,1,2]),(32,[0,3,1,0,0,0])])
        value["records"][1]["resources"]=[{"id":2,"pre_exec":{"kind":"view","resource":1,"is_buffer":False,
            "cached_native_views":[{"key":"D2","retained_parent_allocation":{"resolved":True,"parent_eq":True,"native_allocation":allocation}}],"cached_native_views_omitted":0}}]
        analyzer=m.Analyzer(value); result=analyzer.run()
        edge=next(e for e in analyzer.timeline[-2]["accesses"] if e["through_view"])
        self.assertEqual(edge["cached_native_view_parent_evidence"][0]["native_allocation"]["tag"],123)
        self.assertFalse(edge["native_allocation_known"])
        self.assertEqual(edge["cached_native_view_observed_ordinal"],2)
        self.assertFalse(edge["cached_native_view_evidence_current"])
        self.assertFalse(result["snapshot_complete"])
        value["records"][2]["resources"]=[{"id":2,"pre_exec":{"kind":"view","resource":1,"is_buffer":False,"cached_native_views":[],"cached_native_views_omitted":0}}]
        analyzer=m.Analyzer(value); analyzer.run()
        edge=next(e for e in analyzer.timeline[-2]["accesses"] if e["through_view"])
        self.assertEqual(edge["cached_native_view_parent_evidence"],[])
        self.assertTrue(edge["cached_native_view_evidence_current"])

    def test_native_pass_key_distinct_from_state_and_clear_omissions_preserved(self):
        value=trace([]); key=[1,0,0,28]+[0]*33
        context={"capture_phase":"pre_exec","pass_active":True,"pass_key":key,
                 "attachments":[{"aspect":"color","slot":0,"guest_id":1,"mip":0,"slice":0,"retained_parent":None}],
                 "pending_clears":[],"pending_clears_observed":2,"pending_clears_omitted":2}
        value["records"][0]["native_context"]=context
        analyzer=m.Analyzer(value); result=analyzer.run()
        self.assertEqual(analyzer.timeline[0]["native_context"]["pass_key"][0],1)
        self.assertEqual(analyzer.state["render_targets"][0][0],0)
        self.assertIn("native_pending_clear_cap",[e["kind"] for e in result["issues_retained"]])
        self.assertFalse(result["native_evidence_untruncated"])
        context["attachments"][0]["guest_id"]=2
        with self.assertRaises(ValueError):m.Analyzer(value).run()

    def test_native_identity_conflict_invalidates_candidate_reconstruction(self):
        value=trace([(5,[1,0,0,0,0,0,0]),(5,[1,0,0,0,0,0,0])])
        for row,tag in zip(value["records"][1:3],[100,200]):
            row["resources"]=[{"id":1,"pre_exec":{"kind":"texture","native_allocation":
                {"kind":"texture","tag":tag,"scope":"this_process","basis":"returned handle","backend_id":None,"backend_allocation_validated":False}}}]
        analyzer=m.Analyzer(value); result=analyzer.run()
        self.assertFalse(result["host_candidate_access_reconstruction_complete"])
        self.assertTrue(analyzer.live[1]["native_identity_conflict"])
        value["records"][1]["resources"][0]["pre_exec"]["native_allocation"]["backend_allocation_validated"]=True
        with self.assertRaises(ValueError):m.Analyzer(value).run()

    def test_raw_resource_metadata_cap_invalidates_native_evidence_completeness(self):
        value=trace([])
        for row in value["records"]:
            row["native_context"]={"capture_phase":"pre_exec","pass_active":False,"pass_key":None,"attachments":[],
                                   "pending_clears":[],"pending_clears_observed":0,"pending_clears_omitted":0}
        self.assertTrue(m.Analyzer(value).run()["native_evidence_untruncated"])
        value["records"][0]["resource_access_rows_omitted"]=2
        analyzer=m.Analyzer(value); result=analyzer.run()
        self.assertFalse(result["native_evidence_untruncated"])
        self.assertEqual(analyzer.timeline[0]["resource_metadata_rows_omitted"],2)

    def test_create_new_outputs_and_input_size_duplicate_rejection(self):
        with tempfile.TemporaryDirectory() as temp:
            base=Path(temp); source=base/"trace.json"; source.write_text(json.dumps(trace([])))
            output=base/"analysis"; result=m.analyze(source,output)
            self.assertEqual(len(list(output.iterdir())),3)
            self.assertFalse(result["snapshot_complete"])
            before=(output/"graph.json").read_bytes()
            with self.assertRaises(FileExistsError): m.analyze(source,output)
            self.assertEqual((output/"graph.json").read_bytes(),before)
            source.write_text('{"schema":1,"schema":1}')
            with self.assertRaises(ValueError): m.load_trace(source)
            source.write_bytes(b" "*(m.MAX_INPUT+1))
            with self.assertRaises(ValueError): m.load_trace(source)


if __name__=="__main__": unittest.main()
