//! btop-style single page: cpu strip on top, mem/disk/net column left,
//! processes / profile / network table right, selected-process details below.

use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui,
    Vec2, vec2,
};

use crate::history::History;
use crate::{App, MAX_INTERVAL_MS, MIN_INTERVAL_MS, Proc, Snapshot, View, find};

const BG: Color32 = Color32::from_rgb(0x10, 0x12, 0x18);
const WELL: Color32 = Color32::from_rgb(0x16, 0x19, 0x21);
const DIM: Color32 = Color32::from_rgb(0x2c, 0x31, 0x3c);
const TEXT: Color32 = Color32::from_rgb(0xc8, 0xcc, 0xd4);
const MUTED: Color32 = Color32::from_rgb(0x7a, 0x80, 0x8c);
const C_CPU: Color32 = Color32::from_rgb(0x7f, 0xc8, 0x8f);
const C_MEM: Color32 = Color32::from_rgb(0xe0, 0xb0, 0x6a);
const C_DISK: Color32 = Color32::from_rgb(0xe0, 0x7f, 0x86);
const C_NET: Color32 = Color32::from_rgb(0xb3, 0x8f, 0xe8);
const C_PROC: Color32 = Color32::from_rgb(0x6f, 0xb6, 0xe0);
const LOW: Color32 = Color32::from_rgb(0x50, 0xd0, 0x80);
const MID: Color32 = Color32::from_rgb(0xf0, 0xd0, 0x60);
const HIGH: Color32 = Color32::from_rgb(0xf0, 0x55, 0x55);
const ROW_H: f32 = 17.0;

pub fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        for f in s.text_styles.values_mut() {
            f.family = FontFamily::Monospace;
            f.size = f.size.min(13.0);
        }
        s.visuals.panel_fill = BG;
        s.visuals.window_fill = BG;
        s.visuals.extreme_bg_color = WELL;
        s.visuals.override_text_color = Some(TEXT);
        s.spacing.item_spacing = vec2(6.0, 2.0);
    });
}

/// Green → yellow → red for 0.0 → 1.0.
fn heat(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        LOW.lerp_to_gamma(MID, t * 2.0)
    } else {
        MID.lerp_to_gamma(HIGH, (t - 0.5) * 2.0)
    }
}

pub fn page(app: &mut App, ui: &mut Ui) {
    let Some(snap) = app.snap.take() else {
        egui::CentralPanel::default().show(ui, |ui| ui.label("collecting first sample…"));
        return;
    };
    let panel = |m| egui::Frame::NONE.fill(BG).inner_margin(m);
    let top_h = (ui.available_height() * 0.3).clamp(150.0, 260.0);
    egui::Panel::top("cpu")
        .exact_size(top_h)
        .show_separator_line(false)
        .frame(panel(Margin::symmetric(6, 8)))
        .show(ui, |ui| cpu_box(app, ui, &snap));
    if app.selected.is_some() {
        egui::Panel::bottom("details")
            .resizable(true)
            .default_size((ui.available_height() * 0.36).max(220.0))
            .size_range(200.0..=700.0)
            .show_separator_line(false)
            .frame(panel(Margin::symmetric(6, 8)))
            .show(ui, |ui| details_box(app, ui, &snap));
    }
    let side_w = (ui.available_width() * 0.3).clamp(300.0, 440.0);
    egui::Panel::left("side")
        .exact_size(side_w)
        .show_separator_line(false)
        .frame(panel(Margin::symmetric(6, 8)))
        .show(ui, |ui| side_column(app, ui, &snap));
    egui::CentralPanel::default()
        .frame(panel(Margin::symmetric(6, 8)))
        .show(ui, |ui| main_box(app, ui, &snap));
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.select(None);
    }
    app.snap = Some(snap);
}

// ---------- boxes ----------

fn cpu_box(app: &mut App, ui: &mut Ui, s: &Snapshot) {
    boxed(ui, "cpu", C_CPU, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{:5.1}%", s.cpu))
                    .color(heat(s.cpu / 100.0))
                    .strong(),
            );
            ui.label(RichText::new(format!("{} threads", s.cores.len())).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut ms = app.interval_ms.load(std::sync::atomic::Ordering::Relaxed);
                ui.add(egui::Slider::new(&mut ms, MIN_INTERVAL_MS..=MAX_INTERVAL_MS).suffix("ms"));
                ui.label(RichText::new("refresh").color(MUTED));
                app.interval_ms
                    .store(ms, std::sync::atomic::Ordering::Relaxed);
            });
        });
        let avail = ui.available_size();
        // horizontal rows are interact_size.y (18) + 1 spacing
        let rows = ((avail.y / 19.0).floor() as usize).max(1);
        let cols = s.cores.len().div_ceil(rows).max(1);
        let col_w = 138.0;
        let cores_w = (cols as f32 * col_w).min(avail.x * 0.6);
        ui.horizontal(|ui| {
            graph(
                ui,
                &app.hist.cpu,
                100.0,
                vec2(avail.x - cores_w - 10.0, avail.y),
                None,
            );
            ui.horizontal_top(|ui| {
                for chunk in s.cores.chunks(rows).enumerate() {
                    let (c, cores) = chunk;
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        for (i, v) in cores.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("C{:<3}", c * rows + i))
                                        .color(MUTED)
                                        .size(11.0),
                                );
                                meter(ui, v / 100.0, (cores_w / cols as f32 - 70.0).max(20.0));
                                ui.label(RichText::new(format!("{v:3.0}%")).size(11.0));
                            });
                        }
                    });
                }
            });
        });
    });
}

fn side_column(app: &mut App, ui: &mut Ui, s: &Snapshot) {
    let h = ui.available_height();
    let w = ui.available_width();
    ui.allocate_ui(vec2(w, h * 0.36), |ui| {
        boxed(ui, "mem", C_MEM, |ui| {
            let mw = ui.available_width() - 190.0;
            let free = s.mem_total.saturating_sub(s.mem_used);
            for (label, used, total) in [
                ("used", s.mem_used, s.mem_total),
                ("free", free, s.mem_total),
                ("swap", s.swap_used, s.swap_total),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{label:<5}")).color(MUTED));
                    meter(ui, (pct(used, total) / 100.0) as f32, mw);
                    ui.label(format!(
                        "{:>9} {:3.0}%",
                        bytes(used as f64),
                        pct(used, total)
                    ));
                });
            }
            ui.label(RichText::new(format!("total {}", bytes(s.mem_total as f64))).color(MUTED));
            let sz = ui.available_size();
            graph(ui, &app.hist.mem, 100.0, sz, Some(C_MEM));
        });
    });
    ui.add_space(10.0);
    ui.allocate_ui(vec2(w, h * 0.28), |ui| {
        boxed(ui, "disk", C_DISK, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("▲ read  {}", rate(s.disk_read))).color(C_DISK));
                ui.label(RichText::new(format!("▼ write {}", rate(s.disk_write))).color(C_MEM));
            });
            let sz = ui.available_size();
            dual_graph(ui, &app.hist.disk_r, &app.hist.disk_w, sz, C_DISK, C_MEM);
        });
    });
    ui.add_space(10.0);
    let rest = ui.available_size();
    ui.allocate_ui(rest, |ui| {
        boxed(ui, "net", C_NET, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("▼ in  {}", rate(s.net_rx))).color(C_NET));
                ui.label(RichText::new(format!("▲ out {}", rate(s.net_tx))).color(C_PROC));
            });
            let mut ads: Vec<_> = s.adapters.iter().filter(|a| a.1 + a.2 > 0.0).collect();
            ads.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2)));
            let sz = ui.available_size() - vec2(0.0, ads.len().min(3) as f32 * 15.0 + 4.0);
            dual_graph(ui, &app.hist.net_rx, &app.hist.net_tx, sz, C_NET, C_PROC);
            for (name, rx, tx) in ads.into_iter().take(3) {
                ui.label(
                    RichText::new(format!("{name:.18}  ▼{}  ▲{}", rate(*rx), rate(*tx)))
                        .size(11.0)
                        .color(MUTED),
                );
            }
        });
    });
}

fn main_box(app: &mut App, ui: &mut Ui, s: &Snapshot) {
    boxed(ui, "proc", C_PROC, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (v, name) in [(View::Profile, "profile"), (View::Processes, "processes"), (View::Network, "network")] {
                ui.selectable_value(&mut app.view, v, name);
            }
            ui.add(egui::TextEdit::singleline(&mut app.filter).hint_text("filter").desired_width(160.0));
            if !app.filter.is_empty() && ui.small_button("x").clicked() {
                app.filter.clear();
            }
            match app.view {
                View::Profile => profile_controls(app, ui, s),
                View::Network => {
                    ui.label(RichText::new("open sockets, not traffic").color(MUTED))
                        .on_hover_text("Windows reports which process owns each socket, not how many bytes it moves.");
                }
                View::Processes => {
                    ui.label(RichText::new(format!("{} processes", s.procs.len())).color(MUTED));
                }
            }
        });
        ui.add_space(2.0);
        match app.view {
            View::Processes => processes(app, ui, s),
            View::Profile => profile(app, ui),
            View::Network => network(app, ui, s),
        }
    });
}

fn profile_controls(app: &mut App, ui: &mut Ui, s: &Snapshot) {
    let p = &mut app.profile;
    if p.paused {
        ui.label(RichText::new(format!("❚❚ paused {}", dur(p.elapsed))).color(MID));
    } else {
        ui.label(RichText::new(format!("● rec {}", dur(p.elapsed))).color(HIGH));
    }
    if ui
        .small_button(if p.paused { "resume" } else { "pause" })
        .clicked()
    {
        p.paused = !p.paused;
    }
    if ui.small_button("reset").clicked() {
        p.reset();
    }
    ui.label(RichText::new("(?)").color(MUTED)).on_hover_text(
        "Profile accumulates since start (or reset), grouped by program name.\n\
         cpu time = core-seconds used · avg = share of the whole machine over the recording\n\
         peak = highest sample (all instances summed) · keeps recording while minimized.\n\
         net = real bytes sent/received per process (kernel ETW trace, needs admin).\n\
         Click a row to filter the process list by that program.",
    );
    admin_hint(ui, s);
}

/// Per-process network needs an elevated ETW session; offer a one-click UAC relaunch.
fn admin_hint(ui: &mut Ui, s: &Snapshot) {
    if let Some(e) = &s.net_trace_err {
        ui.label(RichText::new("per-process net: needs admin").color(MID))
            .on_hover_text(e);
        if cfg!(windows)
            && ui.small_button("restart as admin").clicked()
            && crate::inspect::relaunch_as_admin()
        {
            crate::etw::stop();
            std::process::exit(0);
        }
    }
}

fn processes(app: &mut App, ui: &mut Ui, s: &Snapshot) {
    let q = app.filter.to_lowercase();
    let cores = s.cores.len().max(1) as f32;
    let list: Vec<&Proc> = s
        .procs
        .iter()
        .filter(|p| {
            q.is_empty()
                || p.name.to_lowercase().contains(&q)
                || p.pid.to_string() == q
                || p.cmd
                    .as_deref()
                    .is_some_and(|c| c.to_lowercase().contains(&q))
        })
        .collect();
    let rows: Vec<Row> = list
        .iter()
        .map(|p| Row {
            cells: vec![
                p.pid.to_string(),
                p.name.clone(),
                format!("{:5.1}", p.cpu),
                bytes(p.mem as f64),
                rate(p.io),
                p.net.map_or("n/a".into(), |(tx, rx)| rate(tx + rx)),
                p.threads.map_or("n/a".into(), |t| t.to_string()),
                p.cmd.clone().or_else(|| p.exe.clone()).unwrap_or_default(),
            ],
            nums: vec![
                Some(p.pid as f64),
                None,
                Some(p.cpu as f64),
                Some(p.mem as f64),
                Some(p.io),
                Some(p.net.map_or(-1.0, |(tx, rx)| tx + rx)),
                Some(p.threads.map_or(-1.0, |t| t as f64)),
                None,
            ],
            bar: Some(p.cpu * cores / 100.0),
        })
        .collect();
    let sel = app
        .selected
        .and_then(|id| list.iter().position(|p| (p.pid, p.start) == id));
    let cols = [
        ("pid", 64.0),
        ("program", 190.0),
        ("cpu%", 110.0),
        ("mem", 84.0),
        ("i/o", 96.0),
        ("net", 96.0),
        ("thr", 44.0),
        ("command", 0.0),
    ];
    if let Some(i) = table(
        ui,
        &cols,
        Some(2),
        &rows,
        &mut app.sort[View::Processes as usize],
        sel,
    ) {
        app.select(Some((list[i].pid, list[i].start)));
    }
}

fn profile(app: &mut App, ui: &mut Ui) {
    let p = &app.profile;
    let q = app.filter.to_lowercase();
    let entries: Vec<_> = p
        .by_name
        .iter()
        .filter(|(n, _)| n.to_lowercase().contains(&q))
        .collect();
    let rows: Vec<Row> = entries
        .iter()
        .map(|(name, e)| {
            let avg = p.avg_cpu(e);
            Row {
                cells: vec![
                    name.to_string(),
                    e.pids.len().to_string(),
                    if e.cpu_secs < 60.0 {
                        format!("{:.1}s", e.cpu_secs)
                    } else {
                        dur(e.cpu_secs)
                    },
                    format!("{avg:5.1}"),
                    format!("{:5.1}", e.peak_cpu),
                    format!("+{}", dur(e.peak_at)),
                    bytes(e.peak_mem as f64),
                    bytes(e.io_bytes),
                    if p.has_net {
                        bytes(e.net_rx)
                    } else {
                        "n/a".into()
                    },
                    if p.has_net {
                        bytes(e.net_tx)
                    } else {
                        "n/a".into()
                    },
                ],
                nums: vec![
                    None,
                    Some(e.pids.len() as f64),
                    Some(e.cpu_secs),
                    Some(avg),
                    Some(e.peak_cpu as f64),
                    Some(e.peak_at),
                    Some(e.peak_mem as f64),
                    Some(e.io_bytes),
                    Some(e.net_rx),
                    Some(e.net_tx),
                ],
                bar: Some((avg * p.cores as f64 / 100.0) as f32),
            }
        })
        .collect();
    let cols = [
        ("program", 190.0),
        ("pids", 50.0),
        ("cpu time", 84.0),
        ("avg cpu%", 110.0),
        ("peak%", 60.0),
        ("peak at", 80.0),
        ("peak mem", 84.0),
        ("total i/o", 90.0),
        ("net ▼", 90.0),
        ("net ▲", 0.0),
    ];
    if let Some(i) = table(
        ui,
        &cols,
        Some(3),
        &rows,
        &mut app.sort[View::Profile as usize],
        None,
    ) {
        app.filter = entries[i].0.clone();
        app.view = View::Processes;
    }
}

fn network(app: &mut App, ui: &mut Ui, s: &Snapshot) {
    let eps = match &s.endpoints {
        Ok(e) => e,
        Err(e) => {
            ui.colored_label(HIGH, e);
            return;
        }
    };
    let q = app.filter.to_lowercase();
    let name = |pid| s.procs.iter().find(|p| p.pid == pid);
    let mut pids = Vec::new();
    let rows: Vec<Row> = eps
        .iter()
        .map(|e| {
            let cells = vec![
                e.proto.to_string(),
                e.pid.to_string(),
                name(e.pid).map_or("n/a".into(), |p| p.name.clone()),
                e.local.to_string(),
                e.remote.map_or(String::new(), |r| r.to_string()),
                e.state.to_string(),
            ];
            (e.pid, cells)
        })
        .filter(|(_, c)| q.is_empty() || c.iter().any(|c| c.to_lowercase().contains(&q)))
        .map(|(pid, cells)| {
            pids.push(pid);
            Row {
                cells,
                nums: vec![None, Some(pid as f64), None, None, None, None],
                bar: None,
            }
        })
        .collect();
    let cols = [
        ("proto", 50.0),
        ("pid", 64.0),
        ("program", 170.0),
        ("local", 220.0),
        ("remote", 220.0),
        ("state", 0.0),
    ];
    if let Some(i) = table(
        ui,
        &cols,
        None,
        &rows,
        &mut app.sort[View::Network as usize],
        None,
    ) && let Some(p) = name(pids[i])
    {
        app.select(Some((p.pid, p.start)));
    }
}

fn details_box(app: &mut App, ui: &mut Ui, s: &Snapshot) {
    let Some(id) = app.selected else { return };
    let p = find(&s.procs, id);
    let title = match p {
        Some(p) => format!("inspect · {} · pid {}", p.name, p.pid),
        None => format!("inspect · pid {} · exited", id.0),
    };
    boxed(ui, &title, C_CPU, |ui| {
        let Some(p) = p else {
            ui.label("Process has exited. Esc to close.");
            return;
        };
        let i = s
            .inspect
            .as_ref()
            .filter(|(pid, _)| *pid == p.pid)
            .map(|x| x.1.clone())
            .unwrap_or_default();
        let na = || "n/a".to_string();
        let opt = |v: Option<String>| v.unwrap_or_else(na);
        let h = &app.hist;
        let mem_growth = (h.sel_mem.samples.len() >= 20)
            .then(|| h.sel_mem.samples.back().unwrap().1 - h.sel_mem.samples.front().unwrap().1);
        let findings = crate::inspect::findings(&i, p.cpu, p.threads, mem_growth, p.net);
        let mut close = false;

        ui.columns(4, |c| {
            // identity
            section(&mut c[0], "identity");
            let parent = p.ppid.map(|pp| {
                let n = s
                    .procs
                    .iter()
                    .find(|x| x.pid == pp)
                    .map_or("not running", |x| x.name.as_str());
                format!("{n} ({pp})")
            });
            let kids: Vec<&Proc> = s.procs.iter().filter(|x| x.ppid == Some(p.pid)).collect();
            let kid_names = kids
                .iter()
                .take(4)
                .map(|k| k.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let age = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .filter(|_| p.start > 0)
                .map(|now| format!("{} ago", dur(now.as_secs().saturating_sub(p.start) as f64)));
            kv_grid(
                &mut c[0],
                "id",
                &[
                    ("parent", opt(parent)),
                    (
                        "children",
                        if kids.is_empty() {
                            "none".into()
                        } else {
                            format!("{} · {kid_names}", kids.len())
                        },
                    ),
                    (
                        "services",
                        if i.services.is_empty() {
                            "none".into()
                        } else {
                            i.services.join(", ")
                        },
                    ),
                    ("status", p.status.clone()),
                    ("priority", opt(i.raw.priority.map(str::to_string))),
                    ("started", opt(age)),
                    ("exe", opt(p.exe.clone())),
                    ("cmd", opt(p.cmd.clone())),
                ],
            );

            // resources
            section(&mut c[1], "resources");
            let io = |k: usize| {
                i.io_s
                    .map(|v| format!("{:.0} ops/s · {}", v[k], rate(v[k + 3])))
            };
            kv_grid(
                &mut c[1],
                "res",
                &[
                    ("cpu", format!("{:.1}% of machine", p.cpu)),
                    ("  user", opt(i.user_pct.map(|v| format!("{v:.1}%")))),
                    ("  kernel", opt(i.kernel_pct.map(|v| format!("{v:.1}%")))),
                    ("threads", opt(p.threads.map(|t| t.to_string()))),
                    ("handles", opt(i.raw.handles.map(|t| t.to_string()))),
                    (
                        "working set",
                        opt(i.raw.working_set.map(|v| bytes(v as f64))),
                    ),
                    ("private", opt(i.raw.private.map(|v| bytes(v as f64)))),
                    ("peak ws", opt(i.raw.peak_ws.map(|v| bytes(v as f64)))),
                    ("faults/s", opt(i.faults_s.map(|v| format!("{v:.0}")))),
                    ("io read", opt(io(0))),
                    ("io write", opt(io(1))),
                    ("io other", opt(io(2))),
                    (
                        "net",
                        opt(p
                            .net
                            .map(|(tx, rx)| format!("▼ {}  ▲ {}", rate(rx), rate(tx)))),
                    ),
                ],
            );

            // activity graphs
            section(&mut c[2], "last 60 s");
            let gh = ((c[2].available_height() - 4.0 * 16.0) / 4.0).max(16.0);
            let w = c[2].available_width();
            // cpu coloured by load; the others auto-scale, so a fixed colour avoids false "red"
            let net_now = p.net.map_or(na(), |(a, b)| rate(a + b));
            for (label, hist, max, now, color) in [
                (
                    "cpu%",
                    &h.sel_cpu,
                    hmax(&h.sel_cpu).max(5.0),
                    format!("{:.1}%", p.cpu),
                    None,
                ),
                (
                    "mem",
                    &h.sel_mem,
                    hmax(&h.sel_mem).max(1.0),
                    bytes(p.mem as f64),
                    Some(C_MEM),
                ),
                (
                    "i/o",
                    &h.sel_io,
                    hmax(&h.sel_io).max(1024.0),
                    rate(p.io),
                    Some(C_DISK),
                ),
                (
                    "net",
                    &h.sel_net,
                    hmax(&h.sel_net).max(1024.0),
                    net_now,
                    Some(C_NET),
                ),
            ] {
                c[2].label(
                    RichText::new(format!("{label:<5}{now}"))
                        .size(11.0)
                        .color(MUTED),
                );
                graph(&mut c[2], hist, max, vec2(w, gh), color);
            }

            // findings + connections
            c[3].horizontal(|ui| {
                section(ui, "findings");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.small_button("close").on_hover_text("Esc").clicked();
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("findings")
                .max_height(c[3].available_height() * 0.55)
                .show(&mut c[3], |ui| {
                    if findings.is_empty() {
                        ui.label(RichText::new("nothing unusual right now").color(MUTED));
                    }
                    for f in &findings {
                        ui.add(egui::Label::new(RichText::new(format!("• {f}")).color(MID)).wrap());
                    }
                    if s.net_trace_err.is_some() {
                        ui.label(
                            RichText::new("per-process network needs admin")
                                .color(MUTED)
                                .size(11.0),
                        );
                    }
                });
            let conns: Vec<_> = s
                .endpoints
                .iter()
                .flatten()
                .filter(|e| e.pid == p.pid)
                .collect();
            let est = conns.iter().filter(|e| e.state == "ESTABLISHED").count();
            let listen = conns.iter().filter(|e| e.state == "LISTEN").count();
            section(
                &mut c[3],
                &format!(
                    "sockets {} · {est} established · {listen} listening",
                    conns.len()
                ),
            );
            egui::ScrollArea::vertical()
                .id_salt("conns")
                .show(&mut c[3], |ui| {
                    for e in conns
                        .iter()
                        .filter(|e| e.remote.is_some_and(|r| !r.ip().is_unspecified()))
                        .take(30)
                    {
                        ui.label(
                            RichText::new(format!("{} {} {}", e.proto, e.remote.unwrap(), e.state))
                                .size(11.0)
                                .color(TEXT.gamma_multiply(0.8)),
                        );
                    }
                });
        });
        if close {
            app.select(None);
        }
    });
}

fn section(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).color(C_PROC).size(11.0));
}

fn kv_grid(ui: &mut Ui, id: &str, rows: &[(&str, String)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(vec2(10.0, 1.0))
        .show(ui, |ui| {
            for (k, v) in rows {
                ui.label(RichText::new(*k).color(MUTED).size(12.0));
                ui.add(egui::Label::new(RichText::new(v).size(12.0)).truncate())
                    .on_hover_text(v);
                ui.end_row();
            }
        });
}

// ---------- widgets ----------

/// Rounded border with the title cut into the top edge, filling the available space.
fn boxed<R>(ui: &mut Ui, title: &str, color: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = egui::Frame::NONE
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
        .corner_radius(6)
        .inner_margin(Margin {
            left: 8,
            right: 8,
            top: 10,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui)
        });
    let font = FontId::monospace(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(format!(" {title} "), font, color);
    let pos = r.response.rect.left_top() + vec2(12.0, -galley.size().y / 2.0);
    ui.painter()
        .rect_filled(Rect::from_min_size(pos, galley.size()), 0.0, BG);
    ui.painter().galley(pos, galley, color);
    r.inner
}

/// Segmented bar, segment colour by position (btop style).
fn meter(ui: &mut Ui, frac: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
    let (seg, gap) = (4.0, 1.0);
    let n = ((width + gap) / (seg + gap)).floor().max(1.0) as usize;
    let lit = (frac.clamp(0.0, 1.0) * n as f32).round() as usize;
    for i in 0..n {
        let x = rect.left() + i as f32 * (seg + gap);
        let c = if i < lit {
            heat(i as f32 / n as f32)
        } else {
            DIM
        };
        ui.painter().rect_filled(
            Rect::from_min_size(Pos2::new(x, rect.top()), vec2(seg, rect.height())),
            1.0,
            c,
        );
    }
}

/// Bar history, newest on the right. `color: None` colours each bar by its value.
fn graph(ui: &mut Ui, h: &History, max: f64, size: Vec2, color: Option<Color32>) {
    let (rect, _) = ui.allocate_exact_size(size.max(vec2(10.0, 10.0)), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, WELL);
    bars(&p, h, max, rect, rect.bottom(), -1.0, color);
}

/// Two mirrored histories sharing one scale: `up` grows up from the middle, `down` grows down.
fn dual_graph(ui: &mut Ui, up: &History, down: &History, size: Vec2, cu: Color32, cd: Color32) {
    let (rect, resp) = ui.allocate_exact_size(size.max(vec2(10.0, 10.0)), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, WELL);
    let max = hmax(up).max(hmax(down)).max(1024.0);
    let mid = rect.center().y;
    let half = Rect::from_min_max(rect.min, Pos2::new(rect.right(), mid));
    bars(&p, up, max, half, mid, -1.0, Some(cu));
    bars(&p, down, max, half, mid, 1.0, Some(cd));
    p.line_segment(
        [Pos2::new(rect.left(), mid), Pos2::new(rect.right(), mid)],
        Stroke::new(1.0, DIM),
    );
    p.text(
        rect.left_top() + vec2(4.0, 2.0),
        Align2::LEFT_TOP,
        rate(max),
        FontId::monospace(10.0),
        MUTED,
    );
    resp.on_hover_text("scale = peak of the last 60 s");
}

/// Draw `h` as bars from baseline `base` in direction `dir` (-1 up, +1 down), height from `area`.
fn bars(
    p: &egui::Painter,
    h: &History,
    max: f64,
    area: Rect,
    base: f32,
    dir: f32,
    color: Option<Color32>,
) {
    let w = area.width() / h.cap as f32;
    let gap = if w > 3.0 { 1.0 } else { 0.0 };
    for (i, &(_, v)) in h.samples.iter().rev().enumerate() {
        let t = (v / max).clamp(0.0, 1.0) as f32;
        if t <= 0.0 {
            continue;
        }
        let x = area.right() - (i + 1) as f32 * w;
        let bh = (t * area.height()).max(1.0);
        let c = color.unwrap_or_else(|| heat(t));
        p.rect_filled(
            Rect::from_two_pos(Pos2::new(x, base), Pos2::new(x + w - gap, base + dir * bh)),
            0.0,
            c,
        );
    }
}

struct Row {
    cells: Vec<String>,
    /// Numeric sort key per column; `None` sorts by text.
    nums: Vec<Option<f64>>,
    /// 0..1 fill for the mini meter in the bar column.
    bar: Option<f32>,
}

/// Painted (not widget-per-cell) table: click header to sort, click row to pick.
/// Column width 0 = take the remaining width. Returns the clicked row's index in `rows`.
fn table(
    ui: &mut Ui,
    cols: &[(&str, f32)],
    bar_col: Option<usize>,
    rows: &[Row],
    sort: &mut (usize, bool),
    selected: Option<usize>,
) -> Option<usize> {
    let font = FontId::monospace(12.0);
    let xs = |rect: Rect| {
        let mut x = rect.left() + 4.0;
        cols.iter()
            .map(|&(_, w)| {
                let r = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new(
                        if w == 0.0 { rect.right() } else { x + w - 6.0 },
                        rect.bottom(),
                    ),
                );
                x += w;
                r
            })
            .collect::<Vec<_>>()
    };

    // header
    let (hrect, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    for (i, cell) in xs(hrect).into_iter().enumerate() {
        let arrow = match *sort {
            (c, true) if c == i => " ▼",
            (c, false) if c == i => " ▲",
            _ => "",
        };
        let color = if sort.0 == i { C_PROC } else { MUTED };
        ui.painter_at(cell).text(
            cell.left_center(),
            Align2::LEFT_CENTER,
            format!("{}{arrow}", cols[i].0),
            font.clone(),
            color,
        );
        if hresp.clicked()
            && hresp
                .interact_pointer_pos()
                .is_some_and(|p| cell.contains(p))
        {
            *sort = (
                i,
                if sort.0 == i {
                    !sort.1
                } else {
                    rows.first().is_some_and(|r| r.nums[i].is_some())
                },
            );
        }
    }

    let mut order: Vec<usize> = (0..rows.len()).collect();
    let (c, desc) = *sort;
    order.sort_by(|&a, &b| {
        let o = match (rows[a].nums[c], rows[b].nums[c]) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => rows[a].cells[c]
                .to_lowercase()
                .cmp(&rows[b].cells[c].to_lowercase()),
        };
        if desc { o.reverse() } else { o }
    });

    let mut clicked = None;
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        ROW_H,
        order.len(),
        |ui, range| {
            for &ri in &order[range] {
                let row = &rows[ri];
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let p = ui.painter();
                if selected == Some(ri) {
                    p.rect_filled(rect, 2.0, C_PROC.gamma_multiply(0.25));
                } else if resp.hovered() {
                    p.rect_filled(rect, 2.0, DIM.gamma_multiply(0.6));
                }
                for (i, cell) in xs(rect).into_iter().enumerate() {
                    let p = ui.painter_at(cell);
                    let mut x = cell.left();
                    if bar_col == Some(i)
                        && let Some(f) = row.bar
                    {
                        let f = f.clamp(0.0, 1.0);
                        let r = Rect::from_min_size(
                            Pos2::new(x, cell.center().y - 3.0),
                            vec2(40.0, 6.0),
                        );
                        p.rect_filled(r, 1.0, DIM);
                        p.rect_filled(
                            Rect::from_min_size(r.min, vec2(40.0 * f, 6.0)),
                            1.0,
                            heat(f),
                        );
                        x += 46.0;
                    }
                    let color = if i == 0 || bar_col == Some(i) {
                        TEXT
                    } else {
                        TEXT.gamma_multiply(0.85)
                    };
                    p.text(
                        Pos2::new(x, cell.center().y),
                        Align2::LEFT_CENTER,
                        &row.cells[i],
                        font.clone(),
                        color,
                    );
                }
                if resp.clicked() {
                    clicked = Some(ri);
                }
            }
        },
    );
    clicked
}

// ---------- formatting ----------

fn hmax(h: &History) -> f64 {
    h.samples.iter().map(|s| s.1).fold(0.0, f64::max)
}

pub fn pct(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

fn bytes(b: f64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn rate(b: f64) -> String {
    format!("{}/s", bytes(b))
}

fn dur(secs: f64) -> String {
    let s = secs as u64;
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m{s:02}s"),
        (h, m, _) if h < 24 => format!("{h}h{m:02}m"),
        (h, _, _) => format!("{}d{:02}h", h / 24, h % 24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_units() {
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(1536.0), "1.5 KiB");
        assert_eq!(rate(3.0 * 1024.0 * 1024.0), "3.0 MiB/s");
        assert_eq!(pct(1, 0), 0.0);
        assert_eq!(dur(5.0), "5s");
        assert_eq!(dur(125.0), "2m05s");
        assert_eq!(dur(7300.0), "2h01m");
        assert_eq!(dur(927_000.0), "10d17h");
    }

    #[test]
    fn heat_spans_green_to_red() {
        assert_eq!(heat(0.0), LOW);
        assert_eq!(heat(0.5), MID);
        assert_eq!(heat(1.0), HIGH);
        assert_eq!(heat(7.0), HIGH);
    }
}
