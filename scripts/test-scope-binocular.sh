#!/bin/sh
# Scoped binocular registration (CPU only). Extracts the native
# ScopedEyeBaseline block verbatim, runs its standalone rustc tests and emits a
# bit-exact fixture, then compiles the production pure Swift optics/HUD policy
# and checks the complete source -> reprojection -> display chain against the
# collimated graticule with the captured physical eye calibration. No GPU,
# device, simulator, AOT or crate build.
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/opus22-sniper/binocular}
packet=${2:-target/verification/device-session-20261024/scope-optics-design/physical-request.bin}
tools=${SV_RUST_TOOLCHAIN:-.tools/rustup/toolchains/1.98.1-aarch64-apple-darwin}
mkdir -p "$out/build"
awk '/^\/\/ BEGIN ScopedEyeBaseline/{on=1} on{print} /^\/\/ END ScopedEyeBaseline/{on=0}' src/xr_view.rs \
    > "$out/build/scoped_eye_baseline.rs"
grep -q 'pub(crate) fn scoped_head_from_eye' "$out/build/scoped_eye_baseline.rs"
cat > "$out/build/scoped_eye_harness.rs" <<'RS'
#![allow(dead_code)]
include!("scoped_eye_baseline.rs");
fn packet_eyes(path: &str) -> [[f32; 16]; 2] {
    let bytes = std::fs::read(path).expect("captured 288-byte request");
    assert_eq!(bytes.len(), 288);
    let matrix = |offset: usize| std::array::from_fn(|i| f32::from_le_bytes(bytes[offset + i * 4..offset + i * 4 + 4].try_into().unwrap()));
    [matrix(32), matrix(160)]
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let eyes = packet_eyes(&args[1]);
    // Bit fixture consumed by the Swift validation. m uses the native
    // OpticalAuthority::optics formula for the qualified sniper FOVs.
    let mut rows = Vec::new();
    for fov in [45.0f32, 15., 7.5, 5.] {
        let m = ((f64::from(45.0f32).to_radians() * 0.5).tan() / (f64::from(fov).to_radians() * 0.5).tan()) as f32;
        for eye in 0..2 {
            let v = scoped_head_from_eye(eyes, eye, m).unwrap();
            rows.push(format!("{} {} {} {}", fov.to_bits(), m.to_bits(), eye, v.iter().map(|x| x.to_bits().to_string()).collect::<Vec<_>>().join(" ")));
        }
    }
    println!("{}", rows.join("\n"));
}
#[cfg(test)]
mod tests {
    use super::*;
    fn eyes() -> [[f32; 16]; 2] {
        let mut l = [0f32; 16]; let mut r = [0f32; 16];
        for i in [0, 5, 10, 15] { l[i] = 1.; r[i] = 1.; }
        l[12] = -0.03425; l[13] = -0.02519; l[14] = -0.0358; r[12] = 0.03373; r[13] = -0.02566; r[14] = -0.03464;
        l[1] = 0.0007; r[6] = -0.0004;
        [l, r]
    }
    #[test] fn identity_is_bit_exact() {
        let e = eyes();
        for eye in 0..2 { assert!(scoped_head_from_eye(e, eye, 1.).unwrap().iter().zip(e[eye]).all(|(a, b)| a.to_bits() == b.to_bits())); }
    }
    #[test] fn only_baseline_scales_about_midpoint() {
        let e = eyes();
        for m in [1.0001f32, 2., 6.3196, 9.48705, 10.] {
            let l = scoped_head_from_eye(e, 0, m).unwrap(); let r = scoped_head_from_eye(e, 1, m).unwrap();
            for i in (0..12).chain([15]) { assert_eq!(l[i].to_bits(), e[0][i].to_bits()); assert_eq!(r[i].to_bits(), e[1][i].to_bits()); }
            for a in 12..15 {
                assert!(((l[a] + r[a]) - (e[0][a] + e[1][a])).abs() <= 2e-7);
                assert!(((r[a] - l[a]) * m - (e[1][a] - e[0][a])).abs() <= 2e-6);
            }
        }
    }
    #[test] fn malformed_inputs_fail_closed() {
        let mut e = eyes();
        for m in [0.0f32, 0.999, 10.001, f32::NAN, f32::INFINITY] { assert!(scoped_head_from_eye(e, 0, m).is_none()); }
        assert!(scoped_head_from_eye(e, 2, 2.).is_none());
        e[1][13] = f32::NAN; assert!(scoped_head_from_eye(e, 0, 2.).is_none());
    }
}
RS
"$tools/bin/rustc" --edition 2021 --test -O "$out/build/scoped_eye_harness.rs" -o "$out/build/scoped-eye-tests"
"$out/build/scoped-eye-tests" --test-threads=1 > "$out/rust-standalone-tests.txt"
"$tools/bin/rustc" --edition 2021 -O "$out/build/scoped_eye_harness.rs" -o "$out/build/scoped-eye-fixture"
"$out/build/scoped-eye-fixture" "$packet" > "$out/rust-baseline-fixture.txt"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/GameWorldDepthRange.swift visionos/Sources/GameWorldProjection.swift \
    visionos/Sources/GameWorldSourceGuardBand.swift visionos/Sources/GameWorldScopeOptics.swift \
    visionos/Sources/GameWorldHUD.swift visionos/Tests/ScopeBinocularValidation.swift \
    -o "$out/build/scope-binocular-validation"
"$out/build/scope-binocular-validation" "$packet" "$out/rust-baseline-fixture.txt" > "$out/binocular-validation.txt"
cat "$out/rust-standalone-tests.txt" "$out/binocular-validation.txt"
