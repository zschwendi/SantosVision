// muguet - build.rs
// Constantin Clerc (c22dev) - 0.1

use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(game_code)");
    println!("cargo:rerun-if-env-changed=MUGUET_GAME_CODE");
    println!("cargo:rerun-if-env-changed=MUGUET_MOTION_AOT_SHA256");
    if std::env::var("MUGUET_GAME_CODE").is_ok_and(|p| std::path::Path::new(&p).is_file()) {
        println!("cargo:rustc-cfg=game_code");
    }
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "visionos" {
        // Static archives cannot set the final Swift executable segment protections.
        // make-visionos.sh passes -Wl,-segprot,__GAME,rx,rx at the final link.
        return;
    }
    if target_os != "ios" { return; }
    println!("cargo:rustc-link-arg-bins=-Wl,-segprot,__GAME,rx,rx");

    println!("cargo:rerun-if-changed=ios/Launcher.swift");
    let out = std::env::var("OUT_DIR").unwrap();
    let xcrun = |args: &[&str]| {
        let o = Command::new("xcrun").args(args).output().expect("xcrun");
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    };
    let sdk = xcrun(&["--sdk", "iphoneos", "--show-sdk-path"]);
    let swiftc = xcrun(&["--sdk", "iphoneos", "-f", "swiftc"]);
    let status = Command::new(&swiftc)
        .args(["-parse-as-library", "-emit-library", "-static", "-O", "-module-name", "MuguetLauncher"])
        .args(["-target", "arm64-apple-ios15.0", "-sdk", &sdk])
        .arg("ios/Launcher.swift")
        .arg("-o")
        .arg(format!("{out}/libmuguetlauncher.a"))
        .status()
        .expect("swiftc");
    assert!(status.success(), "swiftc failed");
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=muguetlauncher");

    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    let toolchain = std::path::Path::new(&swiftc).parent().unwrap().parent().unwrap().join("lib/swift/iphoneos");
    println!("cargo:rustc-link-search=native={}", toolchain.display());
}
