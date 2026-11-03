# Build environment for Casework's Linux and Windows (MinGW-w64 cross) releases.
# Debian 12 keeps the Linux binary's glibc requirement at 2.36, so it runs on most
# current distros.
ARG RUST_IMAGE=rust:1.97-bookworm
FROM ${RUST_IMAGE}

RUN apt-get update && apt-get install -y --no-install-recommends \
        cmake ninja-build g++ pkg-config zip file \
        mingw-w64 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-pc-windows-gnu

# OpenCascade uses std::thread/std::mutex: use the POSIX-threads MinGW toolchain.
RUN update-alternatives --set x86_64-w64-mingw32-gcc /usr/bin/x86_64-w64-mingw32-gcc-posix \
 && update-alternatives --set x86_64-w64-mingw32-g++ /usr/bin/x86_64-w64-mingw32-g++-posix

COPY mingw-w64-x86_64.cmake /opt/mingw-w64-x86_64.cmake
