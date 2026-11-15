#!/bin/sh
# 编译 vendor/mini-sb-agent（Go），产物 out/mini-sb-agent。
set -eu
PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$HERE/out"
command -v go >/dev/null 2>&1 || PATH="$HOME/go/bin:/usr/local/go/bin:$PATH"
case "$PLATFORM" in
  armv7-unknown-linux-musleabihf) GOOS=linux GOARCH=arm GOARM=7 ;;
  aarch64-unknown-linux-musl)     GOOS=linux GOARCH=arm64 ;;
  x86_64-unknown-linux-musl|x86_64-unknown-linux-gnu) GOOS=linux GOARCH=amd64 ;;
  aarch64-apple-darwin)           GOOS=darwin GOARCH=arm64 ;;
  x86_64-apple-darwin)            GOOS=darwin GOARCH=amd64 ;;
  x86_64-pc-windows-msvc)         GOOS=windows GOARCH=amd64 ;;
  *) echo "unsupported platform: $PLATFORM" >&2; exit 1 ;;
esac
cd "$HERE/vendor/mini-sb-agent"
CGO_ENABLED=0 GOOS=$GOOS GOARCH=$GOARCH GOARM=${GOARM:-} \
  JOBS=${JOBS:-4}
go build -p "$JOBS" -trimpath -tags with_gvisor -ldflags "-s -w" \
  -o "$HERE/out/mini-sb-agent" ./cmd/mini-sb-agent
