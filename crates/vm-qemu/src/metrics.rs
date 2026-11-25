//! Host CPU and memory measurements, plus byte counts at the guest Ethernet device.
use serde::Serialize;
use std::{
    ffi::OsString,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use vm_core::NetworkMode;

#[cfg(unix)]
use crate::traffic::Traffic;

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
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Monitor {
    pub generation: u64,
    previous_cpu: Option<(Instant, u64)>,
    previous_network: Option<(Instant, u64, u64)>,
    mode: NetworkMode,
    network_note: &'static str,
    #[cfg(unix)]
    traffic: Option<Traffic>,
}

impl Monitor {
    pub fn new(mode: NetworkMode) -> Self {
        Self {
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            previous_cpu: None,
            previous_network: None,
            mode,
            network_note: "Virtual NIC counters are unavailable",
            #[cfg(unix)]
            traffic: None,
        }
    }

    pub fn attach_network(&mut self, runtime: &Path, args: &mut Vec<OsString>) {
        #[cfg(unix)]
        if self.mode != NetworkMode::Disconnected {
            match Traffic::start(runtime)
                .map_err(vm_core::VmError::from)
                .and_then(|traffic| Ok((traffic.arguments()?, traffic)))
            {
                Ok((network_args, traffic)) => {
                    args.extend(network_args);
                    self.traffic = Some(traffic);
                    self.network_note = "Waiting for virtual NIC counters";
                }
                Err(_) => self.network_note = "Could not initialize virtual NIC counters",
            }
        }
        #[cfg(not(unix))]
        let _ = (runtime, args);
    }

    fn network_totals(&self) -> Option<(u64, u64)> {
        if self.mode == NetworkMode::Disconnected {
            return Some((0, 0));
        }
        #[cfg(unix)]
        return self.traffic.as_ref()?.totals();
        #[cfg(not(unix))]
        None
    }

    pub fn sample(&mut self, pid: u32) -> VmMetrics {
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
        result.network_note = Some(self.network_note.into());
        if let Some((rx, tx)) = self.network_totals() {
            result.network_available = true;
            result.network_rx_bytes = Some(rx);
            result.network_tx_bytes = Some(tx);
            if self.mode == NetworkMode::Disconnected {
                result.network_note = Some("VM network is disconnected".into());
                result.network_rx_bytes_per_sec = Some(0.);
                result.network_tx_bytes_per_sec = Some(0.);
            } else {
                result.network_note = None;
                if let Some((previous, previous_rx, previous_tx)) = self.previous_network {
                    let elapsed = now.saturating_duration_since(previous).as_secs_f64();
                    if elapsed > 0. {
                        result.network_rx_bytes_per_sec = rx
                            .checked_sub(previous_rx)
                            .map(|bytes| bytes as f64 / elapsed);
                        result.network_tx_bytes_per_sec = tx
                            .checked_sub(previous_tx)
                            .map(|bytes| bytes as f64 / elapsed);
                    }
                }
                self.previous_network = Some((now, rx, tx));
            }
        } else {
            self.previous_network = None;
        }
        result
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
#[cfg(not(target_os = "macos"))]
fn process_sample(_: u32) -> Option<(u64, u64)> {
    None
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
    fn disconnected_is_actual_zero_and_generations_reset() {
        let mut first = Monitor::new(NetworkMode::Disconnected);
        let sample = first.sample(std::process::id());
        assert!(sample.network_available);
        assert_eq!(sample.network_rx_bytes, Some(0));
        assert_eq!(sample.network_tx_bytes_per_sec, Some(0.));
        let second = Monitor::new(NetworkMode::Disconnected);
        assert_ne!(first.generation, second.generation);
    }
}
