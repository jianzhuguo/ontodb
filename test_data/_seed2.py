import urllib.request
import json
import time

API = 'http://127.0.0.1:7912/api/query'

def execute(sql, delay=0.2):
    time.sleep(delay)
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"):
                print(f"  ERR: {result['error'][:150]}")
                return False
            else:
                print(f"  OK")
                return True
    except urllib.error.HTTPError as e:
        body = e.read().decode('utf-8', errors='replace')
        print(f"  HTTP {e.code}: {body[:150]}")
        return False
    except Exception as e:
        print(f"  FAIL: {e}")
        return False

stmts = [
    # 部门
    "CREATE VERTEX TABLE Department (name STRING, code STRING, head STRING, budget DOUBLE, level INT, parent STRING)",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('集团总部', 'HQ', '张明远', 50000000, 1, '')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('技术研发中心', 'TECH', '李建国', 28000000, 2, 'HQ')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('产品事业部', 'PRODUCT', '王芳', 15000000, 2, 'HQ')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('市场营销部', 'MARKET', '陈志强', 12000000, 2, 'HQ')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('人力资源部', 'HR', '刘婷', 5000000, 2, 'HQ')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('财务部', 'FIN', '赵伟', 4000000, 2, 'HQ')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('基础架构组', 'INFRA', '孙鹏', 8000000, 3, 'TECH')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('数据库内核组', 'DBCORE', '周磊', 10000000, 3, 'TECH')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('前端开发组', 'FE', '吴洋', 5000000, 3, 'TECH')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('AI算法组', 'AI', '郑宇', 6000000, 3, 'TECH')",
    # 员工
    "CREATE VERTEX TABLE Employee (name STRING, emp_id STRING, department STRING, title STRING, salary DOUBLE, hire_date STRING, email STRING)",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('张明远', 'EMP001', 'HQ', 'CEO', 85000, '2020-01-15', 'zhangmy@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('李建国', 'EMP002', 'TECH', 'CTO', 75000, '2020-03-01', 'lijg@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('王芳', 'EMP003', 'PRODUCT', 'VP产品', 65000, '2020-06-10', 'wangf@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('陈志强', 'EMP004', 'MARKET', 'VP市场', 60000, '2021-01-20', 'chenzq@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('刘婷', 'EMP005', 'HR', 'HRD', 45000, '2020-04-15', 'liut@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('赵伟', 'EMP006', 'FIN', 'CFO', 70000, '2020-02-01', 'zhaow@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('孙鹏', 'EMP007', 'INFRA', '架构师', 55000, '2021-03-10', 'sunp@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('周磊', 'EMP008', 'DBCORE', '首席工程师', 60000, '2020-08-01', 'zhoul@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('吴洋', 'EMP009', 'FE', '高级前端', 42000, '2021-06-15', 'wuy@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('郑宇', 'EMP010', 'AI', '算法专家', 58000, '2022-01-10', 'zhengy@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('黄浩', 'EMP011', 'DBCORE', '数据库工程师', 45000, '2022-04-01', 'huangh@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('林雪', 'EMP012', 'FE', '前端开发', 38000, '2023-02-15', 'linx@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('徐峰', 'EMP013', 'INFRA', '运维工程师', 40000, '2022-07-20', 'xuf@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('马丽', 'EMP014', 'PRODUCT', '产品经理', 42000, '2023-01-10', 'mal@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('何强', 'EMP015', 'MARKET', '市场总监', 48000, '2021-09-01', 'heq@ontodb.ai')",
    # 项目
    "CREATE VERTEX TABLE Project (name STRING, code STRING, department STRING, status STRING, budget DOUBLE, start_date STRING, progress INT)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('OntoDB内核v2.0', 'PRJ001', 'DBCORE', '进行中', 5000000, '2025-01-01', 65)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('分布式集群方案', 'PRJ002', 'INFRA', '进行中', 3000000, '2025-03-15', 40)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('数字孪生大屏', 'PRJ003', 'FE', '进行中', 1500000, '2026-08-01', 15)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('AI智能运维', 'PRJ004', 'AI', '规划中', 4000000, '2026-09-01', 5)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('政务版定制', 'PRJ005', 'PRODUCT', '进行中', 6000000, '2025-06-01', 80)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('金融版合规引擎', 'PRJ006', 'DBCORE', '已完成', 8000000, '2024-06-01', 100)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('向量搜索引擎优化', 'PRJ007', 'DBCORE', '进行中', 2000000, '2026-05-01', 55)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('时序数据引擎', 'PRJ008', 'DBCORE', '已完成', 3500000, '2025-09-01', 100)",
    # 服务器
    "CREATE VERTEX TABLE Server (name STRING, node_id STRING, role STRING, region STRING, cpu_cores INT, memory_gb INT, storage_tb DOUBLE, status STRING, ip STRING)",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-node-1', 'N1', 'leader', '华东-上海', 64, 256, 4.0, 'online', '10.0.1.1')",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-node-2', 'N2', 'follower', '华东-上海', 64, 256, 4.0, 'online', '10.0.1.2')",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-node-3', 'N3', 'follower', '华北-北京', 32, 128, 2.0, 'online', '10.0.2.1')",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-node-4', 'N4', 'follower', '华南-广州', 32, 128, 2.0, 'online', '10.0.3.1')",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-node-5', 'N5', 'follower', '西南-成都', 16, 64, 1.0, 'maintenance', '10.0.4.1')",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-backup-1', 'B1', 'backup', '华东-上海', 8, 32, 8.0, 'online', '10.0.1.10')",
    # 业务指标
    "CREATE VERTEX TABLE BizMetrics (date STRING, metric STRING, value DOUBLE, unit STRING, department STRING)",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-01', '日活用户', 12580, '人', 'PRODUCT')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-01', 'API调用量', 2850000, '次/日', 'TECH')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-01', '查询延迟P99', 8.5, 'ms', 'DBCORE')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-01', '数据总量', 128.5, 'TB', 'INFRA')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-01', '客户数', 47, '家', 'MARKET')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-01', '营收', 3200000, '元/月', 'FIN')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-02', '日活用户', 13200, '人', 'PRODUCT')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-02', 'API调用量', 3100000, '次/日', 'TECH')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-02', '查询延迟P99', 7.8, 'ms', 'DBCORE')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-02', '数据总量', 129.1, 'TB', 'INFRA')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-03', '日活用户', 14100, '人', 'PRODUCT')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-03', 'API调用量', 3450000, '次/日', 'TECH')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-03', '查询延迟P99', 6.2, 'ms', 'DBCORE')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-03', '数据总量', 129.8, 'TB', 'INFRA')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-04', '日活用户', 15800, '人', 'PRODUCT')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-04', 'API调用量', 3800000, '次/日', 'TECH')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-04', '查询延迟P99', 5.9, 'ms', 'DBCORE')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-05', '日活用户', 16500, '人', 'PRODUCT')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-05', 'API调用量', 4200000, '次/日', 'TECH')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-05', '查询延迟P99', 5.5, 'ms', 'DBCORE')",
    # 告警
    "CREATE VERTEX TABLE Alert (time STRING, level STRING, source STRING, message STRING, resolved INT)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-09 08:15:00', 'WARNING', 'N3', 'CPU使用率超过80%', 1)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-09 10:30:00', 'INFO', 'N1', '定时备份完成', 1)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-09 14:22:00', 'CRITICAL', 'N5', '节点进入维护模式', 0)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-09 16:45:00', 'WARNING', 'N2', '磁盘使用率超过75%', 0)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-09 18:00:00', 'INFO', 'B1', '增量备份完成 2.3GB', 1)",
]

print(f"Seeding {len(stmts)} statements...")
ok = 0
fail = 0
for i, sql in enumerate(stmts):
    print(f"[{i+1}/{len(stmts)}] {sql[:70]}...")
    if execute(sql):
        ok += 1
    else:
        fail += 1

print(f"\nDone: {ok} OK, {fail} failed")

# Verify
print("\n--- Verification ---")
for cls in ['Department', 'Employee', 'Project', 'Server', 'BizMetrics', 'Alert']:
    execute(f"SELECT * FROM {cls}", delay=0.3)
