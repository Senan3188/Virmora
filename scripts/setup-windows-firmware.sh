#!/bin/bash
set -euo pipefail
destination="$HOME/Library/Application Support/Virmora/Firmware"
package='qemu-efi-aarch64_2025.02-8+deb13u1_all.deb'
digest='a00b2411a79c8aeafd95a7c868ac3cd1aab592f1af9fff965f02d81a625276ed'
temp="$(mktemp -d)"
trap 'rm -rf -- "$temp"' EXIT
curl -fLsS --max-time 120 "https://deb.debian.org/debian/pool/main/e/edk2/$package" -o "$temp/firmware.deb"
printf '%s  %s\n' "$digest" "$temp/firmware.deb" | shasum -a 256 -c
(
    cd "$temp"
    ar -p firmware.deb data.tar.xz | tar -xJf - \
        ./usr/share/AAVMF/AAVMF_CODE.secboot.fd \
        ./usr/share/AAVMF/AAVMF_VARS.ms.fd
)
mkdir -p "$destination"
for firmware in AAVMF_CODE.secboot.fd AAVMF_VARS.ms.fd; do
    source="$temp/usr/share/AAVMF/$firmware"
    if [[ -e "$destination/$firmware" ]]; then
        cmp -s "$source" "$destination/$firmware" || {
            echo "Different firmware already exists at $destination/$firmware" >&2
            exit 1
        }
    else
        cp -n "$source" "$destination/$firmware"
    fi
done
echo "Windows ARM64 firmware ready in $destination"
