两条都拿到了。结论比预期更清楚，但也留下一个必须如实说明的矛盾点。
一、grow（b3002d50）= RawVec::grow_one，是正确的
b3002d56  mov  rsi, rcx                 ; rsi = &mut RawVec
b3002d59  mov  rdx, [rcx]               ; rdx = 当前 cap
b3002d5c  lea  rax, [rdx+rdx]           ; 2*cap
b3002d60  cmp  rax, 5
b3002d64  mov  edi, 4
b3002d69  cmovae rdi, rax               ; new_cap = max(2*cap, 4)
b3002d6d  mov  r8, [rcx+8]              ; r8 = 旧 ptr
b3002d71  mov  qword ptr [rsp+28h], 48h ; elem_size = 72
b3002d7a  mov  qword ptr [rsp+20h], 8   ; align = 8
b3002d8b  call b30009a0                 ; 分配/重分配（传 old_ptr+old_cap → realloc 语义，保留内容）
b3002d90  cmp  dword ptr [rsp+30h], 1
b3002d95  je   b3002daa                 ; 失败 → handle_alloc_error → ud2
b3002d97  mov  rax, [rsp+38h]           ; 新 ptr
b3002d9c  mov  [rsi+8], rax             ; ptr 更新 ✓
b3002da0  mov  [rsi], rdi               ; cap 更新 ✓
b3002da9  ret
b30009a0 收到 old_ptr + old_cap，是 realloc 语义（保留旧内容）；OOM 直接 ud2 中止。grow 不丢数据、不残留旧指针 —— 排除"重分配后指针没同步"这条假设。 同一函数后面 b3002dc0 / b3002e30 是同模板的 elem_size=4 / elem_size=2 版本（Vec<u32>/Vec<u16>），互为旁证。
二、0x1c34c9e8a70 不是缓冲区基址，是 slot 2
活的 Vec 头（caller 的 lea rdx,[rbp+420h] @ b2d0f2fb → self = 0x96dfe680，由 push 的 push rdi 槽位 0x96dfe078=0x96dfe680 双重证实）：
0x96dfe680:  cap=8   ptr=0x1c34c9e89e0   len=6    ← heap.data
0x96dfe690:  cap=8   ptr=0x1c2a3911eb0   len=7    ← sources
0x96dfe6b0:  current.key.cap = -1（None 标记）
真实基址 0x1c34c9e89e0，0x1c34c9e8a70 = base + 2×72 = slot 2。它在栈上出现 3 次（0x96dfd8a8、0x96dfd968、0x96dfe688），前两个在 rsp 之下（死栈槽），只有 0x96dfe688 是活的 Vec 头 —— 本线程没有第二个持有者。
⚠️ 必须撤回我上一条的"slot 5 未初始化"结论 —— 那是按 bug.md 的地址当基址造成的对齐错觉。按正确基址重排，6 个 live entry 全部自洽：
slot	addr	key.cap	key.ptr	key.len
0	89e0	0x51	0x1c2b56a6740	0x51
1	8a28	0x70	0x1c2a8c86570	0x70
2	8a70	0x57	0x1c255430690	0x57
3	8ab8	0x60	0x1c2b7099680	0x60
4	8b00	0x85	0x1c2a386d7f0	0x85
5	8b48	0x5e	0x1c260f60fa0	0x5e
6	8b90	—	陈旧残留（len 从没到过 7）	 
7	8bd8	—	从未写入（cap=8 的空位）	 
三、真正的崩溃原因：key.ptr 是垃圾，而 safe_vec_bytes 拦不住
sift_up 保存的 item（rsp=0x96dfdff0）：
[rsp+30h] key.cap   = 0x5e                     ✓
[rsp+28h] key.ptr   = 0xb78d20964436c2b5       ✗ 垃圾
[rsp+20h] key.len   = 0x5e                     ✓
[rsp+40h] value     = cap 0 / ptr 1            ✓
[rsp+38h] seq_no    = 0x1152b226               ✓
r15                 = 4  (= source_idx)        ✓
三条独立证据咬合：
1. 寄存器：rsi=0x1c34c9e89e0（数组基址）、r14=5（idx）、r12=0x5e（item.len）、r15=4（source_idx）、r8=0x57=min(0x57,0x5e)、rbp=-7=0x57-0x5e、r13=0x12=2×9（parent=slot 2）、rcx=0x1c255430690=slot 2 的 key.ptr。
2. memcmp 内部算术：rcx=s1=0x1c255430690，rdx=0xb78d1ed3eef3bc25=s2-s1，rcx+rdx=0xb78d20964436c2b5 = AV 地址 = s2 = item.key.ptr。
3. 栈上第二份寄存器快照（0x96dfd940起）逐字等于 .ecxr，0x96dfd950 = 同一个垃圾值。
safe_vec_bytes（crates/onto-storage/src/lsm/streaming_merge.rs:33）只挡 capacity()==0：
fn safe_vec_bytes(v: &Vec<u8>) -> &[u8] {
    if v.capacity() == 0 { &[] } else { v.as_slice() }
}
slot 5 的 cap = 0x5e ≠ 0 → 走 as_slice() → memcmp(合法指针, 垃圾指针, 0x57) → AV。这个守卫只覆盖了"cap==0 的空分配"，对"非零 cap + 垃圾/悬垂 ptr"完全不设防，而注释里写的正是为了防止 memcmp 吃到 dangling pointer。
四、必须如实说明的残留矛盾
array[5].key.ptr（0x1c34c9e8b50）字节级确认是 a0 0f f6 60 c2 01 00 00 = 0x1c260f60fa0（合法）。而且这条 entry 在另外两处副本里也都合法：
- iter.next() 的 out 槽 [rbp-60h] = 0x96dfe0d0 → key.ptr=0x1c260f60fa0
- push 的 staging [rbp+30h] = 0x96dfe160 → key.ptr=0x1c260f60fa0
8 个字段里 7 个和 slot 5 完全吻合，唯独 key.ptr 在 ptr::read 出来的副本里是垃圾。 x86 TSO 不允许本线程的 load 看不到自己先前对同地址的 store，所以剩下三种可能：
- (a) 那条 movups [rcx+rax*8], xmm0（写 key.cap|key.ptr，是 5 条 store 里的最后一条）落到了别处或丢失；
- (b) 有第二个 writer 在 ptr::read 到 AV 之间的约 20 条指令窗口里改写了 0x1c34c9e8b50（本线程栈上没有第二个持有者，但别的线程可能有）；
- (c) 我算的 index 有偏差（但 r14=5、r13=2×9、min_len=0x57 三者互相印证，不像）。
顺带：self.current 的 None 被编码成 key.cap = -1，take() 写这个标记 —— 而 safe_vec_bytes 对 cap = u64::MAX 同样放行（!= 0），会拿残留的旧 key.ptr/len 去 memcmp。这是同一类漏洞的第二个人口。
建议下一步
1. 先堵安全阀（治标但立刻止血）：safe_vec_bytes 改成同时校验 cap == 0 || cap < len、以及指针非规范（ptr >> 48 不是 0 或 0xffff）时返回空切片；None 状态也别用 cap = -1 这种会被误判成合法 cap 的编码。
2. *查 (b)*：我可以把 dump 里所有线程的栈扫一遍 0x1c34c9e89e0，看有没有别的 StreamingMergeIterator 共享这块缓冲区 —— compaction 如果并发跑多个 merge，而它们的 Vec 因为 free/realloc 撞到同一块内存，就会出现这种"单字段瞬时脏"。
3. *查 (a)*：在 Vec::push 的最后一条 store 后加一条 debug 断言（读回 key.ptr 比对），跑压测复现。
要我先做 2（扫全 dump 的共享者），还是直接按 1 把守卫补上？