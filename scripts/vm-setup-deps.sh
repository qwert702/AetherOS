#!/bin/bash
# VM 构建环境准备：Buildroot 依赖 + Rust（国内镜像）
set -e
sudo apt-get update -q
sudo apt-get install -y -q build-essential wget cpio unzip rsync bc file \
    libssl-dev pkg-config musl-tools curl
echo "=== build deps ok ==="

# Rust（TUNA 静态源）
if ! command -v rustc >/dev/null 2>&1; then
    export RUSTUP_DIST_SERVER="https://mirrors.tuna.tsinghua.edu.cn/rustup"
    export RUSTUP_UPDATE_ROOT="https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh
    sh /tmp/rustup.sh -y --default-toolchain stable --profile minimal
fi
source "$HOME/.cargo/env"

# crates.io 国内源（rsproxy，宿主机同款）
mkdir -p ~/.cargo
cat > ~/.cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = 'rsproxy-sparse'
[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
EOF
rustc --version
cargo --version
echo "=== rust ok ==="
