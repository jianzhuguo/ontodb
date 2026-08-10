import urllib.request
import json
import time

API = 'http://127.0.0.1:7912/api/query'

def execute(sql):
    time.sleep(0.05)
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"):
                print(f"  ERR: {result['error'][:120]}")
                return False
            print(f"  OK")
            return True
    except urllib.error.HTTPError as e:
        body = e.read().decode('utf-8', errors='replace')
        print(f"  HTTP {e.code}: {body[:120]}")
        return False

stmts = [
    # ── 销售部门组织架构 ──
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('销售中心', 'SALES', '杨光', 18000000, 2, 'HQ')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('华东大区', 'SALES_EAST', '朱磊', 6000000, 3, 'SALES')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('华北大区', 'SALES_NORTH', '胡婷', 5000000, 3, 'SALES')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('华南大区', 'SALES_SOUTH', '谢斌', 4000000, 3, 'SALES')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('大客户部', 'SALES_KA', '韩冰', 3000000, 3, 'SALES')",

    # ── 销售员工 ──
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('杨光', 'EMP016', 'SALES', '销售VP', 68000, '2020-05-01', 'yangg@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('朱磊', 'EMP017', 'SALES_EAST', '华东区总监', 45000, '2021-02-15', 'zhul@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('胡婷', 'EMP018', 'SALES_NORTH', '华北区总监', 43000, '2021-04-10', 'hut@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('谢斌', 'EMP019', 'SALES_SOUTH', '华南区总监', 42000, '2021-06-20', 'xieb@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('韩冰', 'EMP020', 'SALES_KA', '大客户总监', 48000, '2020-11-01', 'hanb@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('唐杰', 'EMP021', 'SALES_EAST', '高级客户经理', 35000, '2022-03-01', 'tangj@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('宋丽', 'EMP022', 'SALES_EAST', '客户经理', 28000, '2023-01-15', 'songl@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('邓超', 'EMP023', 'SALES_NORTH', '高级客户经理', 34000, '2022-05-10', 'dengc@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('曹颖', 'EMP024', 'SALES_NORTH', '客户经理', 27000, '2023-04-01', 'caoy@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('彭飞', 'EMP025', 'SALES_SOUTH', '高级客户经理', 33000, '2022-07-20', 'pengf@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('曾瑶', 'EMP026', 'SALES_SOUTH', '客户经理', 26000, '2023-06-10', 'zengy@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('冯刚', 'EMP027', 'SALES_KA', '大客户经理', 38000, '2021-09-15', 'fengg@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('蒋敏', 'EMP028', 'SALES_KA', '大客户经理', 36000, '2022-01-10', 'jiangm@ontodb.ai')",

    # ── 销售项目 ──
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('政务客户拓展Q3', 'PRJ009', 'SALES_EAST', '进行中', 800000, '2026-07-01', 55)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('金融行业攻坚', 'PRJ010', 'SALES_KA', '进行中', 1200000, '2026-04-01', 70)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('华南渠道建设', 'PRJ011', 'SALES_SOUTH', '进行中', 600000, '2026-06-01', 40)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('华北运营商合作', 'PRJ012', 'SALES_NORTH', '规划中', 900000, '2026-09-01', 10)",

    # ── 客户表 ──
    "CREATE VERTEX TABLE Customer (name STRING, industry STRING, region STRING, level STRING, contract_value DOUBLE, status STRING, contact STRING)",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('上海政务云中心', '政务', '华东', 'A', 2800000, '已签约', '张主任')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('中国银行数据中心', '金融', '华北', 'S', 5200000, '已签约', '李处长')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('深圳智慧交通', '交通', '华南', 'A', 1800000, '洽谈中', '王总')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('浙江省公安厅', '政务', '华东', 'S', 4500000, '已签约', '陈处长')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('平安科技', '金融', '华南', 'A', 3200000, '洽谈中', '刘总监')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('北京地铁集团', '交通', '华北', 'B', 1200000, '已签约', '赵经理')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('广州政务大数据局', '政务', '华南', 'A', 2100000, '洽谈中', '黄局长')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('中国人寿', '金融', '华北', 'S', 6800000, '已签约', '周总')",

    # ── 销售线索表 ──
    "CREATE VERTEX TABLE Lead (company STRING, industry STRING, source STRING, stage STRING, value DOUBLE, owner STRING, created STRING)",
    "INSERT INTO Lead (company, industry, source, stage, value DOUBLE, owner, created) VALUES ('腾讯云', '互联网', '官网', '需求确认', 3000000, 'EMP020', '2026-07-15')",
    "INSERT INTO Lead (company, industry, source, stage, value DOUBLE, owner, created) VALUES ('华为政务', '政务', '展会', '方案演示', 4000000, 'EMP017', '2026-07-20')",
    "INSERT INTO Lead (company, industry, source, stage, value DOUBLE, owner, created) VALUES ('建设银行', '金融', '转介绍', '商务谈判', 5500000, 'EMP020', '2026-06-10')",
    "INSERT INTO Lead (company, industry, source, stage, value DOUBLE, owner, created) VALUES ('成都高新区', '政务', '渠道', '初步接触', 1500000, 'EMP019', '2026-08-01')",
    "INSERT INTO Lead (company, industry, source, stage, value DOUBLE, owner, created) VALUES ('字节跳动', '互联网', '官网', '需求确认', 2800000, 'EMP018', '2026-08-05')",

    # ── 更多业务指标 ──
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '新增线索', 5, '条', 'SALES')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '签约金额', 2800000, '元', 'SALES')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '客户拜访', 12, '次', 'SALES')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '线索转化率', 28.5, '%', 'SALES')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '日活用户', 18200, '人', 'PRODUCT')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', 'API调用量', 4800000, '次/日', 'TECH')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '查询延迟P99', 5.2, 'ms', 'DBCORE')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '数据总量', 131.5, 'TB', 'INFRA')",

    # ── 更多告警 ──
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-09 20:15:00', 'INFO', 'N1', '集群心跳正常', 1)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-09 22:00:00', 'WARNING', 'N4', '华南节点延迟升高 >15ms', 0)",
]

ok = 0
fail = 0
for i, sql in enumerate(stmts):
    tag = sql.split(' ')[0]
    short = sql[:80].replace("INSERT INTO ", "").replace("VALUES (", "").replace("CREATE VERTEX TABLE ", "CREATE ")
    print(f"[{i+1}/{len(stmts)}] {short}...")
    if execute(sql):
        ok += 1
    else:
        fail += 1

print(f"\n=== {ok} OK, {fail} failed ===")

# Verify
print("\n--- 验证 ---")
for cls in ['Department', 'Employee', 'Project', 'Customer', 'Lead', 'BizMetrics', 'Alert']:
    execute(f"SELECT count(*) as cnt FROM {cls}")
