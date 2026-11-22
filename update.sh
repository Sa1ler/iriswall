#!/bin/sh
set -e
git pull --ff-only
cargo build --release
iriswall quit 2>/dev/null || true
install -Dm755 target/release/iriswall ~/.local/bin/iriswall
setsid iriswall >/dev/null 2>&1 &
