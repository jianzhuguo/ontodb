"""
OntoDB 技术白皮书生成脚本
生成 Word 文档，包含系统概述、版本信息、技术细节和应用场景
"""

from docx import Document
from docx.shared import Pt, Cm, RGBColor
from docx.enum.text import WD_ALIGN_PARAGRAPH
from docx.enum.section import WD_ORIENTATION
from docx.oxml.ns import qn
from docx.oxml import OxmlElement
import platform

def setup_page(doc, size="A4"):
    """设置页面格式"""
    section = doc.sections[0]
    if size == "A4":
        section.page_width, section.page_height = Cm(21.0), Cm(29.7)
        section.top_margin = section.bottom_margin = Cm(2.54)
        section.left_margin = section.right_margin = Cm(3.18)
    else:
        from docx.shared import Inches
        section.page_width, section.page_height = Inches(8.5), Inches(11.0)
        section.top_margin = section.bottom_margin = Inches(1.0)
        section.left_margin = section.right_margin = Inches(1.25)
    section.orientation = WD_ORIENTATION.PORTRAIT

def tune_styles(doc):
    """调整文档样式"""
    body = doc.styles["Normal"]
    body.font.name = "Calibri"
    body.font.size = Pt(11)
    body.paragraph_format.line_spacing = 1.15
    body.paragraph_format.space_after = Pt(6)
    
    # 设置中文字体
    run = body.element.rPr
    if run is None:
        run = OxmlElement("w:rPr")
        body.element.append(run)
    rFonts = run.find(qn("w:rFonts"))
    if rFonts is None:
        rFonts = OxmlElement("w:rFonts")
        run.append(rFonts)
    rFonts.set(qn("w:eastAsia"), "Microsoft YaHei")

    for n, size in [(1, 18), (2, 14), (3, 12)]:
        s = doc.styles[f"Heading {n}"]
        s.font.name = "Calibri Light"
        s.font.size = Pt(size)
        s.font.bold = True
        s.font.color.rgb = RGBColor(0x1F, 0x3A, 0x5F)
        s.paragraph_format.space_before = Pt(14 - 2 * n)
        s.paragraph_format.space_after = Pt(4)
        
        # 为标题设置中文字体
        run = s.element.rPr
        if run is None:
            run = OxmlElement("w:rPr")
            s.element.append(run)
        rFonts = run.find(qn("w:rFonts"))
        if rFonts is None:
            rFonts = OxmlElement("w:rFonts")
            run.append(rFonts)
        rFonts.set(qn("w:eastAsia"), "Microsoft YaHei")

def add_cover(doc, title, subtitle=None, author=None, date=None):
    """添加封面"""
    for _ in range(6):
        doc.add_paragraph()
    p = doc.add_paragraph(title, style="Title")
    p.alignment = WD_ALIGN_PARAGRAPH.CENTER
    if subtitle:
        p = doc.add_paragraph(subtitle, style="Subtitle")
        p.alignment = WD_ALIGN_PARAGRAPH.CENTER
    for _ in range(10):
        doc.add_paragraph()
    if author or date:
        line = " · ".join(x for x in (author, date) if x)
        p = doc.add_paragraph(line)
        p.alignment = WD_ALIGN_PARAGRAPH.CENTER

def add_toc(doc):
    """添加目录"""
    p = doc.add_paragraph()
    run = p.add_run()
    fldChar1 = OxmlElement("w:fldChar")
    fldChar1.set(qn("w:fldCharType"), "begin")
    instrText = OxmlElement("w:instrText")
    instrText.set(qn("xml:space"), "preserve")
    instrText.text = 'TOC \\o "1-3" \\h \\z \\u'
    fldChar2 = OxmlElement("w:fldChar")
    fldChar2.set(qn("w:fldCharType"), "separate")
    fldChar3 = OxmlElement("w:t")
    fldChar3.text = "右键点击此处，选择'更新域'以生成目录"
    fldChar4 = OxmlElement("w:fldChar")
    fldChar4.set(qn("w:fldCharType"), "end")
    for x in (fldChar1, instrText, fldChar2, fldChar3, fldChar4):
        run._r.append(x)

def add_table(doc, header, rows):
    """添加表格"""
    table = doc.add_table(rows=1 + len(rows), cols=len(header))
    table.style = "Light Grid Accent 1"

    hdr = table.rows[0].cells
    for i, name in enumerate(header):
        hdr[i].text = name
        for p in hdr[i].paragraphs:
            for r in p.runs:
                r.bold = True

    for r_idx, row in enumerate(rows, start=1):
        cells = table.rows[r_idx].cells
        for c_idx, value in enumerate(row):
            cells[c_idx].text = str(value)

def add_page_number(paragraph):
    """添加页码"""
    run = paragraph.add_run()
    fldChar1 = OxmlElement("w:fldChar")
    fldChar1.set(qn("w:fldCharType"), "begin")
    instrText = OxmlElement("w:instrText")
    instrText.text = "PAGE"
    fldChar2 = OxmlElement("w:fldChar")
    fldChar2.set(qn("w:fldCharType"), "end")
    run._r.append(fldChar1)
    run._r.append(instrText)
    run._r.append(fldChar2)

def generate_ontodb_documentation():
    """生成 OntoDB 技术白皮书"""
    doc = Document()
    setup_page(doc)
    tune_styles(doc)
    
    # ========== 封面 ==========
    add_cover(
        doc,
        title="OntoDB 技术白皮书",
        subtitle="本体驱动的语义多模数据库",
        author="OntoDB Team",
        date="2026年8月"
    )
    doc.add_page_break()
    
    # ========== 目录 ==========
    doc.add_paragraph("目录", style="Heading 1")
    add_toc(doc)
    doc.add_page_break()
    
    # ========== 第一章：系统概述 ==========
    doc.add_paragraph("第一章 系统概述", style="Heading 1")
    
    doc.add_paragraph("1.1 产品简介", style="Heading 2")
    doc.add_paragraph(
        "OntoDB 是一款基于 Rust 语言自主研发的本体驱动语义多模数据库。"
        "它将 OWL-lite 本体推理直接嵌入数据库内核，使数据不仅被存储，更被'理解'。"
        "OntoDB 在单一引擎中融合了关系型存储、向量搜索和语义查询能力，"
        "消除了为不同工作负载拼凑多个数据库的需求。"
    )
    
    doc.add_paragraph("1.2 核心特性", style="Heading 2")
    features = [
        "本体原生存储 — 通过 CREATE ONTOLOGY 定义类、属性、继承和约束，推理器自动传播推断（子类、等价类、子属性、逆属性、传递性、对称性）",
        "完整 SQL 引擎 — 手写递归下降解析器，支持 SELECT、INSERT、UPDATE、DELETE、JOIN、子查询、CTE、窗口函数、GROUP BY、HAVING、ORDER BY、LIMIT/OFFSET、UNION、UPSERT 等",
        "SPARQL 端点 — 符合 W3C 标准的 SPARQL 1.1，支持 SELECT、CONSTRUCT、ASK、FILTER、OPTIONAL、UNION",
        "向量搜索 — 内置 HNSW 索引，支持 L2、Cosine、InnerProduct 度量，混合 SQL + 向量查询",
        "LSM-Tree 存储引擎 — WAL + MemTable + SSTable，支持分级压缩、zstd 压缩、布隆过滤器、块缓存、MVCC 快照隔离",
        "B+Tree 索引 — 内存和磁盘双模式（4KB 页，LRU 淘汰缓冲池）",
        "BinaryRow 格式 — 紧凑二进制行编码，字段查找速度提升 4.5 倍，过滤评估速度提升 5.5 倍",
        "生产级服务器 — HTTP API (axum) + TCP 服务器，支持 API Key 认证、令牌桶限流、Prometheus 指标",
        "零 unsafe — 整个代码库不包含任何 unsafe Rust 代码块"
    ]
    for feature in features:
        doc.add_paragraph(feature, style="List Bullet")
    
    doc.add_paragraph("1.3 版本信息", style="Heading 2")
    doc.add_paragraph(
        "当前最新版本为 v0.2.0-alpha，于 2026 年 8 月 9 日发布。"
        "这是首个公开 Alpha 版本，标志着 OntoDB 从内部开发走向公开测试的重要里程碑。"
    )
    
    # ========== 第二章：v0.2.0-alpha 版本详解 ==========
    doc.add_paragraph("第二章 v0.2.0-alpha 版本详解", style="Heading 1")
    
    doc.add_paragraph("2.1 版本亮点", style="Heading 2")
    highlights = [
        "100% Rust 自研的本体原生多模数据库",
        "统一实体锚点：{class}::{pk} 在关系、图、向量、三元组间共享",
        "写入吞吐量：1,002,259 QPS",
        "读取吞吐量：1,319,084 QPS",
        "向量召回率：100%",
        "空间索引：比 PostGIS 快 3 倍",
        "472/472 测试全部通过",
        "4 种协议支持：HTTP REST、PostgreSQL、MySQL、TCP CLI",
        "CDC 集成：支持 Kafka/Flink",
        "安全审计：11 项自动化检查"
    ]
    for h in highlights:
        doc.add_paragraph(h, style="List Bullet")
    
    doc.add_paragraph("2.2 第五阶段新增模块", style="Heading 2")
    
    doc.add_paragraph("2.2.1 GIS 地理信息", style="Heading 3")
    gis_features = [
        "支持 Point/LineString/Polygon 几何类型",
        "WKB/WKT 格式支持",
        "R*树空间索引",
        "Geohash 编码",
        "ST_* 空间函数（ST_Distance、ST_Contains、ST_Intersects 等）"
    ]
    for f in gis_features:
        doc.add_paragraph(f, style="List Bullet")
    
    doc.add_paragraph("2.2.2 时序数据", style="Heading 3")
    ts_features = [
        "TSM 存储引擎",
        "热/温/冷三层数据分层",
        "DTW（动态时间规整）相似性度量",
        "异常检测功能"
    ]
    for f in ts_features:
        doc.add_paragraph(f, style="List Bullet")
    
    doc.add_paragraph("2.2.3 时空数据", style="Heading 3")
    st_features = [
        "四叉树 + 时间线索引",
        "STTRL 规则引擎"
    ]
    for f in st_features:
        doc.add_paragraph(f, style="List Bullet")
    
    doc.add_paragraph("2.2.4 三元组存储", style="Heading 3")
    triple_features = [
        "SPO/POS/OSP 三种索引模式",
        "支持 RDF 三元组持久化",
        "SPARQL 查询优化"
    ]
    for f in triple_features:
        doc.add_paragraph(f, style="List Bullet")
    
    doc.add_paragraph("2.3 兼容性与已知问题", style="Heading 2")
    doc.add_paragraph("破坏性变更：无", style="List Bullet")
    doc.add_paragraph("已知问题：图存储 delete_vertex 存在既有死锁问题（此版本已修复）", style="List Bullet")
    
    # ========== 第三章：统一实体锚点设计 ==========
    doc.add_paragraph("第三章 统一实体锚点设计", style="Heading 1")
    
    doc.add_paragraph("3.1 设计理念", style="Heading 2")
    doc.add_paragraph(
        "OntoDB 的核心创新在于所有数据模态共享同一个语义锚点。"
        "这个锚点的格式为 {class}::{pk}，例如 Product::000001、Employee::alice。"
        "通过这个统一标识，关系型、图、向量、三元组四种数据模态实现了无缝关联。"
    )
    
    doc.add_paragraph("3.2 各模态映射", style="Heading 2")
    add_table(
        doc,
        ["数据模态", "键格式", "说明"],
        [
            ["关系型", "LSM key: Product::000001", "存储引擎的主键"],
            ["图", "Vertex ID: Product::000001", "图顶点的唯一标识"],
            ["向量", "Document key: Product::000001", "HNSW 索引的文档键"],
            ["三元组", "Subject/Object: Product::000001", "RDF 三元组的主语/宾语"]
        ]
    )
    
    doc.add_paragraph("3.3 核心实现", style="Heading 2")
    doc.add_paragraph("EntityId 结构体定义：")
    code = """pub struct EntityId {
    class: String,  // 类名，如 "Product"
    pk: String,     // 主键，如 "000001"
}

impl EntityId {
    pub fn to_lsm_key(&self) -> Vec<u8> {
        format!("{}::{}", self.class, self.pk).into_bytes()
    }
    
    pub fn to_vertex_id(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }
}"""
    p = doc.add_paragraph(code)
    p.style = doc.styles["Normal"]
    for run in p.runs:
        run.font.name = "Consolas"
        run.font.size = Pt(10)
    
    doc.add_paragraph("3.4 核心优势", style="Heading 2")
    advantages = [
        "数据自动同步：写入关系型记录时，自动创建图顶点和向量索引",
        "跨模态查询：向量搜索结果可直接关联关系型属性，图遍历结果可获取详细数据",
        "推理联动：本体推理结果自动持久化为三元组",
        "消除数据孤岛：所有模态通过统一锚点无缝连接"
    ]
    for a in advantages:
        doc.add_paragraph(a, style="List Bullet")
    
    # ========== 第四章：跨模态查询 ==========
    doc.add_paragraph("第四章 跨模态查询", style="Heading 1")
    
    doc.add_paragraph("4.1 SQL + 向量混合查询", style="Heading 2")
    doc.add_paragraph(
        "场景：在满足 SQL 条件的商品中，按向量相似度排序。"
        "这是最常见的跨模态查询模式，结合了结构化过滤和语义相似性。"
    )
    
    doc.add_paragraph("HTTP API 示例：")
    api_example = """curl -X POST http://localhost:7912/api/hybrid/query \\
  -H "Content-Type: application/json" \\
  -d '{
    "sql_filter": "SELECT * FROM Product WHERE category = \\"electronics\\"",
    "vector_column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, 0.4],
    "top_k": 5,
    "class": "Product"
  }'"""
    p = doc.add_paragraph(api_example)
    for run in p.runs:
        run.font.name = "Consolas"
        run.font.size = Pt(10)
    
    doc.add_paragraph("4.2 图遍历 + 向量搜索", style="Heading 2")
    doc.add_paragraph(
        "场景：先通过图关系找到相关实体，再在子图中进行向量相似度搜索。"
        "适用于社交网络推荐、知识图谱问答等场景。"
    )
    
    doc.add_paragraph("Rust 代码示例：")
    rust_example = """// Step 1: 图遍历获取邻居
let neighbors = engine.hop(&vid, Direction::Out, Some("KNOWS"), None, None)?;
let neighbor_ids: Vec<String> = neighbors.iter().map(|v| v.id.clone()).collect();

// Step 2: 在子图中进行向量搜索
let results = index.search_filtered(
    &query_vec,
    5,  // top_k
    &neighbor_ids.iter().map(|id| id.as_bytes().to_vec()).collect()
);"""
    p = doc.add_paragraph(rust_example)
    for run in p.runs:
        run.font.name = "Consolas"
        run.font.size = Pt(10)
    
    doc.add_paragraph("4.3 多模态联合查询", style="Heading 2")
    doc.add_paragraph("完整的电商商品推荐示例：")
    
    steps = [
        "SQL 查询 - 价格过滤：SELECT name, price FROM Product WHERE price < 100",
        "向量搜索 - 语义相似：搜索与 'audio headphones' 最相似的 3 个商品",
        "图遍历 - 关联商品：查找与特定商品相关的所有商品（2 跳以内）",
        "混合查询 - SQL 过滤 + 向量相似：在电子产品类别中搜索与 'wireless audio' 最相似的 3 个商品"
    ]
    for i, step in enumerate(steps, 1):
        doc.add_paragraph(f"{i}. {step}", style="List Number")
    
    doc.add_paragraph("4.4 典型应用场景", style="Heading 2")
    add_table(
        doc,
        ["场景", "查询组合", "示例"],
        [
            ["商品推荐", "向量相似 + 价格/类别过滤", "找到与用户浏览商品相似且价格在预算内的商品"],
            ["知识图谱问答", "图遍历 + 属性查询", "查找某人的所有同事及其专业领域"],
            ["RAG 检索", "向量搜索 + 元数据过滤", "在特定文档集合中搜索与问题最相关的段落"],
            ["社交网络", "图关系 + 用户属性过滤", "查找与用户有共同兴趣爱好的附近用户"]
        ]
    )
    
    # ========== 第五章：向量索引技术详解 ==========
    doc.add_paragraph("第五章 向量索引技术详解", style="Heading 1")
    
    doc.add_paragraph("5.1 HNSW 索引概述", style="Heading 2")
    doc.add_paragraph(
        "OntoDB 目前支持 HNSW（Hierarchical Navigable Small World）向量索引。"
        "HNSW 是一种基于图的近似最近邻搜索算法，由 Malkov 和 Yashunin 于 2018 年提出。"
        "它通过构建多层图结构实现高效的向量相似性搜索。"
    )
    
    doc.add_paragraph("5.2 支持的距离度量", style="Heading 2")
    add_table(
        doc,
        ["度量类型", "公式", "说明", "适用场景"],
        [
            ["L2（欧氏距离）", "sqrt(Σ(aᵢ-bᵢ)²)", "两点间的直线距离", "通用场景，推荐默认使用"],
            ["Cosine（余弦距离）", "1 - (a·b)/(‖a‖·‖b‖)", "向量方向的相似性", "文本嵌入、方向相似性"],
            ["InnerProduct（内积）", "-(a·b)", "向量的点积", "归一化向量、推荐系统"]
        ]
    )
    
    doc.add_paragraph("5.3 HNSW 核心参数", style="Heading 2")
    doc.add_paragraph("5.3.1 M 参数", style="Heading 3")
    doc.add_paragraph(
        "M 控制每层图中每个节点的最大邻居数（第 0 层为 2*M）。"
        "M 越大，图连接越密集，索引质量越高，但内存占用也越大。"
        "推荐值：16（默认值）"
    )
    
    doc.add_paragraph("5.3.2 ef_construction 参数", style="Heading 3")
    doc.add_paragraph(
        "ef_construction 控制索引构建阶段 beam search 的搜索宽度。"
        "它决定每个新节点插入时寻找最近邻的广度，直接影响索引质量。"
    )
    
    add_table(
        doc,
        ["ef_construction 值", "索引质量", "构建速度", "推荐场景"],
        [
            ["100", "低", "快速", "原型验证、频繁更新"],
            ["200（默认）", "高", "中等", "大多数场景"],
            ["400+", "极高", "较慢", "静态数据、追求极致召回"]
        ]
    )
    
    doc.add_paragraph("5.3.3 ef_search 参数", style="Heading 3")
    doc.add_paragraph(
        "ef_search 控制查询阶段 beam search 的搜索宽度。"
        "它决定搜索时保留的候选邻居数量，直接影响查询精度和速度。"
    )
    
    add_table(
        doc,
        ["ef_search 值", "召回率", "查询延迟", "推荐场景"],
        [
            ["50", "低 (~90%)", "低 (~100µs)", "对延迟敏感，可容忍少量丢失"],
            ["100（默认）", "高 (~99%)", "中等 (~300µs)", "大多数场景"],
            ["200+", "极高 (100%)", "高 (~1ms+)", "对召回率要求极高"]
        ]
    )
    
    doc.add_paragraph("5.4 参数调优建议", style="Heading 2")
    doc.add_paragraph("5.4.1 ef_construction 调优", style="Heading 3")
    recommendations = [
        "与 M 参数配合：推荐 ef_construction = 10~20 倍 M",
        "根据数据规模调整：< 10K 数据用 100-200，10K-100K 用 200-400，> 100K 用 400+",
        "根据数据特性调整：均匀分布用默认值，有聚类结构或高维数据需要增大",
        "重要原则：ef_construction 应该 >= ef_search"
    ]
    for r in recommendations:
        doc.add_paragraph(r, style="List Bullet")
    
    doc.add_paragraph("5.4.2 ef_search 调优", style="Heading 3")
    recommendations = [
        "根据 top_k 调整：ef_search 至少等于 top_k，建议 2~5 倍 top_k",
        "根据数据规模调整：< 1K 用 50-100，1K-10K 用 100-200，10K-100K 用 200-400",
        "根据业务场景调整：实时推荐用小值，精确检索用大值",
        "动态调整：OntoDB 支持在查询时动态指定 ef_search"
    ]
    for r in recommendations:
        doc.add_paragraph(r, style="List Bullet")
    
    doc.add_paragraph("5.5 性能基准", style="Heading 2")
    doc.add_paragraph("OntoDB 向量搜索性能测试数据：")
    add_table(
        doc,
        ["数据集", "ef_search", "召回率", "延迟", "QPS"],
        [
            ["1K × 64D", "200", "100%", "294µs", "3,401"],
            ["1K × 64D", "400", "100%", "501µs", "1,996"],
            ["5K × 128D", "200", "100%", "838µs", "1,193"],
            ["5K × 128D", "400", "100%", "1.28ms", "781"],
            ["10K × 256D", "200", "99.7%", "3.75ms", "267"],
            ["10K × 256D", "400", "100%", "4.91ms", "204"]
        ]
    )
    
    # ========== 第六章：行业应用场景 ==========
    doc.add_paragraph("第六章 行业应用场景", style="Heading 1")
    
    doc.add_paragraph("6.1 人工智能与机器学习", style="Heading 2")
    doc.add_paragraph("6.1.1 RAG（检索增强生成）", style="Heading 3")
    doc.add_paragraph(
        "OntoDB 的向量搜索能力使其成为 RAG 应用的理想选择。"
        "用户可以将文档 embedding 存储在 OntoDB 中，通过向量相似性检索最相关的文档片段，"
        "然后将这些片段作为上下文提供给大语言模型。"
        "混合查询能力允许同时考虑语义相似性和元数据过滤（如时间范围、文档类型）。"
    )
    
    doc.add_paragraph("6.1.2 推荐系统", style="Heading 3")
    doc.add_paragraph(
        "通过图关系和向量相似性的结合，OntoDB 可以构建强大的推荐系统。"
        "图遍历可以发现用户的社交关系和行为模式，向量搜索可以找到相似的商品或内容，"
        "SQL 过滤可以应用业务规则（如价格范围、库存状态）。"
    )
    
    doc.add_paragraph("6.1.3 知识图谱", style="Heading 3")
    doc.add_paragraph(
        "OntoDB 的本体推理能力使其特别适合构建企业级知识图谱。"
        "用户可以定义复杂的本体结构（类、属性、约束），"
        "系统自动进行推理（子类传播、属性推断等），"
        "并通过 SPARQL 查询语义关系。"
    )
    
    doc.add_paragraph("6.2 金融科技", style="Heading 2")
    doc.add_paragraph("6.2.1 风险管理", style="Heading 3")
    doc.add_paragraph(
        "金融机构可以使用 OntoDB 构建风险知识图谱，"
        "通过图分析发现关联风险，通过向量搜索识别相似风险案例，"
        "通过时序分析监控风险变化趋势。"
        "本体推理可以自动推断隐含的风险关系。"
    )
    
    doc.add_paragraph("6.2.2 反欺诈检测", style="Heading 3")
    doc.add_paragraph(
        "OntoDB 的多模态查询能力可以同时分析交易模式（关系型）、"
        "用户行为相似性（向量）和关联网络（图），"
        "有效识别复杂的欺诈模式。"
        "实时查询能力支持毫秒级的欺诈检测。"
    )
    
    doc.add_paragraph("6.2.3 客户 360 度视图", style="Heading 3")
    doc.add_paragraph(
        "通过统一实体锚点，OntoDB 可以整合客户的各类数据："
        "基本信息（关系型）、社交关系（图）、行为特征（向量）、"
        "语义标签（三元组），构建完整的客户画像。"
    )
    
    doc.add_paragraph("6.3 医疗健康", style="Heading 2")
    doc.add_paragraph("6.3.1 医学知识图谱", style="Heading 3")
    doc.add_paragraph(
        "OntoDB 可以构建医学知识图谱，整合疾病、症状、药物、治疗方案等信息。"
        "本体推理可以自动推断药物相互作用、疾病关联等隐含关系。"
        "向量搜索可以找到相似的病例和治疗方案。"
    )
    
    doc.add_paragraph("6.3.2 临床决策支持", style="Heading 3")
    doc.add_paragraph(
        "医生可以通过 OntoDB 查询相似病例、推荐治疗方案、"
        "检查药物相互作用。系统可以结合患者的结构化数据（病历）、"
        "非结构化数据（医嘱文本）和时序数据（生命体征）提供综合分析。"
    )
    
    doc.add_paragraph("6.4 电子商务", style="Heading 2")
    doc.add_paragraph("6.4.1 智能搜索", style="Heading 3")
    doc.add_paragraph(
        "OntoDB 支持混合搜索，用户输入自然语言查询，"
        "系统同时进行关键词匹配（SQL）和语义相似性搜索（向量），"
        "返回最相关的结果。图关系可以用于个性化排序。"
    )
    
    doc.add_paragraph("6.4.2 供应链管理", style="Heading 3")
    doc.add_paragraph(
        "通过图分析供应链关系，通过时序分析库存变化，"
        "通过空间分析物流路径。OntoDB 的多模态能力可以"
        "整合供应链的各类数据，提供全局优化视图。"
    )
    
    doc.add_paragraph("6.5 物联网与智慧城市", style="Heading 2")
    doc.add_paragraph("6.5.1 设备管理", style="Heading 3")
    doc.add_paragraph(
        "OntoDB 可以管理海量 IoT 设备数据，包括设备属性（关系型）、"
        "设备关系（图）、传感器读数（时序）、位置信息（空间）。"
        "统一锚点确保设备数据的一致性和可追溯性。"
    )
    
    doc.add_paragraph("6.5.2 智能交通", style="Heading 3")
    doc.add_paragraph(
        "结合 GIS 和时序能力，OntoDB 可以分析交通流量、"
        "预测拥堵、优化路线。图分析可以发现交通网络的关键节点，"
        "向量搜索可以找到相似的交通模式。"
    )
    
    doc.add_paragraph("6.6 企业知识管理", style="Heading 2")
    doc.add_paragraph("6.6.1 文档智能检索", style="Heading 3")
    doc.add_paragraph(
        "企业可以将文档 embedding 存储在 OntoDB 中，"
        "通过语义搜索找到相关文档。本体可以定义文档分类体系，"
        "推理引擎自动进行文档分类和关联。"
    )
    
    doc.add_paragraph("6.6.2 专家系统", style="Heading 3")
    doc.add_paragraph(
        "通过构建领域本体和知识图谱，OntoDB 可以支持专家系统。"
        "推理引擎可以基于规则进行逻辑推导，"
        "向量搜索可以找到相似的问题和解决方案。"
    )
    
    doc.add_paragraph("6.7 大模型应用", style="Heading 2")
    doc.add_paragraph("6.7.1 大模型知识库管理", style="Heading 3")
    doc.add_paragraph(
        "大语言模型（LLM）的知识更新成本高昂，OntoDB 可以作为外挂知识库，"
        "存储和管理模型的领域知识。通过向量索引存储文档 embedding，"
        "实现高效的语义检索。本体推理可以自动发现知识间的隐含关联，"
        "提升知识检索的准确性和完整性。"
    )
    
    doc.add_paragraph("6.7.2 多模态大模型数据管理", style="Heading 3")
    doc.add_paragraph(
        "多模态大模型需要处理文本、图像、音频、视频等多种数据类型。"
        "OntoDB 的统一实体锚点可以将同一实体的不同模态数据关联起来，"
        "例如将产品图片、描述文本、用户评论存储在同一锚点下。"
        "向量搜索支持跨模态检索，图分析可以发现实体间的复杂关系。"
    )
    
    doc.add_paragraph("6.7.3 Agent 记忆与上下文管理", style="Heading 3")
    doc.add_paragraph(
        "AI Agent 需要长期记忆和上下文管理能力。OntoDB 可以存储 Agent 的对话历史、"
        "任务状态、用户偏好等信息。向量搜索可以快速检索相关记忆，"
        "图分析可以发现对话间的逻辑关联，本体推理可以推断用户意图。"
        "时序能力可以追踪记忆的时效性，自动衰减过时信息。"
    )
    
    doc.add_paragraph("6.7.4 模型评估与对齐", style="Heading 3")
    doc.add_paragraph(
        "OntoDB 可以存储模型评估数据集、人工标注结果、对齐偏好数据。"
        "通过向量相似性找到相似的测试用例，通过图分析发现评估维度间的关联，"
        "通过时序分析追踪模型性能的变化趋势。"
        "本体可以定义评估标准和对齐目标的语义框架。"
    )
    
    doc.add_paragraph("6.8 端侧边缘计算", style="Heading 2")
    doc.add_paragraph("6.8.1 边缘 AI 推理", style="Heading 3")
    doc.add_paragraph(
        "在边缘设备上部署 AI 模型时，OntoDB 的轻量级架构和高性能特性使其"
        "成为理想的边缘数据库。设备可以在本地存储和查询向量 embedding，"
        "实现离线推理能力。LSM-Tree 存储引擎对写入密集型场景友好，"
        "适合传感器数据的实时采集和处理。"
    )
    
    doc.add_paragraph("6.8.2 边缘知识同步", style="Heading 3")
    doc.add_paragraph(
        "边缘设备需要与云端保持知识同步。OntoDB 的 Raft 共识层支持"
        "分布式部署，可以实现边缘-云协同的知识管理。"
        "本体定义的知识结构可以在边缘和云端无缝迁移，"
        "确保语义一致性。CDC 集成支持增量数据同步，减少带宽消耗。"
    )
    
    doc.add_paragraph("6.8.3 边缘端实时决策", style="Heading 3")
    doc.add_paragraph(
        "工业物联网、智能家居等场景需要毫秒级的实时决策。"
        "OntoDB 的百万级 QPS 读写能力可以满足实时性要求。"
        "向量搜索支持快速模式匹配，图分析可以发现设备间的关联，"
        "时序分析可以检测异常模式。本地存储避免了网络延迟，"
        "确保关键决策的及时性。"
    )
    
    doc.add_paragraph("6.8.4 联邦学习数据管理", style="Heading 3")
    doc.add_paragraph(
        "联邦学习需要在多个边缘节点上管理本地数据和模型参数。"
        "OntoDB 可以在每个节点上存储本地训练数据的 embedding，"
        "通过向量搜索找到相似的训练样本，通过图分析发现数据分布特征。"
        "本体可以定义联邦学习的语义框架，确保各节点的知识结构一致。"
    )
    
    doc.add_paragraph("6.9 具身智能", style="Heading 2")
    doc.add_paragraph("6.9.1 机器人感知与记忆", style="Heading 3")
    doc.add_paragraph(
        "具身智能机器人需要处理视觉、触觉、听觉等多种感知数据。"
        "OntoDB 的多模态能力可以将同一物体的不同感知数据关联起来，"
        "例如将物体的视觉特征（向量）、物理属性（关系型）、"
        "空间位置（GIS）、抓取历史（图）存储在统一锚点下。"
        "向量搜索可以快速识别相似物体，图分析可以发现物体间的关系。"
    )
    
    doc.add_paragraph("6.9.2 任务规划与执行", style="Heading 3")
    doc.add_paragraph(
        "机器人需要将复杂任务分解为子任务并规划执行顺序。"
        "OntoDB 可以存储任务本体（类、属性、约束），"
        "通过推理引擎自动推导任务间的依赖关系。"
        "图分析可以优化任务执行路径，时序分析可以追踪任务执行进度。"
        "向量搜索可以找到相似的历史任务案例，辅助决策。"
    )
    
    doc.add_paragraph("6.9.3 环境建模与导航", style="Heading 3")
    doc.add_paragraph(
        "机器人需要构建和维护环境地图。OntoDB 的 GIS 能力可以存储"
        "空间信息（点、线、面），R*树索引支持高效的空间查询。"
        "向量搜索可以识别相似的场景，图分析可以发现空间拓扑关系。"
        "时序能力可以追踪环境变化，支持动态地图更新。"
    )
    
    doc.add_paragraph("6.9.4 人机交互理解", style="Heading 3")
    doc.add_paragraph(
        "具身智能需要理解人类的自然语言指令和非语言信号。"
        "OntoDB 可以存储语言 embedding 和手势/表情特征，"
        "通过向量搜索实现多模态意图理解。"
        "本体可以定义交互语义框架，推理引擎可以推断隐含意图。"
        "图分析可以发现交互模式，提升人机协作效率。"
    )
    
    doc.add_paragraph("6.10 自动驾驶", style="Heading 2")
    doc.add_paragraph("6.10.1 感知数据管理", style="Heading 3")
    doc.add_paragraph(
        "自动驾驶系统需要处理海量的传感器数据（摄像头、激光雷达、毫米波雷达）。"
        "OntoDB 的多模态能力可以将同一目标的不同传感器数据关联起来，"
        "例如将车辆的视觉特征（向量）、位置信息（GIS）、"
        "运动轨迹（时序）、与其他目标的关系（图）存储在统一锚点下。"
        "向量搜索可以快速识别相似目标，空间索引支持高效的位置查询。"
    )
    
    doc.add_paragraph("6.10.2 场景理解与决策", style="Heading 3")
    doc.add_paragraph(
        "自动驾驶需要理解复杂的交通场景并做出决策。"
        "OntoDB 可以存储场景本体（道路类型、交通标志、参与者类型等），"
        "通过推理引擎自动推导场景语义。"
        "向量搜索可以找到相似的历史场景，辅助决策。"
        "图分析可以发现参与者间的交互关系，预测行为。"
        "时序分析可以追踪场景演变，支持动态规划。"
    )
    
    doc.add_paragraph("6.10.3 高精地图管理", style="Heading 3")
    doc.add_paragraph(
        "高精地图是自动驾驶的核心基础设施。OntoDB 的 GIS 能力可以存储"
        "道路几何信息（点、线、面），R*树索引支持高效的空间查询。"
        "图结构可以表示道路拓扑关系，支持路径规划。"
        "向量搜索可以识别相似的道路特征，时序能力可以追踪地图更新。"
        "本体可以定义地图语义框架，确保数据一致性。"
    )
    
    doc.add_paragraph("6.10.4 仿真与测试", style="Heading 3")
    doc.add_paragraph(
        "自动驾驶系统需要大量的仿真测试。OntoDB 可以存储仿真场景数据，"
        "通过向量搜索找到相似的测试场景，通过图分析发现场景间的关联。"
        "时序能力可以追踪测试进度和性能变化。"
        "本体可以定义测试标准和评估框架，推理引擎可以自动生成测试用例。"
        "混合查询能力支持复杂的测试数据分析。"
    )
    
    doc.add_paragraph("6.10.5 V2X 协同", style="Heading 3")
    doc.add_paragraph(
        "车路协同（V2X）需要整合车辆、道路、云端的多源数据。"
        "OntoDB 的分布式架构支持多节点协同，统一锚点可以将不同来源的数据关联起来。"
        "实时查询能力支持毫秒级的信息交互，图分析可以发现交通流模式。"
        "本体可以定义 V2X 语义框架，确保信息理解的一致性。"
        "时序分析可以追踪交通流变化，支持协同决策。"
    )
    
    doc.add_paragraph("6.11 硬核科技前沿应用", style="Heading 2")
    doc.add_paragraph("6.11.1 L5 完全自动驾驶", style="Heading 3")
    doc.add_paragraph(
        "L5 级自动驾驶要求系统在任何场景下都能自主决策，无需人类干预。"
        "这对数据管理提出了极高要求：毫秒级响应、100% 可靠性、海量多模态数据处理。"
    )
    doc.add_paragraph("核心技术挑战与 OntoDB 解决方案：")
    l5_challenges = [
        "超大规模感知融合：L5 车辆每秒产生 GB 级传感器数据。OntoDB 的 LSM-Tree 引擎支持百万级 QPS 写入，可以实时摄入摄像头、激光雷达、毫米波雷达数据。统一锚点将同一目标的多传感器数据关联，确保感知一致性。",
        "实时场景理解：需要在 10ms 内完成场景理解和决策。OntoDB 的向量搜索支持快速模式匹配，可以在历史场景库中找到相似案例。本体推理引擎可以自动推导场景语义（如'施工区域'、'行人横穿'），辅助决策。",
        "高精地图实时更新：L5 需要厘米级精度的动态地图。OntoDB 的 GIS 能力支持复杂几何运算，R*树索引实现高效空间查询。时序能力追踪道路变化（如临时施工、车道变更），确保地图时效性。",
        "V2X 协同决策：L5 车辆需要与交通设施、其他车辆实时通信。OntoDB 的分布式架构支持边缘-云协同，统一锚点整合多源信息。图分析发现交通流模式，支持全局优化。",
        "安全冗余与故障恢复：L5 系统必须具备故障容错能力。OntoDB 的 Raft 共识确保数据一致性，MVCC 支持事务回滚，WAL 保证数据持久性。零 unsafe 代码消除内存安全隐患。"
    ]
    for c in l5_challenges:
        doc.add_paragraph(c, style="List Bullet")
    
    doc.add_paragraph("6.11.2 空天算场景", style="Heading 3")
    doc.add_paragraph(
        "空天算（Space-Air-Ground Integrated Computing）融合卫星、无人机、地面站的计算资源，"
        "构建天地一体化信息网络。这对数据管理提出了独特挑战：超远距离通信、极端环境、异构资源。"
    )
    doc.add_paragraph("核心技术挑战与 OntoDB 解决方案：")
    sky_challenges = [
        "星载数据管理：卫星计算资源极其有限（CPU、内存、存储）。OntoDB 的 Rust 实现具有极小的运行时开销，零 unsafe 代码确保可靠性。LSM-Tree 引擎对写入密集型场景友好，适合传感器数据的实时采集。",
        "星地协同知识同步：卫星与地面站通信延迟可达秒级。OntoDB 的 Raft 共识支持分布式部署，可以实现星-地-空多级数据同步。CDC 集成支持增量同步，减少带宽消耗。本体定义的知识结构在各节点无缝迁移。",
        "轨道数据索引：卫星需要快速查询地球表面任意区域的数据。OntoDB 的 GIS 能力支持全球尺度的空间索引，Geohash 编码实现高效区域查询。向量搜索可以识别相似的地物特征，支持遥感图像分析。",
        "时序数据压缩：卫星产生海量时序数据（如气象、遥感）。OntoDB 的时序能力支持热/温/冷三层分层存储，自动压缩历史数据。DTW 相似性度量可以发现时序模式，异常检测可以识别突发事件。",
        "边缘智能推理：卫星需要在轨完成部分 AI 推理任务。OntoDB 的向量索引支持在星载设备上进行语义检索，本体推理可以自动推导知识关系。混合查询能力支持复杂的在轨数据分析。"
    ]
    for c in sky_challenges:
        doc.add_paragraph(c, style="List Bullet")
    
    doc.add_paragraph("6.11.3 工业数字孪生", style="Heading 3")
    doc.add_paragraph(
        "工业数字孪生是物理世界的虚拟镜像，需要实时同步物理实体的状态、行为、关系。"
        "这对数据管理提出了多维度要求：多模态数据融合、实时同步、复杂关系建模。"
    )
    doc.add_paragraph("核心技术挑战与 OntoDB 解决方案：")
    digital_twin_challenges = [
        "多模态数据融合：数字孪生需要整合 CAD 模型（几何）、传感器数据（时序）、知识图谱（语义）、运维记录（文本）。OntoDB 的统一实体锚点将同一设备的多模态数据关联，确保数据一致性。向量搜索支持跨模态检索。",
        "实时状态同步：物理实体状态变化需要实时反映到数字孪生。OntoDB 的百万级 QPS 写入能力支持高频状态更新。时序能力追踪状态演变，异常检测可以识别设备故障征兆。CDC 集成支持增量同步。",
        "复杂关系建模：工业系统具有复杂的拓扑关系（设备-产线-车间-工厂）。OntoDB 的图能力支持多层级关系建模，图分析可以发现系统瓶颈。本体推理可以自动推导设备间的依赖关系，支持故障传播分析。",
        "预测性维护：需要基于历史数据预测设备故障。OntoDB 的时序能力可以分析设备运行趋势，向量搜索可以找到相似的故障模式。本体可以定义维护知识库，推理引擎可以自动生成维护计划。",
        "仿真与优化：数字孪生需要支持 what-if 分析和工艺优化。OntoDB 可以存储仿真场景，通过向量搜索找到相似的历史案例，通过图分析评估变更影响。混合查询能力支持复杂的优化分析。"
    ]
    for c in digital_twin_challenges:
        doc.add_paragraph(c, style="List Bullet")
    
    doc.add_paragraph("6.11.4 人形机器人大脑端侧", style="Heading 3")
    doc.add_paragraph(
        "人形机器人需要在端侧完成复杂的感知、决策、控制任务。"
        "这对数据管理提出了极端要求：超低延迟、超小体积、多模态融合、实时学习。"
    )
    doc.add_paragraph("核心技术挑战与 OntoDB 解决方案：")
    robot_brain_challenges = [
        "端侧资源约束：机器人主控板计算资源有限（通常 ARM 处理器 + 数 GB 内存）。OntoDB 的 Rust 实现具有极小的运行时开销，内存占用可控。LSM-Tree 引擎对存储友好，支持数据压缩。",
        "多模态感知融合：机器人需要处理视觉、触觉、听觉、本体感觉等多种感知数据。OntoDB 的统一锚点将同一物体的多感知数据关联，向量搜索支持跨模态识别。图分析可以发现感知数据间的关联，提升理解能力。",
        "实时任务规划：机器人需要在 100ms 内完成任务规划和决策。OntoDB 的向量搜索支持快速案例检索，本体推理可以自动推导任务依赖。图分析可以优化执行路径，时序能力可以追踪任务进度。",
        "持续学习与适应：机器人需要在工作中不断学习新技能。OntoDB 可以存储学习经验（向量 embedding），通过向量搜索找到相似的学习案例。本体可以定义技能知识框架，推理引擎可以自动泛化知识。",
        "安全与可靠性：机器人控制系统必须高度可靠。OntoDB 的零 unsafe 代码消除内存安全隐患，MVCC 支持事务一致性，WAL 保证数据持久性。本地存储避免网络依赖，确保关键功能可用。"
    ]
    for c in robot_brain_challenges:
        doc.add_paragraph(c, style="List Bullet")
    
    doc.add_paragraph("6.11.5 深空探测", style="Heading 3")
    doc.add_paragraph(
        "深空探测（如火星任务、小行星采样）面临极端环境挑战：超远距离通信（延迟可达数十分钟）、"
        "极端温度、辐射、自主决策需求。这对数据管理提出了前所未有的要求。"
    )
    doc.add_paragraph("核心技术挑战与 OntoDB 解决方案：")
    space_challenges = [
        "超远距离自主决策：地球到火星通信延迟 4-24 分钟，探测器必须具备完全自主能力。OntoDB 的本地存储和查询能力支持离线决策，向量搜索可以快速匹配历史任务模式，本体推理可以自动推导决策逻辑。",
        "极端环境可靠性：深空环境极端恶劣（温度 -120°C 到 +20°C，强辐射）。OntoDB 的 Rust 实现无垃圾回收暂停，运行时行为可预测。零 unsafe 代码消除未定义行为，数据结构稳定可靠。",
        "科学数据管理：探测器产生海量科学数据（图像、光谱、地震波等）。OntoDB 的多模态能力可以将同一目标的多种观测数据关联，向量搜索可以识别相似的地质特征，时序能力可以追踪数据采集过程。",
        "任务规划与执行：深空任务需要复杂的多步骤规划。OntoDB 的本体可以定义任务知识框架，推理引擎可以自动推导任务依赖和约束。图分析可以优化任务执行路径，时序能力可以追踪任务进度。",
        "星表探测与采样：着陆器需要在未知地形中导航和采样。OntoDB 的 GIS 能力可以存储地形数据，R*树索引支持高效空间查询。向量搜索可以识别相似的地形特征，支持路径规划。实时数据摄入支持动态地图更新。",
        "星际知识传承：探测器需要在漫长任务中积累和传承知识。OntoDB 的本体推理可以自动发现知识间的关联，向量搜索可以检索相关知识。数据持久化确保知识不丢失，支持跨任务的知识复用。"
    ]
    for c in space_challenges:
        doc.add_paragraph(c, style="List Bullet")
    
    doc.add_paragraph("6.12 硬核科技场景对比分析", style="Heading 2")
    doc.add_paragraph("各场景对 OntoDB 核心能力的需求矩阵：")
    add_table(
        doc,
        ["应用场景", "向量搜索", "图分析", "时序处理", "GIS 空间", "本体推理", "边缘部署"],
        [
            ["L5 自动驾驶", "★★★★★", "★★★★★", "★★★★★", "★★★★★", "★★★★☆", "★★★★☆"],
            ["空天算场景", "★★★★☆", "★★★☆☆", "★★★★★", "★★★★★", "★★★★☆", "★★★★★"],
            ["工业数字孪生", "★★★★☆", "★★★★★", "★★★★★", "★★★☆☆", "★★★★★", "★★★☆☆"],
            ["人形机器人大脑", "★★★★★", "★★★★☆", "★★★★☆", "★★★☆☆", "★★★★★", "★★★★★"],
            ["深空探测", "★★★★☆", "★★★☆☆", "★★★★☆", "★★★★★", "★★★★★", "★★★★★"]
        ]
    )
    
    doc.add_paragraph("OntoDB 在硬核科技领域的核心优势：")
    core_advantages = [
        "统一实体锚点：在极端环境下，数据一致性至关重要。统一锚点确保多模态数据的无缝关联，避免数据孤岛。",
        "Rust 零 unsafe：在安全关键系统中（自动驾驶、机器人、航天），代码可靠性是生命线。零 unsafe 消除内存安全隐患。",
        "百万级 QPS：实时系统对延迟极其敏感。OntoDB 的高性能确保毫秒级响应，满足硬实时要求。",
        "边缘部署能力：资源受限环境需要轻量级解决方案。OntoDB 的 Rust 实现具有极小运行时开销，适合端侧部署。",
        "多模态融合：复杂系统需要处理多种数据类型。OntoDB 的向量、图、时序、GIS 能力提供全方位支持。"
    ]
    for a in core_advantages:
        doc.add_paragraph(a, style="List Bullet")
    
    # ========== 第七章：部署与运维 ==========
    doc.add_paragraph("第七章 部署与运维", style="Heading 1")
    
    doc.add_paragraph("7.1 部署方式", style="Heading 2")
    deploy_options = [
        "单机部署：适合开发和测试环境",
        "Docker 部署：使用官方镜像快速启动",
        "Kubernetes 部署：支持生产级容器编排",
        "集群部署：基于 Raft 共识的分布式架构"
    ]
    for d in deploy_options:
        doc.add_paragraph(d, style="List Bullet")
    
    doc.add_paragraph("7.2 监控与告警", style="Heading 2")
    doc.add_paragraph(
        "OntoDB 提供完整的监控体系："
    )
    monitoring = [
        "37 个 Prometheus 指标，覆盖查询、延迟、连接、认证、存储、内存、磁盘、缓存、WAL、事务、压缩、备份、Raft 等",
        "12 条告警规则，覆盖服务可用性、性能、安全、存储、Raft、备份",
        "3 个 Grafana 仪表盘（30 个面板）：指标总览、SQL 分析、操作系统监控",
        "Alertmanager 支持 4 种通知渠道：邮件、Slack、钉钉、企业微信"
    ]
    for m in monitoring:
        doc.add_paragraph(m, style="List Bullet")
    
    doc.add_paragraph("7.3 安全特性", style="Heading 2")
    security = [
        "API Key 认证",
        "IP 白名单（全局 + 按 Key，支持 CIDR）",
        "TLS/mTLS 加密传输",
        "查询审计日志（JSONL 格式，每日轮转）",
        "速率限制（可配置，默认 300 请求/分钟）"
    ]
    for s in security:
        doc.add_paragraph(s, style="List Bullet")
    
    # ========== 第八章：性能基准 ==========
    doc.add_paragraph("第八章 性能基准", style="Heading 1")
    
    doc.add_paragraph("8.1 存储引擎性能", style="Heading 2")
    add_table(
        doc,
        ["操作", "QPS", "延迟 (P50)", "说明"],
        [
            ["写入 (put)", "1,010,863/s", "~1µs", "WAL 批量同步"],
            ["读取 (get)", "1,298,431/s", "~0.77µs", "MemTable 命中"],
            ["批量写入 (put_batch)", "1,328,256/s", "~0.75µs", "单次锁获取"],
            ["顺序扫描", "4,553/s", "109.8ms", "100K 行，200 次迭代"]
        ]
    )
    
    doc.add_paragraph("8.2 并发扫描性能", style="Heading 2")
    add_table(
        doc,
        ["线程数", "耗时", "加速比"],
        [
            ["1", "21.96s", "1.00x"],
            ["2", "13.92s", "1.58x"],
            ["4", "11.06s", "1.99x"],
            ["8", "10.13s", "2.17x"]
        ]
    )
    
    doc.add_paragraph("8.3 关键指标汇总", style="Heading 2")
    add_table(
        doc,
        ["指标", "数值"],
        [
            ["写入 QPS", "1,010,863"],
            ["读取 QPS", "1,298,431"],
            ["批量写入 QPS", "1,328,256"],
            ["并发扫描 (8T)", "2.17x 加速"],
            ["向量召回率", "99.7% ~ 100%"],
            ["完整测试套件", "472/472 通过"]
        ]
    )
    
    # ========== 第九章：总结与展望 ==========
    doc.add_paragraph("第九章 总结与展望", style="Heading 1")
    
    doc.add_paragraph("9.1 核心价值", style="Heading 2")
    doc.add_paragraph(
        "OntoDB 作为本体驱动的语义多模数据库，其核心价值在于："
    )
    values = [
        "统一性：通过统一实体锚点消除数据孤岛，实现多模态数据的无缝关联",
        "智能性：内置本体推理引擎，自动推断隐含关系，提升数据价值",
        "高性能：百万级 QPS 的读写能力，100% 的向量召回率",
        "易用性：标准 SQL + SPARQL 接口，降低学习成本",
        "可靠性：零 unsafe 代码，完整的测试覆盖，生产级监控告警"
    ]
    for v in values:
        doc.add_paragraph(v, style="List Bullet")
    
    doc.add_paragraph("9.2 未来规划", style="Heading 2")
    future = [
        "完善数据分片策略，支持更大规模数据",
        "优化 Raft 共识层，提升集群稳定性",
        "增加更多向量索引类型（如 IVF、SCANN）",
        "增强本体推理能力，支持更复杂的推理规则",
        "扩展 SDK 支持，覆盖更多编程语言",
        "推进商业化，提供企业级支持服务"
    ]
    for f in future:
        doc.add_paragraph(f, style="List Bullet")
    
    doc.add_paragraph("9.3 结语", style="Heading 2")
    doc.add_paragraph(
        "OntoDB 代表了数据库技术的新方向——不仅仅是存储数据，更是理解数据。"
        "通过本体语义与多模存储的深度融合，OntoDB 为 AI 应用、知识管理、"
        "智能分析等场景提供了全新的基础设施。"
        "我们相信，随着 v0.2.0-alpha 的公开发布，"
        "OntoDB 将在更多行业中发挥价值，推动数据管理技术的演进。"
    )
    
    # ========== 附录 ==========
    doc.add_page_break()
    doc.add_paragraph("附录", style="Heading 1")
    
    doc.add_paragraph("A. 快速开始", style="Heading 2")
    doc.add_paragraph("从源码构建：")
    code = """# 前提条件：Rust 1.70+
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release"""
    p = doc.add_paragraph(code)
    for run in p.runs:
        run.font.name = "Consolas"
        run.font.size = Pt(10)
    
    doc.add_paragraph("启动服务器：")
    code = """# TCP 服务器（默认：localhost:7913）
./target/release/ontodb-server --data-dir ./mydata

# HTTP API
./target/release/ontodb-server --data-dir ./mydata --http 0.0.0.0:7912

# 启用认证
./target/release/ontodb-server --data-dir ./mydata --http 0.0.0.0:7912 \\
  --auth --api-keys-file config/api_keys.example.json"""
    p = doc.add_paragraph(code)
    for run in p.runs:
        run.font.name = "Consolas"
        run.font.size = Pt(10)
    
    doc.add_paragraph("B. API 端点", style="Heading 2")
    add_table(
        doc,
        ["方法", "端点", "说明"],
        [
            ["GET", "/api/health", "健康检查"],
            ["GET", "/api/health/ready", "Kubernetes 就绪探针"],
            ["GET", "/api/health/live", "Kubernetes 存活探针"],
            ["GET", "/metrics", "Prometheus 指标"],
            ["POST", "/api/query", "执行 SQL/OntoDB 查询"],
            ["POST", "/api/vector/search", "向量相似性搜索"],
            ["POST", "/api/hybrid/query", "混合 SQL + 向量搜索"],
            ["GET", "/api/schema", "Schema 自省"],
            ["POST", "/sparql", "SPARQL 查询端点"]
        ]
    )
    
    # 添加页脚
    section = doc.sections[0]
    footer = section.footer.paragraphs[0]
    footer.alignment = WD_ALIGN_PARAGRAPH.CENTER
    add_page_number(footer)
    
    # 保存文档
    output_path = "E:\\ontodb\\OntoDB技术白皮书.docx"
    doc.save(output_path)
    print(f"文档已生成：{output_path}")
    return output_path

if __name__ == "__main__":
    generate_ontodb_documentation()