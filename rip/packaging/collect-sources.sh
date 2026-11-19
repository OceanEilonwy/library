#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Puts the source code of the third-party libraries bundled in a Library Rip
# package next to it, as the GPL and LGPL ask, and writes SOURCES.txt.
#
#   collect-sources.sh <linux|windows|macos> <folder of bundled libraries> <output folder>
#
# For each bundled library it finds the package it was installed from and
# downloads that package's exact source, as the platform publishes it:
#   linux    Ubuntu source package (`apt-get source`: .dsc, upstream tarball,
#            Ubuntu's changes); needs deb-src lines in APT's sources
#   windows  MSYS2 source package (repo.msys2.org/mingw/sources: PKGBUILD,
#            patches, upstream tarball)
#   macos    the Homebrew formula's source tarball (`brew fetch -s`), with
#            the formula's address
#
# Libraries under the GPL or LGPL (libcdio, libcdio-paranoia, libiconv) get
# their source shipped; so does libFLAC (BSD), which is small. Libraries
# under permissive licences that ask for no source are listed with their
# licence. A library not in either list stops the build, so nothing new is
# bundled without a decision here.
#
# Runs with the bash 3.2 that macOS ships (no associative arrays).

set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 <linux|windows|macos> <library folder> <output folder>" >&2
  exit 2
fi
platform=$1
libdir=$2
out=$3
mkdir -p "$out"
out=$(cd "$out" && pwd)
listing="$out/SOURCES.txt"

# The library's name without its extension or version:
# libcdio.so.19, libcdio-21.dll, libcdio.21.dylib -> libcdio.
stem() {
  printf '%s\n' "$1" | sed -E 's/\.(so|dll|dylib).*$//; s/[.-][0-9][0-9.]*$//'
}

# What to do with a bundled library: "source", or the licence that lets it
# go without its source, or nothing if it has not been decided.
policy() {
  case "$1" in
    libcdio | libcdio_cdda | libcdio_paranoia | libFLAC | libiconv) echo source ;;
    libogg) echo "BSD-3-Clause" ;;
    libwinpthread) echo "MIT and BSD-3-Clause" ;;
    libpng16) echo "libpng-2.0" ;;
    libbrotlicommon | libbrotlidec) echo "MIT" ;;
    *) echo "" ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Downloads the source of the package that provided library file $1 into
# the current folder (the output folder), printing "package version origin"
# and the downloaded file names, one per line, after it.
fetch_linux() {
  local package source version
  package=$(dpkg -S "*/$1" | head -n1 | cut -d: -f1)
  source=$(dpkg-query -W -f='${source:Package}' "$package")
  version=$(dpkg-query -W -f='${source:Version}' "$package")
  echo "$source $version Ubuntu source package (apt-get source $source=$version)"
  local before after
  before=$(ls -1)
  apt-get source --download-only "$source=$version" >&2
  after=$(ls -1)
  comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after")
}

fetch_windows() {
  local package version base url file
  package=$(pacman -Qqo "/ucrt64/bin/$1")
  version=$(pacman -Q "$package" | cut -d' ' -f2)
  base=$(awk '/^%BASE%$/ { getline; print; exit }' "/var/lib/pacman/local/$package-$version/desc")
  file="$base-$version.src.tar.zst"
  url="https://repo.msys2.org/mingw/sources/$file"
  echo "$base $version MSYS2 source package ($url)"
  if [ ! -e "$file" ]; then
    curl -fsSL --retry 3 -o "$file" "$url"
  fi
  echo "$file"
}

fetch_macos() {
  local cellar match formula version cached info url recipe tarball
  cellar=$(brew --cellar)
  match=$(ls -1d "$cellar"/*/*/lib/"$1" | head -n1)
  formula=$(printf '%s\n' "$match" | awk -F/ '{ print $(NF-3) }')
  version=$(printf '%s\n' "$match" | awk -F/ '{ print $(NF-2) }')
  brew fetch --quiet --build-from-source --formula "$formula" >&2
  cached=$(brew --cache --build-from-source --formula "$formula")
  info=$(brew info --json=v2 --formula "$formula")
  url=$(printf '%s' "$info" | jq -r '.formulae[0].urls.stable.url')
  recipe=$(printf '%s' "$info" | jq -r '.formulae[0].ruby_source_path')
  echo "$formula $version Homebrew source ($url; build recipe: https://github.com/Homebrew/homebrew-core/blob/main/$recipe)"
  tarball=$(basename "$cached" | sed 's/^[0-9a-f]*--//')
  if [ ! -e "$tarball" ]; then
    cp "$cached" "$tarball"
  fi
  echo "$tarball"
}

{
  echo "Third-party libraries bundled with Library Rip, and their source code."
  echo "Library Rip itself: https://github.com/OceanEilonwy/library (AGPL-3.0-or-later)."
  echo
} > "$listing"

shipped=""
undecided=""
for path in "$libdir"/*; do
  [ -f "$path" ] || continue
  name=$(basename "$path")
  case "$name" in
    *.so | *.so.* | *.dll | *.dylib) ;;
    *) continue ;;
  esac
  [ "$name" = "library-rip.exe" ] && continue
  decision=$(policy "$(stem "$name")")
  if [ -z "$decision" ]; then
    undecided="$undecided $name"
    continue
  fi
  if [ "$decision" != source ]; then
    echo "$name: $decision; no source required, none included." >> "$listing"
    continue
  fi
  result=$(cd "$out" && "fetch_$platform" "$name")
  header=$(printf '%s\n' "$result" | head -n1)
  set -- $header
  package=$1
  version=$2
  shift 2
  echo "$name: from $package $version, $*" >> "$listing"
  # A source package can provide several bundled libraries: list its files once.
  case " $shipped " in
    *" $package-$version "*) ;;
    *)
      shipped="$shipped $package-$version"
      printf '%s\n' "$result" | tail -n +2 | while IFS= read -r file; do
        [ -n "$file" ] || continue
        echo "    $file  sha256 $(sha256 "$out/$file")" >> "$listing"
      done
      ;;
  esac
done

if [ -n "$undecided" ]; then
  echo "::error::No source decision for bundled libraries:$undecided. Add them to policy() in rip/packaging/collect-sources.sh." >&2
  exit 1
fi
if [ -z "$shipped" ]; then
  echo "::error::No library sources were collected from $libdir." >&2
  exit 1
fi
cat "$listing"
ls -la "$out"
