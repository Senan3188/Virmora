use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use vm_core::{NetworkMode, VmError};

const DIRECTORY: &str = "/Library/Application Support/Virmora Networking/sockets";

pub(crate) fn socket_path(mode: NetworkMode, adapter: &str) -> Option<PathBuf> {
    let socket = match mode {
        NetworkMode::NatNetwork => "shared.sock".into(),
        NetworkMode::HostOnly => "host.sock".into(),
        NetworkMode::Bridged if vm_core::valid_bridge_interface(adapter) => {
            format!("bridged-{adapter}.sock")
        }
        _ => return None,
    };
    Some(Path::new(DIRECTORY).join(socket))
}

#[derive(serde::Serialize)]
pub struct NetworkReadiness {
    pub ready: bool,
    pub message: String,
}

#[cfg(target_os = "macos")]
fn client() -> Option<PathBuf> {
    [
        "/Library/Application Support/Virmora Networking/socket_vmnet_client",
        "/opt/homebrew/opt/socket_vmnet/bin/socket_vmnet_client",
        "/usr/local/opt/socket_vmnet/bin/socket_vmnet_client",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

pub(crate) fn command(
    binary: &str,
    mut args: Vec<OsString>,
    socket: Option<&Path>,
) -> Result<Command, VmError> {
    #[cfg(target_os = "macos")]
    if let Some(socket) = socket.filter(|path| path.exists()) {
        let client = client().ok_or_else(|| {
            VmError::Backend(
                "Install socket_vmnet_client to use the configured network service.".into(),
            )
        })?;
        let netdev = args
            .windows(2)
            .position(|pair| pair[0] == "-netdev")
            .ok_or_else(|| VmError::Backend("VM network arguments are missing".into()))?;
        args[netdev + 1] = "socket,id=net0,fd=3".into();
        let mut command = Command::new(client);
        command.arg(socket).arg(binary).args(args);
        return Ok(command);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (&mut args, socket);
    let mut command = Command::new(binary);
    command.args(args);
    Ok(command)
}

/// Test the selected transport by opening it through the same QEMU launch path as a VM.
pub fn readiness(mode: NetworkMode, adapter: &str) -> NetworkReadiness {
    if !cfg!(target_os = "macos")
        && matches!(
            mode,
            NetworkMode::NatNetwork | NetworkMode::HostOnly | NetworkMode::Bridged
        )
    {
        return NetworkReadiness {
            ready: false,
            message: "This network mode requires macOS.".into(),
        };
    }
    let Some(socket) = socket_path(mode, adapter) else {
        return NetworkReadiness {
            ready: !matches!(mode, NetworkMode::Bridged),
            message: if mode == NetworkMode::Bridged {
                "Choose a valid Mac network adapter."
            } else {
                "No macOS networking service required."
            }
            .into(),
        };
    };
    let netdev = match mode {
        NetworkMode::NatNetwork => "vmnet-shared,id=net0".into(),
        NetworkMode::HostOnly => "vmnet-host,id=net0".into(),
        NetworkMode::Bridged => format!("vmnet-bridged,id=net0,ifname={adapter}"),
        _ => unreachable!(),
    };
    let (qemu, machine) = if cfg!(target_arch = "aarch64") {
        ("qemu-system-aarch64", "virt")
    } else {
        ("qemu-system-x86_64", "q35")
    };
    let args = [
        "-machine",
        machine,
        "-cpu",
        "max",
        "-accel",
        "tcg",
        "-m",
        "128",
        "-nodefaults",
        "-display",
        "none",
        "-monitor",
        "none",
        "-S",
        "-netdev",
        &netdev,
        "-device",
        "virtio-net-pci,netdev=net0,romfile=",
        "-qmp",
        "stdio",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let result = (|| -> Result<(), VmError> {
        use std::{
            io::Write,
            thread,
            time::{Duration, Instant},
        };
        let mut child = command(qemu, args, Some(&socket))?
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"execute\":\"qmp_capabilities\"}\n{\"execute\":\"quit\"}\n");
        let until = Instant::now() + Duration::from_secs(4);
        while Instant::now() < until {
            if let Ok(Some(status)) = child.try_wait() {
                return if status.success() && input.is_ok() {
                    Ok(())
                } else {
                    Err(VmError::Backend(
                        "QEMU could not open the selected network.".into(),
                    ))
                };
            }
            thread::sleep(Duration::from_millis(40));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(VmError::Backend("The network check timed out.".into()))
    })();
    match result {
        Ok(()) => NetworkReadiness { ready: true, message: "QEMU opened this network successfully.".into() },
        Err(error) => NetworkReadiness { ready: false, message: format!("{error} Start the matching Virmora networking service, or use an authorized QEMU build.") },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_paths_are_separate_and_reject_invalid_adapters() {
        assert!(socket_path(NetworkMode::NatNetwork, "")
            .unwrap()
            .ends_with("shared.sock"));
        assert!(socket_path(NetworkMode::HostOnly, "")
            .unwrap()
            .ends_with("host.sock"));
        assert!(socket_path(NetworkMode::Bridged, "en2")
            .unwrap()
            .ends_with("bridged-en2.sock"));
        assert_eq!(socket_path(NetworkMode::Bridged, "en0,multicast=off"), None);
        assert_eq!(socket_path(NetworkMode::Nat, ""), None);
    }
}
