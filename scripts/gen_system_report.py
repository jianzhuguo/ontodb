from docx import Document
from docx.shared import Pt, Cm, RGBColor
from docx.enum.text import WD_ALIGN_PARAGRAPH
from docx.enum.table import WD_TABLE_ALIGNMENT

doc = Document()

section = doc.sections[0]
section.page_width, section.page_height = Cm(21.0), Cm(29.7)
section.top_margin = section.bottom_margin = Cm(2.54)
section.left_margin = section.right_margin = Cm(3.18)

body = doc.styles["Normal"]
body.font.name = "Calibri"
body.font.size = Pt(11)
body.paragraph_format.line_spacing = 1.15
body.paragraph_format.space_after = Pt(6)

for n, size in [(1, 18), (2, 14), (3, 12)]:
    s = doc.styles[f"Heading {n}"]
    s.font.name = "Calibri Light"
    s.font.size = Pt(size)
    s.font.bold = True
    s.font.color.rgb = RGBColor(0x1F, 0x3A, 0x5F)

# Cover
for _ in range(6):
    doc.add_paragraph()
p = doc.add_paragraph("OntoDB 系统全景", style="Title")
p.alignment = WD_ALIGN_PARAGRAPH.CENTER
p = doc.add_paragraph("与 OntoQL 标准规划", style="Subtitle")
p.alignment = WD_ALIGN_PARAGRAPH.CENTER
for _ in range(6):
    doc.add_paragraph()
p = doc.add_paragraph("v1.0 · OntoDB 技术团队 · 2026年8月")
p.alignment = WD_ALIGN_PARAGRAPH.CENTER
doc.add_page_break()

# TOC
doc.add_heading("目录", level=1)
for item in ["一、OntoDB 系统全景", "二、全球竞争格局", "三、应用场景", "四、OntoQL 标准规划", "五、当前状态与下一步", "六、总结"]:
    doc.add_paragraph(item)
doc.add_page_break()

# 一
doc.add_heading("一、OntoDB 系统全景", level=1)
doc.add_heading("1.1 产品定位", level=2)
doc.add_paragraph("全球首个将 OWL 推理引擎嵌入数据库内核的六模态统一语义数据库。核心价值：语义原生（数据插入即生成语义）、六模态统一（关系+图+向量+时序+空间+本体）、白盒推理（OWL 规则推理，推导链可追溯）。")

doc.add_heading("1.2 核心模块", level=2)
t = doc.add_table(rows=9, cols=3)
t.style = "Light Grid Accent 1"
for i, h in enumerate(["模块", "代码行数", "功能"]):
    t.rows[0].cells[i].text = h
    for p in t.rows[0].cells[i].paragraphs:
        for r in p.runs: r.bold = True
for r, row in enumerate([
    ["onto-storage", "13,245", "LSM-Tree 存储引擎"],
    ["onto-query", "17,537", "SQL + SPARQL + 优化器"],
    ["onto-graph", "2,172", "图存储 + BFS/DFS"],
    ["onto-ontology", "3,781", "OWL 推理引擎"],
    ["onto-server", "8,095", "HTTP/PG/MySQL 服务器"],
    ["onto-enterprise", "4,265", "RBAC + 加密 + 备份"],
    ["onto-raft", "3,035", "Raft 共识层"],
    ["onto-cli", "467", "命令行工具"],
], 1):
    for c, val in enumerate(row):
        t.rows[r].cells[c].text = val

doc.add_heading("1.3 核心创新", level=2)
doc.add_paragraph("创新一：数据插入即生成语义（全球唯一）——INSERT 时自动执行 7 步联动：文档写入、图顶点创建、rdf:type 三元组生成、属性三元组生成、OWL 推理推导隐含类型、向量索引、B+Tree 索引更新。")
doc.add_paragraph("创新二：OWL 推理引擎（白盒可追溯）——7 条 OWL 2 RL 推理规则，增量不动点算法，推导链可追溯。")
doc.add_paragraph("创新三：六模态统一查询——一条 SQL 可以跨越关系、图、向量、时序、空间、本体六种模态。")

doc.add_heading("1.4 性能数据", level=2)
t = doc.add_table(rows=8, cols=2)
t.style = "Light Grid Accent 1"
for i, h in enumerate(["指标", "数值"]):
    t.rows[0].cells[i].text = h
    for p in t.rows[0].cells[i].paragraphs:
        for r in p.runs: r.bold = True
for r, row in enumerate([
    ["单线程写入", "870K ops/s"],
    ["单线程读取", "1.2M ops/s"],
    ["批量写入", "1.1M ops/s"],
    ["向量召回率", "100% (ef_search=200)"],
    ["8线程加速", "2.04x"],
    ["GIS 空间关系", "≤8µs"],
    ["TSM 压缩率", "60%+"],
], 1):
    for c, val in enumerate(row):
        t.rows[r].cells[c].text = val

doc.add_heading("1.5 代码质量", level=2)
doc.add_paragraph("零 unsafe 代码块。生产代码仅 3 处 unwrap（均有安全守卫）。Fuzz 测试 10/10 稳定。588/588 测试通过。")

# 二
doc.add_heading("二、全球竞争格局", level=1)
doc.add_heading("2.1 全球唯一性", level=2)
t = doc.add_table(rows=6, cols=2)
t.style = "Light Grid Accent 1"
for i, h in enumerate(["能力", "全球竞品"]):
    t.rows[0].cells[i].text = h
    for p in t.rows[0].cells[i].paragraphs:
        for r in p.runs: r.bold = True
for r, row in enumerate([
    ["六模态统一存储", "无（全球唯一）"],
    ["数据插入即生成语义", "无（全球唯一）"],
    ["OWL 推理引擎内嵌", "Stardog（外部）"],
    ["零 unsafe Rust 数据库", "无（全球唯一）"],
    ["SQL+SPARQL+图 统一查询", "无（全球唯一）"],
], 1):
    for c, val in enumerate(row):
        t.rows[r].cells[c].text = val

# 三
doc.add_heading("三、应用场景", level=1)
doc.add_heading("3.1 九大行业场景", level=2)
t = doc.add_table(rows=10, cols=3)
t.style = "Light Grid Accent 1"
for i, h in enumerate(["行业", "核心需求", "OntoDB 优势"]):
    t.rows[0].cells[i].text = h
    for p in t.rows[0].cells[i].paragraphs:
        for r in p.runs: r.bold = True
for r, row in enumerate([
    ["金融风控", "实时分析 + 合规", "六模态 + OWL + 等保2.0"],
    ["政务数据治理", "数据血缘 + 安全", "本体推理 + RBAC"],
    ["医疗知识图谱", "知识推理 + 检索", "OWL + 向量相似"],
    ["智能制造", "设备监控 + 预测", "时序 + 空间 + 异常检测"],
    ["算法研究", "实验管理 + 检索", "六模态 + 向量搜索"],
    ["具身智能", "传感器 + 空间", "时序 + 空间 + 图"],
    ["自动驾驶", "点云 + 轨迹", "空间 + 时序 + 推理"],
    ["航空航天", "飞行数据 + 合规", "时序 + 空间 + 审计"],
    ["工业 AI", "设备 + 质量", "时序 + 异常检测"],
], 1):
    for c, val in enumerate(row):
        t.rows[r].cells[c].text = val

doc.add_heading("3.2 数字军师", level=2)
doc.add_paragraph("企业决策智能系统。核心功能：决策记录与回溯、决策者人格画像、人格沙盒模拟、决策审计链、决策知识图谱。技术支撑：六模态存储决策数据、OWL 推理建立决策知识图谱、向量搜索匹配相似历史决策。")

# 四
doc.add_heading("四、OntoQL 标准规划", level=1)
doc.add_heading("4.1 背景与动机", level=2)
doc.add_paragraph("现有查询语言无法表达 OntoDB 的六模态能力。SQL 设计用于关系型数据，SPARQL 设计用于 RDF 三元组，Cypher 仅用于图数据库。需要设计 OntoQL 作为 OntoDB 的原生查询语言。")

doc.add_heading("4.2 设计原则", level=2)
doc.add_paragraph("SQL 兼容（降低学习成本）、语义原生（原生支持语义查询和推理）、六模态统一（一套语法覆盖所有模态）、可扩展（支持未来新增模态）、标准化（可提交 ISO/IEC 或 W3C）。")

doc.add_heading("4.3 语法示例", level=2)
doc.add_paragraph("命名空间：CREATE SCHEMA twin; SELECT * FROM twin.Department;")
doc.add_paragraph("图遍历：TRAVERSE FROM Decision::D001 OUT influences DEPTH 3;")
doc.add_paragraph("向量搜索：SEARCH documents.embedding NEAR [0.1, 0.2, 0.3] TOP 10;")
doc.add_paragraph("推理查询：INFER subClassOf FROM Animal WHERE name = '旺财';")
doc.add_paragraph("时空查询：SELECT * FROM Sensor WHERE location NEAR (31.3, 120.6) RADIUS 1km;")

doc.add_heading("4.4 实施路线图", level=2)
t = doc.add_table(rows=6, cols=3)
t.style = "Light Grid Accent 1"
for i, h in enumerate(["阶段", "内容", "时间"]):
    t.rows[0].cells[i].text = h
    for p in t.rows[0].cells[i].paragraphs:
        for r in p.runs: r.bold = True
for r, row in enumerate([
    ["V1", "Schema 命名空间", "2 周"],
    ["V2", "语法扩展（TRAVERSE/SEARCH/INFER）", "4 周"],
    ["V3", "独立解析器", "8 周"],
    ["V4", "标准文档", "4 周"],
    ["V5", "测试套件", "4 周"],
], 1):
    for c, val in enumerate(row):
        t.rows[r].cells[c].text = val

doc.add_heading("4.5 标准化路径", level=2)
doc.add_paragraph("内部验证 → 论文发表 → 开源实现 → 标准提案 → 行业推广。已注册 OntoQL.org 域名。")

# 五
doc.add_heading("五、当前状态与下一步", level=1)
doc.add_heading("5.1 当前状态", level=2)
t = doc.add_table(rows=8, cols=2)
t.style = "Light Grid Accent 1"
for i, h in enumerate(["维度", "得分"]):
    t.rows[0].cells[i].text = h
    for p in t.rows[0].cells[i].paragraphs:
        for r in p.runs: r.bold = True
for r, row in enumerate([
    ["核心引擎", "92/100"],
    ["企业安全", "95/100"],
    ["集群能力", "85/100"],
    ["运维可观测", "95/100"],
    ["文档 SDK", "85/100"],
    ["前端产品", "85/100"],
    ["综合", "92/100"],
], 1):
    for c, val in enumerate(row):
        t.rows[r].cells[c].text = val

doc.add_heading("5.2 下一步", level=2)
doc.add_paragraph("P0：OntoDB 产品打磨 + 种子用户试用。P1：OntoQL 调研规划。P2：OntoQL 标准化（产品稳定后）。")

# 六
doc.add_heading("六、总结", level=1)
doc.add_paragraph('OntoDB 是全球首个将 OWL 推理引擎嵌入数据库内核的六模态统一语义数据库，核心创新是"数据插入即生成语义"。在技术独特性和性能上处于全球领先水平。')
doc.add_paragraph('OntoQL 是规划中的六模态统一查询语言标准，目标是成为全球首个语义原生查询语言标准。已注册 OntoQL.org 域名，产品稳定后正式启动标准化流程。')

doc.save(r"C:\Users\GuoJZ\Desktop\OntoDB_系统全景与OntoQL规划.docx")
print("Done")
