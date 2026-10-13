# Issue: ontodb-server 反复 APPCRASH（0xc0000005）—— SST 迭代器 seek 陈旧索引 + 全量扫描内存压力

## 现象

- 进程 `ontodb-server.exe` 无 Rust panic 输出，直接段错误退出
- Windows Event Log：`Application Error (1000)` / WER `APPCRASH`
- 异常代码 `0xc0000005`（ACCESS_VIOLATION），错误偏移在 `.text` 内
- 本次崩溃：2026-10-12 15:43:31，偏移 `0x8BE171`（RVA，ImageBase 0x140000000）
- 历史崩溃：偏移 `0x8F2368`（见 `analyze_crash.py` / `resolve_symbol.py`）
- **WER 档案中已有 20+ 条 `AppCrash_ontodb-server.exe` 记录**——长期、反复崩溃，非偶发

触发场景：对 13.6M 行 / 9.2 GB LSM 库连续执行 `COUNT(*)` 全表扫描（75 个类连环计数），同时启动预热（warmup）正在统计行数。

## 排查过程

| 假设 | 结论 |
|---|---|
| SST 数据损坏 | **排除**。344/344 个 SST footer 校验通过（magic/index_offset/bloom_offset/max_seq 均合法，无 underflow） |
| 磁盘/硬件 | 存疑。机器同日有 BSOD `0x1a MEMORY_MANAGEMENT`（内存管理），可能放大问题 |
| Rust panic | **排除**。日志无 `panicked at`，stderr 在 sst_scan 中途截断——符合硬 SEH 特征 |
| 代码 bug | **确认至少 2 处**，见下 |

## Bug 1（确认，逻辑/panic）：`SsTableIterator::seek` 使用陈旧条目索引

位置：`crates/onto-storage/src/lsm/sstable.rs`，`seek()` / `load_block()` / `key()`

```rust
pub fn seek(&mut self, target: &[u8]) {
    match self.table.find_block(target) {
        Ok(idx) => {
            if let Err(_) = self.load_block(idx) {
                self.valid = false;
                return;
            }
            // BUG: load_block 只替换 block_data / pos=0，不重置 valid/current_*，
            // 也未 advance 到新块首条目。下面 key() 用的是【上一个块】的
            // current_key_start/end 去切【新块】的 block_data。
            loop {
                if !self.valid { break; }
                if self.key() >= target { break; }
                self.advance_to_next_entry();
            }
        }
        Err(_) => { self.valid = false; }
    }
}

pub fn key(&self) -> &[u8] {
    &self.block_data[self.current_key_start..self.current_key_end]  // 越界 → panic
}
```

**成因**：`SsTableIterator::new()` 会 `load_block(0)+advance` → `valid=true`，`current_*` 指向块 0 首条目。随后 `scan_prefix_internal` 立刻 `iter.seek(prefix)`：

1. `find_block(prefix)` 返回 `idx`（常 ≠ 0）
2. `load_block(idx)` 换掉 `block_data`，但 `valid` 与 `current_key_start/end` **仍是块 0 的**
3. `self.key()` 用旧偏移切新块：
   - 旧 `key_end` > 新块长度 → **slice 越界 panic**
   - 否则返回错误切片，seek 定位错乱（错结果）

**为何平时不崩**：块 0 首条目的偏移通常较小，多数块够大，只返回错 key；仅当新块比旧偏移小时才 panic。多块、块大小不均 + 高并发扫描时概率上升——与"重负载下偶发崩溃"吻合。

**修复建议**（`seek` 内，`load_block` 成功后）：

```rust
if let Err(_) = self.load_block(idx) {
    self.valid = false;
    return;
}
self.advance_to_next_entry(); // 解码新块首条目，刷新 current_* 与 valid
loop {
    if !self.valid { break; }
    if self.key() >= target { break; }
    self.advance_to_next_entry();
}
```

并建议在 `key()` / `value()` 加防御（或 debug_assert）：

```rust
pub fn key(&self) -> &[u8] {
    debug_assert!(self.current_key_end <= self.block_data.len());
    &self.block_data[self.current_key_start..self.current_key_end]
}
```

**单测建议**：构建多块 SST（块大小不均），从块 0 条目状态 seek 到后面更小的块，断言不 panic 且定位正确。

## Bug 2（确认，内存）：`scan_prefix_internal` 把全量 value 塞进 HashMap

位置：`crates/onto-storage/src/engine.rs` `scan_prefix_internal`

```rust
let mut seen: HashMap<Vec<u8>, (Vec<u8>, SeqNo, EntryKind)> = HashMap::new();
// ...
seen.insert(key, (value, seq, kind));  // value 是完整 JSON 文档
```

- `COUNT(*)` / 预热计数都走这条路径，却保存了**所有 value**
- 库规模：80 表 **13,685,627 行**，`sembio.BiologicalEntity` 层级 **1,970,233 行**
- 单机内存：**15.7 GB**（WER: `ram=15706`），SST 9.2 GB + 344 个句柄 + block cache
- 大类全扫时 `seen` 可达数 GB → 分配失败 / OOM，与 SEH 表现纠缠

**修复建议**：
- 计数路径改走已有 `count_prefix()`（只计数不存 value）
- `scan_prefix` 对 `COUNT`/纯扫描场景提供不物化 value 的变体
- 或对 `seen` 加内存上限/流式去重

## Bug 3（次要，防御缺失）：`SsTable::open` footer 偏移无校验

```rust
let bloom_data_len = (file_len - footer_size as u64 - bloom_offset) as usize;
// u64 下溢 → 巨量分配
let index_data_len = (bloom_offset - index_offset) as usize;
```

本批 SST 实际完好，但若遇截断/半写文件会下溢。应对 `bloom_offset` / `index_offset` 做范围检查后返回 `CoreError::corruption`。

## 环境与复现

- 版本：0.6.2，release，`strip = true`（**无符号，偏移无法映射函数**）
- 数据：`E:\sembio\data\ontodb_clean`，344 SST，约 9.2 GB
- 复现：启动后对多个大类连环 `COUNT(*)`（尤其含 `BiologicalEntity` 层级展开）
- 内存：16 GB 机器

## 建议处理顺序

1. **修 Bug 1**（seek 陈旧索引）——改动小、收益直接，附多块单测
2. **修 Bug 2**（count 不物化 value）——消除 OOM 路径
3. release 构建保留 `debug` 符号或单独出 `release-with-symbols`，便于下次定位
4. `SsTable::open` 补偏移范围校验（Bug 3）
5. 长期：STORAGE 级 `eprintln!` 调试日志移出 release（历史 `server_err.log` 达 195 MB）

## 附：关键证据

- 崩溃时刻：`2026-10-12 15:43:31`，PID 0x135c（4956）
- WER：`ReportArchive\AppCrash_ontodb-server.ex_fe49ecf3...`，`Sig[6]=c0000005`，`Sig[7]=00000000008be171`
- 日志截断点：`[STORAGE sst_scan] sst_idx=32, prefix='sembio.BioAssay::'`（扫描中途）
- SST 校验脚本结果：344 文件全部合法（非数据损坏）
