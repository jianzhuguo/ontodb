import urllib.request, json, time

API = 'http://127.0.0.1:7912/api/query'

def execute(sql):
    time.sleep(0.05)
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"):
                print(f"  ERR: {result['error'][:100]}")
                return False
            return True
    except Exception as e:
        print(f"  FAIL: {e}")
        return False

stmts = [
    # ── 海外事业部 ──
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('海外事业部', 'OVERSEAS', 'David Chen', 22000000, 2, 'HQ')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('东南亚区', 'OVERSEA_SEA', 'Nguyen Minh', 8000000, 3, 'OVERSEAS')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('中东区', 'OVERSEA_ME', 'Ahmed Ali', 6000000, 3, 'OVERSEAS')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('欧洲区', 'OVERSEA_EU', 'Hans Mueller', 5000000, 3, 'OVERSEAS')",
    "INSERT INTO Department (name, code, head, budget, level, parent) VALUES ('北美区', 'OVERSEA_NA', 'James Wilson', 3000000, 3, 'OVERSEAS')",

    # ── 海外员工 ──
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('David Chen', 'EMP029', 'OVERSEAS', '海外VP', 72000, '2021-01-10', 'davidc@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('Nguyen Minh', 'EMP030', 'OVERSEA_SEA', '东南亚总监', 50000, '2022-03-15', 'nguyen@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('Ahmed Ali', 'EMP031', 'OVERSEA_ME', '中东总监', 52000, '2022-06-01', 'ahmed@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('Hans Mueller', 'EMP032', 'OVERSEA_EU', '欧洲总监', 55000, '2023-01-15', 'hans@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('James Wilson', 'EMP033', 'OVERSEA_NA', '北美总监', 48000, '2023-04-01', 'james@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('Tran Van', 'EMP034', 'OVERSEA_SEA', '客户经理', 30000, '2023-07-01', 'tranv@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('Omar Hassan', 'EMP035', 'OVERSEA_ME', '客户经理', 32000, '2023-08-15', 'omar@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('Sophie Laurent', 'EMP036', 'OVERSEA_EU', '解决方案架构师', 45000, '2023-06-01', 'sophie@ontodb.ai')",

    # ── 海外项目 ──
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('新加坡智慧国项目', 'PRJ013', 'OVERSEA_SEA', '进行中', 3500000, '2026-03-01', 60)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('沙特NEOM数据平台', 'PRJ014', 'OVERSEA_ME', '进行中', 5000000, '2026-01-15', 45)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('欧盟GDPR合规引擎', 'PRJ015', 'OVERSEA_EU', '规划中', 2800000, '2026-10-01', 8)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('越南银行核心系统', 'PRJ016', 'OVERSEA_SEA', '进行中', 2200000, '2026-05-01', 35)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('迪拜智慧城市IoT', 'PRJ017', 'OVERSEA_ME', '已完成', 4000000, '2025-03-01', 100)",

    # ── 海外客户 ──
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('Singapore GovTech', '政务', '东南亚', 'S', 4200000, '已签约', 'Dr. Tan')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('NEOM Tech', '科技', '中东', 'S', 8500000, '已签约', 'Khalid')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('Deutsche Telekom', '通信', '欧洲', 'A', 3800000, '洽谈中', 'Friedrich')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('Vietcombank', '金融', '东南亚', 'A', 2600000, '已签约', 'Nguyen T.')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('Dubai Police', '政务', '中东', 'S', 5200000, '已签约', 'Major Saeed')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('BNP Paribas', '金融', '欧洲', 'A', 4500000, '洽谈中', 'Pierre')",

    # ── 海外线索 ──
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('Tokyo Metro', '交通', '展会', '需求确认', 3500000, 'EMP030', '2026-08-01')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('Saudi Aramco', '能源', '渠道', '方案演示', 12000000, 'EMP031', '2026-07-20')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('ING Bank', '金融', '转介绍', '初步接触', 2800000, 'EMP032', '2026-08-05')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('Grab Holdings', '互联网', '官网', '需求确认', 1800000, 'EMP030', '2026-08-08')",

    # ── 海外业务指标 ──
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '海外营收', 1850000, '美元/月', 'OVERSEAS')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '海外客户数', 12, '家', 'OVERSEAS')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '海外员工', 8, '人', 'OVERSEAS')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-09', '海外项目', 5, '个', 'OVERSEAS')",

    # ── 海外服务器 ──
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-sg-1', 'SG1', 'follower', '亚太-新加坡', 32, 128, 2.0, 'online', '10.1.1.1')",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-dubai-1', 'DXB1', 'follower', '中东-迪拜', 32, 128, 2.0, 'online', '10.2.1.1')",
    "INSERT INTO Server (name, node_id, role, region, cpu_cores, memory_gb, storage_tb, status, ip) VALUES ('ontodb-frankfurt-1', 'FRA1', 'follower', '欧洲-法兰克福', 16, 64, 1.0, 'online', '10.3.1.1')",

    # ── 更多告警 ──
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-10 02:30:00', 'INFO', 'SG1', '新加坡节点同步完成', 1)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-10 05:15:00', 'WARNING', 'DXB1', '中东节点存储使用率 72%', 0)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-10 09:00:00', 'INFO', 'FRA1', '欧洲节点上线', 1)",
]

ok = fail = 0
for i, sql in enumerate(stmts):
    if execute(sql): ok += 1
    else: fail += 1
    if (i+1) % 10 == 0: print(f"  [{i+1}/{len(stmts)}] processed...")

print(f"\n=== {ok} OK, {fail} failed ===")

# Verify
print("\n--- 最新数据统计 ---")
for cls in ['Department','Employee','Project','Server','Customer','Lead','BizMetrics','Alert']:
    def q(sql):
        d = json.dumps({'query': sql}).encode()
        r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(r, timeout=10) as resp:
            return len(json.loads(resp.read()).get('data', []))
    print(f"  {cls:15s}: {q('SELECT * FROM '+cls)} rows")
