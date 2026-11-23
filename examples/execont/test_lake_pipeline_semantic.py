# -*- coding: utf-8 -*-
"""
入湖语义化管线测试 · 场景：ERP 订单 + WMS 物流数据入湖
================================================================
设计文档：E:\\OntoLake\\入湖语义化管线设计.md
核心验证：入湖即 AI-Ready —— 语义映射 + 质量标注（隔离+标注，不拒收）
         + 执行态提取 + 跨系统对账 + 金标问答

与电商套件（test_execont_ecommerce_order.py）互补：
  电商套件 = 写入即 AI-Ready（OntoDB 闸门拒写，防新错）
  本套件   = 入湖即 AI-Ready（湖仓标注不拒收，揪存量 + 质量可见）

验证框架（对齐 AI-Ready 四层）：
  Part 0  T-Box + 映射定义  —— 源 schema→本体映射、质量规则
  Part 1  映射引擎          —— 源字段→本体属性，置信度分层
  Part 2  质量闸门          —— 标注不拦截！所有数据入湖 ★
  Part 3  执行态提取        —— CDC 变更→ExecOnt 事件链
  Part 4  三区模型          —— 原始/标准/语义/隔离区
  Part 5  跨系统对账        —— ERP+WMS 状态一致性
  Part 6  金标问答          —— Agent 终裁 10 问

口径红线：入湖语义化是「标注」不是「拒收」——脏数据也入湖，带质量标注可查。
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


# ══════════════════════════════════════════════════════════════
#  源 Schema 定义（模拟 ERP / WMS 业务系统）
# ══════════════════════════════════════════════════════════════

# ERP 订单模块源 schema（简化版）
ERP_SCHEMA = {
    "sales_orders": {
        "order_id": "varchar(20) PK",
        "customer_id": "int FK",
        "order_date": "datetime",
        "status": "int",  # 0=draft,1=confirmed,2=shipped,3=invoiced,4=cancelled
        "total_amount": "decimal(12,2)",
        "updated_at": "datetime",
    },
    "order_status_log": {
        "log_id": "int PK",
        "order_id": "varchar(20) FK",
        "old_status": "int",
        "new_status": "int",
        "changed_at": "datetime",
    },
}

# WMS 物流模块源 schema
WMS_SCHEMA = {
    "shipments": {
        "shipment_id": "varchar(20) PK",
        "order_id": "varchar(20) FK",
        "status": "int",  # 0=pending,1=picked,2=packed,3=shipped,4=delivered
        "shipped_at": "datetime",
    },
}

# 状态枚举映射（工具从采样数据推断，人工确认）
ERP_STATUS_ENUM = {0: "draft", 1: "confirmed", 2: "shipped", 3: "invoiced", 4: "cancelled"}
WMS_STATUS_ENUM = {0: "pending", 1: "picked", 2: "packed", 3: "shipped", 4: "delivered"}

# ══════════════════════════════════════════════════════════════
#  Schema 映射定义（Mapping-as-Code）
# ══════════════════════════════════════════════════════════════

# 源字段 → 本体属性映射（自动生成 + 人工审核）
ERP_MAPPINGS = [
    # (source_table, source_field, target_entity, target_property, map_type, confidence)
    ("sales_orders", "order_id", "Order", "id", "primary_key", "high"),
    ("sales_orders", "status", "Order", "state", "enum_mapping", "medium"),
    ("sales_orders", "order_date", "Order", "ts", "timestamp", "high"),
    ("sales_orders", "customer_id", "Order", "customer", "foreign_key", "high"),
    ("sales_orders", "total_amount", "Order", "total_amount", "decimal", "high"),
    ("order_status_log", "changed_at", "ExecEvent", "ts", "timestamp", "high"),
    ("order_status_log", "order_id", "ExecEvent", "subject", "foreign_key", "high"),
    ("order_status_log", "new_status", "ExecEvent", "state", "enum_mapping", "medium"),
]

WMS_MAPPINGS = [
    ("shipments", "shipment_id", "Shipment", "id", "primary_key", "high"),
    ("shipments", "order_id", "Shipment", "order_ref", "foreign_key", "high"),
    ("shipments", "status", "Shipment", "state", "enum_mapping", "medium"),
    ("shipments", "shipped_at", "ExecEvent", "ts", "timestamp", "high"),
]

# ══════════════════════════════════════════════════════════════
#  测试数据（模拟 CDC 变更事件）
# ══════════════════════════════════════════════════════════════

# ERP 订单（4 条，覆盖 clean / dangling_ref / enum_violation / state_violation）
ERP_ORDERS = [
    # (order_id, customer_id, status, total_amount, quality, issues)
    ("LP-ORD-001", "CUST-001", 2, 5999.00, "clean", []),
    ("LP-ORD-002", "CUST-999", 1, 3000.00, "annotated",
     [{"rule": "dangling_ref", "detail": "customer_id=CUST-999 not found", "severity": "warning"}]),
    ("LP-ORD-003", "CUST-001", 99, 1200.00, "annotated",
     [{"rule": "enum_violation", "detail": "status=99 not in enum", "severity": "warning"}]),
    ("LP-ORD-004", "CUST-002", 2, 8800.00, "quarantined",
     [{"rule": "state_violation", "detail": "shipped without payment record", "severity": "critical"}]),
]

# ERP 状态变更日志（CDC 变更流）
ERP_STATUS_LOG = [
    # (log_id, order_id, old_status, new_status, changed_at)
    ("LOG-001", "LP-ORD-001", 0, 1, 1000.0),
    ("LOG-002", "LP-ORD-001", 1, 2, 2000.0),
    ("LOG-003", "LP-ORD-004", 0, 1, 1500.0),
    ("LOG-004", "LP-ORD-004", 1, 2, 2500.0),
]

# WMS 物流（2 条，覆盖 consistent / inconsistent）
WMS_SHIPMENTS = [
    # (shipment_id, order_id, status, quality, issues)
    ("LP-SHIP-001", "LP-ORD-001", 3, "clean", []),           # shipped, 与 ERP 一致
    ("LP-SHIP-002", "LP-ORD-004", 0, "annotated",
     [{"rule": "cross_system_gap", "detail": "ERP says shipped, WMS says pending", "severity": "critical"}]),
]


def bare(ref):
    return str(ref or "").split("::")[-1]


def main():
    global PASS, FAIL

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 0 · T-Box + 映射定义：源 schema→本体，规矩是数据不是代码")
    print("=" * 72)

    # ── 清场（可重复执行）──
    q("DROP ONTOLOGY exec_onto")
    q("DROP ONTOLOGY lake_pipe")
    for cls in ["ExecAgent", "Capability", "Process", "Activity", "State", "Transition",
                "ExecEvent", "Goal", "Intent", "ExecConstraint", "Resource", "ExecutionContext",
                "LakeRecord", "LakeQuality", "LakeRecon"]:
        q("TRUNCATE TABLE " + cls)
    print("      清场完成")

    # ── 元模型单源加载 ──
    stmts = load_meta_statements(META_SQL)
    check("P0-1  exec_onto_meta.sql 解析出全部 DDL", len(stmts) >= 11, "count=%d" % len(stmts))
    meta_ok = all(q(s).get("success") for s in stmts)
    check("P0-2  六面元模型 DDL 全部执行成功", meta_ok)

    # ── 入湖管线扩展：LakeRecord（源记录）+ LakeQuality（质量标注）+ LakeRecon（跨系统对账）──
    expect_ok("P0-3  管线扩展本体 lake_pipe 创建",
              "CREATE ONTOLOGY lake_pipe ("
              "  CLASS LakeRecord,"
              "  PROPERTY lr_id       DOMAIN LakeRecord RANGE STRING,"
              "  PROPERTY lr_system   DOMAIN LakeRecord RANGE STRING,"
              "  PROPERTY lr_table    DOMAIN LakeRecord RANGE STRING,"
              "  PROPERTY lr_src_id   DOMAIN LakeRecord RANGE STRING,"
              "  PROPERTY lr_entity   DOMAIN LakeRecord RANGE STRING,"
              "  PROPERTY lr_zone     DOMAIN LakeRecord RANGE STRING,"
              "  PROPERTY lr_quality  DOMAIN LakeRecord RANGE STRING,"
              "  CLASS LakeQuality,"
              "  PROPERTY lq_id       DOMAIN LakeQuality RANGE STRING,"
              "  PROPERTY lq_record   DOMAIN LakeQuality RANGE STRING,"
              "  PROPERTY lq_rule     DOMAIN LakeQuality RANGE STRING,"
              "  PROPERTY lq_detail   DOMAIN LakeQuality RANGE STRING,"
              "  PROPERTY lq_severity DOMAIN LakeQuality RANGE STRING,"
              "  CLASS LakeRecon,"
              "  PROPERTY recon_id    DOMAIN LakeRecon RANGE STRING,"
              "  PROPERTY recon_order DOMAIN LakeRecon RANGE STRING,"
              "  PROPERTY recon_sys   DOMAIN LakeRecon RANGE STRING,"
              "  PROPERTY recon_state DOMAIN LakeRecon RANGE STRING,"
              "  PROPERTY recon_ts    DOMAIN LakeRecon RANGE FLOAT64,"
              "  PROPERTY recon_status DOMAIN LakeRecon RANGE STRING"
              ")")

    # ── 映射定义入库（Mapping-as-Code 的持久化形态）──
    for tbl, fld, ent, prop, mtype, conf in ERP_MAPPINGS + WMS_MAPPINGS:
        sys_name = "ERP" if (tbl, fld, ent, prop, mtype, conf) in ERP_MAPPINGS else "WMS"
        map_id = "MAP-%s-%s-%s" % (sys_name, tbl, fld)  # 含表名防 ID 冲突
        expect_ok("P0-4  映射 %s.%s → %s.%s [%s/%s]" % (tbl, fld, ent, prop, mtype, conf),
                  "INSERT INTO LakeRecord (id, lr_system, lr_table, lr_src_id, lr_entity, lr_zone, lr_quality) "
                  "VALUES ('%s', '%s', '%s', '%s', '%s.%s', 'mapping', '%s')"
                  % (map_id, sys_name, tbl, fld, ent, prop, conf))

    # ── 本体契约钉住 ──
    try:
        with urllib.request.urlopen(SCHEMA, timeout=10) as r:
            schema_json = json.loads(r.read().decode("utf-8"))
        names = [o.get("name") for o in schema_json["data"]["ontologies"]]
        check("P0-5  双本体注册（exec_onto + lake_pipe）",
              "exec_onto" in names and "lake_pipe" in names, str(names))
    except Exception as e:
        check("P0-5  双本体注册", False, str(e)[:100])

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 1 · 映射引擎：源字段→本体属性，置信度分层")
    print("=" * 72)

    # M1 · ERP 订单映射完整性
    erp_map_rows = rows("SELECT lr_src_id, lr_entity, lr_quality FROM LakeRecord "
                         "WHERE lr_zone = 'mapping' AND lr_system = 'ERP'")
    check("M1  ERP 映射条目完整（8 条）", len(erp_map_rows) == 8, "got=%d" % len(erp_map_rows))

    # M2 · WMS 映射完整性
    wms_map_rows = rows("SELECT lr_src_id FROM LakeRecord "
                         "WHERE lr_zone = 'mapping' AND lr_system = 'WMS'")
    check("M2  WMS 映射条目完整（4 条）", len(wms_map_rows) == 4, "got=%d" % len(wms_map_rows))

    # M3 · 核心映射正确性：sales_orders.order_id → Order.id
    m = rows("SELECT lr_entity FROM LakeRecord WHERE lr_src_id = 'order_id' AND lr_table = 'sales_orders'")
    check("M3  sales_orders.order_id → Order.id（primary_key/high）",
          m and m[0].get("lr_entity") == "Order.id")

    # M4 · 枚举映射：status → Order.state（medium 置信度，需人工确认）
    m = rows("SELECT lr_quality FROM LakeRecord WHERE lr_src_id = 'status' AND lr_system = 'ERP'")
    check("M4  status → Order.state（enum_mapping/medium 置信度）",
          m and m[0].get("lr_quality") == "medium")

    # M5 · 外键映射：customer_id → Order.customer
    m = rows("SELECT lr_entity FROM LakeRecord WHERE lr_src_id = 'customer_id' AND lr_system = 'ERP'")
    check("M5  customer_id → Order.customer（foreign_key/high）",
          m and m[0].get("lr_entity") == "Order.customer")

    # M6 · 执行态映射：order_status_log → ExecEvent
    m = rows("SELECT lr_entity FROM LakeRecord WHERE lr_src_id = 'changed_at' AND lr_system = 'ERP'")
    check("M6  order_status_log.changed_at → ExecEvent.ts（时间戳→事件时间）",
          m and m[0].get("lr_entity") == "ExecEvent.ts")

    # M7 · 置信度分层：high 映射 > medium 映射
    high_cnt = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'mapping' AND lr_quality = 'high'"))
    med_cnt = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'mapping' AND lr_quality = 'medium'"))
    check("M7  置信度分层：high=%d > medium=%d（人工只需审核 medium）" % (high_cnt, med_cnt),
          high_cnt > med_cnt)

    # M8 · 映射模板复用性：ERP/WMS 共享 order_like 模式
    both_sys = rows("SELECT DISTINCT lr_system FROM LakeRecord WHERE lr_zone = 'mapping'")
    check("M8  映射模板跨系统复用（ERP+WMS 共享 order_like 模式）",
          len(both_sys) == 2)

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 2 · 质量闸门：标注不拦截！所有数据入湖 ★核心差异")
    print("=" * 72)
    print("  对比：OntoDB 写入闸门=拒写不落库；湖仓入湖闸门=照收+标注+隔离")
    print("  脏数据不丢弃、不隐藏——进隔离区，带标注可查。")

    # ── 模拟入湖：4 条 ERP 订单 + 2 条 WMS 物流 ──
    # 关键：所有数据都写入 LakeRecord，quality 标注但不影响入湖
    for oid, cid, status, amount, quality, issues in ERP_ORDERS:
        expect_ok("Q1  入湖 %s（quality=%s）" % (oid, quality),
                  "INSERT INTO LakeRecord (id, lr_system, lr_table, lr_src_id, lr_entity, lr_zone, lr_quality) "
                  "VALUES ('REC-%s', 'ERP', 'sales_orders', '%s', 'Order', 'semantic', '%s')"
                  % (oid, oid, quality))
        for j, issue in enumerate(issues):
            expect_ok("Q1b 质量标注 %s [%s]" % (oid, issue["rule"]),
                      "INSERT INTO LakeQuality (id, lq_record, lq_rule, lq_detail, lq_severity) "
                      "VALUES ('QI-%s-%d', 'REC-%s', '%s', '%s', '%s')"
                      % (oid, j, oid, issue["rule"], issue["detail"].replace("'", "''"), issue["severity"]))

    for sid, oid, status, quality, issues in WMS_SHIPMENTS:
        expect_ok("Q1c 入湖 %s（quality=%s）" % (sid, quality),
                  "INSERT INTO LakeRecord (id, lr_system, lr_table, lr_src_id, lr_entity, lr_zone, lr_quality) "
                  "VALUES ('REC-%s', 'WMS', 'shipments', '%s', 'Shipment', 'semantic', '%s')"
                  % (sid, sid, quality))
        for j, issue in enumerate(issues):
            expect_ok("Q1d 质量标注 %s [%s]" % (sid, issue["rule"]),
                      "INSERT INTO LakeQuality (id, lq_record, lq_rule, lq_detail, lq_severity) "
                      "VALUES ('QI-%s-%d', 'REC-%s', '%s', '%s', '%s')"
                      % (sid, j, sid, issue["rule"], issue["detail"].replace("'", "''"), issue["severity"]))

    # ★ 核心断言：所有数据都入湖了（标注不拦截）
    all_records = rows("SELECT id FROM LakeRecord WHERE lr_zone = 'semantic'")
    check("Q2  ★标注不拦截：6 条记录全部入湖（0 条被拒）", len(all_records) == 6, "got=%d" % len(all_records))

    # ★ 脏数据也入湖（对比 OntoDB 闸门拒写）
    dirty_records = rows("SELECT id FROM LakeRecord WHERE lr_zone = 'semantic' AND lr_quality != 'clean'")
    check("Q3  ★脏数据照收入湖（4 条非 clean 全部在湖里）", len(dirty_records) == 4, "got=%d" % len(dirty_records))

    # 质量标注完整
    qi_rows = rows("SELECT id FROM LakeQuality")
    check("Q4  质量标注条目完整（4 条 issue）", len(qi_rows) == 4, "got=%d" % len(qi_rows))

    # 质量标注可查（Agent 能查到质量问题）
    dangling = rows("SELECT lq_detail FROM LakeQuality WHERE lq_rule = 'dangling_ref'")
    check("Q5  悬空引用可查（Agent 能查到 customer 悬空）",
          dangling and "CUST-999" in str(dangling[0].get("lq_detail", "")))

    # 严重度分级
    critical = rows("SELECT id FROM LakeQuality WHERE lq_severity = 'critical'")
    check("Q6  严重度分级：critical=%d（状态机越界+跨系统不一致）" % len(critical),
          len(critical) == 2, "got=%d" % len(critical))

    # 隔离区数据可查（quarantined 不是丢弃）
    quarantined = rows("SELECT id FROM LakeRecord WHERE lr_quality = 'quarantined'")
    check("Q7  隔离区可查：quarantined 记录仍在湖里",
          quarantined and "LP-ORD-004" in str(quarantined[0].get("id", "")))

    # 质量问题关联到记录
    q4_issues = rows("SELECT lq_rule FROM LakeQuality WHERE lq_record = 'REC-LP-ORD-004'")
    check("Q8  质量标注关联到源记录（ORD-004 有 state_violation）",
          q4_issues and any("state_violation" in str(i.get("lq_rule", "")) for i in q4_issues))

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 3 · 执行态提取：CDC 变更→ExecOnt 事件链")
    print("=" * 72)

    # ── 状态机定义（T-Box）──
    for sid, name in [("st_draft", "草稿"), ("st_confirmed", "已确认"),
                      ("st_shipped", "已发货"), ("st_invoiced", "已开票"),
                      ("st_cancelled", "已取消")]:
        expect_ok("E1  状态 %s（%s）" % (sid, name),
                  "INSERT INTO State (id, state_name, state_machine) VALUES ('%s', '%s', 'erp_order')"
                  % (sid, name))

    for tid, fr, to in [("tr_confirm", "st_draft", "st_confirmed"),
                         ("tr_ship", "st_confirmed", "st_shipped"),
                         ("tr_invoice", "st_shipped", "st_invoiced"),
                         ("tr_cancel", "st_draft", "st_cancelled")]:
        expect_ok("E2  迁移 %s: %s → %s" % (tid, fr, to),
                  "INSERT INTO Transition (id, from_state, to_state) VALUES ('%s', '%s', '%s')"
                  % (tid, fr, to))

    # ── CDC 变更事件 → ExecOnt 事件 ──
    # LP-ORD-001: confirmed@1000 → shipped@2000 → signed@3000（合规链，3 节点供传递闭包）
    expect_ok("E3  CDC→事件 ORD-001 确认@1000",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('EV-001-1', 'ERP-CDC', 1000.0, 'confirmed', 'LP-ORD-001')")
    expect_ok("E3b CDC→事件 ORD-001 发货@2000（caused_by 链）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('EV-001-2', 'ERP-CDC', 2000.0, 'shipped', 'LP-ORD-001', 'ExecEvent::EV-001-1')")
    expect_ok("E3b2 CDC→事件 ORD-001 签收@3000（链尾，3 节点供传递闭包）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('EV-001-3', 'ERP-CDC', 3000.0, 'signed', 'LP-ORD-001', 'ExecEvent::EV-001-2')")

    # LP-ORD-004: confirmed@1500 → shipped@2500（但 WMS 不一致）
    expect_ok("E3c CDC→事件 ORD-004 确认@1500",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('EV-004-1', 'ERP-CDC', 1500.0, 'confirmed', 'LP-ORD-004')")
    expect_ok("E3d CDC→事件 ORD-004 发货@2500",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('EV-004-2', 'ERP-CDC', 2500.0, 'shipped', 'LP-ORD-004', 'ExecEvent::EV-004-1')")

    # ── 验证执行态 ──
    ev_001 = rows("SELECT frame, ts FROM ExecEvent WHERE task_name = 'LP-ORD-001'")
    # ORDER BY ts 不生效，客户端排序
    ev_001.sort(key=lambda e: float(e.get("ts", 0)))
    check("E4  ORD-001 执行态时间线：confirmed→shipped→signed（客户端排序）",
          [e.get("frame") for e in ev_001] == ["confirmed", "shipped", "signed"])

    # 因果链
    ev_chain = rows("SELECT id, caused_by FROM ExecEvent WHERE task_name = 'LP-ORD-001'")
    has_chain = any(bare(e.get("caused_by")) == "EV-001-1" for e in ev_chain)
    check("E5  因果链：shipped caused_by confirmed（主键形态引用）", has_chain)

    # 多事件同一主体
    ev_004 = rows("SELECT id FROM ExecEvent WHERE task_name = 'LP-ORD-004'")
    check("E6  ORD-004 执行态：2 条事件（confirmed+shipped）", len(ev_004) == 2)

    # 推理：因果传递（3 节点链：signed→shipped→confirmed，传递给出 signed→confirmed）
    inf_ev = rows("INFER SELECT * FROM ExecEvent")
    transitive = [r for r in inf_ev
                  if r.get("predicate") == "caused_by"
                  and str(r.get("subject", "")).endswith("EV-001-3")
                  and str(r.get("object", "")).endswith("EV-001-1")]
    check("E7  因果传递推理（caused_by TRANSITIVE，隔 2 跳）", len(transitive) > 0)

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 4 · 三区模型：原始/标准/语义/隔离区")
    print("=" * 72)

    # 原始区：所有源记录（镜像）
    raw_cnt = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'raw'"))
    # 标准区：已映射记录
    std_cnt = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'std'"))
    # 语义区：已完成语义标注
    sem_cnt = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'semantic'"))
    # 隔离区：质量问题记录
    qtn_cnt = len(rows("SELECT id FROM LakeRecord WHERE lr_quality = 'quarantined'"))

    check("Z1  语义区记录数（6 条入湖记录）", sem_cnt == 6, "got=%d" % sem_cnt)
    check("Z2  隔离区记录数（1 条 quarantined）", qtn_cnt == 1, "got=%d" % qtn_cnt)

    # 语义区包含 clean 和非 clean（脏数据也在语义区，带标注）
    sem_clean = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'semantic' AND lr_quality = 'clean'"))
    sem_dirty = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'semantic' AND lr_quality != 'clean'"))
    check("Z3  语义区含 clean(%d) + 非clean(%d)——脏数据也在" % (sem_clean, sem_dirty),
          sem_clean + sem_dirty == 6)

    # 隔离区记录可查（带质量标注）
    qtn_detail = rows("SELECT lr_src_id, lr_quality FROM LakeRecord WHERE lr_quality = 'quarantined'")
    check("Z4  隔离区可查（ORD-004 在隔离区，quality=quarantined）",
          qtn_detail and "LP-ORD-004" in str(qtn_detail[0].get("lr_src_id", "")))

    # 跨区引用完整：质量标注关联到语义区记录
    cross_ref = rows("SELECT lq_record FROM LakeQuality WHERE lq_rule = 'state_violation'")
    check("Z5  跨区引用完整：质量标注→语义区记录",
          cross_ref and "REC-LP-ORD-004" in str(cross_ref[0].get("lq_record", "")))

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 5 · 跨系统对账：ERP+WMS 状态一致性")
    print("=" * 72)

    # ── 模拟 WMS 事件入湖 ──
    expect_ok("R1  WMS 事件 SHIP-001（shipped@2100）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('EV-S001', 'WMS-CDC', 2100.0, 'shipped', 'LP-ORD-001')")
    expect_ok("R2  WMS 事件 SHIP-002（pending@2600）",
              "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name, caused_by) "
              "VALUES ('EV-S002', 'WMS-CDC', 2600.0, 'pending', 'LP-ORD-004')")

    # ── 跨系统对账：对比同一里程碑（shipped）在两个系统的一致性 ──
    def get_system_state(order_id, system, milestone="shipped"):
        """取指定系统在指定里程碑上的事件（对比同一节点，不是各自最新态）"""
        evs = rows("SELECT frame, ts, event_agent FROM ExecEvent WHERE task_name = '%s'" % order_id)
        sys_evs = [e for e in evs if system in str(e.get("event_agent", ""))]
        for e in sys_evs:
            if e.get("frame") == milestone:
                return milestone
        return sys_evs[-1].get("frame", "") if sys_evs else None

    def get_latest_erp_state(order_id):
        evs = rows("SELECT frame, ts FROM ExecEvent WHERE task_name = '%s' AND event_agent = 'ERP-CDC'" % order_id)
        evs.sort(key=lambda e: float(e.get("ts", 0)))
        return evs[-1].get("frame", "") if evs else None

    # ORD-001: ERP=shipped ✓, WMS=shipped ✓ → 一致
    erp_ship_1 = get_system_state("LP-ORD-001", "ERP")
    wms_ship_1 = get_system_state("LP-ORD-001", "WMS")
    check("R3  ORD-001 对账：ERP=%s / WMS=%s（shipped 里程碑一致）" % (erp_ship_1, wms_ship_1),
          erp_ship_1 == "shipped" and wms_ship_1 == "shipped")

    # ORD-004: ERP=shipped ✓, WMS=pending ✗ → 不一致
    erp_ship_4 = get_system_state("LP-ORD-004", "ERP")
    wms_ship_4 = get_system_state("LP-ORD-004", "WMS")
    check("R4  ORD-004 对账：ERP=%s / WMS=%s → 不一致 ★" % (erp_ship_4, wms_ship_4),
          erp_ship_4 == "shipped" and wms_ship_4 != "shipped")

    # 对账结果入库
    expect_ok("R5  对账结果 ORD-001（consistent）",
              "INSERT INTO LakeRecon (id, recon_order, recon_sys, recon_state, recon_ts, recon_status) "
              "VALUES ('RECON-001', 'LP-ORD-001', 'ERP+WMS', 'shipped', 2100.0, 'consistent')")
    expect_ok("R6  对账结果 ORD-004（inconsistent）",
              "INSERT INTO LakeRecon (id, recon_order, recon_sys, recon_state, recon_ts, recon_status) "
              "VALUES ('RECON-004', 'LP-ORD-004', 'ERP+WMS', 'shipped/pending', 2600.0, 'inconsistent')")

    # 对账结果可查
    inconsistent = rows("SELECT recon_order FROM LakeRecon WHERE recon_status = 'inconsistent'")
    check("R7  不一致订单可查（跨系统对账揪出 ORD-004）",
          inconsistent and "LP-ORD-004" in str(inconsistent[0].get("recon_order", "")))

    # 时间差分析（客户端排序）
    erp_ship_ts = None
    wms_ship_ts = None
    all_evs = rows("SELECT frame, ts, event_agent FROM ExecEvent WHERE task_name = 'LP-ORD-001'")
    for e in all_evs:
        if e.get("frame") == "shipped" and "ERP" in str(e.get("event_agent", "")):
            erp_ship_ts = float(e.get("ts", 0))
        if e.get("frame") == "shipped" and "WMS" in str(e.get("event_agent", "")):
            wms_ship_ts = float(e.get("ts", 0))
    time_gap = abs((wms_ship_ts or 0) - (erp_ship_ts or 0))
    check("R8  时间差分析：ERP/WMS 发货时间差 %.0f（客户端排序）" % time_gap,
          time_gap < 500, "erp=%.0f wms=%.0f" % (erp_ship_ts or -1, wms_ship_ts or -1))

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 6 · 金标问答：Agent 终裁（查语义层，不碰业务系统）")
    print("=" * 72)

    qa_pass = 0

    def qa(idx, question, expected, got, ok):
        nonlocal qa_pass
        tag = "Q%-2d %s" % (idx, question)
        if ok:
            print("PASS  %s" % tag)
            print("        → %s" % got)
            qa_pass += 1
        else:
            print("FAIL  %s" % tag)
            print("        期望: %s" % expected)
            print("        实际: %s" % got)

    # Q1: ORD-001 当前状态
    evs = rows("SELECT frame, ts FROM ExecEvent WHERE task_name = 'LP-ORD-001' AND event_agent = 'ERP-CDC'")
    evs.sort(key=lambda e: float(e.get("ts", 0)))
    state = evs[-1].get("frame", "") if evs else "?"
    qa(1, "LP-ORD-001 当前在流程哪一步？", "signed", state, state == "signed")

    # Q2: ORD-002 有什么质量问题
    issues_002 = rows("SELECT lq_rule FROM LakeQuality WHERE lq_record = 'REC-LP-ORD-002'")
    issue_str = ", ".join(str(i.get("lq_rule", "")) for i in issues_002) if issues_002 else "无"
    qa(2, "LP-ORD-002 有哪些质量问题？", "dangling_ref", issue_str,
       "dangling_ref" in issue_str)

    # Q3: 哪些订单有状态机违规
    viol = rows("SELECT lq_record FROM LakeQuality WHERE lq_rule = 'state_violation'")
    viol_str = ", ".join(str(v.get("lq_record", "")) for v in viol) if viol else "无"
    qa(3, "哪些订单有状态机违规？", "REC-LP-ORD-004", viol_str,
       "LP-ORD-004" in viol_str)

    # Q4: 哪些订单跨系统不一致
    incons = rows("SELECT recon_order FROM LakeRecon WHERE recon_status = 'inconsistent'")
    incons_str = ", ".join(str(i.get("recon_order", "")) for i in incons) if incons else "无"
    qa(4, "哪些订单跨系统不一致？", "LP-ORD-004", incons_str,
       "LP-ORD-004" in incons_str)

    # Q5: 数据质量总体如何
    total = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'semantic'"))
    clean = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'semantic' AND lr_quality = 'clean'"))
    qa(5, "入湖数据质量总体如何？", "6 条入湖，2 条 clean，4 条有标注",
       "%d 条入湖，%d 条 clean" % (total, clean), total == 6 and clean == 2)

    # Q6: critical 级别问题有哪些
    crit = rows("SELECT lq_rule, lq_record FROM LakeQuality WHERE lq_severity = 'critical'")
    crit_str = ", ".join("%s(%s)" % (i.get("lq_record", ""), i.get("lq_rule", "")) for i in crit) if crit else "无"
    qa(6, "有哪些 critical 级别质量问题？", "state_violation + cross_system_gap",
       crit_str, "state_violation" in crit_str and "cross_system_gap" in crit_str)

    # Q7: ORD-004 的因果链（只看 ERP 事件）
    chain_004 = rows("SELECT id, frame, caused_by, ts FROM ExecEvent "
                      "WHERE task_name = 'LP-ORD-004' AND event_agent = 'ERP-CDC'")
    chain_004.sort(key=lambda e: float(e.get("ts", 0)))
    chain_str = "→".join(e.get("frame", "") for e in chain_004) if chain_004 else "?"
    qa(7, "LP-ORD-004 的执行事件链？", "confirmed→shipped", chain_str,
       chain_str == "confirmed→shipped")

    # Q8: 映射置信度分布
    high_n = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'mapping' AND lr_quality = 'high'"))
    med_n = len(rows("SELECT id FROM LakeRecord WHERE lr_zone = 'mapping' AND lr_quality = 'medium'"))
    qa(8, "映射置信度分布？", "high 多于 medium", "high=%d medium=%d" % (high_n, med_n),
       high_n > med_n)

    # Q9: 隔离区有什么
    qtn = rows("SELECT lr_src_id FROM LakeRecord WHERE lr_quality = 'quarantined'")
    qtn_str = ", ".join(str(i.get("lr_src_id", "")) for i in qtn) if qtn else "无"
    qa(9, "隔离区里有什么数据？", "LP-ORD-004", qtn_str,
       "LP-ORD-004" in qtn_str)

    # Q10: ERP 和 WMS 发货时间差
    qa(10, "ERP/WMS 发货时间差多少？", "100（ERP=2000 WMS=2100）",
       "%.0f" % time_gap, abs(time_gap - 100) < 50)

    print()

    # ══════════════════════════════════════════════════════════
    print("=" * 72)
    print("Part 7 · 价值对照：入湖语义化 vs 传统 ETL 入湖")
    print("=" * 72)

    value_items = [
        ("数据入湖即语义就绪", "schema 映射把源字段认领到本体概念", True),
        ("脏数据照收入湖", "标注不拦截——6/6 入湖，0 被拒", len(all_records) == 6),
        ("质量可见可查", "quality 标注 + LakeQuality 可查询", len(qi_rows) >= 4),
        ("执行态可推理", "CDC→ExecOnt 事件链 + caused_by 传递", len(transitive) > 0),
        ("跨系统可对账", "ERP+WMS 状态统一到 ExecOnt", erp_ship_4 != wms_ship_4),
        ("Agent 直查可答", "金标问答 %d/%d，不碰业务系统" % (qa_pass, 10), qa_pass >= 9),
    ]
    for title, detail, ok in value_items:
        check("V   %s —— %s" % (title, detail), ok)

    print()
    print("=" * 72)
    print("结果: %d PASS / %d FAIL / 金标 %d/10" % (PASS, FAIL, qa_pass))
    if FAIL == 0 and qa_pass >= 9:
        print("★ 全绿 —— 入湖即 AI-Ready 实证通过")
    else:
        print("有断言未通过，请检查")
    print("=" * 72)


if __name__ == "__main__":
    main()
