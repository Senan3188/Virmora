use std::{
    io::{self, Write},
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};
use vm_core::{BootMode, VmConfig};
use vm_manager::VmManager;
use vm_qemu::QemuBackend;

fn main() {
    if let Err(error) = execute() {
        eprintln!("virmora: {error}");
        std::process::exit(1);
    }
}
fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var_os("VIRMORA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".virmora")
        });
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help();
        return Ok(());
    }
    let (headless, monitor) = if args[0] == "run" && args.len() >= 2 {
        parse_run_flags(&args[2..])?
    } else {
        (false, false)
    };
    let backend = Arc::new(QemuBackend::with_display(root.join("run"), !headless));
    let manager = VmManager::new(&root, backend.clone())?;
    match args[0].as_str() {
        "create" if args.len() == 2 => {
            let config: VmConfig = serde_json::from_slice(&std::fs::read(&args[1])?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&manager.create(config)?)?
            );
        }
        "list" => println!("{}", serde_json::to_string_pretty(&manager.list()?)?),
        "boot" if args.len() == 3 => {
            let mode = match args[2].as_str() {
                "installer" => BootMode::Installer,
                "disk" => BootMode::Disk,
                "auto" => BootMode::Auto,
                _ => return Err("boot mode must be installer, disk, or auto".into()),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&manager.update_boot_mode(&args[1], mode)?)?
            );
        }
        "run" if args.len() >= 2 => {
            let id = &args[1];
            manager.start(id)?;
            println!(
                "VM {id} started. Console log: {}",
                manager.console_path(id)?.display()
            );
            println!(
                "Interactive console in another terminal: nc -U {}",
                backend.serial_socket_path(id)?.display()
            );
            println!("Press Enter to stop the VM. This process owns the VM; keep it open.");
            io::stdout().flush()?;
            let input = if monitor {
                monitor_until_input(&backend, id)
            } else {
                let mut line = String::new();
                io::stdin().read_line(&mut line)
            };
            let stopped = manager.stop(id);
            input?;
            stopped?;
            println!("VM stopped; disk retained.");
        }
        "console" if args.len() == 2 => {
            print!(
                "{}",
                std::fs::read_to_string(manager.console_path(&args[1])?)?
            );
        }
        _ => {
            help();
            return Err("invalid arguments".into());
        }
    }
    Ok(())
}
fn parse_run_flags(flags: &[String]) -> Result<(bool, bool), &'static str> {
    let (mut headless, mut monitor) = (false, false);
    for flag in flags {
        match flag.as_str() {
            "--headless" if !headless => headless = true,
            "--monitor" if !monitor => monitor = true,
            _ => return Err("run accepts optional --headless and --monitor flags once each"),
        }
    }
    Ok((headless, monitor))
}

fn monitor_until_input(backend: &QemuBackend, id: &str) -> io::Result<usize> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = sender.send(io::stdin().read_line(&mut line));
    });
    loop {
        let sampled_at = Instant::now();
        let sample = backend
            .metrics(id)
            .map_err(|error| error.to_string())
            .and_then(|metrics| serde_json::to_string(&metrics).map_err(|error| error.to_string()));
        match sample {
            Ok(json) => {
                let mut output = io::stdout().lock();
                if let Err(error) = writeln!(output, "METRICS {json}").and_then(|_| output.flush())
                {
                    eprintln!("Metrics output error: {error}");
                }
            }
            Err(error) => eprintln!("Metrics error for {id}: {error}"),
        }
        // Sample serially; input interrupts the interval immediately on Enter or EOF.
        let remaining = Duration::from_secs(2).saturating_sub(sampled_at.elapsed());
        match receiver.recv_timeout(remaining) {
            Ok(input) => return input,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stdin worker stopped",
                ));
            }
        }
    }
}

fn help() {
    println!("Virmora — Linux VM MVP\n\n  virmora create CONFIG.json\n  virmora list\n  virmora run ID [--headless] [--monitor]   Start; press Enter to stop\n  virmora boot ID installer|disk|auto   Set next boot source while stopped\n  virmora console ID   Print captured serial output\n\nSet VIRMORA_HOME to change the persistent data directory.");
}

#[cfg(test)]
mod tests {
    use super::parse_run_flags;
    #[test]
    fn run_flags_are_composable_in_either_order() {
        for flags in [["--headless", "--monitor"], ["--monitor", "--headless"]] {
            assert_eq!(parse_run_flags(&flags.map(String::from)), Ok((true, true)));
        }
        assert_eq!(parse_run_flags(&[]), Ok((false, false)));
        assert!(parse_run_flags(&["--monitor".into(), "--monitor".into()]).is_err());
        assert!(parse_run_flags(&["--unknown".into()]).is_err());
    }
}
