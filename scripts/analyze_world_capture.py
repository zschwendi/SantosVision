#!/usr/bin/env python3
"""Analyze registered PPM/Float32 files from GameWorldDebugCapture, without a GPU.

Dark pixels are candidates, not automatically sky holes. Source/output crops
have different viewpoints and are deliberately not compared pixel for pixel.
Requires the schema-2 completed-command report and files copied beside it.
"""
import argparse
import array
import hashlib
import json
import math
from pathlib import Path
import re
import sys

MAX_BYTES = 96 * 1024 * 1024
CLASSES = ("exact_clear", "finite_nonclear", "out_of_range", "nonfinite")
COLORS = ((30, 130, 255), (255, 100, 30), (255, 0, 255), (255, 255, 0))


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def local_file(folder, reported_path):
    # Device absolute paths do not exist on the Mac. Resolve only copied files
    # beside the report, never arbitrary paths embedded in diagnostic input.
    name = Path(reported_path).name
    require(name not in ("", ".", ".."), "invalid reported filename")
    path = (folder / name).resolve()
    require(path.parent == folder.resolve(), "capture file leaves report folder")
    require(path.is_file() and path.stat().st_size <= MAX_BYTES,
            "capture file missing or oversized: " + name)
    return path


def ppm(path, expected_width, expected_height):
    data = path.read_bytes()
    # Parse the exact header emitted by saveColor. Do not skip whitespace in
    # the binary payload: RGB channels may themselves equal newline or space.
    match = re.match(rb"P6\n([0-9]+) ([0-9]+)\n255\n", data)
    require(match is not None, "unsupported PPM header")
    width, height = map(int, match.groups())
    require((width, height) == (expected_width, expected_height), "PPM dimensions differ from report")
    payload = data[match.end():]
    require(len(payload) == width * height * 3, "PPM payload size differs from dimensions")
    return payload


def depth_class(z, clear):
    if not math.isfinite(z):
        return 3
    if z < 0 or z > 1:
        return 2
    return 0 if z == clear else 1


def components(mask, classes, width, height):
    visited = bytearray(len(mask))
    count = 0
    internal_count = 0
    largest = []
    for start, dark in enumerate(mask):
        if not dark or visited[start]:
            continue
        count += 1
        queue = array.array("I", [start])
        visited[start] = 1
        min_x = max_x = start % width
        min_y = max_y = start // width
        hist = [0] * len(CLASSES)
        cursor = 0
        while cursor < len(queue):
            index = queue[cursor]
            cursor += 1
            x, y = index % width, index // width
            min_x, max_x = min(min_x, x), max(max_x, x)
            min_y, max_y = min(min_y, y), max(max_y, y)
            hist[classes[index]] += 1
            neighbors = []
            if x > 0:
                neighbors.append(index - 1)
            if x + 1 < width:
                neighbors.append(index + 1)
            if y > 0:
                neighbors.append(index - width)
            if y + 1 < height:
                neighbors.append(index + width)
            for neighbor in neighbors:
                if mask[neighbor] and not visited[neighbor]:
                    visited[neighbor] = 1
                    queue.append(neighbor)
        boundary = min_x == 0 or min_y == 0 or max_x == width - 1 or max_y == height - 1
        internal_count += not boundary
        largest.append({"pixels": len(queue), "bounds_xy_inclusive": [min_x, min_y, max_x, max_y],
                        "touches_image_edge": boundary, "depth_classes": dict(zip(CLASSES, hist))})
        largest.sort(key=lambda item: item["pixels"], reverse=True)
        del largest[20:]
    return {"connectivity": 4, "count": count, "internal_count": internal_count, "largest_20": largest}


def analyze_image(report, folder, kind, threshold, output):
    color_record, depth_record = report[kind + "_color"], report[kind + "_depth"]
    width, height = color_record["width"], color_record["height"]
    require(type(width) is int and type(height) is int and 0 < width <= 4096 and 0 < height <= 4096,
            "invalid or oversized capture dimensions")
    require((width, height) == (depth_record["width"], depth_record["height"]),
            "color/depth dimensions differ")
    require(depth_record["row_bytes"] == width * 4 and depth_record["byte_count"] == width * height * 4,
            "depth is not tightly packed Float32")
    require(depth_record["encoding"] == "IEEE754 binary32 little-endian, tightly packed rows, no header",
            "unsupported depth encoding")
    color_path = local_file(folder, color_record["path"])
    depth_path = local_file(folder, depth_record["path"])
    require(Path(depth_record["paired_color_path"]).name == color_path.name, "depth names a different paired color")
    rgb = ppm(color_path, width, height)
    raw = depth_path.read_bytes()
    require(len(raw) == width * height * 4, "Float32 payload size differs from dimensions")
    values = array.array("f")
    require(values.itemsize == 4, "host does not have 32-bit array floats")
    values.frombytes(raw)
    if sys.byteorder != "little":
        values.byteswap()
    projection = report["source_projection_from_eye"]
    require(len(projection) == 16 and all(math.isfinite(v) for v in projection), "invalid source projection")
    # Matches the production sourceDepthReversed test. Output is reverse-Z.
    clear = 0.0 if kind == "output" or projection[10] >= 0 else 1.0
    classes, dark_mask = bytearray(len(values)), bytearray(len(values))
    colored_mask = bytearray(len(rgb))
    depth_hist, dark_hist = [0] * len(CLASSES), [0] * len(CLASSES)
    exact_black = 0
    minimum, maximum = math.inf, -math.inf
    for index, z in enumerate(values):
        category = depth_class(z, clear)
        classes[index] = category
        depth_hist[category] += 1
        if math.isfinite(z):
            minimum, maximum = min(minimum, z), max(maximum, z)
        offset = index * 3
        peak = max(rgb[offset], rgb[offset + 1], rgb[offset + 2])
        exact_black += peak == 0
        if peak <= threshold:
            dark_mask[index] = 1
            dark_hist[category] += 1
            colored_mask[offset:offset + 3] = bytes(COLORS[category])
    mask_path = output / (kind + "-dark-depth-classes.ppm")
    with mask_path.open("xb") as stream:
        stream.write(f"P6\n{width} {height}\n255\n".encode())
        stream.write(colored_mask)
    return {"width": width, "height": height, "clear_value": clear,
            "color_sha256": hashlib.sha256(color_path.read_bytes()).hexdigest(),
            "depth_sha256": hashlib.sha256(raw).hexdigest(),
            "finite_minimum": minimum if math.isfinite(minimum) else None,
            "finite_maximum": maximum if math.isfinite(maximum) else None,
            "depth_classes": dict(zip(CLASSES, depth_hist)), "exact_black_pixels": exact_black,
            "dark_pixels": sum(dark_mask), "dark_depth_classes": dict(zip(CLASSES, dark_hist)),
            "dark_components": components(dark_mask, classes, width, height), "mask_file": mask_path.name}


def analyze(report_path, output, threshold=8):
    require(0 <= threshold <= 255, "dark threshold must be between 0 and 255")
    require(report_path.stat().st_size <= 1024 * 1024, "capture report oversized")
    report = json.loads(report_path.read_text())
    require(report.get("schema") == 2 and report.get("metal_command_status") == "completed",
            "requires schema-2 completed production command capture")
    output.mkdir(parents=False, exist_ok=False)
    result = {"scope": "CPU analysis of registered captured texels; dark candidates are not a sky-hole diagnosis.",
              "capture_report_sha256": hashlib.sha256(report_path.read_bytes()).hexdigest(),
              "dark_max_channel_8bit": threshold, "mask_legend_rgb": dict(zip(CLASSES, COLORS)),
              "compositor_acknowledged": report.get("compositor_acknowledged"),
              "game_frame": report.get("game_frame"), "request_frame": report.get("request_frame"),
              "source_view_index": report.get("source_view_index", report.get("view_index")),
              "output_view_index": report.get("output_view_index"),
              "rendering_method": report.get("rendering_method", "unspecified"),
              "limits": ["Different source/output viewpoints and crop sizes; no pixelwise subtraction.",
                         "PPM omits alpha; nonzero-alpha count is retained only in the capture report.",
                         "Completed Metal readback does not show later system compositor clipping.",
                         "Dark finite geometry, intentional black content and missing coverage need visual inspection."]}
    for kind in ("source", "output"):
        if kind + "_color" in report and kind + "_depth" in report:
            result[kind] = analyze_image(report, report_path.parent, kind, threshold, output)
    require("source" in result, "source color/depth capture missing")
    with (output / "analysis.json").open("x") as stream:
        json.dump(result, stream, indent=2, allow_nan=False)
        stream.write("\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="New directory; never overwrites earlier analysis")
    parser.add_argument("--dark-threshold", type=int, default=8)
    args = parser.parse_args()
    try:
        result = analyze(args.report, args.output, args.dark_threshold)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print("Capture analysis failed:", error, file=sys.stderr)
        return 2
    print(json.dumps({"analysis": str(args.output / "analysis.json"),
                      "source_dark_pixels": result["source"]["dark_pixels"],
                      "output_dark_pixels": result.get("output", {}).get("dark_pixels")}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
