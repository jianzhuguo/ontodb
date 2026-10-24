# Fix NULL deref when CCtx workspace reservation fails

## Summary

`ZSTD_resetCCtx_internal()` and `ZSTD_reset_matchState()` call `ZSTD_cwksp_reserve_*()` and store the results **without checking** for reservation failure before using the pointers.

When a workspace reservation fails (typical trigger: CCtx resize under memory pressure — `ZSTD_cwksp_create` with a new `neededSpace` after `workspaceTooSmall` / `workspaceWasteful`), those pointers stay `NULL`, but the function still:

1. continues reserving / `memset`ing further buffers
2. sets `zc->initialized = 1`
3. returns `0` (success)

The next compression then writes through the NULL pointers. The most visible crash site is `ZSTD_seqToCodes()` via `seqStore.llCode` / `mlCode` / `ofCode`:

```
llCodeTable[u] = (BYTE)ZSTD_LLcode(llv);   // NULL + u  →  access violation
```

Some reservation sites are immediately followed by `ZSTD_memset()` on the reserved pointer (`ldmState.hashTable`, `ldmState.bucketOffsets`, CDict `tagTable`), so they crash **inside** `ZSTD_resetCCtx_internal` itself, before any later check could run.

## Crash signature

- `EXCEPTION_ACCESS_VIOLATION` (Windows `0xc0000005`) / SIGSEGV
- often with the heap already unstable (`memory allocation of N bytes failed` nearby) when the reservation failed because the process was under allocation pressure
- stack top varies (`ZSTD_seqToCodes`, `ZSTD_compressBlock_*`, or the `ZSTD_memset` on `tagTable` / `bucketOffsets`)

Related: #3950 reports a segfault at the same crash site (`ZSTD_resetCCtx_internal` → seqStore code tables). That report attributes it to **uninitialized** workspace contents and a `calloc` workaround. This patch addresses the distinct case where the **reservation itself fails and is not checked**, leaving NULL pointers. Both issues can produce crashes at the same call site; this fix is independent of initialization.

## How to reproduce (sketch)

1. Use a custom allocator (`ZSTD_createCCtx_advanced`) whose `customAlloc` returns NULL after a budget is exhausted (or wrap `malloc` with a fail-Nth counter).
2. Create a CCtx and compress with parameters that force `ZSTD_resetCCtx_internal` to rebuild the workspace (vary block size / `pledgedSrcSize` / compression level between calls so `workspaceTooSmall` or `workspaceWasteful` is hit).
3. Without the patch: crash or silent corruption when `llCode` / `tagTable` is NULL.
4. With the patch: `ZSTD_compress*` returns `ERROR(memory_allocation)`.

## Fix

For every `ZSTD_cwksp_reserve_*` in the CCtx / match-state / CDict setup path:

- check `ZSTD_cwksp_reserve_failed(ws)` **before** the pointer is used (especially before `ZSTD_memset`)
- return `ERROR(memory_allocation)` on failure
- keep a sticky `ZSTD_cwksp_reserve_failed(ws)` check before `initialized = 1`, so any missed site still fails cleanly

**Why `reserve_failed` and not `ptr == NULL`:** `ZSTD_cwksp_reserve_internal()` returns `NULL` for a zero-byte reserve *without* setting `allocFailed` (e.g. `hashTable3` when `hashLog3 == 0`). A bare `ptr == NULL` check would treat those legitimate zero-size reserves as failures and reject every compress. The sticky flag is only set on a real reservation failure.

### Sites covered

| Function | Reservation |
|---|---|
| `ZSTD_initStaticCCtx` | `prevCBlock`, `nextCBlock`, `tmpWorkspace` (fixed-size objects; `ptr == NULL` is correct here) |
| `ZSTD_reset_matchState` | `tagTable` (both branches — `memset` in CDict path) |
| `ZSTD_resetCCtx_internal` | `sequencesStart`, `ldm hashTable`, `ldmSequences`, `extSeqBuf`, `litStart`, `inBuff`, `outBuff`, `ldm bucketOffsets`, `llCode`, `mlCode`, `ofCode` + sticky catch-all |
| `ZSTD_initCDict_internal` | `entropyWorkspace` |

`ZSTD_reset_matchState` already had sticky checks after the table reserves and at the end; the `tagTable` path needed an immediate check because of the `memset`.

## Testing

A dedicated regression test is included (see `tests/wksp_alloc_fail_test.c` in the attachment / follow-up commit). It plugs a custom `ZSTD_customMem` whose `customAlloc` can fail the Nth allocation or enforce a byte budget, then compresses while sweeping those failure points.

Coverage:

| Test | What it exercises |
|---|---|
| `test_control_success` | Unlimited budget must still compress successfully (guards against over-eager failure checks) |
| `test_sweep_alloc_index` | Fails allocation #1..#40 at several levels / pledged sizes — hits every reserve site in `ZSTD_resetCCtx_internal` |
| `test_sweep_byte_budget` | Allocation-pressure budgets — sticky `allocFailed` path across multiple reserves |
| `test_tag_table_row_match_finder` | `ZSTD_fast`/`dfast`/`greedy`/`lazy` strategies → `tagTable` + `memset` path |
| `test_opt_parser_workspace` | Level 19 opt-parser reserves (`litFreq`, …) in `ZSTD_reset_matchState` |
| `test_reuse_rebuild` | Reused CCtx + changed `pledgedSrcSize` forces workspace rebuild; fail mid-reset |
| `test_cdict_alloc_fail` | `ZSTD_createCDict_advanced` with failing allocator (`entropyWorkspace`, match state) |

Contract asserted: a failed reservation must produce a clean zstd error (`ZSTD_isError`, preferably `ZSTD_error_memory_allocation`) or a successful compress when the failed allocation was unused — **never** a crash.

Verified locally against zstd 1.5.7 + this patch (MSVC x64): **all cases pass**. Without the patch, the same test crashes (access violation) at the sites above.

```
cc -O1 -g -Ilib -DZSTD_STATIC_LINKING_ONLY \
   tests/wksp_alloc_fail_test.c lib/libzstd.a -o wksp_alloc_fail_test
./wksp_alloc_fail_test
# PASSED: all wksp allocation-failure cases returned cleanly
```

Happy to fold the test into `tests/` / the existing fuzzer harness in whatever form the maintainers prefer.

## Notes

- Behavior change: cases that previously crashed (or returned success with a half-initialized CCtx) now return `ERROR(memory_allocation)`. Callers that already handle zstd errors are unaffected.
- No API change.
- `ZSTD_cwksp_reserve_failed()` is sticky (`allocFailed` flag), so the catch-all also covers reserves inside `ZSTD_reset_matchState`.
