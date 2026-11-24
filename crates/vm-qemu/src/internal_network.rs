//! An Ethernet hub for VMs owned by one Virmora process. No host network interface.
use std::{
    fs::{self, DirBuilder},
    io::{self, Read, Write},
    os::unix::{
        fs::DirBuilderExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_FRAME: usize = 65_536;
const MAX_PENDING: usize = 1024 * 1024;
const MAX_PEERS: usize = 64;

pub(crate) struct InternalNetwork {
    directory: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl InternalNetwork {
    pub(crate) fn start(socket: &Path) -> io::Result<Self> {
        let directory = socket.parent().unwrap().to_path_buf();
        fs::create_dir_all(directory.parent().unwrap())?;
        DirBuilder::new().mode(0o700).create(&directory)?;
        let mut network = Self {
            directory,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        };
        let listener = UnixListener::bind(socket)?;
        listener.set_nonblocking(true)?;
        let stop = network.stop.clone();
        network.worker = Some(
            thread::Builder::new()
                .name("virmora-internal-network".into())
                .spawn(move || {
                    let mut peers: Vec<Peer> = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        for _ in 0..MAX_PEERS {
                            match listener.accept() {
                                Ok((stream, _))
                                    if peers.len() < MAX_PEERS
                                        && stream.set_nonblocking(true).is_ok() =>
                                {
                                    peers.push(Peer {
                                        stream,
                                        input: Vec::new(),
                                        output: Vec::new(),
                                        alive: true,
                                    });
                                }
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                        for source in 0..peers.len() {
                            let peer = &mut peers[source];
                            if !peer.alive {
                                continue;
                            }
                            match peer.read_frames() {
                                Ok(frames) => {
                                    for frame in frames {
                                        for (index, target) in peers.iter_mut().enumerate() {
                                            if index == source || !target.alive {
                                                continue;
                                            }
                                            if target.output.len() + frame.len() <= MAX_PENDING {
                                                target.output.extend_from_slice(&frame);
                                            }
                                        }
                                    }
                                }
                                Err(_) => peer.alive = false,
                            }
                        }
                        peers.retain_mut(|peer| peer.alive && peer.flush().is_ok());
                        thread::sleep(Duration::from_millis(2));
                    }
                })?,
        );
        Ok(network)
    }
}

impl Drop for InternalNetwork {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(self.directory.join("lan.sock"));
        let _ = fs::remove_dir(&self.directory);
    }
}

struct Peer {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    alive: bool,
}

impl Peer {
    fn read_frames(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let mut buffer = [0; 16_384];
        match self.stream.read(&mut buffer) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => self.input.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        let mut frames = Vec::new();
        let mut offset = 0;
        // QEMU's stream netdev prefixes each Ethernet frame with a big-endian u32.
        while let Some(header) = self.input.get(offset..offset + 4) {
            let len = u32::from_be_bytes(header.try_into().unwrap()) as usize;
            if !(14..=MAX_FRAME).contains(&len) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let end = offset + 4 + len;
            let Some(frame) = self.input.get(offset..end) else {
                break;
            };
            frames.push(frame.to_vec());
            offset = end;
        }
        self.input.drain(..offset);
        Ok(frames)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.output.is_empty() {
            return Ok(());
        }
        match self.stream.write(&self.output) {
            Ok(count) => {
                self.output.drain(..count);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }
}
