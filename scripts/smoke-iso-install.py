#!/usr/bin/env python3
"""Install an official Alpine ARM64 virt ISO on a NEW temporary VM, then boot its disk.
Requires macOS QEMU/HVF + firmware, Python 3 and internet for Alpine packages.
Only the newly created guest disk is formatted. Logs, disk, NVRAM and screenshots
are retained under /tmp/virmora-iso-* for inspection. Run cargo build -p virmora-cli first.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time
import threading

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('iso', type=Path)
parser.add_argument('--sha256', help='Expected SHA-256 of the ISO')
args = parser.parse_args()
iso = args.iso.resolve(strict=True)
if args.sha256:
    with iso.open('rb') as artifact:
        assert hashlib.file_digest(artifact, 'sha256').hexdigest() == args.sha256, 'ISO checksum mismatch'
binary = Path(__file__).resolve().parents[1] / 'target/debug/virmora'
root = Path(tempfile.mkdtemp(prefix='virmora-iso-', dir='/tmp'))
env = dict(os.environ, VIRMORA_HOME=str(root))
config = dict(id='installer', name='Virmora Alpine installation test', cpus=2, memory_mib=2048,
              disk_path=str(root/'disk.raw'), disk_size_gib=4,
              kernel_path='', initrd_path=None, kernel_cmdline='', iso_path=str(iso),
              architecture='aarch64', backend='qemu', network_mode='nat', boot_mode='installer')
(root/'input').write_text(json.dumps(config))

def cli(*arguments):
    result = subprocess.run([str(binary), *arguments], env=env, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(result.stdout + result.stderr)
    return result.stdout

cli('create', str(root/'input'))
log = root/'run/installer/console.log'
owner = None
serial = None
owner_output = None


def content():
    return log.read_text(errors='replace') if log.exists() else ''


def wait(pattern, offset=0, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        text = content()[offset:]
        if re.search(pattern, text):
            return text
        if owner.poll() is not None:
            raise RuntimeError('VM process exited: ' + (root/'owner.log').read_text())
        time.sleep(.2)
    raise RuntimeError(f'Timed out waiting for {pattern}. Console tail:\n{content()[-5000:]}')


def qmp(execute, arguments=None):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(root/'run/installer/qmp.sock'))
        stream = connection.makefile('rwb', buffering=0)
        json.loads(stream.readline())
        for request in [dict(execute='qmp_capabilities', id='caps'),
                        dict(execute=execute, arguments=arguments or {}, id='command')]:
            stream.write((json.dumps(request)+'\n').encode())
            while True:
                response = json.loads(stream.readline())
                if response.get('id') == request['id']:
                    if 'error' in response:
                        raise RuntimeError(str(response['error']))
                    break
        return response.get('return')


def start():
    global owner, serial, owner_output
    offset = len(content())
    owner_output = (root/'owner.log').open('a')
    owner = subprocess.Popen([str(binary), 'run', 'installer'], env=env,
                             stdin=subprocess.PIPE, stdout=owner_output, stderr=owner_output)
    path = root/'run/installer/serial.sock'
    deadline = time.monotonic()+15
    while time.monotonic() < deadline:
        try:
            serial = socket.socket(socket.AF_UNIX)
            serial.settimeout(3)
            serial.connect(str(path))
            break
        except OSError:
            serial.close()
            if owner.poll() is not None:
                raise RuntimeError((root/'owner.log').read_text())
            time.sleep(.1)
    else:
        raise RuntimeError('Serial socket was not ready')
    # A connected chardev client must consume output or QEMU can backpressure/stall.
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
    return offset


def stop():
    global owner, serial, owner_output
    if serial:
        serial.close()
        serial = None
    if owner:
        owner.communicate(b'\n', timeout=20)
        if owner.returncode:
            raise RuntimeError((root/'owner.log').read_text())
        owner = None
    if owner_output:
        owner_output.close()
        owner_output = None


def command(text, marker, timeout=120):
    offset = len(content())
    serial.sendall((text + "; printf '\\n"+marker+"\\n'\n").encode())
    return wait(r'(?m)^'+marker+r'\r?$', offset, timeout)


try:
    print('Booting installer ISO…', flush=True)
    offset = start()
    wait(r'login:', offset)
    qmp('screendump', dict(filename=str(root/'installer.png'), format='png'))
    mice = qmp('query-mice')
    assert any(pointer.get('absolute') for pointer in mice), 'USB tablet not available'
    # Exercise the virtual USB keyboard on the graphical tty independently of serial.
    for key in ['r', 'o', 'o', 't', 'ret']:
        qmp('send-key', dict(keys=[dict(type='qcode', data=key)], **{'hold-time': 50}))
        time.sleep(.12)
    time.sleep(.5)
    qmp('screendump', dict(filename=str(root/'graphical-keyboard-login.png'), format='png'))
    serial.sendall(b'root\n')
    wait(r'~\s*#', offset)
    command("export TERM=dumb; hostname virmora-smoke; echo virmora-smoke > /etc/hostname; "
            "echo root:virmora-smoke | chpasswd; echo virmora-iso-installed > /etc/virmora-install-proof; "
            "printf 'auto lo\\niface lo inet loopback\\nauto eth0\\niface eth0 inet dhcp\\n' > /etc/network/interfaces; "
            "ifup eth0; printf 'https://dl-cdn.alpinelinux.org/alpine/v3.23/main\\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\\n' > /etc/apk/repositories", 'NETWORK_READY')
    # /dev/vda belongs only to the new 4 GiB VM disk created above.
    print('Installing Linux to temporary virtual disk…', flush=True)
    installed = command('ERASE_DISKS=/dev/vda USE_EFI=1 BOOTLOADER=grub KERNELOPTS="console=tty0 console=ttyAMA0" '
                        'setup-disk -q -m sys -s 0 /dev/vda && echo INSTALL_OK', 'INSTALL_DONE', timeout=480)
    if not re.search(r'(?m)^INSTALL_OK\r?$', installed):
        raise RuntimeError('Installation failed:\n'+installed)
    qmp('screendump', dict(filename=str(root/'installation-complete.png'), format='png'))
    serial.sendall(b'poweroff\n')
    wait(r'Power down|reboot: Power down', timeout=45)
    stop()
    cli('boot', 'installer', 'disk')
    # Prove disk boot does not depend on having the original ISO available.
    registry = root/'installer.json'
    saved = json.loads(registry.read_text())
    saved['iso_path'] = str(root/'not-attached.iso')
    registry.write_text(json.dumps(saved))
    print('Booting installed disk without ISO…', flush=True)
    offset = start()
    wait(r'virmora-smoke login:', offset)
    serial.sendall(b'root\n')
    wait(r'Password:', offset)
    serial.sendall(b'virmora-smoke\n')
    wait(r'~\s*#', offset)
    result = command('test "$(cat /etc/virmora-install-proof)" = virmora-iso-installed && '
                     'grep -Eq "^/dev/vda[0-9]+ / ext4 " /proc/mounts && echo DISK_BOOT_OK', 'VERIFY_DONE')
    if not re.search(r'(?m)^DISK_BOOT_OK\r?$', result):
        raise RuntimeError('Disk boot verification failed:\n'+result)
    blocks = qmp('query-block')
    (root/'disk-block-devices.json').write_text(json.dumps(blocks, indent=2))
    if any(item.get('inserted', {}).get('file', '').endswith('.iso') for item in blocks):
        raise RuntimeError('ISO unexpectedly attached during disk boot')
    qmp('screendump', dict(filename=str(root/'installed-disk.png'), format='png'))
    serial.sendall(b'poweroff\n')
    wait(r'Power down|reboot: Power down', offset=offset, timeout=45)
    stop()
    print('PASS: UEFI ISO boot, graphical display, Linux installation, disk-only boot and persisted guest file')
finally:
    stop()
    print(f'Artifacts retained: {root}', flush=True)
