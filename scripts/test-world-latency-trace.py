#!/usr/bin/env python3
"""Compile actual trace module and run bounded CPU-only off/finish/cap probes."""
import argparse
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rustc", type=Path, required=True)
    parser.add_argument("--deps", type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    source = out / "harness.rs"
    source.write_text(r'''
#![allow(dead_code)]
use std::sync::atomic::{AtomicUsize,Ordering};
struct CountAllocator;
static ALLOCATIONS:AtomicUsize=AtomicUsize::new(0);
unsafe impl std::alloc::GlobalAlloc for CountAllocator {
    unsafe fn alloc(&self,layout:std::alloc::Layout)->*mut u8 {
        ALLOCATIONS.fetch_add(1,Ordering::Relaxed);std::alloc::System.alloc(layout)
    }
    unsafe fn dealloc(&self,p:*mut u8,layout:std::alloc::Layout){std::alloc::System.dealloc(p,layout)}
}
#[global_allocator]static ALLOCATOR:CountAllocator=CountAllocator;
#[path=__SOURCE__]mod trace;
fn main(){
    let mode=std::env::args().nth(1).unwrap();
    trace::initialize_from_env();
    if mode=="disabled" {
        assert!(!trace::enabled());let before=ALLOCATIONS.load(Ordering::Relaxed);
        for _ in 0..10000{trace::record("probe",Default::default(),Default::default());}
        trace::finish();let after=ALLOCATIONS.load(Ordering::Relaxed);assert_eq!(before,after);
        println!("PASS:10000 disabled hooks,zero trace allocations");return;
    }
    assert!(trace::enabled());
    let old=trace::Identity{epoch:1,request:Some(3),head:Some(4),camera:Some(5),eye:Some(0),..Default::default()};
    if mode=="cap" {for _ in 0..trace::MAX_EVENTS+100{trace::record("camera_consumed",old,Default::default());}}
    else {
        trace::record("camera_consumed",old,Default::default());
        trace::record("camera_consumed",trace::Identity{epoch:2,..old},Default::default());
        trace::record("snapshot_callback_observed",old,Default::default());
    }
    trace::finish();trace::finish();assert!(!trace::enabled());
    for _ in 0..2000 {
        let status=trace::status_json();assert!(serde_json::to_vec(&status).unwrap().len()<=2048);
        if status["state"]=="written" {
            let expected=if mode=="cap"{trace::MAX_EVENTS}else{3};assert_eq!(status["recorded"],expected);
            trace::initialize_from_env();assert!(!trace::enabled());
            println!("PASS:once-only {mode} export,status bound,no rearm");return;
        }
        if status["state"]=="failed"{panic!("{status}");}
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("export worker did not complete");
}
'''.replace("__SOURCE__", json.dumps(str(ROOT / "src/world_latency_trace.rs"))))
    serde = sorted(args.deps.glob("libserde_json-*.rlib"))[-1]
    common = [str(args.rustc), "--edition=2021", "-L", f"dependency={args.deps}",
              "--extern", f"serde_json={serde}"]
    commands = [common + ["--test", str(ROOT / "src/world_latency_trace.rs"), "--crate-name", "world_latency_validation",
                          "-o", str(out / "unit-validation")],
                common + [str(source), "--crate-name", "world_latency_probes", "-o", str(out / "probes")]]
    logs = []
    for command in commands:
        result = subprocess.run(command, text=True, capture_output=True)
        logs.append(result.stdout + result.stderr)
        result.check_returncode()
    result = subprocess.run([str(out / "unit-validation"), "--test-threads=1"], text=True, capture_output=True)
    logs.append(result.stdout + result.stderr)
    result.check_returncode()
    for mode in ("disabled", "finish", "cap"):
        env = dict(os.environ)
        env.pop("MUGUET_XR_LATENCY_TRACE", None)
        env.pop("MUGUET_XR_LATENCY_TRACE_PATH", None)
        if mode != "disabled":
            env["MUGUET_XR_LATENCY_TRACE"] = "1"
            env["MUGUET_XR_LATENCY_TRACE_PATH"] = str(out / f"{mode}.ndjson")
        result = subprocess.run([str(out / "probes"), mode], env=env, text=True, capture_output=True)
        logs.append(result.stdout + result.stderr)
        result.check_returncode()
        if mode != "disabled":
            data = (out / f"{mode}.ndjson").read_bytes()
            assert len(data) <= 4 * 1024 * 1024
            rows = [json.loads(line) for line in data.splitlines()]
            assert rows[-1]["type"] == "footer"
            if mode == "finish":
                assert [r["epoch"] for r in rows[1:-1]] == [1, 2, 1]
    (out / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
    (out / "results.log").write_text("\n".join(logs))
    print("PASS:actual Rust unit module + disabled allocation/short finish/cap CPU probes")


if __name__ == "__main__":
    main()
