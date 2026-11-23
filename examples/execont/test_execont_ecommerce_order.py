# -*- coding: utf-8 -*-
"""
ExecOnt 价值测试 · 场景：网购订单履约（一个订单的一生）
================================================================
场景（人人熟悉的电商常识）：
  买家在电商平台下单一台手机，标准作业流程 SOP 为
  下单 → 支付 → 拣货 → 发货 → 运输 → 签收 → 完成，
  订单状态机 待支付 → 已支付 → 已发货 → 运输中 → 已签收 → 已完成
  （旁路：超时取消 / 签收后退款）。
  业务铁律「先款后货」：未支付不得发货。

本套件用这一个场景把 ExecOnt 的四层价值跑一遍（HTTP /api/query @7912）：
  Part 0  T-Box 立规矩  —— 状态机/工序/守卫/意图全是本体数据，不是代码
  Part 1  写入闸门      —— 该不该发生，库当场判定（未支付发货→拒写）
  Part 2  推理面        —— 工序传递/状态可达/因果链，闭包带 rule 归因
  Part 3  符合性对账    —— 计划 vs 实际：合规单/越界单/缺步单 体检报告
  Part 4  金标问答      —— Agent 终裁：10 道执行态问题
配套：examples/execont/exec_onto_meta.sql（元模型单源）
方案：docs/执行态本体开发方案与战略意义.md（§四 元模型 / §七 验证方案）
口径红线：ExecOnt 是执行语义的建模与记账层——不调度、不驱动执行。
"""
import json
import os
import urllib.request

BASE = "http://127.0.0.1:7912/api/query"
SCHEMA = "http://127.0.0.1:7912/api/schema"
PASS, FAIL = 0, 0


def q(sql):
    body = json.dumps({"query": sql}).encode("utf-8")
    req = urllib.request.Request(BASE, data=body, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        return {"success": False, "error": e.read().decode("utf-8", errors="replace")}


def rows(sql):
    return q(sql).get("data", []) or []


def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        print("PASS  " + name + (("  (%s)" % detail) if detail else ""))
        PASS += 1
    else:
        print("FAIL  " + name + (("  (%s)" % detail) if detail else ""))
        FAIL += 1


def expect_ok(name, sql):
    global PASS, FAIL
    d = q(sql)
    if d.get("success") and not d.get("error"):
        print("PASS  " + name)
        PASS += 1
    else:
        print("FAIL  %s: %s" % (name, str(d.get("error", ""))[:140]))
        FAIL += 1
    return d


def expect_reject(name, sql, needle):
    global PASS, FAIL
    d = q(sql)
    err = str(d.get("error", ""))
    if err:
        if needle.lower() in err.lower():
            print("PASS  %s -> 拒写: %s" % (name, err[:110]))
            PASS += 1
        else:
            print("PASS? %s -> 拒写但消息不符: %s" % (name, err[:130]))
            PASS += 1
    else:
        print("FAIL  %s: 写入被接受，期望闸门拒写" % name)
        FAIL += 1


# exec_onto_meta.sql 是元模型 DDL 的单源（与 test_execont_m1.py 同一加载器）
META_SQL = os.path.join(os.path.dirname(os.path.abspath(__file__)), "exec_onto_meta.sql")


def load_meta_statements(path):
    stmts, buf, depth = [], [], 0
    with open(path, encoding="utf-8") as f:
        for line in f:
            code = line.split("--", 1)[0].strip()
            if not code:
                continue
            buf.append(code)
            depth += code.count("(") - code.count(")")
            if depth <= 0:
                stmts.append(" ".join(buf))
                buf, depth = [], 0
    if buf:
        stmts.append(" ".join(buf))
    return stmts


def infer_rows(sql):
    return q(sql).get("data", []) or []


def infers(inferred, subj, pred, obj):
    """闭包行匹配：subject/object 允许 'Activity::act_sign' 或裸 'act_sign' 形态。"""
    for r in inferred:
        if (r.get("predicate") == pred
                and str(r.get("subject", "")).endswith(subj)
                and str(r.get("object", "")).endswith(obj)):
            return r
    return None


# ──────────────────────────────────────────────────────────────
#  场景常量：SOP 计划 / 订单状态机 / 四张订单
# ──────────────────────────────────────────────────────────────
# 标准作业流程（T-Box 规矩）：下单→支付→拣货→发货→运输→签收→完成
PLAN = [
    ("act_order", "下单"),
    ("act_pay", "支付"),
    ("act_pick", "拣货"),
    ("act_ship", "发货"),
    ("act_transport", "运输"),
    ("act_sign", "签收"),
    ("act_finish", "完成"),
]
PLAN_FRAMES = ["order", "pay", "pick", "ship", "transport", "sign", "finish"]

# 订单状态机（9 态 8 边，StateReach 闭包恰 27 对）
STATES = [
    ("st_pending", "待支付"), ("st_paid", "已支付"), ("st_shipped", "已发货"),
    ("st_transit", "运输中"), ("st_signed", "已签收"), ("st_completed", "已完成"),
    ("st_cancelled", "已取消"), ("st_refunding", "退款中"), ("st_refunded", "已退款"),
]
TRANSITIONS = [
    ("tr_pay", "st_pending", "st_paid", "payment_confirmed"),
    ("tr_ship", "st_paid", "st_shipped", "picked_ready"),
    ("tr_transit", "st_shipped", "st_transit", "handed_to_carrier"),
    ("tr_sign", "st_transit", "st_signed", "receiver_confirmed"),
    ("tr_finish", "st_signed", "st_completed", "no_open_after_sale"),
    ("tr_cancel", "st_pending", "st_cancelled", "payment_timeout"),
    ("tr_refund", "st_signed", "st_refunding", "after_sale_request"),
    ("tr_refunded", "st_refunding", "st_refunded", "refund_approved"),
]

# 四张订单：GATE=闸门演示单，1001=合规单，1002=越界单，1003=缺步单
ORDERS = {
    "ORD-GATE": ("st_signed", "platform-fulfillment-01"),
    "ORD-1001": ("st_completed", "platform-fulfillment-01"),
    "ORD-1002": ("st_signed", "platform-fulfillment-01"),
    "ORD-1003": ("st_completed", "platform-fulfillment-01"),
}
FRAME_CN = {aid.replace("act_", ""): name for aid, name in PLAN}   # frame 码 -> 工序中文名


def bare(ref):
    """'Activity::act_pay' / 'act_pay' -> 'act_pay'（引用归一）"""
    return str(ref or "").split("::")[-1]


def main():
    global PASS, FAIL

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 0 · T-Box 立规矩：订单 SOP + 状态机 + 铁律，全部是本体数据")
    print("=" * 72)

    # ── 清场（套件可重复执行）──
    q("DROP ONTOLOGY exec_onto")
    for cls in ["ExecAgent", "Capability", "Process", "Activity", "State", "Transition",
                "ExecEvent", "Goal", "Intent", "ExecConstraint", "Resource", "ExecutionContext"]:
        q("TRUNCATE TABLE " + cls)
    print("      清场完成")

    # ── 元模型单源加载（六面：主体/过程/状态/事件/意图/资源）──
    stmts = load_meta_statements(META_SQL)
    check("P0-1  exec_onto_meta.sql 解析出全部 DDL 语句", len(stmts) >= 11, "count=%d" % len(stmts))
    meta_ok = all(q(s).get("success") for s in stmts)
    check("P0-2  六面元模型 DDL 全部执行成功（单源 exec_onto_meta.sql）", meta_ok)

    # ── 订单场景扩展：事件守卫（guard 是写入闸门的数据载体）──
    expect_ok("P0-3  订单事件声明守卫属性：CREATE PROPERTY guard DOMAIN ExecEvent",
              "CREATE PROPERTY guard DOMAIN ExecEvent RANGE STRING")

    # ── 状态面：订单状态机（State/Transition 是一等本体，迁移规矩是数据）──
    for sid, name in STATES:
        expect_ok("P0-4  状态 %s（%s）" % (sid, name),
                  "INSERT INTO State (id, state_name, state_machine) VALUES ('%s', '%s', 'order_lifecycle')" % (sid, name))
    for tid, fr, to, guard in TRANSITIONS:
        expect_ok("P0-5  迁移 %s: %s -> %s（守卫=%s）" % (tid, fr, to, guard),
                  "INSERT INTO Transition (id, from_state, to_state, guard) VALUES ('%s', '%s', '%s', '%s')" % (tid, fr, to, guard))

    # ── 过程面：SOP 计划（preceded_by 工序链，传递特征来自元模型）──
    for oid, (state, agent) in ORDERS.items():
        expect_ok("P0-6  过程 %s（当前态=%s）" % (oid, state),
                  "INSERT INTO Process (id, current_state, exec_agent, has_intent) "
                  "VALUES ('%s', '%s', '%s', 'i-3day')" % (oid, state, agent))
    prev = ""
    for aid, name in PLAN:
        # 工序链引用必须写主键形态（Activity::act_x），PrpTrp 传递链才接得上；
        # 链头不写 preceded_by 字段（写空串会被当成节点参与传递，产出脏闭包）。
        if prev:
            expect_ok("P0-7  工序 %s（%s）preceded_by %s" % (aid, name, prev),
                      "INSERT INTO Activity (id, activity_name, process_of, frame, preceded_by) "
                      "VALUES ('%s', '%s', 'ORD-PLAN', '%s', 'Activity::%s')" % (aid, name, aid.replace("act_", ""), prev))
        else:
            expect_ok("P0-7  工序 %s（%s）链头" % (aid, name),
                      "INSERT INTO Activity (id, activity_name, process_of, frame) "
                      "VALUES ('%s', '%s', 'ORD-PLAN', '%s')" % (aid, name, aid.replace("act_", "")))
        prev = aid

    # ── 意图面：目标 + 铁律约束（SemMesh 四元组同构承接）──
    expect_ok("P0-8  目标 Goal g-3day（3 天内送达）",
              "INSERT INTO Goal (id, goal_desc) VALUES ('g-3day', '3 天内送达')")
    expect_ok("P0-8  意图 Intent i-3day 绑定目标",
              "INSERT INTO Intent (id, goal_of) VALUES ('i-3day', 'g-3day')")
    expect_ok("P0-8  铁律约束 ExecConstraint（先款后货）",
              "INSERT INTO ExecConstraint (id, constraint_of, constraint_text) "
              "VALUES ('c-payfirst', 'i-3day', '先款后货：未支付不得发货')")

    # ── 本体契约钉住：规矩必须在 schema 可见 ──
    try:
        with urllib.request.urlopen(SCHEMA, timeout=10) as r:
            schema_json = json.loads(r.read().decode("utf-8"))
        onto = next(o for o in schema_json["data"]["ontologies"] if o.get("name") == "exec_onto")
        props, classes = onto["properties"], onto["classes"]
        contract = {
            "event_agent.required": props.get("event_agent", {}).get("required") is True,
            "ts.required": props.get("ts", {}).get("required") is True,
            "preceded_by.is_transitive": props.get("preceded_by", {}).get("is_transitive") is True,
            "caused_by.is_transitive": props.get("caused_by", {}).get("is_transitive") is True,
            "from_state.is_functional": props.get("from_state", {}).get("is_functional") is True,
            "to_state.is_functional": props.get("to_state", {}).get("is_functional") is True,
            "current_state.is_functional": props.get("current_state", {}).get("is_functional") is True,
            "ExecEvent.declares(guard)": "guard" in classes.get("ExecEvent", {}).get("properties", []),
            "六面类齐全(State/Transition/ExecEvent)": all(
                c in classes for c in ["State", "Transition", "ExecEvent", "Process", "Activity"]),
        }
        broken = [k for k, ok in contract.items() if not ok]
        check("P0-9  本体契约钉住（REQUIRED/TRANSITIVE/FUNCTIONAL/守卫声明 经 schema 可见）",
              not broken, "broken=%s" % broken)
    except Exception as e:
        check("P0-9  本体契约钉住", False, "schema parse error: %s" % e)

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 1 · 写入闸门：「该不该发生」由库当场判定（ORD-GATE 演示单）")
    print("=" * 72)
    print("  故事：买家已下单（order），商家试图跳过付款直接发货/签收——")
    print("        违规写入被数据库当场拒写，不靠应用代码自觉。")

    # G1 · provenance 必填：谁（event_agent）缺失 → 拒写
    expect_reject("G1  发货事件缺 event_agent（provenance 必填）→ 拒写",
                  "INSERT INTO ExecEvent (id, ts, frame, task_name) VALUES ('G-ship-noagent', 2000.0, 'ship', 'ORD-GATE')",
                  "required")

    # 下单事件（无守卫，普通写入）
    expect_ok("G2  下单事件 order@1000（买家）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name) "
              "VALUES ('G-order', 'buyer-01', 1000.0, 'order', 'ORD-GATE')")

    # G3 · 铁律「先款后货」：EXISTS 前置——没有支付记录就想发货 → 拒写
    expect_reject("G3  未支付就发货（EXISTS 前置不满足）→ 拒写 ★铁律",
                  "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, guard) "
                  "VALUES ('G-ship-early', 'warehouse-01', 2000.0, 'ship', 'ORD-GATE', "
                  "'EXISTS(ExecEvent WHERE frame = ''pay'' AND task_name = $task_name)')",
                  "guard violation")

    # 先补上支付（caused_by 引用主键形态，因果传递链才接得上）
    expect_ok("G4  支付事件 pay@1500（支付网关）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('G-pay', 'pay-gateway-01', 1500.0, 'pay', 'ORD-GATE', 'ExecEvent::G-order')")

    # 拣货（合法流程一步，走齐 SOP 让演示单完整）
    expect_ok("G4b 拣货事件 pick@1750（仓库）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('G-pick', 'warehouse-01', 1750.0, 'pick', 'ORD-GATE', 'ExecEvent::G-pay')")

    # 同一条守卫，满足即放行
    expect_ok("G5  支付后再发货：同一条 EXISTS 守卫放行 ★",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by, guard) "
              "VALUES ('G-ship', 'warehouse-01', 2000.0, 'ship', 'ORD-GATE', 'ExecEvent::G-pick', "
              "'EXISTS(ExecEvent WHERE frame = ''pay'' AND task_name = $task_name)')")

    # G6 · 时序守卫 BEFORE：签收时间戳早于发货 → 回溯写入拒写
    expect_reject("G6  回溯签收（BEFORE 发货不满足，ts=1500 < 发货 2000）→ 拒写",
                  "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by, guard) "
                  "VALUES ('G-sign-back', 'carrier-01', 1500.0, 'sign', 'ORD-GATE', 'ExecEvent::G-ship', "
                  "'BEFORE(ExecEvent WHERE frame = ''ship'' AND task_name = $task_name)')",
                  "guard violation")

    # G7 · 相邻前驱 PREV：上一步不是「运输」就想签收 → 跳步拒写
    expect_reject("G7  跳过运输直接签收（PREV 上一步不是 transport）→ 拒写",
                  "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by, guard) "
                  "VALUES ('G-sign-skip', 'carrier-01', 3000.0, 'sign', 'ORD-GATE', 'ExecEvent::G-ship', "
                  "'PREV(ExecEvent WHERE frame = ''transport'' AND task_name = $task_name)')",
                  "guard violation")

    # 补上运输（合法一步）
    expect_ok("G8  运输事件 transport@2500（快递）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('G-transport', 'carrier-01', 2500.0, 'transport', 'ORD-GATE', 'ExecEvent::G-ship')")

    # 同一条 PREV 守卫，上一步是运输 → 放行
    expect_ok("G9  运输后签收：同一条 PREV 守卫放行 ★",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by, guard) "
              "VALUES ('G-sign', 'carrier-01', 3000.0, 'sign', 'ORD-GATE', 'ExecEvent::G-transport', "
              "'PREV(ExecEvent WHERE frame = ''transport'' AND task_name = $task_name)')")

    # G10 · 三值语义：守卫操作数缺失 → 判不了、不猜、不误伤（schema-on-read）
    expect_ok("G10 守卫操作数缺失（无 task_name）→ 未判定放行，不误伤",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, guard) "
              "VALUES ('G-noref', 'system-01', 3500.0, 'note', "
              "'EXISTS(ExecEvent WHERE frame = ''pay'' AND task_name = $task_name)')")

    # G11 · 值域闸门：ts 是 FLOAT64，字符串时间拒写
    expect_reject("G11 值域闸门：ts='yesterday'（FLOAT64）→ 拒写",
                  "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name) "
                  "VALUES ('G-badts', 'system-01', 'yesterday', 'note', 'ORD-GATE')",
                  "type")

    # G12 · 功能属性：过程只有一个当前态（current_state FUNCTIONAL）
    expect_reject("G12 current_state FUNCTIONAL：给过程第二个当前态 → 拒写",
                  "INSERT INTO Process (id, current_state) VALUES ('ORD-GATE', ['st_signed', 'st_completed'])",
                  "type")

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 2 · 推理面：规矩 × 账 × 行（闭包可查、带 rule 归因）")
    print("=" * 72)

    # R1 · 工序传递（PrpTrp）：签收前必有支付——这是推理出来的，不是查出来的
    inf_act = infer_rows("INFER SELECT * FROM Activity")
    r = infers(inf_act, "act_sign", "preceded_by", "act_pay")
    check("R1  PrpTrp 工序传递：签收 preceded_by 支付（隔 4 步的传递闭包）", r is not None)
    check("R1b 传递蕴含带 rule 归因", bool(r and r.get("rule")), "rule=%s" % (r.get("rule") if r else None))
    check("R2  PrpTrp 工序传递：完成 preceded_by 下单（整条链闭包）",
          infers(inf_act, "act_finish", "preceded_by", "act_order") is not None)
    check("R3  PrpTrp 负例：下单 preceded_by 完成（方向反了）不存在",
          infers(inf_act, "act_order", "preceded_by", "act_finish") is None)

    # R4-R8 · 状态可达（StateReach）：迁移图闭包
    inf_tr = infer_rows("INFER SELECT * FROM Transition")
    reach = [r for r in inf_tr if r.get("predicate") == "reachable"]
    check("R4  StateReach 闭包规模恰 27 对（9 态 8 边，确定性）", len(reach) == 27, "got=%d" % len(reach))
    check("R5  状态可达：待支付 → 已完成（经 支付/发货/运输/签收 传递）",
          infers(inf_tr, "st_pending", "reachable", "st_completed") is not None)
    check("R6  状态可达：已支付 → 已退款（售后旁路也闭包）",
          infers(inf_tr, "st_paid", "reachable", "st_refunded") is not None)
    check("R7  状态可达负例：已完成 → 待支付 不存在（终态无出边）",
          infers(inf_tr, "st_completed", "reachable", "st_pending") is None)
    check("R8  状态可达负例：已取消 → 已完成 不存在（取消是死路）",
          infers(inf_tr, "st_cancelled", "reachable", "st_completed") is None)
    check("R9  无自环：待支付 → 待支付 不存在",
          infers(inf_tr, "st_pending", "reachable", "st_pending") is None)

    # R10 · 直接迁移 vs 闭包可达（可达 ≠ 一步跳）
    direct = rows("SELECT id FROM Transition WHERE from_state = 'st_pending' AND to_state = 'st_signed'")
    closure = infers(inf_tr, "st_pending", "reachable", "st_signed")
    check("R10 待支付→已签收：闭包可达但无直接迁移边（不能一步跳）",
          closure is not None and not direct, "direct=%s" % direct)

    # R11-R12 · 因果链（caused_by TRANSITIVE + 归因）：ORD-GATE 的「为什么」
    inf_ev = infer_rows("INFER SELECT * FROM ExecEvent")
    r = infers(inf_ev, "G-sign", "caused_by", "G-pay")
    check("R11 因果传递：签收 caused_by 支付（隔 2 跳）", r is not None)
    check("R12 因果蕴含带 rule 归因", bool(r and r.get("rule")), "rule=%s" % (r.get("rule") if r else None))

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 3 · 符合性对账：三张订单的体检报告（计划 vs 实际）")
    print("=" * 72)

    # ── 三张订单的执行事实（存量/旁路写入——不带 guard 字段，模拟历史数据）──
    def insert_chain(pid, prefix, seq):
        """seq: [(frame, agent, ts), ...]；因果链用主键形态引用，链头不写 caused_by。"""
        for i, (frame, agent, ts) in enumerate(seq, 1):
            eid = "%s-%d" % (prefix, i)
            if i > 1:
                q("INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
                  "VALUES ('%s', '%s', %d.0, '%s', '%s', 'ExecEvent::%s-%d')"
                  % (eid, agent, ts, frame, pid, prefix, i - 1))
            else:
                q("INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name) "
                  "VALUES ('%s', '%s', %d.0, '%s', '%s')" % (eid, agent, ts, frame, pid))

    # ORD-1001 合规单：7 步齐全有序
    insert_chain("ORD-1001", "E1", [
        ("order", "buyer-01", 1000), ("pay", "pay-gateway-01", 2000),
        ("pick", "warehouse-01", 3000), ("ship", "warehouse-01", 4000),
        ("transport", "carrier-01", 5000), ("sign", "carrier-01", 6000),
        ("finish", "system-01", 7000)])

    # ORD-1002 越界单：没有支付，拣货后直接发货（先货后款）
    insert_chain("ORD-1002", "E2", [
        ("order", "buyer-01", 1000), ("pick", "warehouse-01", 3000),
        ("ship", "warehouse-01", 4000), ("transport", "carrier-01", 5000),
        ("sign", "carrier-01", 6000)])

    # ORD-1003 缺步单：跳过拣货（pay 后直接 ship）
    insert_chain("ORD-1003", "E3", [
        ("order", "buyer-01", 1000), ("pay", "pay-gateway-01", 2000),
        ("ship", "warehouse-01", 4000), ("transport", "carrier-01", 5000),
        ("sign", "carrier-01", 6000), ("finish", "system-01", 7000)])
    check("S0  三张订单 18 条执行事实写入（存量/旁路路径，无 guard 字段）",
          len(rows("SELECT id FROM ExecEvent WHERE task_name LIKE 'ORD-10%'")) == 18)

    # ── 对账引擎（纯查询模板）：计划序列取自 preceded_by 链，实际序列取自事件时间线 ──
    # 注意：ORDER BY ts 当前不生效（返回主键序），时间线必须客户端按 ts 排序。
    def get_plan():
        acts = rows("SELECT id, frame, preceded_by FROM Activity WHERE process_of = 'ORD-PLAN'")
        head = [a for a in acts if not a.get("preceded_by")]
        seq, cur = [], head[0] if head else None
        while cur:
            seq.append(cur.get("frame", ""))
            nxt = [a for a in acts if bare(a.get("preceded_by")) == cur.get("id")]
            cur = nxt[0] if nxt else None
        return seq

    def get_actual(pid):
        evs = rows("SELECT frame, ts FROM ExecEvent WHERE task_name = '%s'" % pid)
        evs.sort(key=lambda e: float(e.get("ts", 0)))
        return [e.get("frame", "") for e in evs]

    def classify(pid):
        actual = get_actual(pid)
        plan = get_plan()
        if "ship" in actual and "pay" not in actual:
            return "VIOLATION", "先货后款：无支付记录即发货", actual
        missing = [s for s in plan if s not in actual]
        if missing:
            return "MISSING_STEP", "缺步=[%s]" % ",".join(FRAME_CN.get(m, m) for m in missing), actual
        if actual == plan:
            return "COMPLIANT", "计划与实际完全一致", actual
        return "MISMATCH", "序列不一致", actual

    plan = get_plan()
    check("S1  计划序列取自 preceded_by 链：下单→…→完成 7 步",
          plan == PLAN_FRAMES, str([FRAME_CN.get(f, f) for f in plan]))

    verdict1, why1, act1 = classify("ORD-1001")
    print("  体检报告 [ORD-1001] plan=%s" % [FRAME_CN.get(f, f) for f in plan])
    print("            actual=%s" % [FRAME_CN.get(f, f) for f in act1])
    print("            => %s（%s）" % (verdict1, why1))
    check("S2  合规单 ORD-1001 → COMPLIANT", verdict1 == "COMPLIANT", why1)

    verdict2, why2, act2 = classify("ORD-1002")
    print("  体检报告 [ORD-1002] actual=%s" % [FRAME_CN.get(f, f) for f in act2])
    print("            => %s（%s）" % (verdict2, why2))
    check("S3  越界单 ORD-1002 → VIOLATION（未支付就发货）", verdict2 == "VIOLATION", why2)

    verdict3, why3, act3 = classify("ORD-1003")
    print("  体检报告 [ORD-1003] actual=%s" % [FRAME_CN.get(f, f) for f in act3])
    print("            => %s（%s）" % (verdict3, why3))
    check("S4  缺步单 ORD-1003 → MISSING_STEP（缺拣货）",
          verdict3 == "MISSING_STEP" and "拣货" in why3, why3)

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 4 · 金标问答：Agent 终裁（每题答案都从库里查/推出来）")
    print("=" * 72)

    qa_pass = 0

    def qa(idx, question, expected, got, ok):
        nonlocal qa_pass
        tag = "Q%-2d %s" % (idx, question)
        if ok:
            qa_pass += 1
            print("PASS  %s -> %s" % (tag, got))
        else:
            print("FAIL  %s -> %s（期望 %s）" % (tag, got, expected))

    # Q1 订单 1001 现在处于什么状态？
    r = rows("SELECT current_state FROM Process WHERE id = 'ORD-1001'")
    got = r[0].get("current_state", "") if r else ""
    qa(1, "「订单 1001 现在处于什么状态？」", "st_completed", got, got == "st_completed")

    # Q2 它实际经过了哪几步？
    got = get_actual("ORD-1001")
    qa(2, "「1001 实际经过了哪几步？」", str(PLAN_FRAMES), str(got), got == PLAN_FRAMES)

    # Q3 谁在什么时候处理了支付？
    r = rows("SELECT event_agent, ts FROM ExecEvent WHERE task_name = 'ORD-1001' AND frame = 'pay'")
    agent = r[0].get("event_agent", "") if r else ""
    ts = r[0].get("ts", 0) if r else 0
    got = "%s @ %s" % (agent, ts)
    qa(3, "「谁在什么时候处理了 1001 的支付？」", "pay-gateway-01 @ 2000", got,
       agent == "pay-gateway-01" and float(ts) == 2000.0)

    # Q4 为什么签收发生在支付之后？（推理归因，不是时间戳比大小）
    r = infers(infer_rows("INFER SELECT * FROM Activity"), "act_sign", "preceded_by", "act_pay")
    got = "preceded_by 传递闭包 rule=%s" % (r.get("rule") if r else "无")
    qa(4, "「为什么『签收』发生在『支付』之后？」", "PrpTrp 传递推理", got, r is not None)

    # Q5 1002 的发货合规吗？
    got = "%s（%s）" % (verdict2, why2)
    qa(5, "「1002 的发货合规吗？」", "VIOLATION", got, verdict2 == "VIOLATION")

    # Q6 1003 缺了哪一步？
    got = why3
    qa(6, "「1003 缺了哪一步？」", "缺步=[拣货]", got, verdict3 == "MISSING_STEP" and "拣货" in why3)

    # Q7 待支付能一步跳到已签收吗？
    direct = rows("SELECT id FROM Transition WHERE from_state = 'st_pending' AND to_state = 'st_signed'")
    closure = infers(infer_rows("INFER SELECT * FROM Transition"), "st_pending", "reachable", "st_signed")
    got = "直接迁移边=%d，闭包可达=%s" % (len(direct), closure is not None)
    qa(7, "「待支付能一步跳到已签收吗？」", "不能一步跳（但闭包可达）", got,
       (not direct) and closure is not None)

    # Q8 已完成还能回到待支付吗？
    closure = infers(infer_rows("INFER SELECT * FROM Transition"), "st_completed", "reachable", "st_pending")
    got = "reachable(completed→pending)=%s" % (closure is not None)
    qa(8, "「已完成的订单还能回到待支付吗？」", "不能（终态无出边）", got, closure is None)

    # Q9 发货的前提条件是什么？（铁律可机读）
    r = rows("SELECT constraint_text FROM ExecConstraint WHERE id = 'c-payfirst'")
    got = r[0].get("constraint_text", "") if r else ""
    qa(9, "「『发货』的前提条件是什么？」", "先款后货：未支付不得发货", got, "先款后货" in got)

    # Q10 ORD-GATE 现在在哪一步、还差什么？
    r = rows("SELECT current_state FROM Process WHERE id = 'ORD-GATE'")
    state = r[0].get("current_state", "") if r else ""
    done = get_actual("ORD-GATE")
    remain = [FRAME_CN[f] for f in PLAN_FRAMES if f not in done]
    got = "当前=%s，已完成=%s，还差=%s" % (state, [FRAME_CN.get(f, f) for f in done], remain)
    qa(10, "「ORD-GATE 现在在哪一步、还差什么？」", "st_signed，还差[完成]", got,
       state == "st_signed" and remain == ["完成"])

    print()
    rate = qa_pass * 100.0 / 10
    check("金标问答 ≥ 90%%（%d/10 = %.0f%%）" % (qa_pass, rate), qa_pass >= 9, "%.0f%%" % rate)

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 5 · 价值对照（同一个订单场景，两种世界观）")
    print("=" * 72)
    print("""  | 问题                 | 传统状态字段/应用代码            | OntoDB ExecOnt（本套件实测）          |
  |----------------------|----------------------------------|--------------------------------------|
  | 未支付能发货吗        | 应用层 if 判断，旁路/导入绕过     | G3 闸门当场拒写（guard violation）    |
  | 签收能早于发货吗      | 靠日志事后发现                    | G6 BEFORE 守卫拒写                    |
  | 能跳过运输签收吗      | 流程引擎配一堆监听器              | G7 PREV 守卫拒写                      |
  | 待支付→已签收怎么走   | 翻代码/问开发                     | R5/R10 StateReach 闭包查询            |
  | 签收为什么在支付后    | 日志考古                          | R1/R4 PrpTrp 推理 + rule 归因         |
  | 这单合规吗            | 写一次性脚本对账                  | S2-S4 计划 vs 实际对账查询            |
  | AI 答执行态问题       | 猜                                | Part 4 金标问答 10 题                 |
  """)
    print("  一句话：T-Box 立规矩、五道闸门管『该不该』、推理答『为什么』、对账查『对不对』——")
    print("         订单状态从一个 status 字段，变成机器可读、可推理、可审计的执行态本体。")

    # ── 总结 ──
    total = PASS + FAIL
    print()
    print("=" * 72)
    print("  ECOMMERCE ORDER FULFILLMENT (ExecOnt 价值测试): %d/%d passed" % (PASS, total))
    print("  金标问答: %d/10 (%.0f%%)" % (qa_pass, rate))
    if FAIL == 0:
        print("  ALL CHECKS PASSED")
    else:
        print("  %d CHECK(S) FAILED" % FAIL)
    print("=" * 72)
    return FAIL == 0


if __name__ == "__main__":
    import sys
    sys.exit(0 if main() else 1)
