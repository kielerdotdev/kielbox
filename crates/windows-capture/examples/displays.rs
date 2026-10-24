//! Read-only monitor inventory: does not create D3D devices or start capture.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let rows = rcr_windows::display::enumerate()?;
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Display discovery is available on Windows.");
}
