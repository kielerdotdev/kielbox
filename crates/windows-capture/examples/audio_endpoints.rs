//! Explicit read-only discovery: never activates a client or reads audio samples.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use rcr_windows::audio_endpoint::{enumerate_audio_endpoints, AudioEndpointKind};
    use serde_json::json;
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.iter().any(|arg| arg == "--list")
            && args
                .iter()
                .all(|arg| arg == "--list" || arg == "--include-ids"),
        "Use --list [--include-ids] for read-only endpoint discovery; no audio stream is opened"
    );
    let include_ids = args.iter().any(|arg| arg == "--include-ids");
    let mut lists = Vec::new();
    for kind in [AudioEndpointKind::Render, AudioEndpointKind::Capture] {
        let result = enumerate_audio_endpoints(kind)?;
        let endpoints: Vec<_> = result
            .endpoints
            .into_iter()
            .map(|endpoint| {
                let mut item = json!({"name":endpoint.name,"is_default":endpoint.is_default});
                if include_ids {
                    item["id"] = json!(endpoint.id);
                }
                item
            })
            .collect();
        lists.push(json!({"kind":match kind{AudioEndpointKind::Render=>"render",AudioEndpointKind::Capture=>"capture"},"endpoints":endpoints,"truncated":result.truncated,"unavailable_entries":result.unavailable_entries}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"proof":"read_only_audio_endpoint_enumeration","read_only":true,"streams_activated":0,"captured_samples":0,"ids_included":include_ids,"lists":lists})
        )?
    );
    Ok(())
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Windows MMDevice endpoint enumeration only")
}
