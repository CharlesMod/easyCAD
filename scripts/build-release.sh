#!/usr/bin/env bash
# Build easyCAD release packages for macOS, Linux and Windows from a Mac.
#
#   scripts/build-release.sh              # all three
#   scripts/build-release.sh macos        # just one (macos | linux | windows), or several
#
# macOS is built natively (universal arm64 + x86_64 .app). Linux and Windows are
# built inside Docker (Debian 12): Linux natively for the chosen arch, Windows by
# cross-compiling with MinGW-w64. Output goes to dist/.
#
# Environment knobs:
#   MACOS_ARCHS="arm64 x86_64"   arches lipo'd into the .app (use "arm64" for a faster build)
#   LINUX_ARCHS="x86_64"         "x86_64", "aarch64" or both
#   RUST_IMAGE=rust:1.97-bookworm  base image for the Docker builds
#   JOBS=<n>                     parallel compile jobs (default: all cores)
#
# The first run compiles OpenCascade once per platform (~10-30 min each); later runs
# reuse it (target/ on the Mac, Docker volumes easycad-target-* for the others).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TARGETS=("$@")
[[ ${#TARGETS[@]} -eq 0 ]] && TARGETS=(macos linux windows)

MACOS_ARCHS="${MACOS_ARCHS:-arm64 x86_64}"
LINUX_ARCHS="${LINUX_ARCHS:-x86_64}"
RUST_IMAGE="${RUST_IMAGE:-rust:1.97-bookworm}"
JOBS="${JOBS:-$( (sysctl -n hw.ncpu 2>/dev/null || nproc) )}"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
SHA="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
TAG="${VERSION}-${SHA}"
DIST="$REPO/dist"
mkdir -p "$DIST"

# Third-party license notices, bundled into every package (needs cargo-about).
"$REPO/scripts/notices.sh" "$DIST/THIRD-PARTY-NOTICES.html"

log()  { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 not found. $2"; }

# Docker platform for the host's native arch (Windows is cross-compiled there, no emulation).
host_docker_arch() {
  case "$(uname -m)" in
    arm64|aarch64) echo arm64 ;;
    *)             echo amd64 ;;
  esac
}

build_macos() {
  [[ "$(uname -s)" == Darwin ]] || die "macOS builds must run on a Mac."
  need xcode-select "Install the Xcode Command Line Tools: xcode-select --install"
  xcode-select -p >/dev/null 2>&1 || die "Xcode Command Line Tools missing: xcode-select --install"
  need cmake  "brew install cmake"
  need rustup "Install Rust from https://rustup.rs"

  export MACOSX_DEPLOYMENT_TARGET=11.0
  export CMAKE_BUILD_PARALLEL_LEVEL="$JOBS"
  local bins=()
  for arch in $MACOS_ARCHS; do
    local triple
    case "$arch" in
      arm64)  triple=aarch64-apple-darwin ;;
      x86_64) triple=x86_64-apple-darwin ;;
      *) die "unknown MACOS_ARCHS entry: $arch" ;;
    esac
    log "macOS $arch: cargo build ($triple)"
    rustup target add "$triple" >/dev/null
    # manifold-csg-sys runs plain `cmake`, which builds for the host arch unless told
    CMAKE_OSX_ARCHITECTURES="$arch" \
    cargo build --release --locked -p easycad --target "$triple" -j "$JOBS"
    bins+=("target/$triple/release/easycad")
  done

  local stage; stage="$(mktemp -d)"
  local app="$stage/easyCAD.app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp LICENSE "$app/Contents/Resources/LICENSE.txt"
  cp "$DIST/THIRD-PARTY-NOTICES.html" "$app/Contents/Resources/THIRD-PARTY-NOTICES.html"
  lipo -create "${bins[@]}" -output "$app/Contents/MacOS/easycad"
  strip -x "$app/Contents/MacOS/easycad"
  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>easyCAD</string>
  <key>CFBundleDisplayName</key><string>easyCAD</string>
  <key>CFBundleIdentifier</key><string>local.easycad.app</string>
  <key>CFBundleExecutable</key><string>easycad</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${SHA}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>easyCAD document</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>CFBundleTypeExtensions</key><array><string>ecad</string></array>
      <key>LSTypeIsPackage</key><true/>
    </dict>
  </array>
</dict>
</plist>
PLIST
  # Ad-hoc signature: required for arm64 after lipo/strip; fine for running on your own Macs.
  codesign --force --deep --sign - "$app"
  local suffix zip
  suffix="$(echo "$MACOS_ARCHS" | tr ' ' '-' | sed 's/arm64-x86_64/universal/')"
  zip="$DIST/easyCAD-${TAG}-macos-${suffix}.zip"
  rm -f "$zip"
  ditto -c -k --keepParent "$app" "$zip"
  rm -rf "$stage"
  log "macOS -> $zip"
}

docker_build() { # $1 = linux|windows, $2 = docker platform arch (amd64|arm64)
  local kind="$1" arch="$2"
  need docker "Install Docker Desktop (https://www.docker.com/products/docker-desktop/) and start it."
  docker info >/dev/null 2>&1 || die "Docker is installed but not running. Start Docker Desktop."
  local image="easycad-build:${arch}"
  log "Docker image $image (from $RUST_IMAGE)"
  docker build --platform "linux/$arch" --build-arg "RUST_IMAGE=$RUST_IMAGE" \
    -t "$image" -f scripts/docker/Dockerfile scripts/docker
  log "$kind build in linux/$arch container"
  docker run --rm --platform "linux/$arch" \
    -v "$REPO":/src:ro \
    -v "$DIST":/out \
    -v "easycad-target-${kind}-${arch}":/target \
    -v "easycad-cargo-registry-${arch}":/usr/local/cargo/registry \
    -e JOBS="$JOBS" -e TAG="$TAG" \
    "$image" /src/scripts/docker/build-in-container.sh "$kind"
}

for t in "${TARGETS[@]}"; do
  case "$t" in
    macos) build_macos ;;
    linux)
      for a in $LINUX_ARCHS; do
        case "$a" in
          x86_64)  docker_build linux amd64 ;;
          aarch64) docker_build linux arm64 ;;
          *) die "unknown LINUX_ARCHS entry: $a" ;;
        esac
      done ;;
    windows) docker_build windows "$(host_docker_arch)" ;;
    *) die "unknown target '$t' (use macos, linux, windows)" ;;
  esac
done

log "Bundling everything into one zip"
ALL="$DIST/easyCAD-${TAG}-all-platforms.zip"
rm -f "$ALL"
( cd "$DIST" && zip -q "$(basename "$ALL")" easyCAD-"${TAG}"-*.zip -x "$(basename "$ALL")" )
ls -lh "$DIST"/easyCAD-"${TAG}"-*
log "Done. Download/copy: $ALL"
