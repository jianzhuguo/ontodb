# 数据来源与许可声明 / Data Sources & Licenses

OntoDB 生物医药知识图谱整合了以下公开数据源。所有数据均允许**商业使用**，使用时请按各许可要求署名。

## 许可一览 / License Summary

| 数据源 | 内容 | 许可 | 商用 |
|--------|------|------|------|
| [ChEMBL](https://www.ebi.ac.uk/chembl/) | 化合物、活性、文献、ADME、CYP、代谢 | **CC BY-SA 3.0** | ✅（署名 + 同许可） |
| [Gene Ontology](http://geneontology.org/) | GO 术语与本体层级 | **CC BY 4.0** | ✅ |
| [UniProt-GOA](https://www.ebi.ac.uk/goa/) | 蛋白-GO 注释 | **CC BY 4.0** | ✅ |
| [Reactome](https://reactome.org/) | 生物通路 | **CC BY 4.0** | ✅ |
| [Rhea](https://www.rhea-db.org/) | 生化反应 | **CC BY 4.0** | ✅ |
| [AlphaFold DB](https://alphafold.ebi.ac.uk/) | 蛋白质结构 | **CC BY 4.0** | ✅ |
| [OpenFDA](https://open.fda.gov/) | 不良反应（FAERS） | 美国政府公有数据 | ✅ |
| [DrugBank DDI](https://data.mendeley.com/datasets/md5czfsfnd/1) | 药物相互作用（Mendeley 发布） | **CC BY 4.0** | ✅ |

## 推荐署名文本 / Attribution Text

在产品文档、网站页脚或「关于」页面中放置：

> **数据来源 / Data Sources**
>
> 本产品使用以下公开数据集，特此致谢：
>
> - [ChEMBL](https://www.ebi.ac.uk/chembl/)（CC BY-SA 3.0）— 化合物结构、生物活性、文献与 ADMET
> - [Gene Ontology](http://geneontology.org/)（CC BY 4.0）— GO 术语与本体
> - [UniProt-GOA](https://www.ebi.ac.uk/goa/)（CC BY 4.0）— 蛋白功能注释
> - [Reactome](https://reactome.org/)（CC BY 4.0）— 生物通路
> - [Rhea](https://www.rhea-db.org/)（CC BY 4.0）— 生化反应
> - [AlphaFold DB](https://alphafold.ebi.ac.uk/)（CC BY 4.0）— 蛋白质结构预测
> - [OpenFDA](https://open.fda.gov/) — 药物不良事件报告
> - DrugBank DDI 数据集（CC BY 4.0，via [Mendeley Data](https://data.mendeley.com/datasets/md5czfsfnd/1)）
>
> 各数据集版权归其原作者所有。

## CC BY-SA 3.0（ChEMBL）实务影响

| 场景 | 是否触发 ShareAlike |
|------|---------------------|
| **SaaS / 内部查询服务**（不对外分发数据本身） | ❌ 通常不触发 |
| **对外 API 返回查询结果/片段** | ⚠️ 一般视为合理使用，需署名 |
| **打包分发含 ChEMBL 数据的数据库 dump / 全量导出** | ✅ **触发**：衍生数据需同为 CC BY-SA |
| **论文 / 报告引用数据** | ❌ 署名即可 |

**建议：** 产品化以服务形态提供查询（不随软件分发原始数据 dump），并在文档保留上述署名，即可合规商用。若将来需要「离线交付数据包」，请单独评估 ChEMBL 的 SA 传染范围（通常只需对 ChEMBL 部分声明 CC BY-SA）。

## 版本 / Releases

| 数据源 | 版本 / 日期 |
|--------|-------------|
| ChEMBL | 35 |
| Gene Ontology | releases/2026-07-26 |
| UniProt-GOA | goa_human（2026-10） |
| AlphaFold | v6（UP000005640 HUMAN） |
| Rhea | 2026-09-02 |
