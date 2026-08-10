.PHONY: build build-release test bench clean install run docker

# 默认构建
build:
	cargo build

# Release 构建
build-release:
	cargo build --release

# 企业版构建
build-enterprise:
	cargo build --release --features enterprise-standard

build-gov:
	cargo build --release --features enterprise-gov

# 运行测试
test:
	cargo test --workspace

# 运行基准测试
bench:
	cargo bench --bench lock_contention -p onto-storage
	cargo bench --bench batch_import -p onto-storage

# 清理构建产物
clean:
	cargo clean

# 安装到本地
install:
	cargo install --path crates/onto-cli
	cargo install --path crates/onto-server

# 启动服务器
run:
	cargo run --release --bin ontodb-server -- --data-dir ./data --http 127.0.0.1:7912 --no-rate-limit

# 启动服务器（带认证）
run-auth:
	cargo run --release --bin ontodb-server -- --data-dir ./data --http 127.0.0.1:7912 --auth

# 启动 CLI
cli:
	cargo run --release --bin ontodb-cli -- 127.0.0.1:7912

# Docker 构建
docker:
	docker build -t ontodb/ontodb:latest .

# Docker 运行
docker-run:
	docker-compose up -d

# 格式化代码
fmt:
	cargo fmt

# 代码检查
clippy:
	cargo clippy --workspace -- -D warnings

# 生成文档
docs:
	cargo doc --workspace --open

# 代码统计
stats:
	@echo "=== 代码行数统计 ==="
	@find crates -name "*.rs" -exec cat {} + | wc -l
	@echo ""
	@echo "=== 各 Crate 行数 ==="
	@for dir in crates/*/; do \
		name=$$(basename $$dir); \
		lines=$$(find $$dir -name "*.rs" -exec cat {} + 2>/dev/null | wc -l); \
		echo "  $$name: $$lines 行"; \
	done
