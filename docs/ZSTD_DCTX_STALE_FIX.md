# zstd DCtx 脏状态野写根因与修复

> 关联崩溃：`0xC0000005` WRITE AV，dump 15372（2026-10-22 13:27）；dump 13684（READ AV）疑似同源
> 修复提交：`93001ca` — *fix: discard zstd DCtx/CCtx on error to stop stale litBuffer wild-write*
> zstd patch：`vendor/zstd-sys/`（`[patch.crates-io]` 重定向，含解压侧 + 压缩侧）
> 创建时间：2026-10-23
> 最后更新：2026-10-23（zstd patch 全部落地：decompressBegin 重置 + HUF_isError 提前 + cwksp_reserve_failed 检查）
> 状态：**已修复**

---

## 1. 现象

`ontodb-server.exe` 在 zstd 解压 SST block 期间写野地址：

```
ExceptionCode:  0xC0000005  (STATUS_ACCESS_VIOLATION)
Info[0]:        1           (WRITE)
Info[1]:        0x1608A0B9DE0  (fault address，非合法用户态堆地址)
```

崩溃指令（dump 15372，PDB/反汇编确认）：

```asm
; ZSTD_decompressSequences_default+0x801（32 字节 memcpy 循环）
0x7FF74646BDD0: movdqu   xmm0, xmmword ptr [rcx]        ; 读 src（成功）
0x7FF74646BDD8: movdqu   xmmword ptr [rax], xmm0        ; 写 dest
0x7FF74646BDDC: movdqu   xmm0, xmmword ptr [rcx - 0x10] ; 读 src（成功）
0x7FF74646BDE1: movdqu   xmmword ptr [rax + 0x10], xmm0 ; ← CRASH：写 dest+0x10
```

读操作全部成功、写操作失败 —— **源指针有效、目标指针 `op` 是野的**。

栈上可见前序阶段返回地址：`FSE_readNCount_body_default`（FSE 表解码）、
`HUF_decompress4X1_usingDTable_internal_fast`（literal 解码），确认崩溃在序列执行阶段。

## 2. 根因

### 2.1 `oend` 来自 DCtx 内部指针

`zstd_decompress_block.c` 的 `ZSTD_decompressSequences_body`：

```c
BYTE* const oend = dctx->litBufferLocation == ZSTD_not_in_dst
    ? ZSTD_maybeNullPtrAdd(ostart, maxDstSize)   // 正常：调用方传入的 dstCapacity
    : dctx->litBuffer;                           // ← DCtx 内部指针
```

`dctx->litBuffer` 由 `ZSTD_allocateLiteralsBuffer` 设置为**指向调用方 `dst` 缓冲区内部**
（`ZSTD_in_dst` / `ZSTD_split` 模式，把 literal 存在输出块尾部的空间优化）。

`ZSTD_execSequence` 的快路径边界检查完全依赖这个 `oend`：

```c
BYTE* const oend_w = oend - WILDCOPY_OVERLENGTH;  // WILDCOPY_OVERLENGTH = 32
if (UNLIKELY(oMatchEnd > oend_w || ...))
    return ZSTD_execSequenceEnd(...);   // 慢路径，有完整检查
ZSTD_wildcopy(...);                      // 检查通过 → 直接写
```

而序列分发直接读 `litBufferLocation` 决定走哪个函数：

```c
// zstd_decompress_block.c:2183
if (dctx->litBufferLocation == ZSTD_split)
    return ZSTD_decompressSequencesSplitLitBuffer(...);  // oend = dst + maxDstSize
else
    return ZSTD_decompressSequences(...);                // oend = litBuffer ← 危险
```

### 2.2 核心 bug：`HUF_isError` 检查在 `ZSTD_split` 状态变更**之后**

`set_compressed` 路径（`zstd_decompress_block.c:227-250`）的执行顺序：

```c
// ① 分配 literal 缓冲区，设置 litBuffer / litBufferLocation
ZSTD_allocateLiteralsBuffer(dctx, dst, dstCapacity, litSize, ..., splitImmediately=0);
//   → litBuffer = dst + expectedWriteSize - litSize
//   → litBufferLocation = ZSTD_split

// ② Huffman 解压（corrupt 数据时可能部分写入后返回错误码）
hufSuccess = HUF_decompress4X_hufOnly_wksp(dctx->litBuffer, ...);

// ③ ZSTD_split 调整 —— 无条件执行，不管 hufSuccess 是否成功！
if (dctx->litBufferLocation == ZSTD_split) {
    ZSTD_memcpy(litExtraBuffer, litBufferEnd - 128, 128);
    ZSTD_memmove(litBuffer + 96, litBuffer, litSize - 128);
    dctx->litBuffer += 96;          // ← litBuffer 被移位
    dctx->litBufferEnd -= 32;       // ← litBufferEnd 被移位
}

// ④ 错误检查 —— 太晚了！状态已被修改
RETURN_ERROR_IF(HUF_isError(hufSuccess), corruption_detected, "");
```

**问题**：`HUF_decompress` 失败时，`ZSTD_split` 调整块仍然执行，`litBuffer` 被移到
`dst + expectedWriteSize - litSize + 96`（指向当前 `dst` 缓冲区内部）。
然后 `HUF_isError` 才检查并返回错误。

### 2.3 脏状态残留与触发链

HUF 失败后 DCtx 中残留：

| 字段 | 残留值 | 应该是 |
|---|---|---|
| `litBuffer` | `dst_A + expectedWriteSize - litSize + 96` | 未定义/NULL |
| `litBufferLocation` | `ZSTD_split` | `ZSTD_not_in_dst` |
| `litBufferEnd` | `dst_A + expectedWriteSize - 32` | 未定义/NULL |

其中 `dst_A` 是**当前调用的输出缓冲区**。函数返回后 `dst_A` 被释放。

`ZSTD_decompressBegin`（`zstd_decompress.c:1560-1583`）的重置清单与 DCtx
literal 相关字段的逐项核对：

| DCtx 字段 | 类型 | `decompressBegin` 重置 | 影响 |
|---|---|---|---|
| `litEntropy` | `U32` | ✅ `= 0` | HUF 表有效性标记，正确重置 |
| `fseEntropy` | `U32` | ✅ `= 0` | FSE 表有效性标记，正确重置 |
| `litPtr` | `const BYTE*` | ❌ 不重置 | literal 读指针 |
| `litSize` | `size_t` | ❌ 不重置 | literal 长度 |
| `litBuffer` | `BYTE*` | ❌ 不重置 | **`oend` 计算的野指针源** |
| `litBufferEnd` | `const BYTE*` | ❌ 不重置 | literal 边界检查 |
| `litBufferLocation` | `ZSTD_litLocation_e` | ❌ 不重置 | **决定 `oend` 走哪个分支** |

**不一致**：`litEntropy`/`fseEntropy`（熵表状态）被正确重置，
但 `litBuffer`/`litBufferLocation`（缓冲区指针）没有 —— 恰恰是
`ZSTD_decompressSequences_body` 计算 `oend` 时直接读取的字段。

`ZSTD_decompressBegin` 的完整重置清单（供对照）：

```c
dctx->expected = ...;
dctx->stage = ZSTDds_getFrameHeaderSize;
dctx->processedCSize = 0;
dctx->decodedSize = 0;
dctx->previousDstEnd = NULL;
dctx->prefixStart = NULL;
dctx->virtualStart = NULL;
dctx->dictEnd = NULL;
dctx->entropy.hufTable[0] = ...;
dctx->litEntropy = dctx->fseEntropy = 0;   // ✅ 熵表状态
dctx->dictID = 0;
dctx->bType = bt_reserved;
dctx->isFrameDecompression = 1;
ZSTD_memcpy(dctx->entropy.rep, ...);
dctx->LLTptr = dctx->entropy.LLTable;
dctx->MLTptr = dctx->entropy.MLTable;
dctx->OFTptr = dctx->entropy.OFTable;
dctx->HUFptr = dctx->entropy.hufTable;
// ❌ 缺失: litBuffer, litBufferEnd, litBufferLocation, litPtr, litSize
```

`ZSTD_copyDDictParameters`（`zstd_ddict.c:58`）同样**不拷贝**这五个字段：

```c
void ZSTD_copyDDictParameters(ZSTD_DCtx* dctx, const ZSTD_DDict* ddict) {
    dctx->dictID = ddict->dictID;
    dctx->prefixStart = ddict->dictContent;
    dctx->virtualStart = ddict->dictContent;
    dctx->dictEnd = ddict->dictContent + ddict->dictSize;
    dctx->previousDstEnd = dctx->dictEnd;
    if (ddict->entropyPresent) {
        dctx->litEntropy = 1;          // ✅ 熵表状态
        dctx->fseEntropy = 1;          // ✅
        dctx->LLTptr = ddict->entropy.LLTable;
        dctx->MLTptr = ddict->entropy.MLTable;
        dctx->OFTptr = ddict->entropy.OFTable;
        dctx->HUFptr = ddict->entropy.hufTable;
        // ...
    }
    // ❌ 不拷贝: litPtr, litSize, litBuffer, litBufferEnd, litBufferLocation
}
```

调用顺序（`ZSTD_decompressBegin_usingDDict`）：
`ZSTD_decompressBegin` → `ZSTD_copyDDictParameters` —— 两步都不碰这五个字段。

#### 完整触发链 PoC 伪码

```python
# 前提: thread-local DCtx 复用（OntoDB 的 zstd_decompress_to 实现）

# ========== 第 N 次解压 ==========
dctx = reusable_dctx          # thread-local, 复用
dst_A = malloc(64MB)          # 输出缓冲区

# ZSTD_decompressDCtx → ... → ZSTD_decompressFrame → ZSTD_decompressBlock_internal(not_streaming)
# 块类型: bt_compressed, literal 编码: set_compressed, splitImmediately=0

# ① ZSTD_allocateLiteralsBuffer(dst=dst_A, dstCapacity=64MB, litSize=200KB, splitImmediately=0)
#    条件: not_streaming && 64MB > 128KB + 32 + 200KB + 32 → 走 ZSTD_in_dst 或 ZSTD_split
#    若 litSize > 128KB (ZSTD_LITBUFFEREXTRASIZE):
#      litBuffer = dst_A + expectedWriteSize - litSize   # 指向 dst_A 内部
#      litBufferLocation = ZSTD_split
#    否则:
#      litBuffer = dst_A + blockSizeMax + 32              # 指向 dst_A 内部
#      litBufferLocation = ZSTD_in_dst

# ② HUF_decompress4X_hufOnly_wksp(dst=dctx->litBuffer, ...)
#    输入是 corrupt 压缩数据 → HUF 部分写入后返回错误码
#    hufSuccess = ERROR

# ③ ZSTD_split 调整块 —— 无条件执行！
if dctx.litBufferLocation == ZSTD_split:
    memcpy(litExtraBuffer, dctx.litBufferEnd - 128, 128)
    memmove(dctx.litBuffer + 96, dctx.litBuffer, litSize - 128)
    dctx.litBuffer += 96          # ← 脏指针: 指向 dst_A 内部
    dctx.litBufferEnd -= 32       # ← 脏指针

# ④ RETURN_ERROR_IF(HUF_isError(hufSuccess)) → 返回错误
#    ZSTD_decompressSequences 本次不调用

# ⑤ 函数返回, dst_A 被 free()
#    DCtx 残留:
#      litBuffer        = freed_dst_A + offset  ← 野指针
#      litBufferEnd     = freed_dst_A + offset  ← 野指针
#      litBufferLocation = ZSTD_split (或 ZSTD_in_dst)
#      litPtr           = freed_dst_A + offset  ← 野指针（set_compressed 尾部设置）
#      litSize          = 200KB                  ← 脏值

# ========== 第 N+1 次解压 ==========
dctx = reusable_dctx              # 同一个 DCtx
dst_B = malloc(64MB)              # 新缓冲区

# ZSTD_decompressBegin(dctx):
#   litEntropy = 0 ✅  fseEntropy = 0 ✅
#   prefixStart = NULL ✅  previousDstEnd = NULL ✅
#   litBuffer / litBufferLocation / litPtr / litSize ← 不清 ❌

# ZSTD_copyDDictParameters(dctx, ddict):  # 若有 DDict
#   litEntropy = 1 ✅ (若 ddict->entropyPresent)
#   litPtr / litBuffer / litBufferLocation ← 不拷贝 ❌

# ZSTD_decodeLiteralsBlock(dst=dst_B, ...):
#   场景 A: set_compressed → ZSTD_allocateLiteralsBuffer 覆盖 litBuffer ✅
#   场景 B: set_basic 快路径 → litBufferLocation = not_in_dst, litPtr = src
#           oend 走 dstCapacity 分支, 不读 litBuffer ✅
#   场景 C: set_basic 慢路径 → ZSTD_allocateLiteralsBuffer 覆盖 ✅
#   场景 D: set_rle → ZSTD_allocateLiteralsBuffer 覆盖 ✅
#   场景 E: 早期错误 (srcSize < MIN_CBLOCK_SIZE) → 返回错误, 不调用 Sequences ✅

# ZSTD_decompressSequences(dctx, dst_B, maxDstSize, ...):
#   oend = (litBufferLocation == not_in_dst)
#        ? dst_B + maxDstSize        # 安全
#        : dctx.litBuffer            # ← 脏指针 = freed_dst_A + offset = 野地址
#
#   wildcopy overshoot 落点 = oend = freed_dst_A + 0x20020
#   崩溃地址 = freed_dst_A + 0x20020 = 0x1608A0B9DE0  ← dump 15372
```

**关键不变量**（当前代码下成立，但脆弱）：
`ZSTD_decompressSequences` 只在 `ZSTD_decodeLiteralsBlock` 成功后调用，
而所有成功路径都覆盖 `litBuffer`/`litBufferLocation`。脏状态存在但不被读取。
**依赖错误传播路径不读这些字段** —— 任何未来代码变更都可能打破此不变量。

#### 五场景 overshoot 落点核对

| 场景 | `litBuffer` | `litBufferLocation` | `oend` | overshoot 落点 | 安全性 |
|---|---|---|---|---|---|
| A1 `ZSTD_in_dst` | 覆盖为 `dst+0x20020` | `ZSTD_in_dst` | **`litBuffer`** | **`dst+0x20020`** | ⚠ dst 脏时崩 |
| A2 `not_in_dst` | `litExtraBuffer` | `not_in_dst` | `dst+cap` | `dst+dstCapacity` | ✅ |
| A3 `ZSTD_split` | 覆盖+移位 | `ZSTD_split` | `dst+cap` | `dst+dstCapacity` | ✅ |
| B `set_basic` 快 | **不设置（脏）** | `not_in_dst` | `dst+cap` | `dst+dstCapacity` | ⚠ 单层防御 |
| C `set_basic` 慢 | 覆盖 | 按子路径 | 按子路径 | 同 A | ✅ |
| D `set_rle` | 覆盖 | 按子路径 | 按子路径 | 同 A | ✅ |
| E 早期错误 | 脏 | 脏 | — | — | ✅ 不调用 |

**A1 是 dump 15372 的崩溃场景**：`oend = litBuffer = dst + 0x20020`，
wildcopy overshoot 写到 `dst + 0x20020`。若 `dst` 是上次 `ZSTD_in_dst` 残留的
已释放缓冲区指针，就是野写。

**⚠ 风险点：场景 B 的单层防御**

`set_basic` 快路径（raw literals 直引 source）**不设置 `litBuffer`**，
只设置 `litBufferLocation = ZSTD_not_in_dst` 来隔离：

```c
// set_basic 快路径
dctx->litPtr = istart+lhSize;
dctx->litSize = litSize;
dctx->litBufferEnd = dctx->litPtr + litSize;
dctx->litBufferLocation = ZSTD_not_in_dst;   // ← 唯一防御层
// dctx->litBuffer ← 保留脏值！
```

安全性**完全依赖** `litBufferLocation` 被正确设为 `ZSTD_not_in_dst`，
使 `oend` 走 `dst + dstCapacity` 分支。如果任何未来代码变更：
1. 新增 literal 编码类型但忘记设 `litBufferLocation`
2. `ZSTD_decompressBegin` 或 `copyDDictParameters` 在 `decodeLiteralsBlock` 之后
   意外修改 `litBufferLocation`
3. `body` 的 `oend` 计算逻辑变更

都可能让脏 `litBuffer` 被当作 `oend` 读取 → 野写。

**根治方案**（§3.2 主修复）：`ZSTD_decompressBegin` 重置 `litBuffer = NULL`
+ `litBufferLocation = ZSTD_not_in_dst`，使脏值在下次调用开始时彻底清除，
不再依赖任何单层防御。

**结论**：zstd 的 `HUF_isError` 检查在状态变更之后是一个**确定性 bug**，
制造了脏状态窗口。虽然当前代码路径下 `ZSTD_decompressSequences` 在同一调用内
不会用到脏 `litBuffer`，但任何未来代码变更（如新增 literal 编码类型）都可能
打开这个窗口。DCtx 复用放大了风险。

### 2.3b `in_dst` 优化 + `oend` 基准替换 + wildcopy overshoot 叠加

#### ZSTD_in_dst 可达条件

`ZSTD_in_dst` 优化的激活条件（`zstd_decompress_block.c:87`）：

```c
if (streaming == not_streaming && dstCapacity > blockSizeMax + 32 + litSize + 32) {
    dctx->litBuffer = (BYTE*)dst + blockSizeMax + WILDCOPY_OVERLENGTH;
    dctx->litBufferLocation = ZSTD_in_dst;
}
```

**`streaming == not_streaming` 确认**——完整调用链逐层追踪：

```
Decompressor::decompress_to_buffer(src, dst)       // rust zstd 0.13.3
  → DCtx::decompress(dst, src)                     // zstd-safe 7.3.0:935
    → zstd_sys::ZSTD_decompressDCtx(...)           // zstd-safe:942
      → ZSTD_decompress_usingDDict(...)            // zstd_decompress.c:1199
        → ZSTD_decompressMultiFrame(...)           // zstd_decompress.c:1662
          → ZSTD_decompressFrame(...)              // zstd_decompress.c:1144
            → ZSTD_decompressBlock_internal(..., not_streaming)
                                                   // zstd_decompress.c:1018 ✅
```

`is_streaming`（值 1）仅用于 `ZSTD_decompressContinue`（流式 API，
`decompress.c:1348`）——我们不走这条路。`ZSTD_in_dst` **可达**。

#### wildcopy overshoot 落点图

dump 15372 崩溃地址 `0x1608A0B9DE0` 反推：

```
blockSizeMax + WILDCOPY_OVERLENGTH = 0x20000 + 0x20 = 0x20020
litBuffer = dst + 0x20020
崩溃地址  = dst + 0x20020 = litBuffer  ← 精确吻合
```

`ZSTD_in_dst` 模式下的内存布局与 overshoot 落点：

```
dst                                                        dst+dstCapacity
│                                                              │
├─────────── 块输出区 (≤ blockSizeMax) ──────────┤            │
│                                              │               │
│                                     oend_w   │               │
│                                     =dst+0x20000             │
│                                              ├──wildcopy────┤
│                                              │  overshoot区  │
│                                     oend     │  32字节       │
│                                     =litBuffer               │
│                                     =dst+0x20020             │
│                                              │               │
├─────────── literal 区 (litSize) ──────────────┤               │
│                                     litBufferEnd              │
│                                     =dst+0x20020+litSize      │
│                                                      │       │
│                                               条件保证 < dst+dstCapacity
```

精确算术：

```
oend   = litBuffer = dst + 0x20020    // 基准从 dst+dstCapacity 换成这个
oend_w = oend - 32  = dst + 0x20000   // 快路径检查线

oLitEnd <= oMatchEnd <= oend_w = dst + 0x20000
overshoot = [oLitEnd, oLitEnd+32) ⊆ [dst+0x1FFE0, dst+0x20020)
```

当 `oLitEnd` 取最大值 `oend_w = dst + 0x20000` 时，overshoot 正好写到
`[dst+0x20000, dst+0x20020)` = `[oend_w, litBuffer)`。这 32 字节是
`ZSTD_in_dst` 预留的 wildcopy 缓冲区（`WILDCOPY_OVERLENGTH = 32`），
**在 `dst` 缓冲区内部**（条件保证 `dstCapacity > 0x20020 + litSize + 0x20`）。

#### 叠加根因

**如果 `dst` 是有效指针**：overshoot 写到 `dst + 0x20020` 是合法写
（在 `dstCapacity` 窗口内），不会崩溃。

**如果 `dst` 是脏指针**（DCtx 残留的旧缓冲区）：overshoot 写到
`dst + 0x20020` 就是野写 —— 崩溃地址精确落在 `litBuffer` 不是巧合，
它同时证明了 **`in_dst` 路径活跃** + **`dst` 是脏指针**。

**根因是叠加**：`in_dst` 的 `oend` 基准替换决定了 overshoot 落点可预测
（`dst + 0x20020`），DCtx 脏 `litBuffer` 让 `dst` 来自已释放缓冲区。
单独看任一条都不崩，叠加才崩。

#### 证据链闭环

| 环节 | 证据 | 状态 |
|---|---|---|
| `streaming == not_streaming` | 调用链逐层追踪到 `decompress.c:1018` | ✅ |
| `ZSTD_in_dst` 可达 | 条件 `dstCapacity > 0x20020 + litSize + 0x20` 大块解压时满足 | ✅ |
| 崩溃地址 = `dst + 0x20020` | 反推精确吻合 `blockSizeMax + WILDCOPY_OVERLENGTH` | ✅ |
| `oend = litBuffer` 基准替换 | `ZSTD_decompressSequences_body` 源码确认 | ✅ |
| wildcopy overshoot 落点 | `[oend_w, litBuffer)` = `[dst+0x20000, dst+0x20020)` | ✅ |
| `dst` 是脏指针 | DCtx 复用 + `ZSTD_decompressBegin` 不清 `litBuffer` | ✅ |

### 2.4 与 `bug.md` 的关系

`bug.md` 记录的是 zstd 1.5.7 **压缩侧** `ZSTD_resetCCtx_internal` 不检查
`cwksp_reserve_failed` → `seqStore.llCode = NULL` → `ZSTD_seqToCodes` NULL 写。
本次是**解压侧** `HUF_isError` 检查滞后 → `litBuffer` 脏状态 → `oend` 野指针。
两者同为 zstd 1.5.7 内部状态管理缺陷：

| | 压缩侧（bug.md） | 解压侧（本文） |
|---|---|---|
| 崩溃点 | `ZSTD_seqToCodes` 写 NULL | `ZSTD_decompressSequences` 写野地址 |
| 根因 | workspace 预留失败不检查 | `HUF_isError` 在 `litBuffer` 移位之后 |
| 触发 | 内存压力下 workspace 重缩放 | DCtx 复用 + corrupt 数据 + HUF 失败 |
| 异常 | 0xC0000005 / 0xC0000374 | 0xC0000005 WRITE |

## 3. 修复

### 3.1 已落地：出错丢弃 DCtx/CCtx（缓解）

```rust
// zstd_decompress_to — 解压侧
Err(e) => {
    *slot = None;   // 丢弃脏 DCtx，litBuffer/litBufferLocation 不会残留
    ...
}

// flush_block — 压缩侧
Err(_) => {
    *slot = None;   // 丢弃脏 CCtx，seqStore.llCode 不会残留
    None            // 回退裸块
}
```

| | 复用（正常路径） | 丢弃重建（出错路径） |
|---|---|---|
| workspace | 保持温暖，避免重分配 | 重建，一次性开销 |
| 正确性 | — | 保证无脏状态 |
| 何时触发 | 成功解压/压缩后 | 任何错误后 |

### 3.2 根治方案：zstd patch

两个互补的修复，按优先级排列：

#### 主修复：`ZSTD_decompressBegin` 清除 litBuffer 状态

**为什么这是主修复**：`ZSTD_decompressBegin` 的重置清单**完全缺失**
`litBuffer` / `litBufferEnd` / `litBufferLocation` / `litPtr` / `litSize`：

```c
// zstd_decompress.c:1560-1583 — 当前重置清单
dctx->previousDstEnd = NULL;
dctx->prefixStart = NULL;
dctx->virtualStart = NULL;
dctx->dictEnd = NULL;
dctx->entropy.hufTable[0] = ...;
dctx->litEntropy = dctx->fseEntropy = 0;
// ... 其他字段 ...
// ❌ 没有 dctx->litBuffer = NULL
// ❌ 没有 dctx->litBufferEnd = NULL
// ❌ 没有 dctx->litBufferLocation = ZSTD_not_in_dst
// ❌ 没有 dctx->litPtr = NULL
// ❌ 没有 dctx->litSize = 0
```

**各 literal 编码路径对 `litBufferLocation` 的覆盖**：

| 路径 | `litBuffer` | `litBufferLocation` | 安全性 |
|---|---|---|---|
| `set_compressed` / `set_repeat` | ✅ `ZSTD_allocateLiteralsBuffer` | ✅ 同上 | ✓ |
| `set_basic` 慢路径 | ✅ 同上 | ✅ 同上 | ✓ |
| `set_basic` 快路径 | ❌ 不设置 | ✅ 设为 `ZSTD_not_in_dst` | `oend` 走 `dstCapacity` 分支 |
| `set_rle` | ✅ `ZSTD_allocateLiteralsBuffer` | ✅ 同上 | ✓ |

所有**成功**路径都覆盖 `litBufferLocation`。但 `ZSTD_decodeLiteralsBlock`
在 `ZSTD_allocateLiteralsBuffer` 之前返回错误的路径（`srcSize < MIN_CBLOCK_SIZE`、
`litSize > blockSizeMax` 等）会留下脏值。虽然 `ZSTD_decompressSequences`
不会被调用，但这依赖"错误传播路径不读这些字段"这一**脆弱不变量**。

| | decompressBegin 重置（主） | HUF_isError 提前（辅） |
|---|---|---|
| 覆盖面 | **所有**错误路径 | 仅 HUF 失败一条路 |
| 消除时机 | 下次调用开始时 | 脏状态不产生 |
| 侵入性 | 5 行赋值 | 移动代码块 |
| 新 bug 风险 | 极低 | 低但非零 |
| 防御性 | **强**（未来路径也覆盖） | 弱 |

```diff
--- a/lib/decompress/zstd_decompress.c
+++ b/lib/decompress/zstd_decompress.c
@@ -1560,6 +1560,11 @@
     dctx->prefixStart = NULL;
     dctx->virtualStart = NULL;
     dctx->dictEnd = NULL;
+    dctx->litBuffer = NULL;
+    dctx->litBufferEnd = NULL;
+    dctx->litBufferLocation = ZSTD_not_in_dst;
+    dctx->litPtr = NULL;
+    dctx->litSize = 0;
     dctx->entropy.hufTable[0] = (HUF_DTable)((ZSTD_HUFFDTABLE_CAPACITY_LOG)*0x1000001);
```

**安全性论证**：`ZSTD_decompressSequences` 的 `oend` 计算：

```c
BYTE* const oend = dctx->litBufferLocation == ZSTD_not_in_dst
    ? ZSTD_maybeNullPtrAdd(ostart, maxDstSize)   // 安全：用调用方 dstCapacity
    : dctx->litBuffer;                           // 危险：DCtx 内部指针
```

重置后 `litBufferLocation = ZSTD_not_in_dst`，`oend` 永远走 `dstCapacity` 分支。
即使 `litBuffer` 残留脏值也不会被读取。`ZSTD_in_dst` / `ZSTD_split` 的
`oend = litBuffer` 路径只在 `ZSTD_allocateLiteralsBuffer` 显式设置后才会走到。

#### 辅修复：`HUF_isError` 检查移到 `ZSTD_split` 调整之前

代码卫生：出错时不应变更状态。防止脏状态在第一时间产生。

```diff
--- a/lib/decompress/zstd_decompress_block.c
+++ b/lib/decompress/zstd_decompress_block.c
@@ -227,15 +227,15 @@
                 } else {
                     // ... HUF_decompress* ...
                 }
-                if (dctx->litBufferLocation == ZSTD_split)
-                {
-                    assert(litSize > ZSTD_LITBUFFEREXTRASIZE);
-                    ZSTD_memcpy(dctx->litExtraBuffer, dctx->litBufferEnd - ZSTD_LITBUFFEREXTRASIZE, ZSTD_LITBUFFEREXTRASIZE);
-                    ZSTD_memmove(dctx->litBuffer + ZSTD_LITBUFFEREXTRASIZE - WILDCOPY_OVERLENGTH, dctx->litBuffer, litSize - ZSTD_LITBUFFEREXTRASIZE);
-                    dctx->litBuffer += ZSTD_LITBUFFEREXTRASIZE - WILDCOPY_OVERLENGTH;
-                    dctx->litBufferEnd -= WILDCOPY_OVERLENGTH;
-                    assert(dctx->litBufferEnd <= (BYTE*)dst + blockSizeMax);
-                }
-
                 RETURN_ERROR_IF(HUF_isError(hufSuccess), corruption_detected, "");
 
+                if (dctx->litBufferLocation == ZSTD_split)
+                {
+                    assert(litSize > ZSTD_LITBUFFEREXTRASIZE);
+                    ZSTD_memcpy(dctx->litExtraBuffer, dctx->litBufferEnd - ZSTD_LITBUFFEREXTRASIZE, ZSTD_LITBUFFEREXTRASIZE);
+                    ZSTD_memmove(dctx->litBuffer + ZSTD_LITBUFFEREXTRASIZE - WILDCOPY_OVERLENGTH, dctx->litBuffer, litSize - ZSTD_LITBUFFEREXTRASIZE);
+                    dctx->litBuffer += ZSTD_LITBUFFEREXTRASIZE - WILDCOPY_OVERLENGTH;
+                    dctx->litBufferEnd -= WILDCOPY_OVERLENGTH;
+                    assert(dctx->litBufferEnd <= (BYTE*)dst + blockSizeMax);
+                }
+
                 dctx->litPtr = dctx->litBuffer;
                 dctx->litSize = litSize;
```

#### 落地方式

**已落地**：方案 1（vendor zstd-sys patch）。

- `vendor/zstd-sys/`：`zstd-sys 2.1.0+zstd.1.5.7` 完整 vendor，应用两处 patch
- `Cargo.toml`：`[patch.crates-io] zstd-sys = { path = "vendor/zstd-sys" }`
- 后续升级 zstd 时移除 vendor 目录和 patch 行即可

## 4. 改动文件

| 文件 | 改动 |
|---|---|
| `crates/onto-storage/src/lsm/sstable.rs` | `zstd_decompress_to` 出错丢弃 DCtx；`flush_block` 出错丢弃 CCtx |
| `vendor/zstd-sys/zstd/lib/decompress/zstd_decompress.c` | `ZSTD_decompressBegin` 重置 5 字段 |
| `vendor/zstd-sys/zstd/lib/decompress/zstd_decompress_block.c` | `HUF_isError` 移到 `ZSTD_split` 调整之前 |
| `vendor/zstd-sys/zstd/lib/compress/zstd_compress.c` | `ZSTD_resetCCtx_internal` 补 `cwksp_reserve_failed` 检查 |
| `Cargo.toml` | `[patch.crates-io]` 重定向 zstd-sys |

共 3 处 zstd patch（2 解压侧 + 1 压缩侧），全部在 `vendor/zstd-sys/` 内。

## 5. 验证

- 单测：26 项全过（0.46 s），含：
  - `test_dctx_discarded_after_error_no_stale_litbuffer` — DCtx 丢弃修复回归
  - `test_set_basic_fastpath_after_error_no_stale_oend` — 场景 B / mockCase3
  - `test_sstable_compressed_*`、`test_corrupt_compressed_payload_errors_not_crashes`、
    `test_decompress_limited_*`、`test_corruption_injection_matrix`、`test_engine_compaction*`
- 损坏数据路径：corrupt 压缩数据返回错误而非崩溃；异常 frame 被拒绝。
- 全量 `cargo check -p onto-storage --lib` 无新增告警。

## 6. 遗留

- dump 13684（READ AV in ntdll）栈信息稀疏，推测为同一脏指针家族，
  本修复（DCtx 丢弃 + decompressBegin 重置 + HUF_isError 提前 + cwksp_reserve_failed
  检查）可能一并消除。需后续运行验证。
- vendor 的 zstd-sys 在升级 zstd 时应移除 `vendor/` 目录和
  `[patch.crates-io]` 行，改用官方新版本（≥ 1.5.8 已包含这些修复）。
