//! Is cooling keeping up? Windows does not expose fan speed on most machines, but it
//! does report actual CPU speed vs base and thermal-zone limits. No admin needed.

// The verdict is only called by the Windows path but tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

#[derive(Default)]
pub struct Sample {
    /// Actual CPU performance, % of base speed (>100 = turbo).
    pub speed: Option<f32>,
    /// Why the CPU is held back, if it is.
    pub reason: Option<String>,
}

/// `passive`: lowest thermal-zone passive limit % (100 = none); `reasons`: OR of throttle bits.
// ponytail: fixed 80% / 50% thresholds; tune if laptops on battery saver trip it at rest.
fn verdict(speed: f32, cpu: f32, passive: Option<f64>, reasons: Option<u64>) -> Option<String> {
    if let Some(r) = reasons.filter(|&r| r != 0) {
        return Some(format!("thermal zone reports throttling (reasons {r:#x})"));
    }
    if let Some(p) = passive.filter(|&p| p < 100.0) {
        return Some(format!("thermal zone limits the CPU to {p:.0}%"));
    }
    (cpu > 50.0 && speed < 80.0).then(|| {
        format!("running at {speed:.0}% of base speed under {cpu:.0}% load: heat or power limit")
    })
}

#[cfg(not(windows))]
pub struct Throttle;

#[cfg(not(windows))]
impl Throttle {
    pub fn new() -> Result<Self, String> {
        Err("throttle counters need Windows".into())
    }
    pub fn sample(&mut self, _cpu: f32) -> Sample {
        Sample::default()
    }
}

#[cfg(windows)]
pub use imp::Throttle;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::gpu::imp::{read, wide};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::System::Performance::*;

    pub struct Throttle {
        query: PDH_HQUERY,
        perf: PDH_HCOUNTER,
        /// Thermal zones are optional: many machines have none.
        passive: Option<PDH_HCOUNTER>,
        reasons: Option<PDH_HCOUNTER>,
    }

    // The query is only ever touched from the sampler thread that owns it.
    unsafe impl Send for Throttle {}

    impl Throttle {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let mut query = null_mut();
                if PdhOpenQueryW(null(), 0, &mut query) != 0 {
                    return Err("PdhOpenQuery failed".into());
                }
                let add = |path: &str| {
                    let mut c = null_mut();
                    (PdhAddEnglishCounterW(query, wide(path).as_ptr(), 0, &mut c) == 0).then_some(c)
                };
                let Some(perf) = add(r"\Processor Information(_Total)\% Processor Performance")
                else {
                    PdhCloseQuery(query);
                    return Err("no processor performance counter".into());
                };
                let t = Throttle {
                    query,
                    perf,
                    passive: add(r"\Thermal Zone Information(*)\% Passive Limit"),
                    reasons: add(r"\Thermal Zone Information(*)\Throttle Reasons"),
                };
                // Performance is a rate: prime it so the first real sample has a baseline.
                PdhCollectQueryData(query);
                Ok(t)
            }
        }

        /// `cpu`: current machine CPU %, to tell "idle and slow" from "busy and held back".
        pub fn sample(&mut self, cpu: f32) -> Sample {
            unsafe { PdhCollectQueryData(self.query) };
            let all = |c: Option<PDH_HCOUNTER>, f| c.map(|c| read(c, f)).unwrap_or_default();
            let speed = read(self.perf, PDH_FMT_DOUBLE)
                .first()
                .map(|(_, v)| unsafe { v.doubleValue } as f32);
            let passive = all(self.passive, PDH_FMT_DOUBLE)
                .iter()
                .map(|(_, v)| unsafe { v.doubleValue })
                .reduce(f64::min);
            let reasons = all(self.reasons, PDH_FMT_LARGE)
                .iter()
                .map(|(_, v)| unsafe { v.largeValue } as u64)
                .reduce(|a, b| a | b);
            Sample {
                speed,
                reason: speed.and_then(|s| verdict(s, cpu, passive, reasons)),
            }
        }
    }

    impl Drop for Throttle {
        fn drop(&mut self) {
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_flags_only_real_holdbacks() {
        assert_eq!(verdict(40.0, 5.0, Some(100.0), Some(0)), None); // idle power saving
        assert_eq!(verdict(150.0, 90.0, None, None), None); // turbo under load
        assert!(
            verdict(60.0, 90.0, None, None)
                .unwrap()
                .contains("60% of base")
        );
        assert!(
            verdict(150.0, 10.0, Some(70.0), None)
                .unwrap()
                .contains("to 70%")
        );
        assert!(
            verdict(150.0, 10.0, Some(70.0), Some(2))
                .unwrap()
                .contains("0x2")
        );
    }

    #[cfg(windows)]
    #[test]
    fn live_counters_open_and_sample() {
        let mut t = Throttle::new().expect("processor performance counter");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(t.sample(0.0).speed.is_some_and(|s| s > 0.0));
    }
}
