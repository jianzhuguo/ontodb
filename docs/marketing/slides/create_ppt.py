#!/usr/bin/env python3
"""OntoDB 产品介绍PPT生成脚本"""

from pptx import Presentation
from pptx.util import Inches, Pt, Emu
from pptx.enum.shapes import MSO_SHAPE
from pptx.dml.color import RGBColor
from pptx.enum.text import PP_ALIGN, MSO_AUTO_SIZE

# 颜色定义
DARK_BG = RGBColor(0x1F, 0x1F, 0x1F)  # 深色背景
WHITE = RGBColor(0xFF, 0xFF, 0xFF)
BLUE = RGBColor(0x00, 0x7A, 0xD9)  # 主色调-蓝
LIGHT_BLUE = RGBColor(0x00, 0xA8, 0xE8)
GREEN = RGBColor(0x00, 0xB8, 0x94)  # 成功/提升
GRAY = RGBColor(0x7A, 0x7A, 0x7A)

# 创建演示文稿
prs = Presentation()
prs.slide_width = Inches(13.333)  # 16:9
prs.slide_height = Inches(7.5)


def add_bg(slide, color=DARK_BG):
    """添加深色背景"""
    bg = slide.background
    fill = bg.fill
    fill.solid()
    fill.fore_color.rgb = color


def add_text_box(slide, left, top, width, height, text, font_size=18,
                 color=WHITE, bold=False, alignment=PP_ALIGN.LEFT, font_name="Microsoft YaHei"):
    """添加文本框"""
    tb = slide.shapes.add_textbox(Inches(left), Inches(top), Inches(width), Inches(height))
    tf = tb.text_frame
    tf.word_wrap = True
    p = tf.paragraphs[0]
    p.text = text
    p.alignment = alignment
    run = p.runs[0]
    run.font.size = Pt(font_size)
    run.font.color.rgb = color
    run.font.bold = bold
    run.font.name = font_name
    # 设置东亚字体
    from pptx.oxml.ns import qn
    rPr = run._r.get_or_add_rPr()
    rPr.attrib[qn('w:eastAsia')] = font_name if font_name else 'Microsoft YaHei'
    return tb


def add_multi_text(slide, left, top, width, height, lines, font_size=18,
                   color=WHITE, line_spacing=1.5, font_name="Microsoft YaHei"):
    """添加多行文本"""
    tb = slide.shapes.add_textbox(Inches(left), Inches(top), Inches(width), Inches(height))
    tf = tb.text_frame
    tf.word_wrap = True
    for i, line in enumerate(lines):
        if i == 0:
            p = tf.paragraphs[0]
        else:
            p = tf.add_paragraph()
        p.text = line
        p.space_after = Pt(4)
        for run in p.runs:
            run.font.size = Pt(font_size)
            run.font.color.rgb = color
            run.font.name = font_name
    return tb


def add_stat_box(slide, left, top, number, label, number_color=GREEN):
    """添加数据统计框"""
    # 数字
    add_text_box(slide, left, top, 3, 1.2, number, font_size=48,
                 color=number_color, bold=True, alignment=PP_ALIGN.CENTER)
    # 标签
    add_text_box(slide, left, top + 1, 3, 0.6, label, font_size=16,
                 color=GRAY, alignment=PP_ALIGN.CENTER)


# ============================================================
# 幻灯片1：封面
# ============================================================
slide1 = prs.slides.add_slide(prs.slide_layouts[6])  # blank
add_bg(slide1)

# Logo区域
add_text_box(slide1, 4, 1.5, 5.3, 1.2, "OntoDB", font_size=60,
             color=BLUE, bold=True, alignment=PP_ALIGN.CENTER)

# 副标题
add_text_box(slide1, 2, 2.8, 9.3, 0.8, "本体内核驱动的多模态语义数据融合引擎",
             font_size=28, color=WHITE, alignment=PP_ALIGN.CENTER)

# 核心卖点
add_text_box(slide1, 2, 4, 9.3, 0.6, "全球首个将本体推理内嵌数据库内核的产品",
             font_size=20, color=LIGHT_BLUE, alignment=PP_ALIGN.CENTER)
add_text_box(slide1, 2, 4.6, 9.3, 0.6, "一个数据库替代七个系统",
             font_size=20, color=GREEN, alignment=PP_ALIGN.CENTER)

# 联系信息
add_text_box(slide1, 3, 6, 7.3, 0.5, "ontovalue.com",
             font_size=16, color=GRAY, alignment=PP_ALIGN.CENTER)

# ============================================================
# 幻灯片2：行业痛点
# ============================================================
slide2 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide2)

add_text_box(slide2, 0.5, 0.3, 12, 0.8, "传统方案的困境",
             font_size=36, color=WHITE, bold=True)

# 痛点1
add_text_box(slide2, 0.5, 1.3, 4, 0.5, "数据孤岛", font_size=24, color=BLUE, bold=True)
add_multi_text(slide2, 0.5, 1.9, 4, 2.5, [
    "MySQL + Neo4j + Milvus",
    "InfluxDB + PostGIS",
    "Apache Jena + Redis",
    "",
    "7个独立系统，数据分散"
], font_size=16, color=WHITE)

# 痛点2
add_text_box(slide2, 4.8, 1.3, 4, 0.5, "性能瓶颈", font_size=24, color=BLUE, bold=True)
add_multi_text(slide2, 4.8, 1.9, 4, 2.5, [
    "推理延迟：>100ms",
    "跨模态查询：秒级-分钟级",
    "数据同步：小时级",
    "",
    "无法满足实时需求"
], font_size=16, color=WHITE)

# 痛点3
add_text_box(slide2, 9.1, 1.3, 4, 0.5, "成本高昂", font_size=24, color=BLUE, bold=True)
add_multi_text(slide2, 9.1, 1.9, 4, 2.5, [
    "部署复杂度：7倍",
    "运维成本：7倍",
    "学习成本：7种技术栈",
    "",
    "资源浪费严重"
], font_size=16, color=WHITE)

# 问题
add_text_box(slide2, 2, 5, 9.3, 0.8, "有没有一个数据库，能替代这七个系统？",
             font_size=28, color=GREEN, bold=True, alignment=PP_ALIGN.CENTER)

# ============================================================
# 幻灯片3：解决方案
# ============================================================
slide3 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide3)

add_text_box(slide3, 0.5, 0.3, 12, 0.8, "OntoDB 的解决方案",
             font_size=36, color=WHITE, bold=True)

add_text_box(slide3, 0.5, 1.2, 12, 0.6, "一个数据库替代七个系统",
             font_size=28, color=GREEN, bold=True, alignment=PP_ALIGN.CENTER)

# 六种数据模态
modalities = [
    ("结构化记录", "MySQL"),
    ("图结构", "Neo4j"),
    ("向量索引", "Milvus"),
    ("时序数据", "InfluxDB"),
    ("空间数据", "PostGIS"),
    ("语义三元组", "Apache Jena"),
]

for i, (name, replace) in enumerate(modalities):
    col = i % 3
    row = i // 3
    x = 0.8 + col * 4.2
    y = 2.2 + row * 1.6

    # 模态名称
    add_text_box(slide3, x, y, 3.5, 0.5, f"  {name}", font_size=20, color=WHITE, bold=True)
    # 替代产品
    add_text_box(slide3, x, y + 0.5, 3.5, 0.4, f"替代 {replace}", font_size=14, color=GRAY)

# 核心创新
add_text_box(slide3, 1, 5.8, 11.3, 0.5, "核心创新：本体内核化架构 + 零映射表关联 + 写入即语义就绪",
             font_size=20, color=LIGHT_BLUE, alignment=PP_ALIGN.CENTER)

# ============================================================
# 幻灯片4：技术架构
# ============================================================
slide4 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide4)

add_text_box(slide4, 0.5, 0.3, 12, 0.8, "八层架构",
             font_size=36, color=WHITE, bold=True)

# 八层架构
layers = [
    ("扩展能力层", "数据分片 | 水平扩展 | 多协议接入 | API网关"),
    ("命名空间层", "多租户隔离 | 本体层隔离 | 数据层隔离"),
    ("活数据管理层", "价值衰减 | 自动激活 | 实时计算 | 优先级排序"),
    ("查询融合层", "统一查询语言 | 多模态融合 | 查询优化"),
    ("写入联动层", "七步联动管线 | 三阶段锁 | 原子执行"),
    ("统一标识层", "语义化标识 | 零映射表关联 | 跨模态锚点"),
    ("语义引擎层", "本体定义 | 推理引擎 | 增量推理"),
    ("存储引擎层", "LSM-Tree | 六模态统一 | 键前缀路由"),
]

for i, (name, desc) in enumerate(layers):
    y = 1.2 + i * 0.72
    # 层名称
    add_text_box(slide4, 0.8, y, 3, 0.5, name, font_size=18, color=BLUE, bold=True)
    # 层描述
    add_text_box(slide4, 4, y, 8.5, 0.5, desc, font_size=14, color=WHITE)

# ============================================================
# 幻灯片5：核心创新1 - 本体内核化
# ============================================================
slide5 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide5)

add_text_box(slide5, 0.5, 0.3, 12, 0.8, "核心创新1：本体内核化架构",
             font_size=36, color=WHITE, bold=True)

# 传统方案
add_text_box(slide5, 0.5, 1.3, 5.5, 0.5, "传统方案", font_size=24, color=GRAY, bold=True)
add_multi_text(slide5, 0.5, 1.9, 5.5, 2, [
    "存储引擎 + 外挂推理引擎",
    "网络延迟 + 序列化开销",
    "推理延迟：>100ms",
], font_size=16, color=WHITE)

# OntoDB方案
add_text_box(slide5, 7, 1.3, 5.5, 0.5, "OntoDB", font_size=24, color=GREEN, bold=True)
add_multi_text(slide5, 7, 1.9, 5.5, 2, [
    "推理引擎直接嵌入存储引擎",
    "共享内存空间，零网络开销",
    "推理延迟：<1ms",
], font_size=16, color=WHITE)

# 性能提升
add_stat_box(slide5, 2, 4.5, "150倍", "推理延迟提升", GREEN)
add_stat_box(slide5, 5.2, 4.5, "<1ms", "推理延迟", GREEN)
add_stat_box(slide5, 8.4, 4.5, "0", "网络开销", GREEN)

# ============================================================
# 幻灯片6：核心创新2 - 零映射表
# ============================================================
slide6 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide6)

add_text_box(slide6, 0.5, 0.3, 12, 0.8, "核心创新2：零映射表关联",
             font_size=36, color=WHITE, bold=True)

# 传统方案
add_text_box(slide6, 0.5, 1.3, 5.5, 0.5, "传统方案", font_size=24, color=GRAY, bold=True)
add_multi_text(slide6, 0.5, 1.9, 5.5, 2.5, [
    "每个实体维护映射表：",
    "MySQL id=1",
    "映射表: id=1 → neo4j_id=abc",
    "Neo4j: id=abc",
    "跨模态查询需要多次映射表查询",
], font_size=14, color=WHITE)

# OntoDB方案
add_text_box(slide6, 7, 1.3, 5.5, 0.5, "OntoDB", font_size=24, color=GREEN, bold=True)
add_multi_text(slide6, 7, 1.9, 5.5, 2.5, [
    "统一标识：Device::001",
    "同时作为：",
    "- MySQL的主键",
    "- Neo4j的顶点ID",
    "- Milvus的文档键",
], font_size=14, color=WHITE)

# 性能提升
add_stat_box(slide6, 2, 5, "100倍", "查询延迟降低", GREEN)
add_stat_box(slide6, 5.2, 5, "0", "映射表开销", GREEN)
add_stat_box(slide6, 8.4, 5, "强一致", "数据一致性", GREEN)

# ============================================================
# 幻灯片7：核心创新3 - 写入即语义就绪
# ============================================================
slide7 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide7)

add_text_box(slide7, 0.5, 0.3, 12, 0.8, "核心创新3：写入即语义就绪",
             font_size=36, color=WHITE, bold=True)

# 传统方案
add_text_box(slide7, 0.5, 1.3, 5.5, 0.5, "传统方案", font_size=24, color=GRAY, bold=True)
add_multi_text(slide7, 0.5, 1.9, 5.5, 2, [
    "1. 写入MySQL（1ms）",
    "2. ETL同步（1-60秒）",
    "3. 生成语义（1-60秒）",
    "总延迟：2-120秒",
], font_size=16, color=WHITE)

# OntoDB方案
add_text_box(slide7, 7, 1.3, 5.5, 0.5, "OntoDB", font_size=24, color=GREEN, bold=True)
add_multi_text(slide7, 7, 1.9, 5.5, 2, [
    "1. 写入存储引擎",
    "2. 自动创建图顶点",
    "3. 自动生成三元组",
    "4. 执行增量推理",
    "总延迟：<1ms",
], font_size=16, color=WHITE)

# 性能提升
add_stat_box(slide7, 2, 5, "1000倍", "写入延迟降低", GREEN)
add_stat_box(slide7, 5.2, 5, "<1ms", "写入延迟", GREEN)
add_stat_box(slide7, 8.4, 5, "原子", "事务保证", GREEN)

# ============================================================
# 幻灯片8：性能对比
# ============================================================
slide8 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide8)

add_text_box(slide8, 0.5, 0.3, 12, 0.8, "性能对比",
             font_size=36, color=WHITE, bold=True)

# 性能数据
stats = [
    ("推理延迟", "120ms", "0.8ms", "150倍"),
    ("跨模态查询", "50ms", "3ms", "16.7倍"),
    ("写入吞吐量", "800/s", "162,000/s", "202.5倍"),
    ("存储效率", "151GB", "90GB", "40%↓"),
]

# 表头
add_text_box(slide8, 0.5, 1.3, 3.5, 0.5, "指标", font_size=20, color=BLUE, bold=True)
add_text_box(slide8, 4, 1.3, 3, 0.5, "传统方案", font_size=20, color=GRAY, bold=True)
add_text_box(slide8, 7, 1.3, 3, 0.5, "OntoDB", font_size=20, color=GREEN, bold=True)
add_text_box(slide8, 10, 1.3, 3, 0.5, "提升", font_size=20, color=GREEN, bold=True)

# 数据行
for i, (name, traditional, onto, improvement) in enumerate(stats):
    y = 2 + i * 1.2
    add_text_box(slide8, 0.5, y, 3.5, 0.8, name, font_size=20, color=WHITE)
    add_text_box(slide8, 4, y, 3, 0.8, traditional, font_size=20, color=GRAY)
    add_text_box(slide8, 7, y, 3, 0.8, onto, font_size=20, color=GREEN, bold=True)
    add_text_box(slide8, 10, y, 3, 0.8, improvement, font_size=20, color=GREEN, bold=True)

# ============================================================
# 幻灯片9：应用场景
# ============================================================
slide9 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide9)

add_text_box(slide9, 0.5, 0.3, 12, 0.8, "应用场景",
             font_size=36, color=WHITE, bold=True)

scenarios = [
    ("物联网", "设备管理、传感器数据\n实时监控预警"),
    ("智慧城市", "交通、环境、安防\n数据融合"),
    ("工业互联网", "设备监控\n预测性维护"),
    ("AI平台", "多模态数据统一\n语义搜索"),
    ("SaaS平台", "多租户隔离\n数据安全"),
]

for i, (name, desc) in enumerate(scenarios):
    x = 0.5 + i * 2.6
    add_text_box(slide9, x, 1.5, 2.3, 0.5, name, font_size=20, color=BLUE, bold=True,
                 alignment=PP_ALIGN.CENTER)
    add_text_box(slide9, x, 2.2, 2.3, 1.5, desc, font_size=14, color=WHITE,
                 alignment=PP_ALIGN.CENTER)

# 核心价值
add_multi_text(slide9, 1, 4.5, 11, 2, [
    "统一存储：一个系统替代7个系统",
    "高性能：推理延迟<1ms，跨模态查询<3ms",
    "低成本：运维成本降低87%",
], font_size=20, color=GREEN)

# ============================================================
# 幻灯片10：客户案例
# ============================================================
slide10 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide10)

add_text_box(slide10, 0.5, 0.3, 12, 0.8, "客户案例",
             font_size=36, color=WHITE, bold=True)

cases = [
    ("智慧城市", "50万设备", "600ms→10ms", "60倍"),
    ("工业互联网", "5000台设备", "6h→10ms", "2000倍"),
    ("AI平台", "1亿条数据", "700ms→50ms", "14倍"),
    ("SaaS平台", "500企业", "50ms→5ms", "10倍"),
]

for i, (industry, scale, perf, improvement) in enumerate(cases):
    y = 1.3 + i * 1.5
    add_text_box(slide10, 0.5, y, 3, 0.5, industry, font_size=20, color=BLUE, bold=True)
    add_text_box(slide10, 3.5, y, 3, 0.5, scale, font_size=16, color=WHITE)
    add_text_box(slide10, 6.5, y, 3.5, 0.5, perf, font_size=16, color=WHITE)
    add_text_box(slide10, 10, y, 3, 0.5, improvement, font_size=24, color=GREEN, bold=True)

# ============================================================
# 幻灯片11：专利保护
# ============================================================
slide11 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide11)

add_text_box(slide11, 0.5, 0.3, 12, 0.8, "专利保护",
             font_size=36, color=WHITE, bold=True)

add_text_box(slide11, 2, 1.2, 9.3, 0.8, "12项发明专利申请",
             font_size=40, color=BLUE, bold=True, alignment=PP_ALIGN.CENTER)

# 专利族
patents = [
    ("核心架构", "1项"),
    ("关键算法", "5项"),
    ("应用场景", "3项"),
    ("存储优化", "3项"),
]

for i, (category, count) in enumerate(patents):
    x = 1 + i * 3.2
    add_text_box(slide11, x, 2.5, 2.8, 0.5, category, font_size=20, color=WHITE, bold=True,
                 alignment=PP_ALIGN.CENTER)
    add_text_box(slide11, x, 3.1, 2.8, 0.5, count, font_size=28, color=GREEN, bold=True,
                 alignment=PP_ALIGN.CENTER)

# 技术壁垒
add_multi_text(slide11, 1, 4.5, 11, 2, [
    "本体内核化：规避代价150倍性能下降",
    "零映射表：规避代价100倍性能下降",
    "统一标识：无法规避",
    "写入联动：规避代价10倍性能下降+数据不一致",
], font_size=16, color=WHITE)

add_text_box(slide11, 2, 6.5, 9.3, 0.5, "大厂无法完全规避",
             font_size=24, color=GREEN, bold=True, alignment=PP_ALIGN.CENTER)

# ============================================================
# 幻灯片12：竞争优势
# ============================================================
slide12 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide12)

add_text_box(slide12, 0.5, 0.3, 12, 0.8, "竞争优势",
             font_size=36, color=WHITE, bold=True)

comparisons = [
    ("vs 传统多系统", "系统简化87%\n推理性能150倍\n运维成本降低87%"),
    ("vs 单一模态数据库", "功能完整\n6种数据模态\n跨模态查询"),
    ("vs 外挂式推理", "推理延迟<1ms\n数据强一致\n系统复杂度降低50%"),
]

for i, (vs, advantage) in enumerate(comparisons):
    x = 0.5 + i * 4.3
    add_text_box(slide12, x, 1.5, 3.8, 0.5, vs, font_size=20, color=BLUE, bold=True,
                 alignment=PP_ALIGN.CENTER)
    add_text_box(slide12, x, 2.2, 3.8, 2, advantage, font_size=16, color=WHITE,
                 alignment=PP_ALIGN.CENTER)

# ============================================================
# 幻灯片13：技术路线图
# ============================================================
slide13 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide13)

add_text_box(slide13, 0.5, 0.3, 12, 0.8, "技术路线图",
             font_size=36, color=WHITE, bold=True)

milestones = [
    ("2026 Q4", "v1.0发布\n开源版本\n首批客户"),
    ("2027", "v2.0发布\n云服务版本\n国际市场"),
    ("2028", "生态建设\n行业深耕\n技术领先"),
]

for i, (time, content) in enumerate(milestones):
    x = 1 + i * 4.3
    add_text_box(slide13, x, 1.5, 3.5, 0.5, time, font_size=24, color=BLUE, bold=True,
                 alignment=PP_ALIGN.CENTER)
    add_text_box(slide13, x, 2.2, 3.5, 2, content, font_size=18, color=WHITE,
                 alignment=PP_ALIGN.CENTER)

# ============================================================
# 幻灯片14：商业模式
# ============================================================
slide14 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide14)

add_text_box(slide14, 0.5, 0.3, 12, 0.8, "商业模式",
             font_size=36, color=WHITE, bold=True)

models = [
    ("产品销售", "软件许可费\n按节点/按数据量计费"),
    ("云服务", "SaaS订阅费\n按使用量计费"),
    ("技术授权", "向其他厂商授权\n交叉授权"),
    ("专业服务", "技术咨询\n定制开发"),
]

for i, (name, desc) in enumerate(models):
    x = 0.5 + i * 3.2
    add_text_box(slide14, x, 1.5, 2.8, 0.5, name, font_size=20, color=BLUE, bold=True,
                 alignment=PP_ALIGN.CENTER)
    add_text_box(slide14, x, 2.2, 2.8, 1.5, desc, font_size=14, color=WHITE,
                 alignment=PP_ALIGN.CENTER)

# ============================================================
# 幻灯片15：联系方式
# ============================================================
slide15 = prs.slides.add_slide(prs.slide_layouts[6])
add_bg(slide15)

add_text_box(slide15, 3, 1.5, 7.3, 1.2, "OntoDB",
             font_size=60, color=BLUE, bold=True, alignment=PP_ALIGN.CENTER)

add_text_box(slide15, 3, 2.8, 7.3, 0.8, "一个数据库替代七个系统",
             font_size=28, color=GREEN, alignment=PP_ALIGN.CENTER)

add_multi_text(slide15, 3, 4, 7.3, 2, [
    "官网：ontovalue.com",
    "邮箱：contact@ontovalue.com",
    "GitHub：github.com/ontodb",
], font_size=18, color=WHITE)

add_text_box(slide15, 3, 6, 7.3, 0.5, "欢迎联系我们：技术交流 | 商务合作 | 投资咨询",
             font_size=16, color=GRAY, alignment=PP_ALIGN.CENTER)

# ============================================================
# 保存
# ============================================================
output_path = r"E:\ontodb\docs\marketing\slides\OntoDB产品介绍.pptx"
prs.save(output_path)
print(f"✅ PPT已生成: {output_path}")
print(f"共 {len(prs.slides)} 页幻灯片")
