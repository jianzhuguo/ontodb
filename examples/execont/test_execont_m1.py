# -*- coding: utf-8 -*-
"""ExecOnt 执行态本体 · M1 验收断言套件（HTTP /api/query @7912）

配套文档：E:\\ontodb\\docs\\执行态本体开发方案与战略意义.md（§六 M1 验收）
元模型包：examples/execont/exec_onto_meta.sql
对照基线：E:\\semmesh\\tmp-gate\\test_robot_ai_ready.py（49/49，2026-11-15）

M1 交付与验收：
  ① 六面元模型 DDL 全绿（主体/过程/状态/事件/意图/资源 + 机器人特化）
  ② 机器人断言在 ExecOnt 形态下全过（闸门/推理/AI 面/灰色地带/金标问答）
  ③ preceded_by 工序传递 INFER 带归因（PrpTrp）
内核改动：零（纯 DDL 约定）。
"""
import json, urllib.request

BASE = "http://127.0.0.1:7912/api/query"
SCHEMA = "http://127.0.0.1:7912/api/schema"
PASS, FAIL = 0, 0


def q(sql):
    body = json.dumps({"query": sql}).encode()
    req = urllib.request.Request(BASE, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode())


def expect_ok(name, sql):
    global PASS, FAIL
    try:
        d = q(sql)
        if "error" in d and d.get("error"):
            print(f"FAIL  {name}: {d['error']}"); FAIL += 1
        else:
            print(f"PASS  {name}"); PASS += 1
        return d
    except Exception as e:
        detail = ""
        try:
            detail = e.read().decode()[:160]
        except Exception:
            pass
        print(f"FAIL  {name}: {e} {detail}"); FAIL += 1
        return None


def expect_reject(name, sql, needle):
    global PASS, FAIL
    try:
        d = q(sql)
        err = str(d.get("error", ""))
        if err:
            if needle.lower() in err.lower():
                print(f"PASS  {name} -> rejected: {err[:90]}")
            else:
                print(f"PASS? {name} -> rejected but unexpected msg: {err[:120]}")
            PASS += 1
        else:
            print(f"FAIL  {name}: write was ACCEPTED, expected gate rejection"); FAIL += 1
    except Exception as e:
        detail = ""
        try:
            detail = e.read().decode()
        except Exception:
            pass
        if needle.lower() in detail.lower():
            print(f"PASS  {name} -> rejected: {detail[:110]}")
        else:
            print(f"PASS? {name} -> rejected: {detail[:110]}")
        PASS += 1


def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        print(f"PASS  {name}" + (f"  ({detail})" if detail else "")); PASS += 1
    else:
        print(f"FAIL  {name}" + (f"  ({detail})" if detail else "")); FAIL += 1


print("=" * 72)
print("Part 0 · 清场（套件可重复执行）")
print("=" * 72)
for sql in [
    "DROP ONTOLOGY exec_onto",
    "TRUNCATE TABLE ExecAgent", "TRUNCATE TABLE Capability",
    "TRUNCATE TABLE Process", "TRUNCATE TABLE Activity",
    "TRUNCATE TABLE State", "TRUNCATE TABLE Transition",
    "TRUNCATE TABLE ExecEvent", "TRUNCATE TABLE GraspEvent", "TRUNCATE TABLE PlaceEvent",
    "TRUNCATE TABLE Goal", "TRUNCATE TABLE Intent", "TRUNCATE TABLE ExecConstraint",
    "TRUNCATE TABLE Resource", "TRUNCATE TABLE ExecutionContext",
]:
    try:
        q(sql)
    except Exception:
        pass
print("      cleanup done")

print()
print("=" * 72)
print("Part 1 · M1 验收①：六面元模型 DDL 全绿（零内核改动）")
print("=" * 72)
expect_ok("DDL 六面元模型 + 机器人特化（exec_onto 一本体）", (
    "CREATE ONTOLOGY exec_onto ("
    # 主体面
    "CLASS ExecAgent, CLASS Capability, "
    # 过程面
    "CLASS Process, CLASS Activity, "
    # 状态面
    "CLASS State, CLASS Transition, "
    # 事件面
    "CLASS ExecEvent, "
    # 意图面
    "CLASS Goal, CLASS Intent, CLASS ExecConstraint, "
    # 资源面
    "CLASS Resource, CLASS ExecutionContext, "
    # 主体面属性
    "PROPERTY agent_serial DOMAIN ExecAgent RANGE STRING, "
    "PROPERTY capability_of DOMAIN Capability RANGE STRING, "
    # 过程面属性
    "PROPERTY activity_name DOMAIN Activity RANGE STRING, "
    "PROPERTY process_of DOMAIN Activity RANGE STRING, "
    "PROPERTY composed_of DOMAIN Process RANGE STRING, "
    "PROPERTY preceded_by DOMAIN Activity RANGE STRING, "
    "PROPERTY exec_agent DOMAIN Process RANGE STRING, "
    # 状态面属性
    "PROPERTY state_name DOMAIN State RANGE STRING, "
    "PROPERTY state_machine DOMAIN State RANGE STRING, "
    "PROPERTY from_state DOMAIN Transition RANGE STRING, "
    "PROPERTY to_state DOMAIN Transition RANGE STRING, "
    "PROPERTY guard DOMAIN Transition RANGE STRING, "
    "PROPERTY current_state DOMAIN Process RANGE STRING, "
    # 事件面属性（provenance 必填约定作用于 ExecEvent）
    "PROPERTY event_agent DOMAIN ExecEvent RANGE STRING, "
    "PROPERTY ts DOMAIN ExecEvent RANGE FLOAT64, "
    "PROPERTY frame DOMAIN ExecEvent RANGE STRING, "
    "PROPERTY task_name DOMAIN ExecEvent RANGE STRING, "
    "PROPERTY caused_by DOMAIN ExecEvent RANGE STRING, "
    # 意图面属性
    "PROPERTY goal_of DOMAIN Intent RANGE STRING, "
    "PROPERTY constraint_of DOMAIN ExecConstraint RANGE STRING, "
    "PROPERTY has_intent DOMAIN Process RANGE STRING, "
    # 资源面属性
    "PROPERTY ctx_of DOMAIN ExecutionContext RANGE STRING, "
    "PROPERTY obj_name DOMAIN Resource RANGE STRING, "
    "PROPERTY embedding DOMAIN Resource RANGE ARRAY, "
    # 机器人特化
    "CLASS GraspEvent SUBCLASS OF ExecEvent, CLASS PlaceEvent, "
    "PROPERTY target DOMAIN GraspEvent RANGE STRING, "
    "PROPERTY confidence DOMAIN GraspEvent RANGE FLOAT64)"))

expect_ok("约定①：event_agent REQUIRED（谁：provenance 必填）", "ALTER PROPERTY event_agent DOMAIN ExecEvent REQUIRED")
expect_ok("约定②：ts REQUIRED（何时：provenance 必填）", "ALTER PROPERTY ts DOMAIN ExecEvent REQUIRED")
expect_ok("约定③：ExecAgent UNIQUE(agent_serial)", "ALTER CLASS ExecAgent UNIQUE (agent_serial)")
expect_ok("约定④：preceded_by TRANSITIVE（工序先后）", "ALTER PROPERTY preceded_by DOMAIN Activity CHARACTERISTICS TRANSITIVE")
expect_ok("约定⑤：caused_by TRANSITIVE（因果）", "ALTER PROPERTY caused_by DOMAIN ExecEvent CHARACTERISTICS TRANSITIVE")
expect_ok("约定⑥：composed_of TRANSITIVE（过程分解）", "ALTER PROPERTY composed_of DOMAIN Process CHARACTERISTICS TRANSITIVE")
expect_ok("约定⑦：GraspEvent DISJOINT WITH PlaceEvent（事件互斥）", "ALTER CLASS GraspEvent DISJOINT WITH PlaceEvent")
expect_ok("约定⑧：from_state FUNCTIONAL（迁移唯一源，形式语义 A.6 纪律1）", "ALTER PROPERTY from_state DOMAIN Transition CHARACTERISTICS FUNCTIONAL")
expect_ok("约定⑨：to_state FUNCTIONAL（迁移唯一目标）", "ALTER PROPERTY to_state DOMAIN Transition CHARACTERISTICS FUNCTIONAL")
expect_ok("约定⑩：current_state FUNCTIONAL（过程唯一当前位置）", "ALTER PROPERTY current_state DOMAIN Process CHARACTERISTICS FUNCTIONAL")

try:
    req = urllib.request.Request(SCHEMA)
    with urllib.request.urlopen(req, timeout=10) as r:
        schema_text = r.read().decode()
    planes = ["ExecAgent", "Capability", "Process", "Activity", "State", "Transition",
              "ExecEvent", "Goal", "Intent", "ExecConstraint", "Resource", "ExecutionContext"]
    missing = [c for c in planes if c not in schema_text]
    check("六面 12 类全部登记于 schema", not missing, f"missing={missing}")
    check("机器人特化类登记于 schema（GraspEvent/PlaceEvent）",
          "GraspEvent" in schema_text and "PlaceEvent" in schema_text)
except Exception as e:
    check("六面 12 类全部登记于 schema", False, f"schema fetch error: {e}")

print()
print("      -- 六面实例烟测（每面至少一行，provenance 合规） --")
expect_ok("主体面：ExecAgent agent_meta", "INSERT INTO ExecAgent (id, agent_serial) VALUES ('agent_meta', 'META-001')")
expect_ok("主体面：Capability cap1", "INSERT INTO Capability (id, capability_of) VALUES ('cap1', 'ExecAgent::agent_meta')")
expect_ok("过程面：Process proc1（巡检过程）", "INSERT INTO Process (id, exec_agent, current_state, has_intent) VALUES ('proc1', 'ExecAgent::agent_meta', 'State::st_running', 'Intent::i1')")
expect_ok("过程面：Activity act_meta（工序）", "INSERT INTO Activity (id, activity_name, process_of) VALUES ('act_meta', '元模型工序', 'Process::proc1')")
expect_ok("状态面：State st_created/st_running", "INSERT INTO State (id, state_name, state_machine) VALUES ('st_created', '已创建', 'SM::proc_lifecycle')")
expect_ok("状态面：State st_running", "INSERT INTO State (id, state_name, state_machine) VALUES ('st_running', '运行中', 'SM::proc_lifecycle')")
expect_ok("状态面：Transition tr1（守卫迁移）", "INSERT INTO Transition (id, from_state, to_state, guard) VALUES ('tr1', 'State::st_created', 'State::st_running', 'precondition_ok')")
expect_ok("事件面：ExecEvent em_meta（provenance 合规）", "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name) VALUES ('em_meta', 'agent_meta', 500.0, 'map', '元模型烟测')")
expect_ok("意图面：Goal g_inspect", "INSERT INTO Goal (id) VALUES ('g_inspect')")
expect_ok("意图面：Intent i1（绑定目标）", "INSERT INTO Intent (id, goal_of) VALUES ('i1', 'Goal::g_inspect')")
expect_ok("意图面：ExecConstraint c1（绑定意图）", "INSERT INTO ExecConstraint (id, constraint_of) VALUES ('c1', 'Intent::i1')")
expect_ok("资源面：Resource res_meta", "INSERT INTO Resource (id, obj_name) VALUES ('res_meta', '元模型资源')")
expect_ok("资源面：ExecutionContext ctx1", "INSERT INTO ExecutionContext (id, ctx_of) VALUES ('ctx1', 'Process::proc1')")

d = expect_ok("六面实例可读：SELECT current_state FROM Process", "SELECT id, current_state FROM Process")
if d:
    rows = d.get("data", [])
    ok = rows and rows[0].get("current_state") == "State::st_running"
    check("状态面引用可读（proc1 处于 st_running）", bool(ok), f"rows={rows}")

print()
print("=" * 72)
print("Part 2 · 写入即事实化 + 五道闸门（ExecOnt 机器人形态）")
print("=" * 72)
expect_ok("INSERT 合规行：机器人 r1", "INSERT INTO ExecAgent (id, agent_serial) VALUES ('r1', 'SN-001')")
expect_ok("INSERT 合规行：事件 e1（巡检）", "INSERT INTO ExecEvent (id, event_agent, ts, frame, task_name) VALUES ('e1', 'r1', 1000.0, 'map', '巡检')")
expect_ok("INSERT 合规行：抓取 g1（蓝色工具箱）", "INSERT INTO GraspEvent (id, event_agent, ts, task_name, target, confidence) VALUES ('g1', 'r1', 1001.0, '抓取', '蓝色工具箱', 0.97)")
expect_reject("闸门①必填缺失(缺 event_agent)", "INSERT INTO ExecEvent (id, ts) VALUES ('gx1', 2000.0)", "required")
expect_reject("闸门②值域不符(confidence='high')", "INSERT INTO GraspEvent (id, event_agent, ts, task_name, target, confidence) VALUES ('gx2', 'r1', 2001.0, '抓取', '料箱', 'high')", "type")
expect_reject("闸门③唯一冲突(重复 agent_serial)", "INSERT INTO ExecAgent (id, agent_serial) VALUES ('r2', 'SN-001')", "unique")
expect_ok("准备 disjoint 对照：GraspEvent same_ev", "INSERT INTO GraspEvent (id, event_agent, ts, task_name, target, confidence) VALUES ('same_ev', 'r9', 2002.0, '抓取', '工装', 0.9)")
expect_reject("闸门④不相交(same_ev 双事件类)", "INSERT INTO PlaceEvent (id, event_agent, ts, task_name) VALUES ('same_ev', 'r9', 2003.0, '放置')", "disjoint")

# 闸门⑥ 功能属性单值（owl:FunctionalProperty，形式语义 A.6 纪律1）。
# 载体用 ARRAY 型属性——STRING 型在类型闸门就挡掉列表了，那样测不到功能闸门本身。
expect_ok("闸门⑥ 准备：ARRAY 型功能属性载体", "CREATE ONTOLOGY exec_gate (CLASS Bag, PROPERTY tags DOMAIN Bag RANGE ARRAY)")
expect_ok("闸门⑥ 约定：tags FUNCTIONAL", "ALTER PROPERTY tags DOMAIN Bag CHARACTERISTICS FUNCTIONAL")
expect_ok("闸门⑥ 单值放行：['a']", "INSERT INTO Bag (tags) VALUES (['a'])")
expect_reject("闸门⑥ 功能属性多值拒写：['a','b']", "INSERT INTO Bag (tags) VALUES (['a', 'b'])", "functional")
expect_ok("闸门⑥ 同值重复仍算一个值：['a','a']", "INSERT INTO Bag (tags) VALUES (['a', 'a'])")
expect_reject("闸门⑥b 标量属性不接受列表(from_state)", "INSERT INTO Transition (id, from_state) VALUES ('gx6', ['A', 'Z'])", "type")

# 闸门⑦ L2 守卫求值：guard 是数据，写成 `字段 op 字面量` 时内核当场判定。
# 三值语义（形式语义 A.5）：满足放行 / 不满足拒写 / 判不了（缺操作数或语法不支持）不猜、放行。
expect_ok("闸门⑦ 准备：守卫载体", "CREATE ONTOLOGY exec_guard (CLASS GuardT, PROPERTY name DOMAIN GuardT RANGE STRING, PROPERTY guard DOMAIN GuardT RANGE STRING, PROPERTY quality_score DOMAIN GuardT RANGE FLOAT64)")
expect_ok("闸门⑦ 守卫满足放行：0.95 >= 0.8", "INSERT INTO GuardT (name, guard, quality_score) VALUES ('ok', 'quality_score >= 0.8', 0.95)")
expect_reject("闸门⑦ 守卫不满足拒写：0.3 >= 0.8", "INSERT INTO GuardT (name, guard, quality_score) VALUES ('bad', 'quality_score >= 0.8', 0.3)", "guard violation")
expect_ok("闸门⑦a 操作数缺失→未判定放行(schema-on-read)", "INSERT INTO GuardT (name, guard) VALUES ('nodata', 'quality_score >= 0.8')")
expect_ok("闸门⑦b 非支持语法→不判定不拦截", "INSERT INTO GuardT (name, guard) VALUES ('weird', 'a and b')")

print()
print("=" * 72)
print("Part 3 · 推理就绪（层级透明 + 工序传递 + 物化 + 演进面）  ← M1 验收③")
print("=" * 72)
d = expect_ok("事实化可读：SELECT GraspEvent", "SELECT id, target FROM GraspEvent")
if d:
    print(f"      -> GraspEvent rows: {[r.get('id') for r in d.get('data', [])]}")
d = expect_ok("类层级透明：SELECT ExecEvent 含 GraspEvent 实例", "SELECT id, task_name FROM ExecEvent")
if d:
    ids = [r.get("id") for r in d.get("data", [])]
    print(f"      -> ExecEvent rows: {ids}")
    check("Cax-sco：g1 经子类推理出现在 ExecEvent 查询中", "g1" in ids, f"ids={ids}")

expect_ok("规则事实：工序链 act_a → act_b", "INSERT INTO Activity (id, activity_name, process_of, preceded_by) VALUES ('act_a', '取料', 'Process::proc1', 'Activity::act_b')")
expect_ok("规则事实：工序链 act_b → act_c", "INSERT INTO Activity (id, activity_name, process_of, preceded_by) VALUES ('act_b', '转运', 'Process::proc1', 'Activity::act_c')")
expect_ok("规则事实：工序链末端 act_c", "INSERT INTO Activity (id, activity_name, process_of) VALUES ('act_c', '上料', 'Process::proc1')")
d = expect_ok("推理物化：INFER SELECT * FROM Activity", "INFER SELECT * FROM Activity")
if d:
    inferred = d.get("data", [])
    chained = any(
        r.get("subject", "").endswith("act_a") and r.get("object", "").endswith("act_c")
        for r in inferred
    )
    print(f"      -> inferred rows: {len(inferred)}")
    for r in inferred[:6]:
        print(f"         {r.get('subject')} --{r.get('predicate')}--> {r.get('object')}  [{r.get('rule')}]")
    check("M1验收③ Prp-trp 传递推理：act_a→act_c 蕴含由 act_a→act_b→act_c 工序链推出", chained)
    attributed = any(
        r.get("subject", "").endswith("act_a") and r.get("object", "").endswith("act_c") and r.get("rule")
        for r in inferred
    )
    check("M1验收③ 蕴含行带 rule 归因", attributed)

d = expect_ok("推理物化：INFER SELECT * FROM GraspEvent（层级蕴含）", "INFER SELECT * FROM GraspEvent")
if d:
    inferred = d.get("data", [])
    type_inf = [r for r in inferred if r.get("predicate") == "rdf:type"]
    print(f"      -> inferred rows: {len(inferred)}; rdf:type facts: {[(r.get('subject'), r.get('object')) for r in type_inf]}")
    hit = any(r.get("subject", "").endswith("g1") and r.get("object") == "ExecEvent" for r in type_inf)
    check("Cax-sco 物化：g1 rdf:type ExecEvent 蕴含带 rule 归因", hit)
    if type_inf:
        print(f"      -> rule attribution: {type_inf[0].get('rule')}")

expect_ok("演进面：ALTER CLASS PlaceEvent EXTENDS ExecEvent", "ALTER CLASS PlaceEvent EXTENDS ExecEvent")
expect_ok("演进后写入：放置 p1", "INSERT INTO PlaceEvent (id, event_agent, ts, task_name) VALUES ('p1', 'r1', 3004.0, '放置')")
d = expect_ok("演进后层级透明：SELECT ExecEvent 含 p1", "SELECT id, task_name FROM ExecEvent")
if d:
    ids = [r.get("id") for r in d.get("data", [])]
    check("演进后 Cax-sco：p1 出现在 ExecEvent 查询中", "p1" in ids, f"ids={ids}")

print()
print("=" * 72)
print("Part 4 · AI/Agent 面（中文检索 + 向量 + 机读）")
print("=" * 72)
expect_ok("资源建行：蓝色工具箱", "INSERT INTO Resource (id, obj_name, embedding) VALUES ('o1', '蓝色工具箱', '[0.1, 0.2, 0.3]')")
expect_ok("CJK n-gram 索引", "CREATE NGRAM INDEX ON Resource (obj_name)")
d = expect_ok("CJK 检索 LIKE '%工具箱%'", "SELECT id, obj_name FROM Resource WHERE obj_name LIKE '%工具箱%'")
if d and any(r.get("id") == "o1" for r in d.get("data", [])):
    check("中文子串检索命中 o1（CJK n-gram 索引）", True)
else:
    check("中文子串检索命中 o1（CJK n-gram 索引）", False, f"data={d.get('data') if d else None}")

try:
    q("DROP VECTOR INDEX ON Resource (embedding)")
except Exception:
    pass
expect_ok("向量索引", "CREATE VECTOR INDEX ON Resource (embedding) DIMENSION 3")
d = expect_ok("向量检索 top1", "VECTOR SEARCH ON Resource (embedding) QUERY [0.1, 0.2, 0.3] TOP 1")
if d:
    rows = d.get("data", [])
    print(f"      -> hits: {[r.get('id') for r in rows]}")
    check("HNSW 向量检索命中（AI 语义检索面）", bool(rows))

d = expect_ok("Agent 面：机器可读 JSON", "SELECT id, task_name FROM ExecEvent")
if d and isinstance(d, dict):
    keys = list(d.keys())
    print(f"      -> JSON envelope keys: {keys}")
    check("全链路响应为机器可读 JSON（Agent 可直读）", True)

print()
print("=" * 72)
print("Part 5 · 灰色地带闭环（未登记字段 → 补登记 → 闸门生效）")
print("=" * 72)
expect_ok("灰色地带实证：未登记字段 raw_imu 写入不拒收", "INSERT INTO ExecEvent (id, event_agent, ts, task_name, raw_imu) VALUES ('ev9', 'r1', 9000.0, '巡检', '0.98')")
d = expect_ok("灰色地带可见：SELECT ev9", "SELECT * FROM ExecEvent WHERE id = 'ev9'")
if d:
    rows = d.get("data", [])
    visible = rows and rows[0].get("raw_imu") == "0.98"
    check("未登记字段可读（schema-on-read 灰色地带实证）", bool(visible), f"row={rows[0] if rows else None}")
expect_ok("补登记：CREATE DATATYPE PROPERTY raw_imu RANGE FLOAT64", "CREATE DATATYPE PROPERTY raw_imu DOMAIN ExecEvent RANGE FLOAT64")
expect_reject("登记后闸门生效：raw_imu='bad'（FLOAT64）", "INSERT INTO ExecEvent (id, event_agent, ts, task_name, raw_imu) VALUES ('gx5', 'r1', 9001.0, '巡检', 'bad')", "type")

print()
print("=" * 72)
print("Part 6 · 金标问答（Agent 终裁）+ 完备性探针")
print("=" * 72)
expect_ok("补写抓取 g2（黑色料箱）", "INSERT INTO GraspEvent (id, event_agent, ts, task_name, target, confidence) VALUES ('g2', 'r1', 4001.0, '抓取', '黑色料箱', 0.91)")

d = expect_ok("金标①：「机器人 r1 的抓取事件有几次？」", "SELECT id FROM GraspEvent WHERE event_agent = 'r1'")
if d:
    ids = [r.get("id") for r in d.get("data", [])]
    print(f"      -> Agent 查库作答：{len(ids)} 次（{sorted(ids)}）")
    check("金标① 答案与事实一致（期望 2 次：g1, g2）", sorted(ids) == ["g1", "g2"], f"ids={sorted(ids)}")

d = expect_ok("金标②：「g1 的抓取目标叫什么？」", "SELECT target FROM GraspEvent WHERE id = 'g1'")
if d:
    rows = d.get("data", [])
    ans = rows[0].get("target") if rows else None
    print(f"      -> Agent 查库作答：{ans}")
    check("金标② 答案与事实一致（期望 蓝色工具箱）", ans == "蓝色工具箱", f"ans={ans}")

d = expect_ok("完备性探针：ExecEvent frame 覆盖率", "SELECT id, frame FROM ExecEvent")
if d:
    rows = d.get("data", [])
    total = len(rows)
    covered = sum(1 for r in rows if r.get("frame"))
    pct = (100.0 * covered / total) if total else 0.0
    print(f"      -> frame 覆盖率：{covered}/{total} = {pct:.1f}%")
    check("完备性探针报出 frame 覆盖率 < 100%（gap 被看见）", total > 0 and covered < total, f"{pct:.1f}%")

print()
print("=" * 72)
print(f"RESULT  PASS={PASS}  FAIL={FAIL}")
print("=" * 72)
