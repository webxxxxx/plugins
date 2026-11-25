#!/bin/sh
# 编译 vendor/mini-sb-agent（Go），产物 out/mini-sb-agent；顺带编译 certgen/
# 里的证书生成器，直接放进 payload。两个都用标准库/无 cgo，交叉编译靠 Go 自己。
set -eu
NAME=$1; VERSION=$2; PLATFORM=$3
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$HERE/out"

command -v go >/dev/null 2>&1 || PATH="$HOME/go/bin:/usr/local/go/bin:$PATH"
command -v go >/dev/null 2>&1 || { echo 'hysteria: go is required to build mini-sb-agent' >&2; exit 1; }

case "$PLATFORM" in
  armv7-unknown-linux-musleabihf) GOOS=linux GOARCH=arm GOARM=7 ;;
  aarch64-unknown-linux-musl)     GOOS=linux GOARCH=arm64 ;;
  x86_64-unknown-linux-musl|x86_64-unknown-linux-gnu) GOOS=linux GOARCH=amd64 ;;
  aarch64-apple-darwin)           GOOS=darwin GOARCH=arm64 ;;
  x86_64-apple-darwin)            GOOS=darwin GOARCH=amd64 ;;
  x86_64-pc-windows-msvc)         GOOS=windows GOARCH=amd64 ;;
  *) echo "unsupported platform: $PLATFORM" >&2; exit 1 ;;
esac

export CGO_ENABLED=0 GOOS GOARCH
if [ -n "${GOARM:-}" ]; then export GOARM; fi
JOBS=${JOBS:-4}

cd "$HERE/vendor/mini-sb-agent"
go build -p "$JOBS" -trimpath -tags with_gvisor -ldflags "-s -w" \
  -o "$HERE/out/mini-sb-agent" ./cmd/mini-sb-agent

# 证书和密码的生成器：用户机器上可能没有 openssl，也不一定有 od，而 hysteria2
# 入站必须要 cert/key。它只依赖标准库，跟着同一个 GOOS/GOARCH 编。
(
  cd "$HERE/certgen"
  go build -p "$JOBS" -trimpath -ldflags "-s -w" -o "$HERE/out/hysteria-certgen" .
)
mkdir -p "$HERE/package/payload/bin"
cp "$HERE/out/hysteria-certgen" "$HERE/package/payload/bin/hysteria-certgen"
