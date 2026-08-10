import urllib.request, json, time

API = 'http://127.0.0.1:7912/api/query'

def execute(sql):
    time.sleep(0.03)
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"): return False
            return True
    except: return False

stmts = [
    # ── 更多员工（各部门补充） ──
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('陈磊', 'EMP037', 'DBCORE', '高级数据库工程师', 48000, '2022-09-01', 'chenl@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('杨帆', 'EMP038', 'DBCORE', '存储引擎工程师', 46000, '2023-03-15', 'yangf@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('刘洋', 'EMP039', 'INFRA', '云原生架构师', 52000, '2022-01-10', 'liuy@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('赵敏', 'EMP040', 'INFRA', 'SRE工程师', 43000, '2023-05-20', 'zhaom@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('孙悦', 'EMP041', 'FE', 'UI设计师', 36000, '2023-08-01', 'suny@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('周涵', 'EMP042', 'AI', 'NLP工程师', 50000, '2022-11-15', 'zhouh@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('吴桐', 'EMP043', 'AI', '机器学习工程师', 48000, '2023-02-01', 'wut@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('郑浩', 'EMP044', 'PRODUCT', '高级产品经理', 44000, '2022-06-10', 'zhengh@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('钱进', 'EMP045', 'MARKET', '品牌总监', 42000, '2022-08-20', 'qianj@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('许诺', 'EMP046', 'MARKET', '内容运营', 30000, '2024-01-10', 'xun@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('韩梅', 'EMP047', 'HR', '招聘经理', 35000, '2022-04-15', 'hanm@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('冯琳', 'EMP048', 'HR', '培训专员', 28000, '2023-09-01', 'fengl@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('蔡文', 'EMP049', 'FIN', '财务分析师', 32000, '2023-04-01', 'caiw@ontodb.ai')",
    "INSERT INTO Employee (name, emp_id, department, title, salary, hire_date, email) VALUES ('邓辉', 'EMP050', 'FIN', '审计专员', 30000, '2023-07-15', 'dengh@ontodb.ai')",

    # ── 更多项目 ──
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('OntoDB Cloud托管版', 'PRJ018', 'INFRA', '进行中', 4500000, '2026-04-01', 30)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('智能SQL助手', 'PRJ019', 'AI', '进行中', 2800000, '2026-06-01', 25)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('数据脱敏引擎', 'PRJ020', 'DBCORE', '进行中', 1800000, '2026-07-01', 20)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('移动端管理APP', 'PRJ021', 'FE', '规划中', 1200000, '2026-10-01', 5)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('品牌升级计划', 'PRJ022', 'MARKET', '进行中', 800000, '2026-05-01', 60)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('校园招聘季', 'PRJ023', 'HR', '进行中', 500000, '2026-07-15', 45)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('年度审计', 'PRJ024', 'FIN', '已完成', 300000, '2026-01-01', 100)",
    "INSERT INTO Project (name, code, department, status, budget, start_date, progress) VALUES ('东南亚渠道拓展', 'PRJ025', 'OVERSEA_SEA', '进行中', 1500000, '2026-06-01', 35)",

    # ── 更多客户 ──
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('阿里云', '互联网', '华东', 'S', 6500000, '洽谈中', '李总')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('国家电网', '能源', '华北', 'A', 3800000, '已签约', '王处长')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('深圳地铁', '交通', '华南', 'A', 2200000, '已签约', '张经理')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('浙江大学', '教育', '华东', 'B', 800000, '已签约', '陈教授')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('Toyota Connected', '汽车', '亚太', 'A', 4200000, '洽谈中', 'Tanaka')",
    "INSERT INTO Customer (name, industry, region, level, contract_value, status, contact) VALUES ('Emirates NBD', '金融', '中东', 'A', 5800000, '已签约', 'Ahmed')",

    # ── 更多线索 ──
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('中国移动', '通信', '展会', '方案演示', 8000000, 'EMP020', '2026-08-02')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('Shopee', '互联网', '渠道', '需求确认', 2500000, 'EMP030', '2026-08-06')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('西门子中国', '制造', '转介绍', '初步接触', 3200000, 'EMP017', '2026-08-08')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('Standard Chartered', '金融', '官网', '商务谈判', 7000000, 'EMP031', '2026-07-25')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('小米集团', '科技', '展会', '方案演示', 4500000, 'EMP020', '2026-08-03')",
    "INSERT INTO Lead (company, industry, source, stage, value, owner, created) VALUES ('Samsung SDS', '科技', '渠道', '需求确认', 5500000, 'EMP030', '2026-08-07')",

    # ── 更多告警 ──
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-10 10:00:00', 'INFO', 'N1', '数据压缩完成，节省 12% 空间', 1)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-10 11:30:00', 'WARNING', 'N3', '北京节点 QPS 突增 >2000', 0)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-10 12:15:00', 'INFO', 'SG1', '新加坡节点备份完成', 1)",
    "INSERT INTO Alert (time, level, source, message, resolved) VALUES ('2026-08-10 13:00:00', 'CRITICAL', 'FRA1', '欧洲节点网络抖动 >50ms', 0)",

    # ── 更多业务指标 ──
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-10', '日活用户', 19500, '人', 'PRODUCT')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-10', 'API调用量', 5200000, '次/日', 'TECH')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-10', '海外营收', 2100000, '美元/月', 'OVERSEAS')",
    "INSERT INTO BizMetrics (date, metric, value, unit, department) VALUES ('2026-08-10', '新增签约', 3, '家', 'SALES')",
]

ok = fail = 0
for sql in stmts:
    if execute(sql): ok += 1
    else: fail += 1
print(f"Done: {ok} OK, {fail} failed")
