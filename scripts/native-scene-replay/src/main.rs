use native_scene_replay::{execute, import, require, sha, validate::Input, Result};
use std::{fs, path::Path};
fn main() -> Result<()> {
    let a = std::env::args().collect::<Vec<_>>();
    if a.len() == 3 && a[1] == "make-float-depth" {
        native_scene_replay::float_depth_fixture::make(Path::new(&a[2]))?;
        return Ok(());
    }
    if a.len() == 3 && a[1] == "make-finished-ops" {
        native_scene_replay::finished_ops_fixture::make(Path::new(&a[2]))?;
        return Ok(());
    }
    if a.len() == 3 && a[1] == "make-depth-proxy" {
        native_scene_replay::depth_proxy_fixture::make(Path::new(&a[2]))?;
        return Ok(());
    }
    if a.len() == 3 && a[1] == "make-compute" {
        native_scene_replay::compute_fixture::make(Path::new(&a[2]))?;
        return Ok(());
    }
    if a.len() == 3 && a[1] == "make-volume" {
        native_scene_replay::volume_fixture::make(Path::new(&a[2]))?;
        return Ok(());
    }
    if a.len() == 3 && a[1] == "make-mixed" {
        native_scene_replay::fixture::make(Path::new(&a[2]))?;
        return Ok(());
    }
    require(
        a.len() == 4,
        "usage: native-scene-replay import-legacy|import-native|import-native-compute|import-native-graph|validate|run INPUT NEW_OUTPUT",
    )?;
    let input = Path::new(&a[2]);
    let out = Path::new(&a[3]);
    if a[1] == "import-native-graph" {
        native_scene_replay::graph_import::native(input, out)?;
        println!("Imported native graph with explicit provenance and strict CPU admission; no GPU parity or density claim");
        return Ok(());
    }
    if a[1] == "import-native-compute" {
        native_scene_replay::compute_import::native(input, out)?;
        println!("Imported isolated original compute dispatch; no finished-world or eye claim");
        return Ok(());
    }
    if a[1] == "import-native" {
        native_scene_replay::native_import::native(input, out)?;
        println!("Imported native complete target interval; original-density parity only");
        return Ok(());
    }
    if a[1] == "import-legacy" {
        import::legacy(input, out)?;
        println!("Imported explicit legacy parity-only scene");
        return Ok(());
    }
    let i = Input::load(input)?;
    fs::create_dir(out)?;
    let original = fs::read(input)?;
    require(
        sha(&original) == i.manifest_sha256,
        "manifest changed during admission",
    )?;
    fs::write(out.join("scene.json"), &original)?;
    if a[1] == "validate" {
        fs::write(
            out.join("admission.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"gpu_executed":false,"manifest_sha256":i.manifest_sha256,"provenance":i.scene.provenance,"targets_have_resolution_independent_initialization":i.targets_have_resolution_independent_initialization}),
            )?,
        )?;
        return Ok(());
    }
    require(a[1] == "run", "unknown mode")?;
    let result = execute::run(&i)?;
    for (n, b) in result.aspect_bytes.iter().enumerate() {
        fs::write(out.join(format!("aspect-{n}.bin")), b)?;
    }
    let mut report = result.report;
    report["binary_sha256"] = sha(&fs::read(std::env::current_exe()?)?).into();
    report["original_inputs_unchanged"] =
        serde_json::json!(Input::load(input)?.manifest_sha256 == i.manifest_sha256);
    fs::write(out.join("result.json"), serde_json::to_vec_pretty(&report)?)?;
    require(
        report["all_expected_bytes_equal"] == true,
        "original final output differs",
    )?;
    println!("Generic ordered replay: all original expected bytes match");
    Ok(())
}
