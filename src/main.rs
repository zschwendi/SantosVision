// The desktop/iOS launcher and native visionOS shell share the same runtime.
#[cfg(not(target_os = "visionos"))]
fn main() -> anyhow::Result<()> { muguet::desktop_main() }

#[cfg(target_os = "visionos")]
fn main() -> anyhow::Result<()> { anyhow::bail!("visionOS uses the Swift CompositorServices application and libmuguet.a") }
