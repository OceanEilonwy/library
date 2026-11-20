#!/bin/sh
# Builds Library Rip for this machine and puts it in downloads/library-rip/,
# the folder the library serves on /ripper out of the box.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/rip/Cargo.toml" | head -n1)
os=$(uname -s | tr 'A-Z' 'a-z'); arch=$(uname -m)
name="library-rip-$version-$os-$arch"
out="$root/downloads/library-rip"
(cd "$root/rip" && cargo build --release --locked)
stage=$(mktemp -d); trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name/bin" "$out"
cp "$root/rip/target/release/library-rip" "$stage/$name/bin/"
cp "$root/LICENSE" "$root/rip/README.md" "$stage/$name/"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
echo "wrote $out/$name.tar.gz"
