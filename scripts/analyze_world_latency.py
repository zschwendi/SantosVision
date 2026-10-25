#!/usr/bin/env python3
"""Analyze bounded native latency NDJSON without merging eyes/lifecycles."""
import argparse
import json
from pathlib import Path

MAX_BYTES = 4 * 1024 * 1024
MAX_EVENTS = 8192


def camera_key(event):
    names = ("session_id", "epoch", "request", "head", "camera", "eye")
    values = tuple(event.get(name) for name in names)
    return values if all(value is not None for value in values) else None


def analyze(objects):
    if not objects or objects[0].get("type") != "header" or objects[0].get("schema") != 1:
        raise ValueError("missing schema1 header")
    header = objects[0]
    if objects[-1].get("type") != "footer":
        raise ValueError("missing complete footer; partial exports are not accepted")
    footer = objects[-1]
    session = header.get("session_id")
    events = objects[1:-1]
    if len(events) > MAX_EVENTS or footer.get("exported") != len(events):
        raise ValueError("event count mismatch/limit")
    if footer.get("session_id") != session:
        raise ValueError("footer session mismatch")
    previous = (-1, -1)
    for event in events:
        if event.get("type") != "event" or event.get("session_id") != session:
            raise ValueError("mixed session or invalid event")
        sequence, timestamp = event.get("sequence"), event.get("elapsed_ns")
        epoch = event.get("epoch")
        if any(type(v) is not int or v < 0 for v in (sequence, timestamp, epoch)):
            raise ValueError("invalid monotonic identity/time")
        if sequence <= previous[0] or timestamp < previous[1]:
            raise ValueError("nonmonotonic event order")
        previous = sequence, timestamp

    requests, cameras, rings, submissions, callbacks, snapshots, acquired, commits = {}, {}, {}, {}, {}, {}, {}, {}
    completed = []
    counts = {}

    def delay(start, end):
        if start is None or end["elapsed_ns"] < start["elapsed_ns"]:
            return None
        return (end["elapsed_ns"] - start["elapsed_ns"]) / 1_000_000

    for event in events:
        stage = event.get("stage")
        counts[stage] = counts.get(stage, 0) + 1
        request = (session, event["epoch"], event.get("request"))
        camera = camera_key(event)
        source = camera + (event.get("game"),) if camera and event.get("game") is not None else None
        lease = source + (event.get("lease"),) if source and event.get("lease") is not None else None
        if stage == "request_received" and request[-1] is not None:
            requests[request] = event
        elif stage == "camera_consumed" and camera:
            cameras[camera] = event
        elif stage == "ring_camera_dequeued" and camera:
            rings.setdefault(camera, event)
        elif stage == "snapshot_submitted" and source:
            submissions[source] = event
        elif stage == "snapshot_callback_observed" and source:
            callbacks[source] = event
        elif stage == "snapshot_completed" and source:
            snapshots[source] = event
        elif stage == "source_acquired" and lease:
            acquired[lease] = event
        elif stage == "compositor_commit" and lease:
            commits[lease] = event
        elif stage == "compositor_completed" and lease:
            cam, ring = cameras.get(camera), rings.get(camera)
            submitted, snapshot = submissions.get(source), snapshots.get(source)
            callback = callbacks.get(source)
            acquire, commit = acquired.get(lease), commits.get(lease)
            completed.append({
                "session_id": session, "epoch": event["epoch"], "request": event.get("request"),
                "head": event.get("head"), "camera": event.get("camera"), "game": event.get("game"),
                "eye": event.get("eye"), "lease": event.get("lease"), "metal_status": event.get("metal_status"),
                "successful_metal_completion": event.get("metal_status") == 4,
                "milliseconds": {
                    "request_to_camera": delay(requests.get(request), cam) if cam else None,
                    "camera_to_first_ring_marker": delay(cam, ring) if ring else None,
                    "ring_marker_to_snapshot_submit": delay(ring, submitted) if submitted else None,
                    "snapshot_submit_to_observed_completion": delay(submitted, snapshot) if snapshot else None,
                    "snapshot_submit_to_callback_observed": delay(submitted, callback) if callback else None,
                    "snapshot_callback_to_ready_publication": delay(callback, snapshot) if snapshot else None,
                    "snapshot_ready_to_acquire": delay(snapshot, acquire) if acquire else None,
                    "acquire_to_compositor_commit": delay(acquire, commit) if commit else None,
                    "compositor_commit_to_observed_completion": delay(commit, event),
                    "camera_to_observed_compositor_completion": delay(cam, event),
                },
            })
    return {"schema": 1, "session_id": session, "events": len(events), "stage_counts": counts,
            "footer": footer, "completed_lease_observations": completed,
            "scope": "Observed native elapsed stage intervals; includes scheduling, queues and callbacks. Not isolated GPU or motion-to-photon timing.",
            "same_epoch_stereo_certified": False,
            "unmatched_intervals_are_null": True}


def read(path):
    if path.stat().st_size > MAX_BYTES:
        raise ValueError("trace exceeds4MiB")
    return analyze([json.loads(line) for line in path.read_text().splitlines()])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = json.dumps(read(args.trace), indent=2) + "\n"
    if args.output:
        with args.output.open("x") as file:
            file.write(result)
    else:
        print(result, end="")


if __name__ == "__main__":
    main()
