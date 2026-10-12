//! Session profiler: accumulates load per program name so the heaviest apps
//! over minutes/hours stand out, not just whoever spiked in the last second.

use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct Entry {
    /// Core-seconds of CPU consumed (1.0 = one core busy for one second).
    pub cpu_secs: f64,
    pub peak_cpu: f32,
    /// Recording time (s) at which `peak_cpu` occurred.
    pub peak_at: f64,
    pub peak_mem: u64,
    pub io_bytes: f64,
    /// Bytes sent / received on the network (ETW; 0 when unavailable).
    pub net_tx: f64,
    pub net_rx: f64,
    /// GPU %·seconds (busiest engine per process, instances summed); / elapsed = avg %.
    pub gpu_secs: f64,
    pub peak_gpu: f32,
    pub peak_gpu_mem: u64,
    pub pids: HashSet<u32>,
}

#[derive(Default)]
pub struct Profile {
    pub paused: bool,
    /// Seconds of recorded wall time.
    pub elapsed: f64,
    pub cores: usize,
    /// True once any sample carried per-process network data.
    pub has_net: bool,
    pub by_name: HashMap<String, Entry>,
}

/// One process as seen in one sample: (name, pid, machine-normalized cpu %, mem bytes,
/// io bytes/s, net (tx, rx) bytes/s if known, gpu %, gpu memory bytes).
pub type Sample<'a> = (&'a str, u32, f32, u64, f64, Option<(f64, f64)>, f32, u64);

impl Profile {
    pub fn record<'a>(&mut self, secs: f64, cores: usize, procs: impl Iterator<Item = Sample<'a>>) {
        if self.paused {
            return;
        }
        self.cores = cores;
        // Sum across all instances of a program within this sample.
        #[derive(Default)]
        struct Sum {
            cpu: f32,
            mem: u64,
            io: f64,
            tx: f64,
            rx: f64,
            gpu: f32,
            gpu_mem: u64,
            pids: Vec<u32>,
        }
        let mut now: HashMap<&str, Sum> = HashMap::new();
        for (name, pid, cpu, mem, io, net, gpu, gpu_mem) in procs {
            let e = now.entry(name).or_default();
            e.cpu += cpu;
            e.mem += mem;
            e.io += io;
            e.gpu += gpu;
            e.gpu_mem += gpu_mem;
            if let Some((tx, rx)) = net {
                self.has_net = true;
                e.tx += tx;
                e.rx += rx;
            }
            e.pids.push(pid);
        }
        for (
            name,
            Sum {
                cpu,
                mem,
                io,
                tx,
                rx,
                gpu,
                gpu_mem,
                pids,
            },
        ) in now
        {
            let e = self.by_name.entry(name.to_string()).or_default();
            e.cpu_secs += cpu as f64 / 100.0 * cores as f64 * secs;
            if cpu > e.peak_cpu {
                e.peak_cpu = cpu;
                e.peak_at = self.elapsed;
            }
            e.peak_mem = e.peak_mem.max(mem);
            e.io_bytes += io * secs;
            e.net_tx += tx * secs;
            e.net_rx += rx * secs;
            e.gpu_secs += gpu as f64 * secs;
            e.peak_gpu = e.peak_gpu.max(gpu.min(100.0));
            e.peak_gpu_mem = e.peak_gpu_mem.max(gpu_mem);
            e.pids.extend(pids);
        }
        self.elapsed += secs;
    }

    /// Average machine CPU % this program used over the recording.
    pub fn avg_cpu(&self, e: &Entry) -> f64 {
        if self.elapsed == 0.0 || self.cores == 0 {
            return 0.0;
        }
        e.cpu_secs / (self.elapsed * self.cores as f64) * 100.0
    }

    /// Average GPU % (busiest engine) this program used over the recording.
    pub fn avg_gpu(&self, e: &Entry) -> f64 {
        if self.elapsed == 0.0 {
            return 0.0;
        }
        e.gpu_secs / self.elapsed
    }

    pub fn reset(&mut self) {
        *self = Profile {
            paused: self.paused,
            ..Default::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_name_and_accumulates() {
        let mut p = Profile::default();
        // 4 cores, two chrome processes at 25% + 25% of machine for 2 s = 2 cores busy = 4 core-s.
        p.record(
            2.0,
            4,
            [
                ("chrome", 1, 25.0, 100, 10.0, Some((5.0, 1.0)), 30.0, 1000),
                ("chrome", 2, 25.0, 50, 0.0, Some((0.0, 2.0)), 10.0, 500),
            ]
            .into_iter(),
        );
        p.record(
            2.0,
            4,
            [("chrome", 3, 0.0, 400, 0.0, None, 0.0, 0)].into_iter(),
        );
        let c = &p.by_name["chrome"];
        assert_eq!(c.cpu_secs, 4.0);
        assert_eq!(c.peak_cpu, 50.0);
        assert_eq!(c.peak_at, 0.0);
        assert_eq!(c.peak_mem, 400);
        assert_eq!(c.io_bytes, 20.0);
        assert_eq!((c.net_tx, c.net_rx), (10.0, 6.0));
        assert!(p.has_net);
        assert_eq!(c.pids.len(), 3);
        assert_eq!(p.elapsed, 4.0);
        assert_eq!(p.avg_cpu(c), 25.0); // 4 core-s / (4 s * 4 cores)
        assert_eq!((c.peak_gpu, c.peak_gpu_mem), (40.0, 1500));
        assert_eq!(p.avg_gpu(c), 20.0); // 40% for 2 s, 0% for 2 s
    }

    #[test]
    fn paused_records_nothing() {
        let mut p = Profile {
            paused: true,
            ..Default::default()
        };
        p.record(1.0, 1, [("x", 1, 50.0, 1, 1.0, None, 0.0, 0)].into_iter());
        assert!(p.by_name.is_empty());
        p.reset();
        assert!(p.paused);
    }
}
