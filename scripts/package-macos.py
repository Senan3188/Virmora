#!/usr/bin/env python3
"""Build a macOS DMG with a relocated QEMU and swtpm runtime."""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request


ROOT = Path(__file__).resolve().parent.parent
DESKTOP = ROOT / "apps/desktop"
TAURI = DESKTOP / "src-tauri"
FIRMWARE_PACKAGE = "qemu-efi-aarch64_2025.02-8+deb13u1_all.deb"
FIRMWARE_URL = f"https://deb.debian.org/debian/pool/main/e/edk2/{FIRMWARE_PACKAGE}"
FIRMWARE_SHA256 = "a00b2411a79c8aeafd95a7c868ac3cd1aab592f1af9fff965f02d81a625276ed"
QEMU_PROGRAMS = ("qemu-system-aarch64", "qemu-system-x86_64")
NETWORK_HELPERS = ("socket_vmnet", "socket_vmnet_client")
SYSTEM_LIBRARIES = ("/usr/lib/", "/System/Library/")


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def output(*args):
    return run(*args, capture_output=True, text=True).stdout.strip()


def install_id(binary):
    lines = output("otool", "-D", binary).splitlines()
    return lines[1].strip() if len(lines) > 1 else None


def dependencies(binary, own_id):
    lines = output("otool", "-L", binary).splitlines()[1:]
    for line in lines:
        if " (compatibility version " in line:
            library = line.strip().split(" (compatibility version ", 1)[0]
            if library != own_id:
                yield library


def rpaths(binary):
    return re.findall(r"^\s*path (.+) \(offset \d+\)", output("otool", "-l", binary), re.M)


def resolve_library(binary, reference, search_paths, executables):
    if reference.startswith("@loader_path/"):
        candidates = [binary.parent / reference.removeprefix("@loader_path/")]
    elif reference.startswith("@executable_path/"):
        candidates = [executables / reference.removeprefix("@executable_path/")]
    elif reference.startswith("@rpath/"):
        name = reference.removeprefix("@rpath/")
        origins = (
            path.replace("@loader_path", str(binary.parent))
                .replace("@executable_path", str(executables))
            for path in search_paths
        )
        candidates = [Path(path) / name for path in origins]
    elif reference.startswith("/"):
        candidates = [Path(reference)]
    else:
        candidates = []
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise ValueError(f"Cannot resolve {reference} from {binary.name}")


class Runtime:
    def __init__(self, contents, cellar, arch):
        self.contents = contents
        self.cellar = cellar.resolve()
        self.arch = arch
        self.copied = {}
        self.formulas = set()
        self.frameworks = contents / "Frameworks"
        self.frameworks.mkdir(exist_ok=True)

    def copy(self, source, destination, executable_dir):
        source = source.resolve(strict=True)
        if destination in self.copied:
            if self.copied[destination] != source:
                raise ValueError(f"Two runtime libraries have the same name: {destination.name}")
            return
        self.copied[destination] = source
        formula, version = source.relative_to(self.cellar).parts[:2]
        self.formulas.add(self.cellar / formula / version)
        if output("lipo", "-archs", source) != self.arch:
            raise ValueError(f"Expected a native {self.arch} binary: {source.name}")
        shutil.copyfile(source, destination)
        destination.chmod(0o755)
        identity = install_id(source)
        search_paths = rpaths(source)
        changes = []
        if identity:
            changes.extend(("-id", f"@loader_path/{destination.name}"))
        for reference in dependencies(source, identity):
            if reference.startswith(SYSTEM_LIBRARIES):
                continue
            library = resolve_library(source, reference, search_paths, executable_dir)
            bundled = self.frameworks / library.name
            self.copy(library, bundled, executable_dir)
            relative = os.path.relpath(bundled, destination.parent)
            changes.extend(("-change", reference, f"@loader_path/{relative}"))
        for path in dict.fromkeys(search_paths):
            changes.extend(("-delete_rpath", path))
        if changes:
            run("install_name_tool", *changes, destination)

    def verify_linkage(self):
        for binary in self.copied:
            for reference in dependencies(binary, install_id(binary)):
                if reference.startswith(SYSTEM_LIBRARIES):
                    continue
                if not reference.startswith("@loader_path/"):
                    raise ValueError(f"Unexpected runtime reference: {reference}")
                library = resolve_library(binary, reference, (), binary.parent)
                if library.parent != self.frameworks.resolve() or library not in self.copied:
                    raise ValueError(f"Reference escapes the bundled libraries: {reference}")

    def write_notices(self):
        notices = self.contents / "Resources" / "ThirdParty"
        notices.mkdir()
        manifest = []
        for package in sorted(self.formulas):
            name = package.parent.name
            package_notices = notices / name
            package_notices.mkdir()
            for entry in package.iterdir():
                if entry.is_file() and re.match(r"^(LICENSE|COPYING|NOTICE|COPYRIGHT)", entry.name, re.I):
                    shutil.copyfile(entry, package_notices / entry.name)
            for formula in (package / ".brew").glob("*.rb"):
                shutil.copyfile(formula, package_notices / formula.name)
            manifest.append({"formula": name, "version": package.name})
        manifest.append({"package": FIRMWARE_PACKAGE, "url": FIRMWARE_URL, "sha256": FIRMWARE_SHA256})
        (notices / "runtime.json").write_text(json.dumps(manifest, indent=2) + "\n")
        return notices


def add_windows_firmware(package, data_dir, notices):
    if hashlib.sha256(package.read_bytes()).hexdigest() != FIRMWARE_SHA256:
        raise ValueError("Debian firmware package SHA-256 does not match")
    archive = subprocess.check_output(["ar", "-p", package, "data.tar.xz"])
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:xz") as data:
        for name in ("AAVMF_CODE.secboot.fd", "AAVMF_VARS.ms.fd"):
            (data_dir / name).write_bytes(data.extractfile(f"./usr/share/AAVMF/{name}").read())
        copyright_path = "./usr/share/doc/qemu-efi-aarch64/copyright"
        (notices / "qemu-efi-aarch64.copyright").write_bytes(data.extractfile(copyright_path).read())


def set_minimum_macos(contents, binaries):
    plist = contents / "Info.plist"
    info = plistlib.loads(plist.read_bytes())
    versions = [info.get("LSMinimumSystemVersion", "10.13")]
    for binary in binaries:
        versions.extend(re.findall(r"^\s*minos (\d+(?:\.\d+)*)$", output("otool", "-l", binary), re.M))
    minimum = max(versions, key=lambda version: tuple(map(int, version.split("."))))
    info["LSMinimumSystemVersion"] = minimum
    plist.write_bytes(plistlib.dumps(info))
    return minimum


def sign(path, identity, *, executable=False, hypervisor=False):
    args = ["codesign", "--force", "--sign", identity, "--timestamp=none" if identity == "-" else "--timestamp"]
    if executable and identity != "-":
        args.extend(("--options", "runtime"))
    if hypervisor:
        args.extend(("--entitlements", TAURI / "Entitlements.plist"))
    run(*args, path)


def make_dmg(args, work, arch):
    image = work / "image"
    image.mkdir()
    app = image / "Virmora.app"
    shutil.copytree(args.app, app, symlinks=True)
    contents = app / "Contents"
    executables = contents / "MacOS"
    runtime = Runtime(contents, Path(output("brew", "--cellar")), arch)
    for formula, programs in (("qemu", QEMU_PROGRAMS), ("swtpm", ("swtpm",)), ("socket_vmnet", NETWORK_HELPERS)):
        keg = Path(output("brew", "--prefix", formula))
        for program in programs:
            source = (keg / "bin" / program).resolve(strict=True)
            if program in NETWORK_HELPERS and any(
                not lib.startswith(SYSTEM_LIBRARIES) for lib in dependencies(source, None)
            ):
                raise ValueError(f"{program} must link only to macOS system libraries")
            runtime.copy(source, executables / program, source.parent)
    runtime.verify_linkage()

    data_dir = contents / "Resources" / "qemu"
    qemu_share = Path(output("brew", "--prefix", "qemu")) / "share" / "qemu"
    shutil.copytree(qemu_share, data_dir)
    shutil.copyfile(ROOT / "scripts/setup-macos-networking.sh", contents / "Resources/setup-macos-networking.sh")
    notices = runtime.write_notices()
    if arch == "arm64":
        package = args.firmware_package
        if package is None:
            package = work / FIRMWARE_PACKAGE
            with urllib.request.urlopen(FIRMWARE_URL, timeout=60) as response, package.open("wb") as target:
                shutil.copyfileobj(response, target)
        add_windows_firmware(package, data_dir, notices)
    info = plistlib.loads((contents / "Info.plist").read_bytes())
    main_binary = executables / info["CFBundleExecutable"]
    if output("lipo", "-archs", main_binary) != arch:
        raise ValueError(f"Tauri app must use the same architecture as the bundled runtime ({arch})")
    minimum = set_minimum_macos(contents, [main_binary, *runtime.copied])
    print(f"Bundled {len(runtime.copied)} runtime binaries and libraries. Minimum macOS {minimum}.", flush=True)

    for binary in sorted(runtime.copied):
        sign(binary, args.identity, executable=binary.parent == executables, hypervisor=binary.name in QEMU_PROGRAMS)
    sign(app, args.identity, executable=True, hypervisor=True)
    run("codesign", "--verify", "--deep", "--strict", app)
    (image / "Applications").symlink_to("/Applications")
    dmg = work / args.output.name
    run("hdiutil", "create", "-fs", "HFS+", "-volname", "Virmora", "-srcfolder", image, "-format", "UDZO", dmg)
    if args.identity != "-":
        sign(dmg, args.identity)
    if args.notary_profile:
        run("xcrun", "notarytool", "submit", dmg, "--keychain-profile", args.notary_profile, "--wait")
        run("xcrun", "stapler", "staple", dmg)
        run("xcrun", "stapler", "validate", dmg)
    run("hdiutil", "verify", dmg)
    os.link(dmg, args.output)


def main():
    arch = platform.machine()
    if platform.system() != "Darwin" or arch not in ("arm64", "x86_64"):
        raise SystemExit("Build on a native Apple Silicon or Intel Mac.")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, help="Use an existing Tauri .app instead of building it.")
    parser.add_argument("--output", type=Path, help="Output .dmg path.")
    parser.add_argument("--firmware-package", type=Path, help="Use a local copy of the pinned Debian package.")
    parser.add_argument("--identity", default=os.environ.get("APPLE_SIGNING_IDENTITY", "-"))
    parser.add_argument("--notary-profile", default=os.environ.get("VIRMORA_NOTARY_PROFILE"))
    args = parser.parse_args()
    if args.notary_profile and args.identity == "-":
        parser.error("Notarization requires a Developer ID Application signing identity.")
    if args.output is None:
        version = json.loads((TAURI / "tauri.conf.json").read_text())["version"]
        variant = "" if args.notary_profile else "-local"
        args.output = TAURI / "target" / "release" / "bundle" / "dmg" / f"Virmora_{version}_{arch}{variant}.dmg"
    args.output = args.output.resolve()
    if args.output.suffix != ".dmg" or args.output.exists():
        parser.error("Choose a new output file with a .dmg extension.")
    if args.app is None:
        run("npm", "run", "tauri", "--", "build", "--bundles", "app",
            "--config", '{"bundle":{"active":true}}', "--no-sign", "--", "--locked", cwd=DESKTOP)
        args.app = TAURI / "target" / "release" / "bundle" / "macos" / "Virmora.app"
    if not (args.app / "Contents" / "Info.plist").is_file():
        parser.error("Expected a Tauri macOS app at the given path.")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".virmora-package-", dir=args.output.parent) as work:
        make_dmg(args, Path(work).resolve(), arch)
    print(f"DMG ready at {args.output}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
