//! What the operating system says this process holds, to check the ear's own accounting against:
//! the Mach call on macOS, procfs on Linux.

#[derive(Clone, Copy, Debug, Default)]
pub struct Mem {
    /// macOS `phys_footprint` (Activity Monitor's figure); Linux `RssAnon`, which only swapping
    /// reclaims and so is what competes with the LLM on the Orin.
    pub footprint: u64,
    /// Resident set size, file-backed pages included.
    pub resident: u64,
}

#[cfg(target_os = "macos")]
pub fn now() -> Option<Mem> {
    // <mach/task_info.h>: struct task_vm_info, flavor TASK_VM_INFO (22).
    #[repr(C)]
    #[derive(Default)]
    struct TaskVmInfo {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }
    extern "C" {
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }
    let mut info = TaskVmInfo::default();
    let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
    // SAFETY: `info` is a repr(C) mirror of the kernel struct up to phys_footprint (rev 1), and
    // `count` tells the kernel how many 32-bit words it may write.
    let kr = unsafe {
        task_info(
            mach_task_self_,
            22,
            &mut info as *mut TaskVmInfo as *mut i32,
            &mut count,
        )
    };
    (kr == 0).then_some(Mem {
        footprint: info.phys_footprint,
        resident: info.resident_size,
    })
}

#[cfg(target_os = "linux")]
pub fn now() -> Option<Mem> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb = |key: &str| {
        s.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| v * 1024)
    };
    Some(Mem {
        footprint: kb("RssAnon:")?,
        resident: kb("VmRSS:")?,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn now() -> Option<Mem> {
    None
}

/// Hand freed-but-kept memory back to the OS, so a before/after measurement sees what a load costs
/// rather than recycled pages.
pub fn release() {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut u8, goal: usize) -> usize;
        }
        // SAFETY: a null zone means every zone; a goal of 0 means as much as possible.
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: glibc's malloc_trim takes the padding to keep and touches nothing of ours.
        unsafe { malloc_trim(0) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_touched_allocation_shows_up() {
        let before = super::now().expect("supported OS").footprint;
        let mut v = vec![0u8; 64 << 20];
        for i in (0..v.len()).step_by(4096) {
            v[i] = 1;
        }
        let after = super::now().unwrap().footprint;
        std::hint::black_box(&v);
        let grew = after.saturating_sub(before) as f64 / (64u64 << 20) as f64;
        assert!(
            grew > 0.8,
            "64 MB touched, footprint grew by {:.0}%",
            grew * 100.0
        );
    }
}
