#!/usr/bin/env bash
# Runs inside the easycad-build Docker image (see Dockerfile).
#   build-in-container.sh linux     native Linux build for the container's arch
#   build-in-container.sh windows   MinGW-w64 cross build for x86_64-pc-windows-gnu
# Expects the repo at /src (read-only), writes zips to /out, caches in /target.
# Also usable directly on a Debian/Ubuntu machine: set SRC, OUT and CARGO_TARGET_DIR.
set -euo pipefail

KIND="${1:?usage: build-in-container.sh linux|windows}"
SRC="${SRC:-/src}"
OUT="${OUT:-/out}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/target}"
JOBS="${JOBS:-$(nproc)}"
export CMAKE_BUILD_PARALLEL_LEVEL="$JOBS"
TAG="${TAG:-dev}"

cd "$SRC"
mkdir -p "$OUT"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

case "$KIND" in
  linux)
    ARCH="$(uname -m)"
    cargo build --release --locked -p easycad -j "$JOBS"
    PKG="$STAGE/easyCAD"
    mkdir -p "$PKG"
    cp "$CARGO_TARGET_DIR/release/easycad" "$PKG/easycad"
    strip "$PKG/easycad"
    cat > "$PKG/README.txt" <<'EOF'
easyCAD for Linux

Run:  ./easycad            (or ./easycad path/to/file.ecad, ./easycad --sample)

Needs a Vulkan driver and the X11/Wayland keyboard library, which most desktops have.
If it fails to start, install them:
  Debian/Ubuntu: sudo apt install mesa-vulkan-drivers libvulkan1 libxkbcommon-x11-0
  Fedora:        sudo dnf install mesa-vulkan-drivers vulkan-loader libxkbcommon-x11
  Arch:          sudo pacman -S vulkan-icd-loader vulkan-radeon|vulkan-intel libxkbcommon-x11
File dialogs use the XDG desktop portal (xdg-desktop-portal-gtk or -kde).
EOF
    cat > "$PKG/easycad.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=easyCAD
Comment=Enclosure CAD
Exec=easycad %f
Terminal=false
Categories=Graphics;Engineering;
EOF
    ZIP="$OUT/easyCAD-${TAG}-linux-${ARCH}.zip"
    ;;

  windows)
    T=x86_64-pc-windows-gnu
    # C/C++ for the cxx shim (cc crate) and OpenCascade (cmake crate).
    export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc-posix
    export CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++-posix
    export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
    export CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_gnu=/opt/mingw-w64-x86_64.cmake
    # manifold-csg-sys runs plain `cmake`, which ignores the target-suffixed
    # variable above; CMake >= 3.21 reads this one from the environment.
    export CMAKE_TOOLCHAIN_FILE=/opt/mingw-w64-x86_64.cmake
    # Static OpenCascade on Windows: headers must not use dllimport; big OCCT
    # translation units exceed the default COFF section limit.
    export CFLAGS_x86_64_pc_windows_gnu="-Wa,-mbig-obj"
    export CXXFLAGS_x86_64_pc_windows_gnu="-Wa,-mbig-obj -DOCCT_STATIC_BUILD -DHAVE_NO_DLL"
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc-posix
    # Fully static (no MinGW DLLs to ship), GUI subsystem (no console window), and
    # the Win32 libraries OpenCascade's TKernel uses, appended after the static libs.
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static -C link-arg=-Wl,--subsystem,windows -C link-arg=-ladvapi32 -C link-arg=-lws2_32 -C link-arg=-lpsapi -C link-arg=-lgdi32 -C link-arg=-luser32 -C link-arg=-lshell32"
    cargo build --release --locked -p easycad --target "$T" -j "$JOBS"
    PKG="$STAGE/easyCAD"
    mkdir -p "$PKG"
    cp "$CARGO_TARGET_DIR/$T/release/easycad.exe" "$PKG/easyCAD.exe"
    x86_64-w64-mingw32-strip "$PKG/easyCAD.exe"
    printf 'easyCAD for Windows (x64)\r\n\r\nRun easyCAD.exe. No installer or extra DLLs needed.\r\nIf Windows SmartScreen warns about an unrecognized app, choose More info > Run anyway\r\n(the build is unsigned).\r\n' > "$PKG/README.txt"
    ZIP="$OUT/easyCAD-${TAG}-windows-x86_64.zip"
    ;;

  *) echo "unknown kind: $KIND" >&2; exit 2 ;;
esac

# License texts: easyCAD's own (LGPL-2.1) and every bundled third party's.
test -f /out/THIRD-PARTY-NOTICES.html || { echo "missing /out/THIRD-PARTY-NOTICES.html (scripts/notices.sh)" >&2; exit 1; }
cp /src/LICENSE "$PKG/LICENSE.txt"
cp /out/THIRD-PARTY-NOTICES.html "$PKG/THIRD-PARTY-NOTICES.html"

rm -f "$ZIP"
( cd "$STAGE" && zip -qr "$ZIP" easyCAD )
file "$PKG"/* | sed 's|^.*/||'
echo "$KIND -> $ZIP"
