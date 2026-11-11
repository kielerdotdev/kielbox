#!/usr/bin/env bash
# Cross-build Kielbox for x86_64-pc-windows-msvc on Linux and run its Windows tests
# under Wine. cargo-xwin supplies clang-cl/lld-link plus the MSVC CRT and Windows SDK
# (downloaded by xwin; accepting Microsoft's licence via XWIN_ACCEPT_LICENSE).
#
# Usage: scripts/ci/build-windows-cross.sh [setup] [test] [build] [smoke] [render]
#   setup  install apt packages, the pinned toolchain's Windows target and cargo-xwin
#   test   cargo test the Windows build of disk, clean and desktop under Wine
#   build  cargo build --release desktop and clean (static CRT)
#   smoke  run the release desktop.exe under Wine (CLI paths that need no GPU)
#   render draw each page of the release desktop.exe to target/wine-shots/*.png, through
#          Wine's Vulkan on Mesa's software rasteriser under Xvfb
# With no arguments, runs all five. Output: target/x86_64-pc-windows-msvc/release.
set -euo pipefail

TARGET=x86_64-pc-windows-msvc
CARGO_XWIN_VERSION=0.23.1
# Windows FFmpeg for the recording/editor tests (libx264 + AAC), pinned by checksum.
FFMPEG_VERSION=9.0.2
FFMPEG_URL=https://github.com/GyanD/codexffmpeg/releases/download/$FFMPEG_VERSION/ffmpeg-$FFMPEG_VERSION-essentials_build.zip
FFMPEG_SHA256=60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba
# Portable Git for Windows, for the disk crate's worktree-cleanup tests.
MINGIT_VERSION=2.56.0.4
MINGIT_URL=https://github.com/git-for-windows/git/releases/download/v2.56.0.windows.4/MinGit-$MINGIT_VERSION-64-bit.zip
MINGIT_SHA256=e3bf78351fe777fba47ddee5950082f8a98bb0f02604fdc7ba3ee2cb68ffa877
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

steps=("$@")
[ ${#steps[@]} -eq 0 ] && steps=(setup test build smoke render)
want() { [[ " ${steps[*]} " == *" $1 "* ]]; }

export XWIN_ACCEPT_LICENSE=1
export XWIN_ARCH=x86_64
export XWIN_CACHE_DIR=${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin}
export CARGO_TERM_COLOR=${CARGO_TERM_COLOR:-always}
# Wine: a private, quiet prefix; no desktop integration or Mono/Gecko prompts.
export WINEPREFIX=${WINEPREFIX:-$HOME/.wine-kielbox}
export WINEDEBUG=${WINEDEBUG:--all}
export WINEDLLOVERRIDES=${WINEDLLOVERRIDES:-mscoree,mshtml=}
# Wine maps Windows file names to Unix bytes through the locale: without UTF-8, names
# outside ASCII (the tests use 日本語 and æ) cannot exist.
export LANG=C.UTF-8 LC_ALL=C.UTF-8
ffmpeg_dir=${KIELBOX_FFMPEG_DIR:-$HOME/.cache/kielbox-ffmpeg/$FFMPEG_VERSION}
mingit_dir=${KIELBOX_MINGIT_DIR:-$HOME/.cache/kielbox-mingit/$MINGIT_VERSION}
# Wine's PATH additions, as Windows paths (Z: is the Unix root).
winpath() { printf 'Z:%s' "$(printf '%s' "$1" | tr / '\\')"; }
export WINEPATH="$(winpath "$ffmpeg_dir/bin");$(winpath "$mingit_dir/cmd")"

sudo_() { if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi; }

if want setup; then
  sudo_ apt-get update -qq
  # clang/lld provide clang-cl and lld-link; llvm provides llvm-rc (embed-resource),
  # llvm-lib and llvm-ar. Wine runs the test binaries.
  sudo_ env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    ca-certificates curl unzip build-essential clang lld llvm wine64 wine \
    mesa-vulkan-drivers libvulkan1 xvfb xauth >/dev/null
  # Debian/Ubuntu ship versioned LLVM tools; expose the unversioned names cc-rs and
  # embed-resource look for, when the distro did not.
  llvm_bin=$(dirname "$(readlink -f "$(command -v clang)")")
  for tool in llvm-rc llvm-lib llvm-ar clang-cl lld-link; do
    if ! command -v "$tool" >/dev/null && [ -x "$llvm_bin/$tool" ]; then
      sudo_ ln -sf "$llvm_bin/$tool" "/usr/local/bin/$tool"
    fi
  done
  command -v rustup >/dev/null || {
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  }
  rustup show active-toolchain >/dev/null # installs the toolchain pinned by rust-toolchain.toml
  rustup target add "$TARGET"
  if ! cargo xwin --version 2>/dev/null | grep -q "$CARGO_XWIN_VERSION"; then
    cargo install --locked cargo-xwin --version "$CARGO_XWIN_VERSION"
  fi
  if [ ! -x "$ffmpeg_dir/bin/ffmpeg.exe" ]; then
    mkdir -p "$ffmpeg_dir"
    curl -sSfL -o "$ffmpeg_dir.zip" "$FFMPEG_URL"
    echo "$FFMPEG_SHA256  $ffmpeg_dir.zip" | sha256sum -c -
    unzip -q -j -o "$ffmpeg_dir.zip" '*/bin/ffmpeg.exe' '*/bin/ffprobe.exe' -d "$ffmpeg_dir/bin"
    rm "$ffmpeg_dir.zip"
  fi
  if [ ! -x "$mingit_dir/cmd/git.exe" ]; then
    mkdir -p "$mingit_dir"
    curl -sSfL -o "$mingit_dir.zip" "$MINGIT_URL"
    echo "$MINGIT_SHA256  $mingit_dir.zip" | sha256sum -c -
    unzip -q -o "$mingit_dir.zip" -d "$mingit_dir"
    rm "$mingit_dir.zip"
  fi
  wine64 --version || wine --version
  # Create the prefix once, up front, so the first test does not pay for it. Under a
  # display: a prefix made without one has no graphics driver, so Vulkan is unavailable.
  (xvfb-run -a wineboot --init >/dev/null 2>&1 || true)
fi

# Static CRT, matching the native Windows release build.
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static"
# cargo-xwin sets Wine as the runner for `cargo xwin test`; be explicit for clarity.
wine_bin=$(command -v wine64 || command -v wine)
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER="$wine_bin"
xwin=(cargo xwin)

if want test; then
  "${xwin[@]}" test --locked --no-fail-fast --target "$TARGET" -p disk -p clean -p desktop \
    -j "${CARGO_BUILD_JOBS:-2}"
fi

if want build; then
  "${xwin[@]}" build --locked --release --target "$TARGET" -p desktop -p clean -j "${CARGO_BUILD_JOBS:-2}"
  ls -la "target/$TARGET/release/"*.exe
fi

if want smoke; then
  exe="target/$TARGET/release/desktop.exe"
  [ -f "$exe" ] || { echo "Build first: $exe is missing" >&2; exit 1; }
  # The Retired desktop proof flags must be refused, not mistaken for success.
  set +e
  "$wine_bin" "$exe" --proof-seconds 1 >/dev/null 2>&1
  code=$?
  set -e
  [ "$code" -eq 2 ] || { echo "Unported proof flag returned $code, expected 2" >&2; exit 1; }
  # The cleanup CLI runs end to end under Wine.
  "$wine_bin" "target/$TARGET/release/clean.exe" --help >/dev/null
  echo "Wine smoke checks passed."
fi

if want render; then
  exe="target/$TARGET/release/desktop.exe"
  [ -f "$exe" ] || { echo "Build first: $exe is missing" >&2; exit 1; }
  out=target/wine-shots
  rm -rf "$out" && mkdir -p "$out"
  # Without d3d12, Rune takes its Vulkan path, which Wine forwards to the host's Mesa
  # (lavapipe). Wine's D3DCompile cannot build Rune's DX12 shaders.
  export WINEDLLOVERRIDES="$WINEDLLOVERRIDES;d3d12,d3d12core="
  win_out="Z:$(cd "$out" && pwd -P | tr / '\\')"
  # Wine keeps one desktop per prefix: one started without a display (by the smoke step)
  # would be reused and offer no Vulkan. Stop it, then render inside one Xvfb session.
  wineserver -k 2>/dev/null || true
  wineserver -w 2>/dev/null || true
  export exe win_out wine_bin
  xvfb-run -a bash -c '
    for scene in library capture editor disk tools settings; do
      "$wine_bin" "$exe" --scene "$scene" --size 1200x800 --scale 1 \
        --snap "$win_out\\$scene.png" 2>&1 | grep -v "^rune:" || true
    done
    wineserver -w'
  for scene in library capture editor disk tools settings; do
    [ -s "$out/$scene.png" ] || { echo "Rendering $scene under Wine produced no image" >&2; exit 1; }
  done
  ls -la "$out"
fi
