#!/bin/bash
set -euo pipefail

if [[ "$(/usr/bin/uname -s)" != Darwin || "$EUID" == 0 || "$#" -gt 1 ]]; then
    echo "Run as your normal macOS user: bash scripts/setup-macos-networking.sh [bridge-adapter]" >&2
    exit 1
fi

bridge="${1:-en0}"
if [[ ! "$bridge" =~ ^en[0-9]{1,5}$ ]] || ! /sbin/ifconfig "$bridge" >/dev/null 2>&1; then
    echo "Choose a Mac adapter from: /sbin/ifconfig -l" >&2
    exit 1
fi

prefix=""
for candidate in /opt/homebrew/opt/socket_vmnet /usr/local/opt/socket_vmnet; do
    if [[ -x "$candidate/bin/socket_vmnet" && -x "$candidate/bin/socket_vmnet_client" ]]; then
        prefix="$candidate"
        break
    fi
done
if [[ -z "$prefix" ]]; then
    echo "Install the socket_vmnet package first: brew install socket_vmnet" >&2
    exit 1
fi

base="/Library/Application Support/Virmora Networking"
account="$(/usr/bin/id -un)"
group="$(/usr/bin/id -gn)"
temp="$(/usr/bin/mktemp -d)"
trap '/bin/rm -rf -- "$temp"' EXIT

# Authenticate before making any system changes.
sudo -v
for directory in "$base" "$base/sockets"; do
    if [[ -L "$directory" ]] || { [[ -e "$directory" ]] && [[ "$(/usr/bin/stat -f '%u' "$directory")" != 0 ]]; }; then
        echo "Refusing to use an existing non-root directory or symlink: $directory" >&2
        exit 1
    fi
done
sudo /usr/bin/install -d -o root -g wheel -m 0755 "$base"
sudo /usr/bin/install -d -o root -g wheel -m 0700 "$base/sockets"
sudo /bin/chmod -N "$base/sockets"
sudo /bin/chmod +a "user:$account allow search" "$base/sockets"
sudo /usr/bin/install -o root -g wheel -m 0755 "$prefix/bin/socket_vmnet" "$base/socket_vmnet"
sudo /usr/bin/install -o root -g wheel -m 0755 "$prefix/bin/socket_vmnet_client" "$base/socket_vmnet_client"

install_service() {
    local mode="$1" suffix="$2" argument="$3"
    local label="dev.virmora.network.$suffix"
    local plist="/Library/LaunchDaemons/$label.plist"
    local socket="$base/sockets/$suffix.sock"
    cat > "$temp/$label.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$label</string>
<key>ProgramArguments</key><array>
<string>$base/socket_vmnet</string>
<string>--socket-group=$group</string>
<string>--vmnet-mode=$mode</string>
<string>$argument</string>
<string>$socket</string>
</array>
<key>RunAtLoad</key><true/>
<key>StandardErrorPath</key><string>/var/log/$label.log</string>
</dict></plist>
PLIST
    /usr/bin/plutil -lint "$temp/$label.plist"
    if sudo /bin/launchctl print "system/$label" >/dev/null 2>&1; then
        sudo /bin/launchctl bootout "system/$label"
    fi
    sudo /usr/bin/install -o root -g wheel -m 0644 "$temp/$label.plist" "$plist"
    sudo /bin/launchctl enable "system/$label"
    sudo /bin/launchctl bootstrap system "$plist"
}

install_service shared shared --vmnet-gateway=192.168.105.1
install_service host host --vmnet-gateway=192.168.106.1
install_service bridged "bridged-$bridge" "--vmnet-interface=$bridge"

echo "Virmora networking services loaded. Use Check network in Edit Settings to verify them."
