#!/usr/bin/env python3
"""Verify real VM CPU, memory and network monitoring using a new temporary guest.
Requires Alpine ARM64 virt kernel/initramfs, QEMU and macOS process-stat access.
Network traffic stays between the isolated guest and a localhost HTTP fixture.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('kernel', type=Path)
parser.add_argument('initramfs', type=Path)
args = parser.parse_args()
root = Path(tempfile.mkdtemp(prefix='vm-monitor-', dir='/tmp'))
env = dict(os.environ, VIRMORA_HOME=str(root))
binary = Path(__file__).resolve().parents[1]/'target/debug/virmora'
config = dict(id='monitor', name='Monitoring smoke', cpus=2, memory_mib=512,
              disk_path=str(root/'disk.raw'), disk_size_gib=1,
              kernel_path=str(args.kernel.resolve()), initrd_path=str(args.initramfs.resolve()),
              kernel_cmdline='console=ttyAMA0 rdinit=/bin/sh', architecture='aarch64', backend='qemu')
(root/'input').write_text(json.dumps(config))
subprocess.run([str(binary), 'create', str(root/'input')], env=env, check=True, capture_output=True)

class Payload(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Length', str(16*1024*1024))
        self.end_headers()
        try:
            for _ in range(256):
                self.wfile.write(b'v'*65536)
                self.wfile.flush()
                time.sleep(.03)
        except (BrokenPipeError, ConnectionResetError):
            pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Payload)
threading.Thread(target=server.serve_forever, daemon=True).start()
output = (root/'owner.log').open('w')
owner = subprocess.Popen([str(binary), 'run', 'monitor', '--headless', '--monitor'],
                         env=env, stdin=subprocess.PIPE, stdout=output, stderr=output)
log = root/'run/monitor/console.log'
serial = None

def content():
    return log.read_text(errors='replace') if log.exists() else ''

def wait(pattern, offset=0, timeout=60):
    end = time.monotonic()+timeout
    while time.monotonic()<end:
        text = content()[offset:]
        if re.search(pattern, text):
            return text
        if owner.poll() is not None:
            raise RuntimeError((root/'owner.log').read_text())
        time.sleep(.1)
    raise RuntimeError(f'Guest timed out: {pattern}\n'+content()[-3000:])

def command(text, marker):
    offset = len(content())
    serial.sendall((text+"; printf '\\n"+marker+"\\n'\n").encode())
    return wait(r'(?m)^'+marker+r'\r?$', offset)

def samples():
    return [json.loads(line.removeprefix('METRICS '))
            for line in (root/'owner.log').read_text().splitlines() if line.startswith('METRICS ')]

try:
    path = root/'run/monitor/serial.sock'
    deadline = time.monotonic()+15
    while time.monotonic()<deadline:
        try:
            serial = socket.socket(socket.AF_UNIX)
            serial.settimeout(1)
            serial.connect(str(path))
            break
        except OSError:
            serial.close()
            serial = None
            time.sleep(.1)
    assert serial is not None, 'Serial socket never appeared'
    connection = serial
    def drain():
        while True:
            try:
                if not connection.recv(65536):
                    break
            except socket.timeout:
                continue
            except OSError:
                break
    threading.Thread(target=drain, daemon=True).start()
    wait(r'job control turned off')
    serial.sendall(b'\x1b[1;1R\n')
    command('/bin/busybox --install -s /bin; export PATH=/bin:/sbin; export TERM=dumb; '
            'mkdir -p /proc /sys /dev /tmp; mount -t proc proc /proc; mount -t sysfs sysfs /sys; '
            'mount -t devtmpfs devtmpfs /dev; modprobe virtio_pci; modprobe virtio_net; '
            'ip link set eth0 up; ip addr add 10.0.2.15/24 dev eth0; '
            'ip route add default via 10.0.2.2', 'READY')
    print('Sampling idle VM…', flush=True)
    time.sleep(5)
    baseline = samples()[-2:]
    assert any(s.get('memory_bytes', 0) and s['memory_bytes'] > 0 for s in baseline), baseline
    command('dd if=/dev/zero of=/tmp/memory-load bs=1M count=64; yes > /dev/null & '
            'echo $! > /tmp/busy.pid', 'LOAD_STARTED')
    print('Sampling CPU load, 64 MiB guest allocation and localhost network transfer…', flush=True)
    command(f'wget -O /tmp/download http://10.0.2.2:{server.server_port}/payload && '
            'test "$(wc -c < /tmp/download)" = 16777216 && echo DOWNLOAD_OK', 'TRANSFER_DONE')
    time.sleep(5)
    observed = samples()
    (root/'samples.json').write_text(json.dumps(observed, indent=2))
    assert len(observed) >= 4, 'Too few monitoring samples'
    assert max(s.get('cpu_percent') or 0 for s in observed) > max(s.get('cpu_percent') or 0 for s in baseline)+20, 'CPU reading did not rise under load'
    assert max(s.get('memory_bytes') or 0 for s in observed) > max(s.get('memory_bytes') or 0 for s in baseline)+32*1024*1024, 'Memory footprint did not rise after guest allocation'
    assert max(s.get('network_rx_bytes') or 0 for s in observed) >= 4*1024*1024, 'Observed receive bytes missing'
    assert max(s.get('network_tx_bytes') or 0 for s in observed) > 0, 'Observed transmit bytes missing'
    assert max(s.get('network_rx_bytes_per_sec') or 0 for s in observed) > 0, 'Receive rate missing'
    assert all(s.get('pid') == observed[0]['pid'] for s in observed), observed
    (root/'verified-samples.json').write_text(json.dumps(observed, indent=2))
    command('kill "$(cat /tmp/busy.pid)"; sync', 'CLEANED')
    print('PASS: live per-VM CPU load, physical memory growth, receive/transmit totals and receive rate', flush=True)
finally:
    if serial:
        serial.close()
    owner.communicate(b'\n', timeout=20)
    output.close()
    server.shutdown()
    server.server_close()
    print(f'Artifacts retained: {root}', flush=True)
    assert owner.returncode == 0, (root/'owner.log').read_text()
