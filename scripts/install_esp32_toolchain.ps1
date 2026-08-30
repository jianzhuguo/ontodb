# ESP32 工具链手动安装脚本
# 从国内镜像下载所需文件

$TOOLS_DIR = "$env:USERPROFILE\.espressif"
$RUST_DIR = "$env:USERPROFILE\.rustup\toolchains\esp"

# 创建目录
New-Item -ItemType Directory -Force -Path $TOOLS_DIR | Out-Null
New-Item -ItemType Directory -Force -Path $RUST_DIR | Out-Null

Write-Host "=== ESP32 工具链手动安装 ==="
Write-Host ""
Write-Host "需要下载 3 个文件（需要能访问 GitHub）："
Write-Host ""
Write-Host "1. GCC 编译器 (xtensa-esp-elf)"
Write-Host "   https://github.com/espressif/crosstool-NG/releases/download/esp-15.2.0_20250920/xtensa-esp-elf-15.2.0_20250920-x86_64-w64-mingw32.zip"
Write-Host "   解压到: $TOOLS_DIR\xtensa-esp-elf\"
Write-Host ""
Write-Host "2. LLVM (clang)"
Write-Host "   https://github.com/espressif/llvm-project/releases/download/esp-20.1.1_20250829/libs-clang-esp-20.1.1_20250829-x86_64-w64-mingw32.tar.xz"
Write-Host "   解压到: $TOOLS_DIR\xtensa-esp-elf-clang\"
Write-Host ""
Write-Host "3. Rust 工具链 (Xtensa)"
Write-Host "   https://github.com/esp-rs/rust-build/releases/download/v1.97.0.0/rust-1.97.0.0-x86_64-pc-windows-msvc.zip"
Write-Host "   解压到: $RUST_DIR\"
Write-Host ""
Write-Host "下载完成后运行: espup install --targets esp32"
