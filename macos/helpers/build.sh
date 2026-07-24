#!/usr/bin/env bash
# Build the native helper used by the macOS "fullscreen Space" launch path.
#
#   acspaces.dylib  x86_64 dylib, injected into Wine (DYLD_INSERT_LIBRARIES), calls
#                   AC's own window's -toggleFullScreen: in-process (no Accessibility).
#
# core/build.rs builds it automatically at `cargo build` time (clang is always present
# on a macOS build host), so this script is only for building it by hand.
#
# There used to be a second helper, acwindow.exe: a 32-bit Windows process that polled
# for AC's window and added WS_THICKFRAME so winemac would grant it the fullscreen
# capability. The `resizable-window` byte patch in core/src/patches.rs makes AC apply
# that style itself, so the helper -- and the mingw-w64 build dependency it needed --
# is gone.
set -euo pipefail
cd "$(dirname "$0")"

echo "building acspaces.dylib (x86_64)…"
clang -arch x86_64 -dynamiclib -framework AppKit -O2 -o acspaces.dylib acspaces.m
echo "done."
