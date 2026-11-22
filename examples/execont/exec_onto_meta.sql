-- =============================================================
-- ExecOnt 执行态本体 · 元模型参考包（M1 交付）
-- =============================================================
-- 性质：纯 DDL 约定（零内核改动），全部基于现有 CREATE/ALTER 能力。
-- 配套：examples/execont/test_execont_m1.py（验收断言套件）
-- 方案：docs/执行态本体开发方案与战略意义.md（§四 元模型 / §六 M1）
--
-- 六面元模型：
--   主体面 Agent      ExecAgent / Capability          —— 谁在跑、能干什么
--   过程面 Process    Process / Activity              —— 在跑什么、怎么分解
--   状态面 State      State / Transition              —— 跑到哪了、怎么迁移
--   事件面 Event      ExecEvent                       —— 发生了什么、什么导致什么
--   意图面 Intent     Goal / Intent / ExecConstraint  —— 想干什么、该不该干
--   资源面 Resource   Resource / ExecutionContext     —— 用什么跑、在哪跑
--
-- provenance 必填约定（执行态硬规矩）：
--   ExecEvent.agent_id REQUIRED（谁）
--   ExecEvent.ts       REQUIRED（何时）
--   高频原始流不进本体面——进本体面的是事件化后的语义事实。
-- =============================================================

CREATE ONTOLOGY exec_onto (
  -- ── 主体面 ──────────────────────────────────────────────
  CLASS ExecAgent,
  CLASS Capability,
  -- ── 过程面 ──────────────────────────────────────────────
  CLASS Process,
  CLASS Activity,
  -- ── 状态面 ──────────────────────────────────────────────
  CLASS State,
  CLASS Transition,
  -- ── 事件面 ──────────────────────────────────────────────
  CLASS ExecEvent,
  -- ── 意图面 ──────────────────────────────────────────────
  CLASS Goal,
  CLASS Intent,
  CLASS ExecConstraint,
  -- ── 资源面 ──────────────────────────────────────────────
  CLASS Resource,
  CLASS ExecutionContext,

  -- 主体面属性
  PROPERTY agent_serial   DOMAIN ExecAgent      RANGE STRING,
  PROPERTY capability_of  DOMAIN Capability     RANGE STRING,
  -- 过程面属性
  PROPERTY activity_name  DOMAIN Activity       RANGE STRING,
  PROPERTY process_of     DOMAIN Activity       RANGE STRING,
  PROPERTY composed_of    DOMAIN Process        RANGE STRING,
  PROPERTY preceded_by    DOMAIN Activity       RANGE STRING,
  PROPERTY exec_agent     DOMAIN Process        RANGE STRING,
  -- 状态面属性
  PROPERTY state_name     DOMAIN State          RANGE STRING,
  PROPERTY state_machine  DOMAIN State          RANGE STRING,
  PROPERTY from_state     DOMAIN Transition     RANGE STRING,
  PROPERTY to_state       DOMAIN Transition     RANGE STRING,
  PROPERTY guard          DOMAIN Transition     RANGE STRING,
  PROPERTY current_state  DOMAIN Process        RANGE STRING,
  -- 事件面属性（provenance 必填约定作用于 ExecEvent）
  PROPERTY event_agent    DOMAIN ExecEvent      RANGE STRING,
  PROPERTY ts             DOMAIN ExecEvent      RANGE FLOAT64,
  PROPERTY frame          DOMAIN ExecEvent      RANGE STRING,
  PROPERTY task_name      DOMAIN ExecEvent      RANGE STRING,
  PROPERTY caused_by      DOMAIN ExecEvent      RANGE STRING,
  -- 意图面属性
  PROPERTY goal_of        DOMAIN Intent         RANGE STRING,
  PROPERTY constraint_of  DOMAIN ExecConstraint RANGE STRING,
  PROPERTY has_intent     DOMAIN Process        RANGE STRING,
  -- 资源面属性
  PROPERTY ctx_of         DOMAIN ExecutionContext RANGE STRING,
  PROPERTY obj_name       DOMAIN Resource       RANGE STRING,
  -- 向量载体（ARRAY 形态，与 DiseaseNameVec.embedding 口径一致），向量索引建于其上
  PROPERTY embedding      DOMAIN Resource       RANGE ARRAY,

  -- ── 机器人场景特化（M1 验收载体，SUBCLASS OF 事件面）──
  CLASS GraspEvent SUBCLASS OF ExecEvent,
  CLASS PlaceEvent,
  PROPERTY target         DOMAIN GraspEvent     RANGE STRING,
  PROPERTY confidence     DOMAIN GraspEvent     RANGE FLOAT64
)

-- provenance 必填约定：谁 + 何时
ALTER PROPERTY event_agent DOMAIN ExecEvent REQUIRED
ALTER PROPERTY ts DOMAIN ExecEvent REQUIRED

-- 工序/因果传递（M1 点亮，复用 PrpTrp）
ALTER PROPERTY preceded_by DOMAIN Activity CHARACTERISTICS TRANSITIVE
ALTER PROPERTY caused_by DOMAIN ExecEvent CHARACTERISTICS TRANSITIVE

-- 过程分解为传递（A composed_of B，B composed_of C ⇒ A composed_of C 为工序链闭包的类比）
ALTER PROPERTY composed_of DOMAIN Process CHARACTERISTICS TRANSITIVE

-- 身份键唯一性（主体面）
ALTER CLASS ExecAgent UNIQUE (agent_serial)

-- 机器人特化：事件互斥（同一事件不能既是抓取又是放置）
ALTER CLASS GraspEvent DISJOINT WITH PlaceEvent
