#!/usr/bin/env python3
"""Production config and actual window drawable-aspect checks; no game/device.

The Rust harness imports the exact config.rs with two unused desktop services
stubbed. The Swift harness executes the exact WindowGameSurface class and the
actual aspectRatio expression and updateUIView body with a tiny UIView holder;
CAMetalLayer, Combine and actor scheduling are real macOS frameworks.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rustc", type=Path, required=True)
    parser.add_argument("--deps", type=Path, required=True)
    parser.add_argument("--config-source", type=Path)
    parser.add_argument("--window-source", type=Path)
    parser.add_argument("--capture", type=Path, help="optional registered schema2 third-person capture report; no image modification")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    rust = out / "config-harness.rs"
    config_source = args.config_source or ROOT / 'src/config.rs'
    window_source = args.window_source or ROOT / 'visionos/Sources/WindowGameView.swift'
    rust.write_text('#![allow(dead_code)]\nfn code_embedded()->bool{true}\nmod gpu{pub fn max_msaa()->u32{8}}\n'
                    + '#[path=' + json.dumps(str(config_source)) + '] mod config;\n')
    deps = []
    for name in ("anyhow", "regex", "serde_json"):
        matches = sorted(args.deps.glob(f"lib{name}-*.rlib"))
        if not matches:
            raise RuntimeError(f"missing prebuilt {name} in {args.deps}")
        deps += ["--extern", f"{name}={matches[-1]}"]
    compile_rust = [str(args.rustc), "--edition=2021", "--test", str(rust), "--crate-name", "source_aspect_config",
                    "-L", f"dependency={args.deps}", *deps, "-o", str(out / "config-validation")]
    subprocess.run(compile_rust, check=True)
    r = subprocess.run([str(out / "config-validation"), "config::", "--test-threads=1"], capture_output=True, text=True)
    (out / "rust-results.log").write_text(r.stdout + r.stderr)

    if not re.search(r"running (?:1[7-9]|[2-9][0-9]+) tests", r.stdout):
        raise RuntimeError("config tests were not executed")
    view = window_source.read_text()
    start = view.index("@MainActor\nfinal class WindowGameSurface")
    end = view.index("\nprivate struct GameCanvas", start)
    actual_surface = view[start:end]
    expression = re.search(r"\.aspectRatio\(([^\n]+), contentMode: \.fit\)", view).group(1)
    update_body = re.search(r"func updateUIView\(_ view: UIView, context: Context\) \{([^}]+|)\}", view).group(1)
    validation = r'''
@main struct Validation {
    @MainActor static func main() async throws {
        var failures:[String] = []; var checks = 0
        func check(_ value:Bool,_ message:String){checks += 1;if !value{failures.append(message)}}
        let surface = WindowGameSurface()
        func displayedAspect()->CGFloat { __ACTUAL_EXPRESSION__ }
        func updateView(){ __ACTUAL_UPDATE__ }
        check(displayedAspect() == 16.0/9.0, "original initial aspect remains16:9")
        for (width,height) in [(640,360),(960,540),(1280,720),(1920,1080),(2560,1440),(3840,2160),(2560,2304),(1920,1080)] {
            surface.metalLayer.drawableSize = CGSize(width:width,height:height)
            updateView()
            try await Task.sleep(for:.milliseconds(25))
            check(abs(displayedAspect() - CGFloat(width)/CGFloat(height)) < 1e-12,
                  "actual drawable \(width)x\(height) must fit without stretching")
        }
        // Zero size is a real pre-configuration layer state. It must retain the
        // last usable source aspect, rather than publishing an invalid ratio.
        surface.metalLayer.drawableSize = .zero
        updateView()
        try await Task.sleep(for:.milliseconds(25))
        check(displayedAspect() == 16.0/9.0,"zero drawable preserves prior valid aspect")
        __INVALID_CHECKS__
        let result:[String:Any] = ["checks":checks,"failures":failures,"scope":"actual production surface, updateUIView and aspect expression; UIKit holder stub; real CAMetalLayer; no game/device/display-pixel acceptance"]
        print(String(data:try JSONSerialization.data(withJSONObject:result,options:[.sortedKeys]),encoding:.utf8)!)
        if !failures.isEmpty{exit(1)}
    }
}
'''.replace("__ACTUAL_EXPRESSION__", expression).replace("__ACTUAL_UPDATE__", update_body)
    invalid_checks = ""
    extension = ""
    if "updateSourceAspectRatio" in actual_surface:
        extension = "\nextension WindowGameSurface{func validationSize(_ size:CGSize){updateSourceAspectRatio(size)}}\n"
        invalid_checks = '''for size in [CGSize(width:0,height:3),CGSize(width:3,height:0),CGSize(width:-3,height:3),CGSize(width:Double.nan,height:3),CGSize(width:3,height:Double.infinity)] {
            surface.validationSize(size);check(displayedAspect() == 16.0/9.0,"invalid source dimensions cannot change aspect")
        }'''
    validation = validation.replace("__INVALID_CHECKS__", invalid_checks)
    swift = out / "surface-validation.swift"
    swift.write_text('''import Foundation
import Combine
import QuartzCore
import Metal
@MainActor class UIView {let layer:CALayer;init(layer:CALayer){self.layer=layer}}
@MainActor final class GameMetalView:UIView {
 var windowAttachmentChanged:((Bool)->Void)?
 init(frame:CGRect){super.init(layer:CAMetalLayer())}
}
''' + actual_surface + extension + validation)
    subprocess.run(["xcrun", "swiftc", "-O", "-swift-version", "6", "-strict-concurrency=complete", "-parse-as-library",
                    str(swift), "-o", str(out / "surface-validation")], check=True)
    s = subprocess.run([str(out / "surface-validation")], capture_output=True, text=True)
    (out / "swift-results.log").write_text(s.stdout + s.stderr)
    subprocess.run(["xcrun", "swiftc", "-O", str(ROOT / "visionos/Sources/NativeGraphicsSettings.swift"),
                    str(ROOT / "visionos/Tests/NativeGraphicsSettingsValidation.swift"), "-o", str(out / "graphics-validation")], check=True)
    g = subprocess.run([str(out / "graphics-validation")], capture_output=True, text=True)
    (out / "graphics-results.log").write_text(g.stdout + g.stderr)
    projection = None
    if args.capture:
        projection_files = ["GameWorldDepthRange.swift", "GameWorldProjection.swift", "GameWorldSourceGuardBand.swift",
                            "GameWorldDepthEdgeFillPolicy.swift", "GameWorldHUD.swift"]
        subprocess.run(["xcrun", "swiftc", "-O", *[str(ROOT / "visionos/Sources" / name) for name in projection_files],
                        str(ROOT / "visionos/Tests/SourceAspectProjectionValidation.swift"), "-o", str(out / "projection-validation")], check=True)
        projection = subprocess.run([str(out / "projection-validation"), str(args.capture.resolve())], capture_output=True, text=True)
        (out / "projection-results.log").write_text(projection.stdout + projection.stderr)
    receipt = {"rust_result": r.returncode, "swift_result": s.returncode,
               "graphics_result": g.returncode, "projection_result": projection.returncode if projection else None,
               "files": {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                         for p in (config_source, window_source)},
               "extracted_aspect_expression": expression,
               "registered_capture_sha256": hashlib.sha256(args.capture.read_bytes()).hexdigest() if args.capture else None,
               "scope": "source/config and actual native layer aspect transport; no game, HUD or physical acceptance"}
    (out / "results.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(r.stdout, s.stdout, g.stdout, projection.stdout if projection else "")
    raise SystemExit(1 if r.returncode or s.returncode or g.returncode or (projection and projection.returncode) else 0)


if __name__ == "__main__":
    main()
