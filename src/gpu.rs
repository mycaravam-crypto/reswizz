//! GPU load from the Windows "GPU Engine" / "GPU Process Memory" performance counters,
//! the same source Task Manager uses. No admin needed.

// Parsers are only called by the Windows path but tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashMap;

#[derive(Default)]
pub struct Sample {
    /// Busiest engine across all adapters, % (Task Manager's "GPU" number).
    pub util: f32,
    /// GPU memory in use (dedicated + shared), bytes.
    pub mem: u64,
    /// pid -> (util % of its busiest engine, gpu memory bytes).
    pub procs: HashMap<u32, (f32, u64)>,
}

/// "pid_1234_luid_0x0_0x1_phys_0_eng_3_engtype_3D" -> (1234, "luid_0x0_0x1_phys_0_eng_3").
fn split(inst: &str) -> Option<(u32, &str)> {
    let rest = inst.strip_prefix("pid_")?;
    let (pid, rest) = rest.split_once('_')?;
    let engine = rest.split("_engtype_").next()?;
    Some((pid.parse().ok()?, engine))
}

/// Engine utilization per instance -> (machine util, per-pid util).
/// An engine's load is the sum of its processes; the GPU number is the busiest engine.
fn engines<'a>(items: impl Iterator<Item = (&'a str, f64)>) -> (f32, HashMap<u32, f32>) {
    let mut per_engine: HashMap<&str, f64> = HashMap::new();
    let mut per_pid: HashMap<u32, f32> = HashMap::new();
    for (inst, v) in items {
        let Some((pid, eng)) = split(inst) else {
            continue;
        };
        *per_engine.entry(eng).or_default() += v;
        let p = per_pid.entry(pid).or_default();
        *p = p.max(v as f32);
    }
    let total = per_engine
        .values()
        .fold(0.0_f64, |a, &b| a.max(b))
        .min(100.0);
    (total as f32, per_pid)
}

#[cfg(not(windows))]
pub struct Gpu;

#[cfg(not(windows))]
impl Gpu {
    pub fn new() -> Result<Self, String> {
        Err("GPU counters need Windows".into())
    }
    pub fn sample(&mut self) -> Sample {
        Sample::default()
    }
}

#[cfg(windows)]
pub use imp::Gpu;

#[cfg(windows)]
pub(crate) mod imp {
    use super::*;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::System::Performance::*;

    pub struct Gpu {
        query: PDH_HQUERY,
        engine: PDH_HCOUNTER,
        /// Dedicated + shared: integrated GPUs (Intel Iris etc.) only use shared memory.
        proc_mem: [PDH_HCOUNTER; 2],
        adapter_mem: [PDH_HCOUNTER; 2],
    }

    // The query is only ever touched from the sampler thread that owns it.
    unsafe impl Send for Gpu {}

    pub(crate) fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    impl Gpu {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let mut query = null_mut();
                if PdhOpenQueryW(null(), 0, &mut query) != 0 {
                    return Err("PdhOpenQuery failed".into());
                }
                let add = |path: &str| {
                    let mut c = null_mut();
                    match PdhAddEnglishCounterW(query, wide(path).as_ptr(), 0, &mut c) {
                        0 => Ok(c),
                        e => Err(format!("no GPU counters ({path}: {e:#x})")),
                    }
                };
                let g = (|| {
                    Ok(Gpu {
                        query,
                        engine: add(r"\GPU Engine(*)\Utilization Percentage")?,
                        proc_mem: [
                            add(r"\GPU Process Memory(*)\Dedicated Usage")?,
                            add(r"\GPU Process Memory(*)\Shared Usage")?,
                        ],
                        adapter_mem: [
                            add(r"\GPU Adapter Memory(*)\Dedicated Usage")?,
                            add(r"\GPU Adapter Memory(*)\Shared Usage")?,
                        ],
                    })
                })();
                if g.is_err() {
                    PdhCloseQuery(query);
                } else {
                    // Utilization is a rate: prime it so the first real sample has a baseline.
                    PdhCollectQueryData(query);
                }
                g
            }
        }

        /// Call once per sampler tick; utilization covers the time since the previous call.
        pub fn sample(&mut self) -> Sample {
            unsafe { PdhCollectQueryData(self.query) };
            let (util, per_pid) = engines(
                read(self.engine, PDH_FMT_DOUBLE)
                    .iter()
                    .map(|(n, v)| (n.as_str(), unsafe { v.doubleValue })),
            );
            let mut procs: HashMap<u32, (f32, u64)> =
                per_pid.into_iter().map(|(p, u)| (p, (u, 0))).collect();
            for (n, v) in self.proc_mem.iter().flat_map(|&c| read(c, PDH_FMT_LARGE)) {
                if let Some((pid, _)) = split(&n) {
                    procs.entry(pid).or_default().1 += unsafe { v.largeValue }.max(0) as u64;
                }
            }
            let mem = self
                .adapter_mem
                .iter()
                .flat_map(|&c| read(c, PDH_FMT_LARGE))
                .map(|(_, v)| unsafe { v.largeValue }.max(0) as u64)
                .sum();
            Sample { util, mem, procs }
        }
    }

    impl Drop for Gpu {
        fn drop(&mut self) {
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    /// All valid (instance name, value) pairs of a wildcard counter.
    pub(crate) fn read(
        counter: PDH_HCOUNTER,
        fmt: PDH_FMT,
    ) -> Vec<(String, PDH_FMT_COUNTERVALUE_0)> {
        unsafe {
            let (mut size, mut count) = (0u32, 0u32);
            if PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count, null_mut())
                != PDH_MORE_DATA
            {
                return Vec::new();
            }
            // u64 backing keeps the item array 8-byte aligned.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count, items) != 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts(items, count as usize)
                .iter()
                .filter(|i| {
                    matches!(
                        i.FmtValue.CStatus,
                        PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
                    )
                })
                .map(|i| {
                    let len = (0..).take_while(|&k| *i.szName.add(k) != 0).count();
                    let name = String::from_utf16_lossy(std::slice::from_raw_parts(i.szName, len));
                    (name, i.FmtValue.Anonymous)
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_instance_names() {
        assert_eq!(
            split("pid_1234_luid_0x00000000_0x0000D1B2_phys_0_eng_3_engtype_3D"),
            Some((1234, "luid_0x00000000_0x0000D1B2_phys_0_eng_3"))
        );
        assert_eq!(
            split("pid_9_luid_0x0_0x1_phys_0"),
            Some((9, "luid_0x0_0x1_phys_0"))
        );
        assert_eq!(split("luid_0x0_0x1_phys_0"), None);
    }

    #[test]
    fn engine_load_sums_processes_gpu_is_busiest_engine() {
        let (total, pids) = engines(
            [
                ("pid_1_luid_a_phys_0_eng_0_engtype_3D", 30.0),
                ("pid_2_luid_a_phys_0_eng_0_engtype_3D", 25.0),
                ("pid_1_luid_a_phys_0_eng_1_engtype_Copy", 40.0),
                ("garbage", 99.0),
            ]
            .into_iter(),
        );
        assert_eq!(total, 55.0); // engine 0: 30 + 25
        assert_eq!(pids[&1], 40.0); // its busiest engine
        assert_eq!(pids[&2], 25.0);
    }

    #[cfg(windows)]
    #[test]
    fn live_counters_open_and_sample() {
        // CI runners have no GPU, so no counters: nothing to check there.
        let Ok(mut g) = Gpu::new() else { return };
        std::thread::sleep(std::time::Duration::from_millis(300));
        let s = g.sample();
        assert!((0.0..=100.0).contains(&s.util));
        if !s.procs.is_empty() {
            assert!(s.mem > 0, "processes on the GPU hold gpu memory");
        }
    }
}
