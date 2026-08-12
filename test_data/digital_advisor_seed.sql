-- ============================================================
-- 数字军师 (Digital Advisor) — 技术原型数据模型
-- 基于 OntoDB 六模态统一存储
-- ============================================================

-- ═══════════════════════════════════════════════════
-- 一、决策本体 (Decision Ontology)
-- ═══════════════════════════════════════════════════

-- 决策者
CREATE VERTEX TABLE DecisionMaker (
    name STRING,           -- 姓名
    role STRING,           -- 角色：CEO / CTO / VP / Director
    company STRING,        -- 公司
    style STRING,          -- 决策风格：激进型/稳健型/分析型/直觉型
    avatar_url STRING      -- 头像URL
);

-- 决策记录（核心表）
CREATE VERTEX TABLE Decision (
    title STRING,              -- 决策标题
    description STRING,        -- 决策描述
    category STRING,           -- 类别：战略/产品/人事/财务/危机
    priority STRING,           -- 优先级：P0/P1/P2
    status STRING,             -- 状态：待定/已决策/已执行/已复盘
    decision_text STRING,      -- 最终决策内容
    decision_date STRING,      -- 决策日期
    context_summary STRING,    -- 背景摘要
    emotion_state STRING,      -- 情绪状态：冷静/焦虑/兴奋/犹豫
    confidence_score DOUBLE,   -- 决策信心指数 0-1
    pressure_level INT,        -- 压力等级 1-10
    embedding STRING           -- 决策文本的向量嵌入（用于相似度搜索）
);

-- 决策上下文（四维：事件+情感+环境+商业）
CREATE VERTEX TABLE DecisionContext (
    decision_id STRING,        -- 关联决策
    event_type STRING,         -- 事件类型：市场变化/内部危机/竞对动态/政策变化
    event_detail STRING,       -- 事件详情
    market_condition STRING,   -- 市场环境：牛市/熊市/震荡/平稳
    competition STRING,        -- 竞对动态
    team_morale INT,           -- 团队士气 1-10
    financial_pressure INT,    -- 财务压力 1-10
    time_pressure INT,         -- 时间压力 1-10
    iot_data STRING            -- IoT环境数据（JSON）
);

-- 决策结果
CREATE VERTEX TABLE DecisionOutcome (
    decision_id STRING,
    result_type STRING,        -- 成功/部分成功/失败/待评估
    result_detail STRING,      -- 结果详情
    revenue_impact DOUBLE,     -- 收入影响（万元）
    team_impact INT,           -- 团队影响 1-10
    lesson_learned STRING,     -- 经验教训
    review_date STRING         -- 复盘日期
);

-- 决策链（思维路径）
CREATE VERTEX TABLE DecisionStep (
    decision_id STRING,
    step_order INT,            -- 步骤顺序
    thought STRING,            -- 思考内容
    option STRING,             -- 考虑的选项
    reason STRING,             -- 选择/排除理由
    emotion STRING,            -- 当时情绪
    timestamp STRING           -- 时间戳
);

-- 人格特征（用于沙盒模拟）
CREATE VERTEX TABLE PersonalityTrait (
    maker_id STRING,           -- 决策者
    trait_name STRING,         -- 特征名：风险偏好/决策速度/信息依赖/直觉权重
    trait_value DOUBLE,        -- 特征值 0-1
    evidence STRING            -- 依据
);

-- ═══════════════════════════════════════════════════
-- 二、测试数据：CEO 张明远的决策记录
-- ═══════════════════════════════════════════════════

-- 决策者
INSERT INTO DecisionMaker (name, role, company, style) VALUES ('张明远', 'CEO', 'OntoDB科技', '分析型');

-- 人格特征
INSERT INTO PersonalityTrait (maker_id, trait_name, trait_value, evidence) VALUES ('张明远', '风险偏好', 0.65, '多次在不确定环境下做出中高风险决策');
INSERT INTO PersonalityTrait (maker_id, trait_name, trait_value, evidence) VALUES ('张明远', '决策速度', 0.8, '危机决策平均响应时间2小时');
INSERT INTO PersonalityTrait (maker_id, trait_name, trait_value, evidence) VALUES ('张明远', '数据依赖', 0.7, '70%决策基于数据分析，30%基于直觉');
INSERT INTO PersonalityTrait (maker_id, trait_name, trait_value, evidence) VALUES ('张明远', '直觉权重', 0.3, '在数据不足时依赖行业经验判断');

-- ═══════════════════════════════════════════════════
-- 决策1：是否进入政务市场（战略决策）
-- ═══════════════════════════════════════════════════
INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('是否进入政务市场', '评估政务市场机会，决定是否投入资源开发政务版产品', '战略', 'P0', '已执行', '决定进入政务市场，先以浙江省为试点，投入5人团队，6个月内完成等保三级认证和首个标杆客户', '2025-06-15', '等保2.0政策推动政务数字化，竞对尚未布局，团队有技术优势但缺乏政务行业经验', '冷静', 0.75, 6, '0.82,0.15,0.91,0.33,0.67,0.45,0.78,0.12');

INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D001', '政策变化', '等保2.0标准发布，政务系统强制要求国产化数据库', '平稳', '竞对暂无政务版产品', 8, 4, 6);

INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D001', 1, '政务市场政策窗口期明确，但团队缺乏行业经验', '选项A: 立即进入 / 选项B: 等待观望 / 选项C: 找合作伙伴', '选择A：窗口期不等人，先占位再补能力', '冷静', '2025-06-10 09:00');
INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D001', 2, '试点区域选择：浙江数字化程度高，对新技术接受度好', '浙江/广东/北京', '浙江政务云基础好，且有潜在客户资源', '兴奋', '2025-06-12 14:00');
INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D001', 3, '资源投入规模：5人团队，预算200万', '3人/5人/8人', '5人是最低可行配置，覆盖产品+技术+商务', '冷静', '2025-06-15 10:00');

INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D001', '成功', '6个月内签约浙江省公安厅，合同额450万，等保三级认证通过', 450, 7, '政务市场需要耐心，关系建设比技术更重要', '2026-01-15');

-- ═══════════════════════════════════════════════════
-- 决策2：是否接受某大厂收购要约（战略决策）
-- ═══════════════════════════════════════════════════
INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('是否接受大厂收购要约', '某头部云厂商提出全资收购，估值3亿', '战略', 'P0', '已决策', '拒绝收购，坚持独立发展。理由：技术壁垒足够深，政务市场刚打开，被收购会失去产品方向控制权', '2025-09-20', '公司估值3亿，团队50人，年营收3000万，政务市场刚突破', '犹豫', 0.6, 9, '0.45,0.82,0.33,0.91,0.67,0.12,0.78,0.25');

INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D002', '市场变化', '某头部云厂商提出全资收购，同时竞对获得大额融资', '震荡', '竞对融资2亿，市场格局可能变化', 6, 7, 8);

INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D002', 1, '估值合理，但团队愿景会丢失', '接受/拒绝/反提条件', '技术护城河足够深，不应在此时放弃', '犹豫', '2025-09-10');
INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D002', 2, '政务市场刚打开，未来3年增长空间巨大', '坚持独立/并入大厂', '独立发展能保持产品方向控制权', '坚定', '2025-09-15');
INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D002', 3, '拒绝后需要加快融资节奏', '拒绝+启动B轮', '用拒绝收购的信号吸引更多投资人', '兴奋', '2025-09-20');

INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D002', '待评估', '拒绝后3个月完成B轮融资1.5亿，估值翻倍', 0, 8, '坚持独立发展需要更强的资金储备和更快的增长', '');

-- ═══════════════════════════════════════════════════
-- 决策3：产品定价策略调整（产品决策）
-- ═══════════════════════════════════════════════════
INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('企业版定价策略调整', '原定价过高导致转化率低，需要调整定价策略', '产品', 'P1', '已执行', '企业版降价40%，推出标准版(5万/年)和高级版(15万/年)两档，同时提供免费社区版引流', '2026-03-01', '企业版定价25万/年，转化率仅2%，竞对定价8-12万', '焦虑', 0.7, 7, '0.67,0.33,0.82,0.45,0.91,0.12,0.78,0.56');

INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D003', '市场变化', '连续3个月新客户转化率低于3%，竞对价格优势明显', '平稳', '竞对定价8-12万，功能覆盖70%', 5, 7, 8);

INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D003', 1, '定价过高是转化率低的主因', '降价/加功能/换目标客户', '降价是最直接的解决方案', '焦虑', '2026-02-20');
INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D003', 2, '分层定价：社区版引流，标准版走量，高级版盈利', '两档/三档/按用量', '两档简单清晰，社区版降低试用门槛', '冷静', '2026-02-25');
INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D003', 3, '降价幅度：对标竞对，略高10-15%', '降30%/40%/50%', '降40%有足够竞争力且不伤品牌', '坚定', '2026-03-01');

INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D003', '成功', '调整后2个月转化率从2%提升至8%，月新增客户从2家增至8家', 480, 5, '定价要贴近市场，免费版是最好的获客渠道', '2026-05-01');

-- ═══════════════════════════════════════════════════
-- 决策4：核心技术人才争夺（人事决策）
-- ═══════════════════════════════════════════════════
INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('是否用期权留住核心架构师', '核心架构师收到竞对3倍薪资offer，是否用期权留人', '人事', 'P0', '已执行', '给核心架构师5年100万股期权(价值约500万)，同时调整技术团队薪资结构', '2026-05-20', '核心架构师是OntoDB内核的主要贡献者，离开会严重影响产品进度', '焦虑', 0.8, 8, '0.56,0.78,0.33,0.91,0.45,0.67,0.82,0.23');

INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D004', '内部危机', '核心架构师收到竞对offer，团队士气受影响', '平稳', '竞对挖角力度大', 4, 6, 9);

INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D004', '成功', '架构师接受期权方案，团队稳定，后续3个月产品迭代速度提升30%', 0, 9, '核心人才要用期权绑定，不能只靠薪资', '2026-08-20');
