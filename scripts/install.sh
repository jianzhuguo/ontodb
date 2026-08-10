#!/usr/bin/env bash
# OntoDB 一键安装脚本 (Linux/macOS)
# 用法: curl -fsSL https://get.ontodb.io | bash

set -euo pipefail

VERSION="${ONTODB_VERSION:-v0.3.0}"
INSTALL_DIR="${ONTODB_INSTALL_DIR:-$HOME/.ontodb}"
DATA_DIR="${ONTODB_DATA_DIR:-$HOME/.ontodb/data}"
PORT="${ONTODB_PORT:-7912}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log()   { echo -e "${GREEN}[✓]${NC} $1"; }
warn()  { echo -e "${YELLOW}[!]${NC} $1"; }
error() { echo -e "${RED}[✗]${NC} $1"; exit 1; }
info()  { echo -e "${BLUE}[i]${NC} $1"; }

echo ""
echo "  ╔══════════════════════════════════════════════╗"
echo "  ║     OntoDB — 本体驱动的六模态语义数据库       ║"
echo "  ║     安装脚本 $VERSION                         ║"
echo "  ╚══════════════════════════════════════════════╝"
echo ""

# 检测操作系统和架构
detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    
    case "$os" in
        Linux*)   PLATFORM="linux" ;;
        Darwin*)  PLATFORM="macos" ;;
        *)        error "不支持的操作系统: $os" ;;
    esac
    
    case "$arch" in
        x86_64|amd64)  ARCH="x86_64" ;;
        aarch64|arm64) ARCH="aarch64" ;;
        *)             error "不支持的架构: $arch" ;;
    esac
    
    info "检测到平台: ${PLATFORM}-${ARCH}"
}

# 检查依赖
check_deps() {
    local missing=()
    
    if ! command -v curl &>/dev/null && ! command -v wget &>/dev/null; then
        missing+=("curl 或 wget")
    fi
    
    if ! command -v tar &>/dev/null; then
        missing+=("tar")
    fi
    
    if [ ${#missing[@]} -gt 0 ]; then
        error "缺少依赖: ${missing[*]}"
    fi
    
    log "依赖检查通过"
}

# 下载 OntoDB
download() {
    local url="https://release.ontodb.io/ontodb-${VERSION}-${PLATFORM}-${ARCH}.tar.gz"
    local tmp_file="/tmp/ontodb-${VERSION}.tar.gz"
    
    info "下载 OntoDB ${VERSION}..."
    
    if command -v curl &>/dev/null; then
        curl -fSL "$url" -o "$tmp_file" 2>/dev/null || error "下载失败，请检查网络连接"
    else
        wget -q "$url" -O "$tmp_file" 2>/dev/null || error "下载失败，请检查网络连接"
    fi
    
    log "下载完成"
    
    # 解压
    mkdir -p "$INSTALL_DIR"
    tar xzf "$tmp_file" -C "$INSTALL_DIR" --strip-components=1
    rm -f "$tmp_file"
    
    log "安装到 $INSTALL_DIR"
}

# 创建数据目录
setup_data() {
    mkdir -p "$DATA_DIR"
    log "数据目录: $DATA_DIR"
}

# 生成配置文件
generate_config() {
    local config_file="$INSTALL_DIR/ontodb.toml"
    
    if [ -f "$config_file" ]; then
        warn "配置文件已存在，跳过生成"
        return
    fi
    
    cat > "$config_file" <<EOF
# OntoDB 配置文件

[data]
dir = "${DATA_DIR}"

[server]
http = "127.0.0.1:${PORT}"
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"

[auth]
enabled = false
# api_key = "your-secret-key"

[performance]
memtable_size_mb = 64
block_cache_mb = 256
sync_wal_on_commit = false

[security]
rate_limit_rpm = 60
max_query_size_mb = 10
EOF
    
    log "生成配置文件: $config_file"
}

# 添加到 PATH
setup_path() {
    local shell_rc
    
    if [ -f "$HOME/.zshrc" ]; then
        shell_rc="$HOME/.zshrc"
    elif [ -f "$HOME/.bashrc" ]; then
        shell_rc="$HOME/.bashrc"
    elif [ -f "$HOME/.bash_profile" ]; then
        shell_rc="$HOME/.bash_profile"
    else
        shell_rc="$HOME/.profile"
    fi
    
    if ! echo "$PATH" | grep -q "$INSTALL_DIR/bin"; then
        echo "export PATH=\"\$PATH:$INSTALL_DIR/bin\"" >> "$shell_rc"
        export PATH="$PATH:$INSTALL_DIR/bin"
        log "已添加到 PATH (重启终端生效)"
    fi
}

# 创建 systemd 服务 (Linux)
setup_systemd() {
    if [ "$PLATFORM" != "linux" ]; then
        return
    fi
    
    if ! command -v systemctl &>/dev/null; then
        return
    fi
    
    local service_file="/etc/systemd/system/ontodb.service"
    
    if [ -f "$service_file" ]; then
        warn "systemd 服务已存在"
        return
    fi
    
    info "创建 systemd 服务..."
    
    sudo tee "$service_file" > /dev/null <<EOF
[Unit]
Description=OntoDB Semantic Database
After=network.target

[Service]
Type=simple
User=$USER
ExecStart=$INSTALL_DIR/bin/ontodb-server --data-dir $DATA_DIR --http 0.0.0.0:${PORT}
Restart=on-failure
RestartSec=5
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF
    
    sudo systemctl daemon-reload
    log "systemd 服务已创建"
    info "启动: sudo systemctl start ontodb"
    info "开机自启: sudo systemctl enable ontodb"
}

# 打印使用说明
print_usage() {
    echo ""
    echo "  ╔══════════════════════════════════════════════╗"
    echo "  ║              安装完成！                       ║"
    echo "  ╚══════════════════════════════════════════════╝"
    echo ""
    echo "  启动服务器:"
    echo "    $INSTALL_DIR/bin/ontodb-server --data-dir $DATA_DIR --http 127.0.0.1:${PORT}"
    echo ""
    echo "  或使用 systemd (Linux):"
    echo "    sudo systemctl start ontodb"
    echo ""
    echo "  访问地址:"
    echo "    Web 控制台:  http://127.0.0.1:${PORT}/console"
    echo "    API 文档:    http://127.0.0.1:${PORT}/api/docs"
    echo "    健康检查:    http://127.0.0.1:${PORT}/api/health"
    echo ""
    echo "  CLI 工具:"
    echo "    $INSTALL_DIR/bin/ontodb-cli 127.0.0.1:${PORT}"
    echo ""
    echo "  配置文件:"
    echo "    $INSTALL_DIR/ontodb.toml"
    echo ""
    echo "  数据目录:"
    echo "    $DATA_DIR"
    echo ""
}

# 主流程
main() {
    detect_platform
    check_deps
    download
    setup_data
    generate_config
    setup_path
    setup_systemd
    print_usage
}

main "$@"
