#!/usr/bin/env python3
"""Real ARM64 Alpine guest test. Uses a new temporary disk, never a user's disk.
Run after cargo build -p virmora-cli. Requires local Alpine virt kernel/initramfs,
QEMU on PATH and permission for Unix sockets + HVF. HTTP test needs internet.
"""
import argparse
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('kernel', type=Path)
parser.add_argument('initramfs', type=Path)
args = parser.parse_args()
binary = Path(__file__).resolve().parents[1] / 'target/debug/virmora'
root = Path(tempfile.mkdtemp(prefix='vm-smoke-', dir='/tmp'))
env = dict(os.environ, VIRMORA_HOME=str(root))
config = dict(id='alpine', name='Alpine smoke', cpus=2, memory_mib=1024,
              disk_path=str(root/'disk.raw'), disk_size_gib=1,
              kernel_path=str(args.kernel.resolve()), initrd_path=str(args.initramfs.resolve()),
              kernel_cmdline='console=ttyAMA0 rdinit=/bin/sh', architecture='aarch64', backend='qemu')
config_path = root/'input'
config_path.write_text(json.dumps(config))
subprocess.run([str(binary), 'create', str(config_path)], env=env, check=True, capture_output=True)
log = root/'run/alpine/console.log'

def wait_for(pattern, offset, owner, timeout=45):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        data = log.read_text(errors='replace')[offset:] if log.exists() else ''
        if re.search(pattern, data):
            return data
        if owner.poll() is not None:
            raise RuntimeError('VM owner exited; see ' + str(root/'owner.log'))
        time.sleep(.1)
    raise RuntimeError(f'timed out waiting for {pattern}; inspect {log}')

try:
    for cycle in range(2):
        offset = len(log.read_text(errors='replace')) if log.exists() else 0
        with (root/'owner.log').open('a') as output:
            owner = subprocess.Popen([str(binary), 'run', 'alpine'], env=env,
                                     stdin=subprocess.PIPE, stdout=output, stderr=output)
            serial = None
            try:
                # Alpine's minimal initramfs provides /bin/sh but no preinstalled applet links.
                wait_for(r'Run /bin/sh as init process', offset, owner)
                serial = socket.socket(socket.AF_UNIX)
                serial.settimeout(3)
                serial.connect(str(root/'run/alpine/serial.sock'))
                def command(text, marker):
                    start = len(log.read_text(errors='replace'))
                    serial.sendall((text + "; printf '\\n" + marker + "\\n'\n").encode())
                    return wait_for(r'(?m)^' + marker + r'\r?$', start, owner)
                time.sleep(.5)
                serial.sendall(b'\x1b[1;1R\n')
                command('/bin/busybox --install -s /bin; export PATH=/bin:/sbin; '
                        'export TERM=dumb; mkdir -p /proc /sys /dev /tmp /mnt/disk /etc; '
                        'mount -t proc proc /proc; mount -t sysfs sysfs /sys; '
                        'mount -t devtmpfs devtmpfs /dev; '
                        'modprobe virtio_pci; modprobe virtio_blk; modprobe virtio_net; modprobe vfat', 'READY')
                if cycle == 0:
                    result = command('mkfs.vfat /dev/vda && mount -t vfat /dev/vda /mnt/disk && '
                                     'echo virmora-persistent > /mnt/disk/proof && sync && '
                                     'umount /mnt/disk && echo FILE_WRITTEN', 'WRITE_DONE')
                    assert re.search(r'(?m)^FILE_WRITTEN\r?$', result), result
                    result = command('ip link set eth0 up; udhcpc -i eth0 -n -q -s /bin/true; '
                                     'ip addr add 10.0.2.15/24 dev eth0; ip route add default via 10.0.2.2; '
                                     'echo nameserver 10.0.2.3 > /etc/resolv.conf; '
                                     'wget -T 15 -O /tmp/net http://example.com && '
                                     'test -s /tmp/net && echo NAT_HTTP_OK', 'NET_DONE')
                    assert 'lease of 10.0.2.15 obtained' in result, result
                    assert re.search(r'(?m)^NAT_HTTP_OK\r?$', result), result
                else:
                    result = command('mount -t vfat /dev/vda /mnt/disk && '
                                     'test "$(cat /mnt/disk/proof)" = virmora-persistent && '
                                     'echo PERSISTENCE_OK; umount /mnt/disk', 'READ_DONE')
                    assert re.search(r'(?m)^PERSISTENCE_OK\r?$', result), result
            finally:
                if serial:
                    serial.close()
                owner.communicate(b'\n', timeout=15)
                assert owner.returncode == 0, (root/'owner.log').read_text()
    print('PASS: Linux shell, guest filesystem persistence across restart, DHCP and outbound NAT HTTP')
finally:
    print(f'Artifacts retained for inspection: {root}')
