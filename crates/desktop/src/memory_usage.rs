//! Read-only process measurements for the explicit editor resource fixture.
//! No working-set trimming: distinguish resident memory from private commit.
use std::mem::size_of;
use windows::{core::BOOL, Win32::{Foundation::HANDLE, System::Threading::GetCurrentProcess}};

// PROCESS_MEMORY_COUNTERS_EX2, psapi.h. EX2 requires a recent Windows 10/11;
// older systems can return the EX prefix without private-working-set data.
#[repr(C)]
#[derive(Default)]
struct Counters {
    cb: u32, faults: u32,
    peak_working_set: usize, working_set: usize,
    peak_paged_pool: usize, paged_pool: usize,
    peak_nonpaged_pool: usize, nonpaged_pool: usize,
    pagefile: usize, peak_pagefile: usize, private_usage: usize,
    private_working_set: usize, shared_commit: u64,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn K32GetProcessMemoryInfo(process: HANDLE, counters: *mut Counters, bytes: u32) -> BOOL;
}
pub fn snapshot() -> serde_json::Value {
    let mut counters = Counters { cb: size_of::<Counters>() as u32, ..Default::default() };
    let extended = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size_of::<Counters>() as u32) }.as_bool();
    if !extended {
        counters = Counters { cb: std::mem::offset_of!(Counters, private_working_set) as u32, ..Default::default() };
        let bytes = counters.cb;
        if !unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, bytes) }.as_bool() {
            return serde_json::json!({"available":false});
        }
    }
    serde_json::json!({"available":true,"units":"bytes",
        "working_set_bytes":counters.working_set,"private_commit_bytes":counters.private_usage,
        "private_working_set_bytes":extended.then_some(counters.private_working_set),
        "shared_commit_bytes":extended.then_some(counters.shared_commit),
        "scope":"Current process; resident/private commit are not retained-cache or GPU allocation totals"})
}
#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    #[test]
    fn process_counters_match_windows_x64_ex_and_ex2_layouts() {
        assert_eq!(std::mem::offset_of!(Counters, private_usage), 72);
        assert_eq!(std::mem::offset_of!(Counters, private_working_set), 80);
        assert_eq!(std::mem::offset_of!(Counters, shared_commit), 88);
        assert_eq!(size_of::<Counters>(), 96);
        let measured = snapshot();
        assert_eq!(measured["available"], true);
        assert!(measured["working_set_bytes"].as_u64().unwrap() > 0);
        assert!(measured["private_commit_bytes"].as_u64().unwrap() > 0);
    }
}
