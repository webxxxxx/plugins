#!/bin/sh
# 旧名字，真正的实现在 docker-build.sh。
# 保留它只为已经写死的调用（老脚本、老习惯），参数原样转发。
exec sh "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/docker-build.sh" "$@"
