# reswizz

A small native Windows dashboard in the style of btop. It is written in Rust with egui.
One page shows CPU (total and per core), memory, GPU, disk, network, processes, and open TCP/UDP endpoints.
A built-in **profiler** ranks programs by how much load they caused since start: CPU time, average and
peak CPU, peak memory, average and peak GPU, peak GPU memory, and total I/O, all grouped by program name, plus **real per-process network bytes**. It keeps recording while the window is minimized.
Select any process to **inspect** it. You get the user/kernel CPU split, threads, handles, private memory, page faults,
the I/O breakdown, the services it hosts (useful for svchost), its parent and children, 60 s graphs, and
**findings**: plain-language hints about why the process is busy.

## Build

The app targets Windows 10/11 x64. Two ways to build it:

**On Windows** (MSVC toolchain, with the Windows SDK installed through Visual Studio Build Tools):

    cargo build --release

**From Linux/WSL** (needs mingw-w64: `apt install gcc-mingw-w64-x86-64`):

    rustup target add x86_64-pc-windows-gnu
    cargo build --release --target x86_64-pc-windows-gnu

The output is a single `reswizz.exe`. It has no runtime dependencies and makes no network calls.

## Checks

    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test                                      # pure logic, any OS
    cargo test --target x86_64-pc-windows-gnu       # adds live IP Helper test (Windows / WSL)

## CI and releases

GitHub Actions run on every pull request and on `main`:

- **test**: `fmt`, `clippy -D warnings` and `cargo test` on Linux; `clippy`, `cargo test` and the release build on Windows.
- **security**: Trivy scans `Cargo.lock` for CVEs (weekly too). A fixable CRITICAL fails the job.
- **release**: each merged PR into `main` tags the next SemVer version and publishes a GitHub release. The release has
  `reswizz-vX.Y.Z-windows-x64.exe` and `SHA256SUMS.txt`. Git tags are the source of truth. A PR label picks the bump:
  `release:patch` (the default), `release:minor`, `release:major`, or `release:none` to skip the release.
  The version shows in the window title.

## Known limitations

- **Per-process CPU** is a percentage of the whole machine: all logical cores together equal 100%.
- **Per-process I/O** uses Windows I/O counters. They include disk, network, and device I/O, not just disk.
- **Per-process network traffic needs administrator rights**, because it uses a kernel ETW trace.
  Use the "restart as admin" button in the profile header. Without it, the net columns show `n/a`.
  The network view itself lists open sockets, which are not traffic.
- **The app runs without elevation.** For protected or system processes, the executable path, command line, and
  parent PID may show as `n/a`. Run it as Administrator to see more.
- **The profile lives in memory only.** It is lost when you close the app.
- **Thread count is not shown**, because `sysinfo` does not report it on Windows.
- **GPU %** is the busiest engine (3D, copy, video…), as in Task Manager. **GPU memory** is dedicated plus shared,
  so integrated GPUs show real numbers. It comes from Windows performance counters and needs no admin rights.
- **There is no fan speed.** Windows does not expose fan RPM on most machines without a kernel driver. Instead, the
  cpu box shows actual CPU speed as % of base and flags **throttled** when a thermal zone limits the CPU, or when
  speed drops under heavy load.
