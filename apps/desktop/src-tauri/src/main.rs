#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::Arc,
};
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;
use vm_core::{BackendKind, BootMode, GuestArchitecture, VmConfig, VmInfo, VmSettingsUpdate};
use vm_manager::VmManager;
use vm_qemu::{QemuBackend, VmMetrics};
struct Desktop {
    manager: Arc<VmManager>,
    backend: Arc<QemuBackend>,
    vm_directory: PathBuf,
}
#[derive(serde::Deserialize)]
struct CreateVm {
    #[serde(default)]
    vm_directory: Option<PathBuf>,
    name: String,
    cpus: u8,
    memory_mib: u64,
    disk_size_gib: u64,
    #[serde(default)]
    iso_path: Option<PathBuf>,
    architecture: GuestArchitecture,
    #[serde(default)]
    guest_os: vm_core::GuestOs,
    drivers_iso_path: Option<PathBuf>,
    #[serde(default)]
    network_mode: vm_core::NetworkMode,
    #[serde(default)]
    bridge_interface: Option<String>,
}
#[tauri::command]
async fn list_bridge_interfaces() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let output = std::process::Command::new("/sbin/ifconfig")
            .arg("-l")
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err("Could not list Mac network adapters".into());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .filter(|name| {
                name.strip_prefix("en")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .map(str::to_owned)
            .collect())
    })
    .await
    .map_err(|error| error.to_string())?
}
#[tauri::command]
async fn check_network(
    mode: vm_core::NetworkMode,
    adapter: String,
) -> Result<vm_qemu::vmnet::NetworkReadiness, String> {
    tauri::async_runtime::spawn_blocking(move || vm_qemu::vmnet::readiness(mode, &adapter))
        .await
        .map_err(|error| error.to_string())
}
#[tauri::command]
fn list_vms(state: State<'_, Desktop>) -> Result<Vec<VmInfo>, String> {
    state.manager.list().map_err(|e| e.to_string())
}
#[tauri::command]
fn create_vm(state: State<'_, Desktop>, request: CreateVm) -> Result<VmInfo, String> {
    if request.guest_os == vm_core::GuestOs::Windows11 && !cfg!(target_arch = "aarch64") {
        return Err("The Windows 11 ARM64 profile requires an Apple Silicon Mac".into());
    }
    let iso_path = request
        .iso_path
        .ok_or("Select an ISO image before creating a machine")?;
    // Internal IDs are generated, never entered by the user.
    let id = format!(
        "vm-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let parent = request
        .vm_directory
        .unwrap_or_else(|| state.vm_directory.clone());
    if !parent.is_absolute() || !parent.is_dir() {
        return Err("Select an existing folder for this virtual machine".into());
    }
    let parent = fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let machine_directory = parent.join(&id);
    let config = VmConfig {
        guest_os: request.guest_os,
        drivers_iso_path: request.drivers_iso_path,
        disk_path: machine_directory.join("disk.raw"),
        id,
        name: request.name,
        cpus: request.cpus,
        memory_mib: request.memory_mib,
        disk_size_gib: request.disk_size_gib,
        kernel_path: PathBuf::new(),
        iso_path: Some(iso_path),
        initrd_path: None,
        kernel_cmdline: String::new(),
        architecture: request.architecture,
        backend: BackendKind::Qemu,
        network_mode: request.network_mode,
        bridge_interface: request.bridge_interface,
        boot_mode: BootMode::Installer,
        firmware_code_path: None,
        firmware_vars_template_path: None,
    };
    config.validate().map_err(|error| error.to_string())?;
    fs::create_dir_all(&machine_directory).map_err(|error| error.to_string())?;
    state.manager.create(config).map_err(|e| e.to_string())
}
#[tauri::command]
async fn start_vm(state: State<'_, Desktop>, id: String) -> Result<VmInfo, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.start(&id).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn shutdown_vm(state: State<'_, Desktop>, id: String) -> Result<VmInfo, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.shutdown(&id).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn restart_vm(state: State<'_, Desktop>, id: String) -> Result<VmInfo, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.restart(&id).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn force_stop_vm(state: State<'_, Desktop>, id: String) -> Result<VmInfo, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.force_stop(&id).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn update_vm_settings(
    state: State<'_, Desktop>,
    id: String,
    update: VmSettingsUpdate,
) -> Result<VmInfo, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager
            .update_settings(&id, update)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn delete_vm(state: State<'_, Desktop>, id: String, keep_disk: bool) -> Result<(), String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.delete(&id, keep_disk).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[derive(serde::Serialize)]
struct VmDiagnostics {
    last_error: Option<String>,
    console: String,
    log: String,
}
#[tauri::command]
async fn get_vm_diagnostics(
    state: State<'_, Desktop>,
    id: String,
) -> Result<VmDiagnostics, String> {
    let manager = state.manager.clone();
    let backend = state.backend.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<VmDiagnostics, vm_core::VmError> {
        Ok(VmDiagnostics {
            last_error: manager.last_error(&id)?,
            console: read_log_tail(manager.console_path(&id)?)?,
            log: read_log_tail(backend.log_path(&id)?)?,
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}
#[tauri::command]
async fn set_boot_mode(
    state: State<'_, Desktop>,
    id: String,
    mode: BootMode,
) -> Result<VmInfo, String> {
    let manager = state.manager.clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager
            .update_boot_mode(&id, mode)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_vm_metrics(state: State<'_, Desktop>, id: String) -> Result<VmMetrics, String> {
    let manager = state.manager.clone();
    let backend = state.backend.clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.get(&id).map_err(|e| e.to_string())?;
        backend.metrics(&id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn read_console(state: State<'_, Desktop>, id: String) -> Result<String, String> {
    let path = state.manager.console_path(&id).map_err(|e| e.to_string())?;
    read_log_tail(path).map_err(|e| e.to_string())
}

fn read_log_tail(path: PathBuf) -> std::io::Result<String> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(128 * 1024)))?;
    let mut bytes = Vec::new();
    file.take(128 * 1024).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
#[tauri::command]
fn send_console(state: State<'_, Desktop>, id: String, text: String) -> Result<(), String> {
    use std::io::Write;
    if text.len() > 4096 {
        return Err("Console input is limited to 4096 bytes".into());
    }
    if state.manager.get(&id).map_err(|e| e.to_string())?.state != vm_core::VmState::Running {
        return Err("Machine is not running".into());
    }
    #[cfg(unix)]
    {
        let path = state
            .backend
            .serial_socket_path(&id)
            .map_err(|e| e.to_string())?;
        let mut stream =
            std::os::unix::net::UnixStream::connect(path).map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(text.as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        Err("Serial input requires a Unix host".into())
    }
}
#[derive(serde::Serialize)]
struct DesktopSettings {
    vm_directory: PathBuf,
}
#[tauri::command]
fn get_desktop_settings(state: State<'_, Desktop>) -> DesktopSettings {
    DesktopSettings {
        vm_directory: state.vm_directory.clone(),
    }
}
#[tauri::command]
async fn choose_vm_folder(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
) -> Result<Option<PathBuf>, String> {
    let directory = state.vm_directory.clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Select virtual machine folder")
            .set_directory(directory)
            .blocking_pick_folder()
            .map(|path| path.into_path().map_err(|e| e.to_string()))
            .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn choose_iso_file(app: tauri::AppHandle) -> Result<Option<PathBuf>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Select ISO Image")
            .add_filter("ISO Image", &["iso"])
            .blocking_pick_file()
            .map(|path| path.into_path().map_err(|e| e.to_string()))
            .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
}
fn main() {
    // GUI apps launched from Finder may not inherit Homebrew's shell PATH.
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{path}:/opt/homebrew/bin:/usr/local/bin"));
    let root = std::env::var_os("VIRMORA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".virmora")
        });
    let vm_directory = if std::env::var_os("VIRMORA_HOME").is_some() {
        root.join("virtual machines")
    } else {
        PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"))
            .join("vermora")
            .join("virtual machines")
    };
    fs::create_dir_all(&vm_directory).expect("cannot create virtual machine directory");
    fs::create_dir_all(&root).expect("cannot create Virmora data directory");
    let root = fs::canonicalize(root).expect("cannot resolve Virmora directory");
    let backend = Arc::new(QemuBackend::new(root.join("run")));
    let manager = Arc::new(
        VmManager::new(&root, backend.clone())
            .expect("cannot open VM registry; close other Virmora processes"),
    );
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Desktop {
            manager,
            backend,
            vm_directory,
        })
        .invoke_handler(tauri::generate_handler![
            list_vms,
            list_bridge_interfaces,
            check_network,
            create_vm,
            start_vm,
            shutdown_vm,
            restart_vm,
            force_stop_vm,
            update_vm_settings,
            delete_vm,
            get_vm_diagnostics,
            set_boot_mode,
            read_console,
            get_vm_metrics,
            send_console,
            get_desktop_settings,
            choose_iso_file,
            choose_vm_folder
        ])
        .build(tauri::generate_context!())
        .expect("Virmora desktop failed")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                let state = app.state::<Desktop>();
                if let Ok(machines) = state.manager.list() {
                    for machine in machines {
                        if machine.state == vm_core::VmState::Running {
                            if let Err(error) = state.manager.stop(&machine.config.id) {
                                eprintln!("shutdown {}: {error}", machine.config.id);
                            }
                        }
                    }
                }
            }
        });
}
