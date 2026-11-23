# VM resource monitoring

The VM details view shows live CPU, host memory and network charts. It samples the selected running VM every two seconds and keeps the latest 60 samples in memory (roughly two minutes). History resets when the VM changes or restarts. Stopped VMs show a stopped state rather than stale values; unavailable measurements show as unavailable rather than zero.

These are host-side measurements of the owned QEMU process, not measurements from inside the guest:

- CPU: QEMU user and system CPU time divided by elapsed time. One fully used host core is 100%, so a multithreaded VM can exceed 100%. Apple Silicon Mach time units are converted using the host timebase before calculating percentages.
- Memory: the QEMU process physical memory footprint reported by macOS. This includes virtualization overhead and is distinct from both configured guest RAM and the guest operating system's used memory. Guest-internal usage would require a guest agent.
- Network: observed host TCP/UDP socket bytes for the QEMU process, collected using macOS `nettop`. Receive/send rates and accumulated observed totals are shown. For NAT this reflects traffic through host sockets, not full virtual Ethernet traffic: guest DHCP/link traffic is excluded and short-lived sockets can be missed between samples. These totals are activity indicators, not exact traffic accounting.

Each running VM has its own process identity and instance generation. CPU deltas and network totals start over for a new instance. The network helper is stopped and reaped when the VM stops or exits, including application shutdown. No packets or guest payloads are captured or stored. Network collection uses a pseudo-terminal to obtain live output; pipe buffering must not delay the display. Missing/stale collector output is reported as unavailable. Disconnected networking reports actual zero traffic.

Process inspection and network collection currently target macOS. Other hosts report unsupported measurements as unavailable. macOS may also restrict process inspection or `nettop`; these failures do not prevent the VM from running. The native command executes off the UI thread. Frontend requests never overlap, and results for a previously selected VM are discarded.

## CLI

```sh
cargo run -p virmora-cli -- run MACHINE_ID --monitor
cargo run -p virmora-cli -- run MACHINE_ID --headless --monitor
```

The owning CLI prints `METRICS {json}` lines immediately and then at two-second intervals. Press Enter or send EOF to stop the VM normally. The first CPU value is null until a second sample establishes an interval.

## Real workload verification

`scripts/smoke-monitoring.py` creates a fresh temporary Linux VM, measures idle behavior, starts a busy loop, allocates 64 MiB in the guest, and downloads a throttled 16 MiB fixture from a localhost HTTP server. This exercises real QEMU CPU time, host memory growth, and network receive/send counters without requiring guest access to the public internet. Only its new temporary guest disk is used.

```sh
cargo build -p virmora-cli
python3 scripts/smoke-monitoring.py /absolute/path/to/alpine-virt-kernel /absolute/path/to/alpine-virt-initramfs
```

On Apple Silicon with QEMU/HVF, the test passed with CPU rising to 98.9%, host footprint growing beyond the idle baseline after allocation, 16,826,330 observed received bytes, and peak receive throughput around 1.97 MB/s. Its verified samples and console logs are retained at `/tmp/vm-monitor-j9e_674i` on the development host. The guest HTTP request produced observed sent bytes and a nonzero send rate as well. A separate opt-in native-process test verified sustained receive and send rates using loopback UDP.

```sh
cargo test -p vm-qemu live_macos_process_cpu_and_network_stream -- --ignored --nocapture
```

The real tests need permission for Unix sockets, HVF/process inspection and local network connections, so run them outside a restrictive sandbox. Headless CI runs deterministic tests for CPU intervals, parsing, idle-to-traffic rates, stale samples, disconnected networking, instance resets and helper cleanup.
