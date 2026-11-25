//! Per-NIC traffic accounting from QEMU's two truncated pcap streams.
use std::{
    ffi::{CString, OsString},
    fs::{self, DirBuilder, File},
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        Arc,
    },
    thread::{self, JoinHandle},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SNAPLEN: u32 = 1;

pub(crate) struct Traffic {
    rx: Counter,
    tx: Counter,
    directory: Directory,
}

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("rx"));
        let _ = fs::remove_file(self.0.join("tx"));
        let _ = fs::remove_dir(&self.0);
    }
}

impl Traffic {
    pub fn start(runtime: &Path) -> io::Result<Self> {
        let directory = runtime.join(format!(
            "traffic-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&directory)?;
        let directory = Directory(directory);
        Ok(Self {
            rx: Counter::start(&directory.0.join("rx"))?,
            tx: Counter::start(&directory.0.join("tx"))?,
            directory,
        })
    }

    pub fn arguments(&self) -> Result<Vec<OsString>, vm_core::VmError> {
        let mut args = Vec::new();
        // QEMU names queues from the backend's perspective. Backend TX is guest RX.
        for (direction, queue) in [("rx", "tx"), ("tx", "rx")] {
            let path = super::escape_option(&self.directory.0.join(direction))?;
            args.extend([
                "-object".into(),
                format!("filter-dump,id=traffic-{direction},netdev=net0,queue={queue},file={path},maxlen={SNAPLEN}").into(),
            ]);
        }
        Ok(args)
    }

    pub fn totals(&self) -> Option<(u64, u64)> {
        Some((self.rx.total()?, self.tx.total()?))
    }
}

#[derive(Default)]
struct State {
    ready: AtomicBool,
    stop: AtomicBool,
    total: AtomicU64,
}

struct Counter {
    state: Arc<State>,
    worker: Option<JoinHandle<()>>,
}

impl Counter {
    fn start(path: &Path) -> io::Result<Self> {
        let name = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: name is a valid C string. The FIFO lives in our private directory.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // Holding both ends lets QEMU open without waiting, and avoids EOF before launch.
        let fifo = File::options()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)?;
        let state = Arc::new(State::default());
        let worker_state = state.clone();
        let worker = thread::Builder::new()
            .name("virmora-traffic".into())
            .spawn(move || {
                let _ = count_packets(fifo, &worker_state);
                worker_state.ready.store(false, Relaxed);
            })?;
        Ok(Self {
            state,
            worker: Some(worker),
        })
    }

    fn total(&self) -> Option<u64> {
        self.state
            .ready
            .load(Relaxed)
            .then(|| self.state.total.load(Relaxed))
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        self.state.stop.store(true, Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn count_packets(mut fifo: File, state: &State) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd: fifo.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let mut buffer = [0; 8192];
    let mut pcap = Pcap::default();
    while !state.stop.load(Relaxed) {
        // SAFETY: descriptor remains valid and the file is owned by this thread.
        let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if ready > 0 {
            let count = match fifo.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(error),
            };
            let bytes = pcap.ingest(&buffer[..count])?;
            state.total.fetch_add(bytes, Relaxed);
            state.ready.store(pcap.header, Relaxed);
        }
    }
    Ok(())
}

#[derive(Default)]
struct Pcap {
    data: Vec<u8>,
    header: bool,
}

impl Pcap {
    fn ingest(&mut self, bytes: &[u8]) -> io::Result<u64> {
        const HEADER: usize = 24;
        const RECORD: usize = 16;
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid QEMU counter stream");
        self.data.extend_from_slice(bytes);
        let mut offset = 0;
        if !self.header {
            if self.data.len() < HEADER {
                return Ok(0);
            }
            if self.u32_at(0) != 0xa1b2c3d4 || self.u32_at(16) != SNAPLEN || self.u32_at(20) != 1 {
                return Err(invalid());
            }
            self.header = true;
            offset = HEADER;
        }
        let mut total = 0;
        while self.data.len() - offset >= RECORD {
            let captured = self.u32_at(offset + 8);
            let original = self.u32_at(offset + 12);
            if captured != original.min(SNAPLEN) {
                return Err(invalid());
            }
            let end = offset + RECORD + captured as usize;
            if end > self.data.len() {
                break;
            }
            total += u64::from(original);
            offset = end;
        }
        self.data.drain(..offset);
        Ok(total)
    }

    fn u32_at(&self, offset: usize) -> u32 {
        u32::from_ne_bytes(self.data[offset..offset + 4].try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_packet_lengths_across_stream_boundaries() {
        let mut input = Vec::new();
        for value in [0xa1b2c3d4u32, 0, 0, 0, SNAPLEN, 1, 0, 0, 1, 1514] {
            input.extend(value.to_ne_bytes());
        }
        input.push(0);
        let mut parser = Pcap::default();
        assert_eq!(parser.ingest(&input[..20]).unwrap(), 0);
        assert_eq!(parser.ingest(&input[20..40]).unwrap(), 0);
        assert_eq!(parser.ingest(&input[40..]).unwrap(), 1514);
    }
}
