#!/usr/bin/env bash
# 在**构建机内**跑 ISO 构建（由宿主通过 `scripts/vm.py sh` 触发）。
#
# 为什么需要这一层：`platform/build-iso.sh` 里用的是裸 `cargo` / `rustup`，而
# SSH **非交互式会话的 PATH 不包含 `~/.cargo/bin`** —— 直接调会得到
# `cargo: command not found`，看起来像"构建脚本坏了"。这里显式补 PATH。
#
# 另外把输出落到日志：增量构建约 4 分钟、首次 30–60 分钟，全程挂在 SSH 通道上
# 既容易断也无法观察进度。宿主侧用 `vm.py sh "tail -20 /home/aether/iso-build.log"`
# 或 `vm-pull.py` 取回。
#
# 用法（在构建机内）:
#   bash scripts/vm-build-iso.sh              # 后台由调用方决定
# 产物:  /home/aether/aetheros-0.1-amd64.iso
set -uo pipefail

REPO="${REPO:-/home/aether}"
LOG="$REPO/iso-build.log"
export PATH="$REPO/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

cd "$REPO" || exit 1

{
  echo "=== $(date -Is) ISO 构建开始 ==="
  bash platform/build-iso.sh
  rc=$?
  echo "=== $(date -Is) 结束 rc=$rc ==="
  if [ "$rc" -eq 0 ]; then echo RESULT=PASS; else echo RESULT=FAIL; fi
  ls -la "$REPO"/aetheros-*.iso 2>/dev/null
} > "$LOG" 2>&1
