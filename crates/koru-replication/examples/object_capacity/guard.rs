// SPDX-License-Identifier: MPL-2.0

//! Test-harness-only Win32 ABI. No production Job implementation is changed.
#[cfg(any(windows, test))]
pub const CAP: usize = 512 * 1024 * 1024;
#[cfg(any(windows, test))]
pub const FLAGS: u32 = 0x8 | 0x100 | 0x200 | 0x2000;

#[cfg(any(windows, test))]
#[repr(C)]
#[derive(Default)]
struct Basic {
    process_time: i64,
    job_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_processes: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}
#[cfg(any(windows, test))]
#[repr(C)]
#[derive(Default)]
struct Extended {
    basic: Basic,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

#[cfg(any(windows, test))]
fn valid(info: &Extended) -> bool {
    info.basic.flags == FLAGS
        && info.basic.active_processes == 1
        && info.process_memory == CAP
        && info.job_memory == CAP
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryInformationJobObject(
        job: *mut std::ffi::c_void,
        class: u32,
        info: *mut std::ffi::c_void,
        size: u32,
        returned: *mut u32,
    ) -> i32;
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn GetProcessTimes(
        process: *mut std::ffi::c_void,
        creation: *mut u64,
        exit: *mut u64,
        kernel: *mut u64,
        user: *mut u64,
    ) -> i32;
}

pub fn require_job() -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut info = Extended::default();
        let mut returned = 0;
        // NULL queries the immediate Job, including in a nested-job hierarchy.
        // The helper inherits NO Job handle: launcher death closes the sole handle.
        let ok = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                9,
                (&mut info as *mut Extended).cast(),
                std::mem::size_of::<Extended>() as u32,
                &mut returned,
            )
        };
        if ok == 0 || returned as usize != std::mem::size_of::<Extended>() || !valid(&info) {
            return Err(
                "required immediate hard-memory/one-process/kill-on-close Job absent".into(),
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    Err("this preliminary harness requires Windows; no unguarded fallback".into())
}

pub fn cpu_ns() -> Result<u64, String> {
    #[cfg(windows)]
    {
        let (mut creation, mut exit, mut kernel, mut user) = (0, 0, 0, 0);
        if unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err("GetProcessTimes".into());
        }
        Ok((kernel + user) * 100)
    }
    #[cfg(not(windows))]
    Err("Windows CPU clock required".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_guard_shape_refuses_missing_or_weakened_limits_without_os_calls() {
        let mut info = Extended::default();
        assert!(!valid(&info));
        info.basic.flags = FLAGS;
        info.basic.active_processes = 1;
        info.process_memory = CAP;
        info.job_memory = CAP;
        assert!(valid(&info));
        info.basic.flags |= 0x800; // BREAKAWAY_OK must not be allowed.
        assert!(!valid(&info));
        info.basic.flags = FLAGS;
        info.process_memory += 1;
        assert!(!valid(&info));
    }
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn win64_abi_sizes_are_explicit() {
        assert_eq!(std::mem::size_of::<Basic>(), 64);
        assert_eq!(std::mem::size_of::<Extended>(), 144);
    }
}
