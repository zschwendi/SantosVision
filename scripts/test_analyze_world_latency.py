import copy
import unittest
from analyze_world_latency import analyze


def trace():
    stages = ("request_received", "camera_consumed", "ring_camera_dequeued", "snapshot_submitted",
              "snapshot_completed", "source_acquired", "compositor_commit", "compositor_completed")
    events = []
    for sequence, stage in enumerate(stages):
        event = dict(type="event", sequence=sequence, elapsed_ns=sequence * 1_000_000, session_id=5,
                     stage=stage, epoch=2, request=3, head=4, camera=6, game=7, eye=0, views=2,
                     lease=8, metal_status=4)
        if sequence == 0:
            event.update(head=None, camera=None, game=None, eye=None, lease=None)
        elif sequence < 3:
            event.update(game=None, lease=None)
        elif sequence < 5:
            event.update(lease=None)
        events.append(event)
    return [dict(type="header", schema=1, session_id=5), *events,
            dict(type="footer", session_id=5, exported=8, record_limit_reached=False)]


class Timeline(unittest.TestCase):
    def test_complete_exact_identity_chain(self):
        result = analyze(trace())
        row = result["completed_lease_observations"][0]
        self.assertEqual(row["milliseconds"]["request_to_camera"], 1)
        self.assertEqual(row["milliseconds"]["camera_to_observed_compositor_completion"], 6)
        self.assertFalse(result["same_epoch_stereo_certified"])

    def test_callback_wait_is_separate_from_publication(self):
        data = trace()
        callback = copy.deepcopy(data[4])  # submitted identity retained
        callback.update(stage="snapshot_callback_observed", elapsed_ns=3_500_000)
        data[5:5] = [callback]
        for sequence, event in enumerate(data[1:-1]):
            event["sequence"] = sequence
        data[-1]["exported"] += 1
        row = analyze(data)["completed_lease_observations"][0]
        self.assertEqual(row["milliseconds"]["snapshot_submit_to_callback_observed"], 0.5)
        self.assertEqual(row["milliseconds"]["snapshot_callback_to_ready_publication"], 0.5)

    def test_error_metal_status_is_not_a_success(self):
        data = trace()
        data[8]["metal_status"] = 5
        row = analyze(data)["completed_lease_observations"][0]
        self.assertFalse(row["successful_metal_completion"])

    def test_other_eye_cannot_fill_missing_stage(self):
        data = trace()
        data[5]["eye"] = 1  # snapshot_completed only
        row = analyze(data)["completed_lease_observations"][0]
        self.assertIsNone(row["milliseconds"]["snapshot_ready_to_acquire"])

    def test_old_epoch_callback_cannot_join_new_source(self):
        data = trace()
        data[5]["epoch"] = 1
        row = analyze(data)["completed_lease_observations"][0]
        self.assertIsNone(row["milliseconds"]["snapshot_submit_to_observed_completion"])

    def test_other_lease_cannot_fill_commit(self):
        data = trace()
        data[7]["lease"] = 88
        row = analyze(data)["completed_lease_observations"][0]
        self.assertIsNone(row["milliseconds"]["compositor_commit_to_observed_completion"])

    def test_unknown_camera_not_inferred(self):
        data = trace()
        data[3]["camera"] = None
        row = analyze(data)["completed_lease_observations"][0]
        self.assertIsNone(row["milliseconds"]["camera_to_first_ring_marker"])

    def test_mixed_session_rejected(self):
        data = trace()
        data[5]["session_id"] = 6
        with self.assertRaises(ValueError):
            analyze(data)

    def test_partial_export_rejected(self):
        with self.assertRaises(ValueError):
            analyze(trace()[:-1])

    def test_nonmonotonic_rejected(self):
        data = trace()
        data[5]["elapsed_ns"] = 1
        with self.assertRaises(ValueError):
            analyze(data)

    def test_reused_source_gets_separate_lease_observation(self):
        data = trace()
        other = copy.deepcopy(data[6:9])
        for index, event in enumerate(other, 8):
            event.update(sequence=index, elapsed_ns=index * 1_000_000, lease=9)
        data[-1]["exported"] += len(other)
        data[-1:-1] = other
        rows = analyze(data)["completed_lease_observations"]
        self.assertEqual([r["lease"] for r in rows], [8, 9])


if __name__ == "__main__":
    unittest.main()
