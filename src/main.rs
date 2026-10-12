#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod etw;
mod gpu;
mod history;
mod inspect;
mod net;
mod profile;
mod ui;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use eframe::egui;
use history::History;
use profile::Profile;
use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const MIN_INTERVAL_MS: u64 = 500;
const MAX_INTERVAL_MS: u64 = 5000;
const HISTORY_SECS: u64 = 60;

pub struct Proc {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    /// Percent of the whole machine (all logical cores = 100%).
    pub cpu: f32,
    pub mem: u64,
    /// Bytes/s of all process I/O (disk, network, devices), as Windows counts it.
    pub io: f64,
    /// Network (sent, received) bytes/s from ETW; `None` when tracing is unavailable.
    pub net: Option<(f64, f64)>,
    pub threads: Option<u32>,
    /// GPU % of the process's busiest engine (0 when counters are unavailable).
    pub gpu: f32,
    /// GPU memory (dedicated + shared), bytes.
    pub gpu_mem: u64,
    pub start: u64,
    pub status: String,
    pub exe: Option<String>,
    pub cmd: Option<String>,
}

pub struct Snapshot {
    /// Wall time this sample covers.
    pub secs: f64,
    pub cpu: f32,
    pub cores: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub disk_read: f64,
    pub disk_write: f64,
    pub net_rx: f64,
    pub net_tx: f64,
    pub adapters: Vec<(String, f64, f64)>,
    /// Busiest GPU engine, %.
    pub gpu: f32,
    pub gpu_mem: u64,
    /// Why GPU counters are unavailable, if they are.
    pub gpu_err: Option<String>,
    pub procs: Vec<Proc>,
    pub endpoints: Result<Vec<net::Endpoint>, String>,
    /// Why per-process network is unavailable, if it is.
    pub net_trace_err: Option<String>,
    /// Deep details for the selected pid.
    pub inspect: Option<(u32, inspect::Inspect)>,
}

/// sysinfo reports per-process CPU per core (up to cores × 100%); scale to whole machine.
fn normalize_cpu(per_core_pct: f32, cores: usize) -> f32 {
    per_core_pct / cores.max(1) as f32
}

/// PID + start time identifies a process; a reused PID has a different start time.
pub fn find(procs: &[Proc], id: (u32, u64)) -> Option<&Proc> {
    procs.iter().find(|p| (p.pid, p.start) == id)
}

/// `selected`: pid to inspect in depth, 0 = none.
fn spawn_sampler(
    ctx: egui::Context,
    interval_ms: Arc<AtomicU64>,
    selected: Arc<AtomicU32>,
) -> Receiver<Snapshot> {
    // Capacity 1: if the UI is behind, the newest snapshot is skipped rather than queued.
    let (tx, rx) = sync_channel(1);
    std::thread::spawn(move || {
        let mut sys = System::new();
        let mut disks = Disks::new_with_refreshed_list();
        let mut nets = Networks::new_with_refreshed_list();
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_disk_usage()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet);
        let net_trace_err = etw::start().err();
        let (mut gpu, gpu_err) = match gpu::Gpu::new() {
            Ok(g) => (Some(g), None),
            Err(e) => (None, Some(e)),
        };
        let mut prev_raw: Option<(u32, inspect::Raw)> = None;
        sys.refresh_cpu_usage();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        let mut last = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(interval_ms.load(Ordering::Relaxed)));
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
            disks.refresh(true);
            nets.refresh(true);
            let secs = last.elapsed().as_secs_f64();
            last = Instant::now();

            let cores = sys.cpus().len();
            let net = etw::take();
            let threads = inspect::thread_counts();
            let g = gpu.as_mut().map(|g| g.sample()).unwrap_or_default();
            let procs = sys
                .processes()
                .values()
                .map(|p| {
                    let d = p.disk_usage();
                    let cmd = p
                        .cmd()
                        .iter()
                        .map(|s| s.to_string_lossy())
                        .collect::<Vec<_>>();
                    let pid = p.pid().as_u32();
                    let (gpu, gpu_mem) = g.procs.get(&pid).copied().unwrap_or_default();
                    Proc {
                        pid,
                        net: net_trace_err.is_none().then(|| {
                            let (tx, rx) = net.get(&pid).copied().unwrap_or_default();
                            (tx as f64 / secs, rx as f64 / secs)
                        }),
                        threads: threads.get(&pid).copied(),
                        gpu,
                        gpu_mem,
                        ppid: p.parent().map(|x| x.as_u32()),
                        name: p.name().to_string_lossy().into_owned(),
                        cpu: normalize_cpu(p.cpu_usage(), cores),
                        mem: p.memory(),
                        io: (d.read_bytes + d.written_bytes) as f64 / secs,
                        start: p.start_time(),
                        status: p.status().to_string(),
                        exe: p.exe().map(|e| e.display().to_string()),
                        cmd: (!cmd.is_empty()).then(|| cmd.join(" ")),
                    }
                })
                .collect();
            let (r, w) = disks.list().iter().fold((0, 0), |(r, w), d| {
                (r + d.usage().read_bytes, w + d.usage().written_bytes)
            });
            let adapters: Vec<_> = nets
                .iter()
                .map(|(n, d)| {
                    (
                        n.clone(),
                        d.received() as f64 / secs,
                        d.transmitted() as f64 / secs,
                    )
                })
                .collect();
            let snap = Snapshot {
                secs,
                cpu: sys.global_cpu_usage(),
                cores: sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
                mem_used: sys.used_memory(),
                mem_total: sys.total_memory(),
                swap_used: sys.used_swap(),
                swap_total: sys.total_swap(),
                disk_read: r as f64 / secs,
                disk_write: w as f64 / secs,
                net_rx: adapters.iter().map(|a| a.1).sum(),
                net_tx: adapters.iter().map(|a| a.2).sum(),
                adapters,
                gpu: g.util,
                gpu_mem: g.mem,
                gpu_err: gpu_err.clone(),
                procs,
                endpoints: net::endpoints(),
                net_trace_err: net_trace_err.clone(),
                inspect: match selected.load(Ordering::Relaxed) {
                    0 => None,
                    pid => {
                        let raw = inspect::raw(pid);
                        let prev = prev_raw.as_ref().filter(|p| p.0 == pid).map(|p| &p.1);
                        let i =
                            inspect::derive(prev, raw.clone(), secs, cores, inspect::services(pid));
                        prev_raw = Some((pid, raw));
                        Some((pid, i))
                    }
                },
            };
            match tx.try_send(snap) {
                Err(TrySendError::Disconnected(_)) => return,
                _ => ctx.request_repaint(),
            }
        }
    });
    rx
}

#[derive(PartialEq, Clone, Copy)]
pub enum View {
    Profile,
    Processes,
    Network,
}

pub struct Histories {
    pub cpu: History,
    pub mem: History,
    pub disk_r: History,
    pub disk_w: History,
    pub net_rx: History,
    pub net_tx: History,
    pub gpu: History,
    /// Selected process (cleared when the selection changes).
    pub sel_cpu: History,
    pub sel_mem: History,
    pub sel_io: History,
    pub sel_net: History,
    pub sel_gpu: History,
}

pub struct App {
    rx: Receiver<Snapshot>,
    pub interval_ms: Arc<AtomicU64>,
    inspect_pid: Arc<AtomicU32>,
    pub snap: Option<Snapshot>,
    started: Instant,
    pub hist: Histories,
    pub profile: Profile,
    pub view: View,
    pub filter: String,
    /// (column index, descending) per view.
    pub sort: [(usize, bool); 3],
    pub selected: Option<(u32, u64)>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        ui::setup_style(&cc.egui_ctx);
        let interval_ms = Arc::new(AtomicU64::new(1000));
        let inspect_pid = Arc::new(AtomicU32::new(0));
        let cap = (HISTORY_SECS * 1000 / MIN_INTERVAL_MS) as usize;
        Self {
            rx: spawn_sampler(
                cc.egui_ctx.clone(),
                interval_ms.clone(),
                inspect_pid.clone(),
            ),
            interval_ms,
            inspect_pid,
            snap: None,
            started: Instant::now(),
            hist: Histories {
                cpu: History::new(cap),
                mem: History::new(cap),
                disk_r: History::new(cap),
                disk_w: History::new(cap),
                net_rx: History::new(cap),
                net_tx: History::new(cap),
                gpu: History::new(cap),
                sel_cpu: History::new(cap),
                sel_mem: History::new(cap),
                sel_io: History::new(cap),
                sel_net: History::new(cap),
                sel_gpu: History::new(cap),
            },
            profile: Profile::default(),
            view: View::Profile,
            filter: String::new(),
            // indexed by View: profile by cpu time, processes by cpu%, network by proto
            sort: [(2, true), (2, true), (0, false)],
            selected: None,
        }
    }

    pub fn select(&mut self, id: Option<(u32, u64)>) {
        if id != self.selected {
            let h = &mut self.hist;
            for x in [
                &mut h.sel_cpu,
                &mut h.sel_mem,
                &mut h.sel_io,
                &mut h.sel_net,
                &mut h.sel_gpu,
            ] {
                x.samples.clear();
            }
        }
        self.selected = id;
        self.inspect_pid
            .store(id.map_or(0, |i| i.0), Ordering::Relaxed);
    }
}

impl eframe::App for App {
    /// Runs even while minimized, so the profiler keeps recording in the background.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(s) = self.rx.try_recv() {
            let t = self.started.elapsed().as_secs_f64();
            let h = &mut self.hist;
            h.cpu.push(t, s.cpu as f64);
            h.mem.push(t, ui::pct(s.mem_used, s.mem_total));
            h.disk_r.push(t, s.disk_read);
            h.disk_w.push(t, s.disk_write);
            h.net_rx.push(t, s.net_rx);
            h.net_tx.push(t, s.net_tx);
            h.gpu.push(t, s.gpu as f64);
            if let Some(p) = self.selected.and_then(|id| find(&s.procs, id)) {
                h.sel_cpu.push(t, p.cpu as f64);
                h.sel_mem.push(t, p.mem as f64);
                h.sel_io.push(t, p.io);
                h.sel_net.push(t, p.net.map_or(0.0, |(a, b)| a + b));
                h.sel_gpu.push(t, p.gpu as f64);
            }
            self.profile.record(
                s.secs,
                s.cores.len(),
                s.procs.iter().map(|p| {
                    (
                        p.name.as_str(),
                        p.pid,
                        p.cpu,
                        p.mem,
                        p.io,
                        p.net,
                        p.gpu,
                        p.gpu_mem,
                    )
                }),
            );
            self.snap = Some(s);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui::page(self, ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        etw::stop();
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(concat!("reswizz ", env!("CARGO_PKG_VERSION")))
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("reswizz", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, start: u64) -> Proc {
        Proc {
            pid,
            ppid: None,
            name: String::new(),
            cpu: 0.0,
            mem: 0,
            io: 0.0,
            start,
            net: None,
            threads: None,
            gpu: 0.0,
            gpu_mem: 0,
            status: String::new(),
            exe: None,
            cmd: None,
        }
    }

    #[test]
    fn cpu_is_normalized_to_whole_machine() {
        assert_eq!(normalize_cpu(400.0, 8), 50.0);
        assert_eq!(normalize_cpu(100.0, 0), 100.0);
    }

    #[test]
    fn reused_pid_is_not_the_same_process() {
        let procs = [proc(42, 2000)];
        assert!(find(&procs, (42, 2000)).is_some());
        assert!(find(&procs, (42, 1000)).is_none());
    }
}
