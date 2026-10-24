//! Run from an administrator terminal for a whole-volume NTFS measurement.
use rcr_toolkit::{core::workflow::CancelToken, disk_usage};
use std::{env, path::PathBuf};

fn main() -> std::io::Result<()> {
    let mut args = env::args_os().skip(1);
    let root = args
        .next()
        .map(PathBuf::from)
        .expect("Usage: disk_scan_bench <root> [--directory]");
    let directory = args.next().is_some_and(|arg| arg == "--directory");
    let cancel = CancelToken::default();
    let result = if directory {
        disk_usage::scan_directories(root, &cancel, |_| {})?
    } else {
        disk_usage::scan(root, &cancel, |_| {})?
    };
    let root = &result.nodes[0];
    println!(
        "method={} elapsed={:.3}s bytes={} files={} folders={} skipped={} errors={}",
        result.method.label(),
        result.elapsed.as_secs_f64(),
        root.bytes,
        root.files,
        root.directories,
        root.skipped,
        root.errors
    );
    if let Some(reason) = result.fallback_reason {
        println!("fallback={reason}");
    }
    Ok(())
}
