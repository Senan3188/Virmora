//! Per-process measurements. Network counts are observed host NAT socket bytes,
//! not guest Ethernet traffic, and can exclude sockets closed between samples.
use serde::Serialize;
#[cfg(any(target_os = "macos", test))]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::{
    io::{BufRead, BufReader},
    process::Stdio,
    thread,
};
use std::{
    process::Child,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize)]
pub struct VmMetrics {
    pub running: bool,
    pub pid: Option<u32>,
    pub instance_generation: u64,
    pub sampled_at_ms: u64,
    pub cpu_percent: Option<f64>,
    pub cpu_note: String,
    pub memory_bytes: Option<u64>,
    pub memory_note: String,
    pub network_rx_bytes: Option<u64>,
    pub network_tx_bytes: Option<u64>,
    pub network_rx_bytes_per_sec: Option<f64>,
    pub network_tx_bytes_per_sec: Option<f64>,
    pub network_available: bool,
    pub network_note: Option<String>,
}
impl VmMetrics {
    pub(crate) fn stopped(generation: u64) -> Self {
        Self {
            running: false,
            pid: None,
            instance_generation: generation,
            sampled_at_ms: timestamp(),
            cpu_percent: None,
            cpu_note: "QEMU process CPU; one host core = 100%".into(),
            memory_bytes: None,
            memory_note: memory_note().into(),
            network_rx_bytes: None,
            network_tx_bytes: None,
            network_rx_bytes_per_sec: None,
            network_tx_bytes_per_sec: None,
            network_available: false,
            network_note: Some("VM is stopped".into()),
        }
    }
}
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn memory_note() -> &'static str {
    if cfg!(target_os = "macos") {
        "QEMU process physical footprint (not guest RAM usage)"
    } else {
        "QEMU process memory measurement unavailable on this host"
    }
}
#[cfg(any(target_os = "macos", test))]
const NETWORK_NOTE: &str = "Observed QEMU host TCP/UDP socket traffic (NAT); excludes guest link/DHCP traffic and may omit short-lived sockets";
static GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct NetworkSample {
    available: bool,
    rx: u64,
    tx: u64,
    rx_rate: Option<f64>,
    tx_rate: Option<f64>,
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    previous: Option<Instant>,
    note: Option<String>,
}
impl NetworkSample {
    #[cfg(any(target_os = "macos", test))]
    fn add(&mut self, rx: u64, tx: u64, now: Instant) {
        let elapsed = self
            .previous
            .map(|p| now.saturating_duration_since(p).as_secs_f64());
        self.rx_rate = elapsed.filter(|s| *s > 0.).map(|s| rx as f64 / s);
        self.tx_rate = elapsed.filter(|s| *s > 0.).map(|s| tx as f64 / s);
        self.rx = self.rx.saturating_add(rx);
        self.tx = self.tx.saturating_add(tx);
        self.previous = Some(now);
        self.available = true;
        self.note = Some(NETWORK_NOTE.into());
    }
}

pub(crate) struct Monitor {
    pub generation: u64,
    previous_cpu: Option<(Instant, u64)>,
    network: Arc<Mutex<NetworkSample>>,
    collector: Option<Child>,
    reader: Option<JoinHandle<()>>,
}
impl Monitor {
    pub fn new(pid: u32, mode: vm_core::NetworkMode) -> Self {
        let network = Arc::new(Mutex::new(NetworkSample {
            note: Some("Waiting for per-process network samples".into()),
            ..Default::default()
        }));
        let mut monitor = Self {
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            previous_cpu: None,
            network,
            collector: None,
            reader: None,
        };
        if mode == vm_core::NetworkMode::Disconnected {
            *monitor.network.lock().unwrap() = NetworkSample {
                available: true,
                rx_rate: Some(0.),
                tx_rate: Some(0.),
                note: Some("VM network is disconnected".into()),
                ..Default::default()
            };
        } else if mode == vm_core::NetworkMode::Nat {
            monitor.start_network(pid);
        } else {
            monitor.network.lock().unwrap().note =
                Some("Traffic counters are unavailable for this network mode.".into());
        }
        monitor
    }
    #[cfg(target_os = "macos")]
    fn start_network(&mut self, pid: u32) {
        let child = spawn_nettop(pid);
        match child {
            Ok((child, output)) => {
                let network = self.network.clone();
                self.reader = Some(thread::spawn(move || {
                    let mut parser = NetworkCsv::default();
                    for line in BufReader::new(output).lines() {
                        let Ok(line) = line else { break };
                        if let Some((rx, tx, at)) = parser.ingest(&line, pid, Instant::now()) {
                            if let Ok(mut sample) = network.lock() {
                                sample.add(rx, tx, at);
                            }
                        }
                    }
                    if let Ok(mut sample) = network.lock() {
                        sample.available = false;
                        sample.rx_rate = None;
                        sample.tx_rate = None;
                        sample.note=Some("Per-process network collector unavailable or exited (nettop permissions may be required)".into());
                    }
                }));
                self.collector = Some(child);
            }
            Err(error) => {
                self.network.lock().unwrap().note = Some(format!(
                    "Per-process network collector unavailable: {error}"
                ));
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    fn start_network(&mut self, _: u32) {
        self.network.lock().unwrap().note =
            Some("Per-process NAT network monitoring requires macOS nettop".into());
    }
    pub fn sample(&mut self, pid: u32) -> VmMetrics {
        // Reap collectors that fail early (for example permission denial),
        // without waiting until the VM is eventually stopped.
        if self
            .collector
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
        {
            self.collector = None;
        }
        let now = Instant::now();
        let mut result = VmMetrics::stopped(self.generation);
        result.running = true;
        result.pid = Some(pid);
        if let Some((cpu, memory)) = process_sample(pid) {
            result.memory_bytes = Some(memory);
            result.cpu_percent = self.previous_cpu.and_then(|(then, previous)| {
                cpu_delta(
                    previous,
                    cpu,
                    now.saturating_duration_since(then).as_secs_f64(),
                )
            });
            self.previous_cpu = Some((now, cpu));
        }
        if let Ok(sample) = self.network.lock() {
            let stale = sample.previous.is_some_and(|at| {
                now.saturating_duration_since(at) > std::time::Duration::from_secs(5)
            });
            result.network_available = sample.available && !stale;
            result.network_note = if stale {
                Some("Per-process network samples are stale; collector may be unavailable".into())
            } else {
                sample.note.clone()
            };
            if result.network_available {
                result.network_rx_bytes = Some(sample.rx);
                result.network_tx_bytes = Some(sample.tx);
                result.network_rx_bytes_per_sec = sample.rx_rate;
                result.network_tx_bytes_per_sec = sample.tx_rate;
            }
        }
        result
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        if let Some(mut child) = self.collector.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn cpu_delta(previous: u64, current: u64, elapsed: f64) -> Option<f64> {
    if elapsed <= 0. || !elapsed.is_finite() {
        return None;
    }
    current
        .checked_sub(previous)
        .map(|delta| delta as f64 / 1_000_000_000. / elapsed * 100.)
}

#[cfg(target_os = "macos")]
fn process_sample(pid: u32) -> Option<(u64, u64)> {
    #[repr(C)]
    struct RusageV2 {
        uuid: [u8; 16],
        fields: [u64; 18],
    }
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut std::ffi::c_void) -> i32;
    }
    let mut usage = RusageV2 {
        uuid: [0; 16],
        fields: [0; 18],
    };
    // RUSAGE_INFO_V2 uses Mach absolute time units for CPU on Apple Silicon;
    // convert through the host timebase before computing wall-clock percentages.
    // Field 7 is ri_phys_footprint in bytes.
    let status = unsafe { proc_pid_rusage(pid as i32, 2, (&mut usage as *mut RusageV2).cast()) };
    if status == 0 {
        Some((
            mach_ticks_to_ns(usage.fields[0].saturating_add(usage.fields[1]))?,
            usage.fields[7],
        ))
    } else {
        None
    }
}
#[cfg(target_os = "macos")]
fn mach_ticks_to_ns(ticks: u64) -> Option<u64> {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    static TIMEBASE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    let (numer, denom) = (*TIMEBASE.get_or_init(|| {
        let mut info = Timebase { numer: 0, denom: 0 };
        let status = unsafe { mach_timebase_info(&mut info) };
        if status == 0 && info.denom != 0 {
            Some((info.numer, info.denom))
        } else {
            None
        }
    }))?;
    Some(((ticks as u128 * numer as u128) / denom as u128).min(u64::MAX as u128) as u64)
}
#[cfg(target_os = "macos")]
fn spawn_nettop(pid: u32) -> std::io::Result<(Child, std::fs::File)> {
    use std::os::fd::FromRawFd;
    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, ...) -> i32;
        fn openpty(
            master: *mut i32,
            slave: *mut i32,
            name: *mut std::ffi::c_char,
            term: *const std::ffi::c_void,
            window: *const std::ffi::c_void,
        ) -> i32;
    }
    let mut master = -1;
    let mut slave = -1;
    // A PTY makes nettop line-buffer stdout. A pipe otherwise retains samples
    // in stdio buffers for minutes, so live network graphs never update.
    let status = unsafe {
        openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let output = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    use std::os::fd::AsRawFd;
    // Prevent other VM/collector spawns from inheriting either PTY endpoint.
    // Child stdout is duped by Command and remains open after exec.
    for fd in [output.as_raw_fd(), slave.as_raw_fd()] {
        if unsafe { fcntl(fd, 2, 1) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let child = Command::new("/usr/bin/nettop")
        .args([
            "-P",
            "-L",
            "0",
            "-n",
            "-x",
            "-d",
            "-s",
            "1",
            "-J",
            "bytes_in,bytes_out",
            "-p",
            &pid.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave))
        .stderr(Stdio::null())
        .spawn()?;
    Ok((child, output))
}
#[cfg(not(target_os = "macos"))]
fn process_sample(_: u32) -> Option<(u64, u64)> {
    None
}

/// Publish exactly once per complete CSV interval, including intervals in
/// which the target process has no sockets. Row parsing never changes the clock.
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct NetworkCsv {
    columns: Option<(usize, usize)>,
    at: Option<Instant>,
    pending: (u64, u64),
    valid: bool,
}
#[cfg(any(target_os = "macos", test))]
impl NetworkCsv {
    fn ingest(&mut self, line: &str, pid: u32, now: Instant) -> Option<(u64, u64, Instant)> {
        let values = csv_fields(line);
        if let (Some(rx), Some(tx)) = (
            values.iter().position(|v| v == "bytes_in"),
            values.iter().position(|v| v == "bytes_out"),
        ) {
            let complete = self
                .at
                .filter(|_| self.valid)
                .map(|at| (self.pending.0, self.pending.1, at));
            self.columns = Some((rx, tx));
            self.at = Some(now);
            self.pending = (0, 0);
            self.valid = true;
            return complete;
        }
        if let Some((rx, tx)) = self.columns {
            if let Some((rx, tx)) = parse_process_row(&values, pid, rx, tx) {
                self.pending = (rx, tx);
            } else if values
                .first()
                .and_then(|v| v.rsplit_once('.'))
                .and_then(|(_, v)| v.parse::<u32>().ok())
                == Some(pid)
            {
                self.valid = false;
            }
        }
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut value = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                value.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(std::mem::take(&mut value));
            }
            _ => value.push(c),
        }
    }
    fields.push(value);
    fields
}
#[cfg(any(target_os = "macos", test))]
fn parse_process_row(values: &[String], pid: u32, rx: usize, tx: usize) -> Option<(u64, u64)> {
    let (_, row_pid) = values.first()?.rsplit_once('.')?;
    if row_pid.parse::<u32>().ok()? != pid {
        return None;
    }
    Some((values.get(rx)?.parse().ok()?, values.get(tx)?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_interval_supports_multicore_and_resets() {
        assert_eq!(cpu_delta(1_000_000_000, 5_000_000_000, 2.), Some(200.));
        assert_eq!(cpu_delta(9, 1, 1.), None);
        assert_eq!(cpu_delta(0, 1, 0.), None);
    }
    #[test]
    fn csv_matches_exact_pid_and_reordered_columns() {
        let row = csv_fields("\"qemu, ARM.123\",42,99,");
        assert_eq!(parse_process_row(&row, 123, 2, 1), Some((99, 42)));
        assert_eq!(parse_process_row(&row, 12, 2, 1), None);
        assert_eq!(
            parse_process_row(&csv_fields("qemu.123,?,0"), 123, 1, 2),
            None
        );
    }
    #[test]
    fn network_deltas_accumulate_and_compute_interval_rates() {
        let now = Instant::now();
        let mut state = NetworkSample::default();
        state.add(100, 50, now);
        assert_eq!(state.rx_rate, None);
        state.add(200, 80, now + std::time::Duration::from_secs(2));
        assert_eq!(state.rx, 300);
        assert_eq!(state.tx, 130);
        assert_eq!(state.rx_rate, Some(100.));
        assert_eq!(state.tx_rate, Some(40.));
    }
    #[test]
    fn no_socket_to_traffic_transition_uses_full_interval() {
        let at = Instant::now();
        let mut parser = NetworkCsv::default();
        let mut sample = NetworkSample::default();
        assert!(parser.ingest(",bytes_in,bytes_out,", 123, at).is_none());
        let (rx, tx, time) = parser
            .ingest(
                ",bytes_in,bytes_out,",
                123,
                at + std::time::Duration::from_secs(1),
            )
            .unwrap();
        sample.add(rx, tx, time);
        assert!(parser
            .ingest(
                "qemu.123,200,100,",
                123,
                at + std::time::Duration::from_millis(1001)
            )
            .is_none());
        let (rx, tx, time) = parser
            .ingest(
                ",bytes_in,bytes_out,",
                123,
                at + std::time::Duration::from_secs(2),
            )
            .unwrap();
        sample.add(rx, tx, time);
        assert_eq!(sample.rx_rate, Some(200.));
        assert_eq!(sample.tx_rate, Some(100.));
    }
    #[test]
    fn disconnected_is_actual_zero_and_generations_reset() {
        let mut first = Monitor::new(std::process::id(), vm_core::NetworkMode::Disconnected);
        let sample = first.sample(std::process::id());
        assert!(sample.network_available);
        assert_eq!(sample.network_rx_bytes, Some(0));
        let second = Monitor::new(std::process::id(), vm_core::NetworkMode::Disconnected);
        assert_ne!(first.generation, second.generation);
        assert_eq!(second.network.lock().unwrap().rx, 0);
    }
    #[test]
    fn stale_collector_samples_are_unavailable_but_disconnected_stays_zero() {
        let mut monitor = Monitor::new(std::process::id(), vm_core::NetworkMode::Disconnected);
        {
            let mut network = monitor.network.lock().unwrap();
            network.add(100, 10, Instant::now() - std::time::Duration::from_secs(6));
        }
        let sample = monitor.sample(std::process::id());
        assert!(!sample.network_available);
        assert_eq!(sample.network_rx_bytes, None);
        assert_eq!(sample.network_rx_bytes_per_sec, None);
        let mut disconnected = Monitor::new(std::process::id(), vm_core::NetworkMode::Disconnected);
        assert_eq!(
            disconnected.sample(std::process::id()).network_rx_bytes,
            Some(0)
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires macOS process inspection, PTYs and loopback networking outside the sandbox"]
    fn live_macos_process_cpu_and_network_stream() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let mut monitor = Monitor::new(std::process::id(), vm_core::NetworkMode::Nat);
        monitor.sample(std::process::id());
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + std::time::Duration::from_secs(4);
            let mut buffer = [0u8; 512];
            while Instant::now() < deadline {
                socket.send_to(&buffer, address).unwrap();
                socket.recv_from(&mut buffer).unwrap();
                for n in 0..10000 {
                    std::hint::black_box(n * n);
                }
            }
        });
        std::thread::sleep(std::time::Duration::from_secs(4));
        let sample = monitor.sample(std::process::id());
        worker.join().unwrap();
        assert!(sample.cpu_percent.unwrap_or_default() > 20., "{sample:?}");
        assert!(sample.memory_bytes.unwrap_or_default() > 0, "{sample:?}");
        assert!(sample.network_available, "{sample:?}");
        assert!(
            sample.network_rx_bytes.unwrap_or_default() > 4096,
            "{sample:?}"
        );
        assert!(
            sample.network_tx_bytes.unwrap_or_default() > 4096,
            "{sample:?}"
        );
        assert!(
            sample.network_rx_bytes_per_sec.unwrap_or_default() > 0.,
            "{sample:?}"
        );
        eprintln!("Live process metrics: {sample:?}");
    }
    #[test]
    fn collector_is_reaped_on_drop() {
        let mut monitor = Monitor::new(std::process::id(), vm_core::NetworkMode::Disconnected);
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        #[cfg(target_os = "macos")]
        let pid = child.id();
        monitor.collector = Some(child);
        drop(monitor);
        #[cfg(target_os = "macos")]
        assert!(process_sample(pid).is_none());
    }
}
