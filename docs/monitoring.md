# VM resource monitoring

The VM details view shows live CPU, host memory and network charts. It samples the selected running VM every two seconds and keeps the latest 60 samples in memory (roughly two minutes). History resets when the VM changes or restarts. Stopped VMs show a stopped state rather than stale values; unavailable measurements show as unavailable rather than zero.

Virmora reads CPU and memory from the host process and traffic from the guest’s virtual network adapter.

- CPU: QEMU user and system CPU time divided by elapsed time. One fully used host core is 100%, so a multithreaded VM can exceed 100%. Apple Silicon Mach time units are converted using the host timebase before calculating percentages.
- Memory: the QEMU process physical memory footprint reported by macOS. This includes virtualization overhead and is distinct from both configured guest RAM and the guest operating system's used memory. Guest-internal usage would require a guest agent.
- Network: received and sent Ethernet bytes at the VM’s virtual adapter. Counts include link traffic such as DHCP and work in Shared / NAT, NAT Network, Bridged, Host-only, and Internal Network. Disconnected reports zero.

QEMU supplies two truncated packet streams via private FIFOs, one per direction. Virmora counts each original frame length and discards the captured byte in memory. It writes no packet payloads to disk and requires no external monitor or guest agent. Totals reset when the VM restarts, and rates use the difference between successive samples. macOS process-inspection failures can make CPU and memory unavailable independently of network traffic.

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

The opt-in Internal Network test also verifies that both VMs report nonzero receive and send totals after exchanging ICMP traffic over their virtual Ethernet adapters. See [VM validation](validation.md) for environment setup. Real tests require Unix sockets and host virtualization and may need to run outside a restrictive sandbox.
