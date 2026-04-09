-- ============================================================
-- SCHEMA VERSION TRACKING
-- ============================================================

DEFINE TABLE meta SCHEMAFULL;
DEFINE FIELD version ON meta TYPE int;

-- ============================================================
-- KNOWLEDGE NODES
-- ============================================================

DEFINE TABLE topic SCHEMAFULL;
DEFINE FIELD key         ON topic TYPE string  ASSERT $value != NONE;
DEFINE FIELD title       ON topic TYPE string  ASSERT $value != NONE;
DEFINE FIELD summary     ON topic TYPE string  DEFAULT "";
DEFINE FIELD blocks      ON topic TYPE string DEFAULT "[]";
DEFINE FIELD tags        ON topic TYPE array<string> DEFAULT [];
DEFINE FIELD created_at  ON topic TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at  ON topic TYPE datetime DEFAULT time::now();
DEFINE FIELD deprecated  ON topic TYPE bool DEFAULT false;

DEFINE INDEX idx_topic_key ON topic FIELDS key UNIQUE;
DEFINE INDEX idx_topic_tags ON topic FIELDS tags;

DEFINE ANALYZER cairn_analyzer TOKENIZERS blank, class
  FILTERS lowercase, snowball(english);
DEFINE INDEX idx_topic_title_search ON topic FIELDS title
  SEARCH ANALYZER cairn_analyzer BM25;
DEFINE INDEX idx_topic_summary_search ON topic FIELDS summary
  SEARCH ANALYZER cairn_analyzer BM25;

-- ============================================================
-- PERSONALITY / PREFERENCES
-- ============================================================

DEFINE TABLE voice SCHEMAFULL;
DEFINE FIELD content     ON voice TYPE string  ASSERT $value != NONE;
DEFINE FIELD updated_at  ON voice TYPE datetime DEFAULT time::now();

DEFINE TABLE preferences SCHEMAFULL;
DEFINE FIELD prime_max_tokens     ON preferences TYPE int    DEFAULT 4000;
DEFINE FIELD prime_include_gotchas ON preferences TYPE bool  DEFAULT true;
DEFINE FIELD learn_verbosity      ON preferences TYPE string DEFAULT "normal";
DEFINE FIELD learn_auto           ON preferences TYPE bool   DEFAULT true;
DEFINE FIELD updated_at           ON preferences TYPE datetime DEFAULT time::now();

-- ============================================================
-- HISTORY / AUDIT
-- ============================================================

DEFINE TABLE history SCHEMAFULL;
DEFINE FIELD op          ON history TYPE string;
DEFINE FIELD target      ON history TYPE string;
DEFINE FIELD detail      ON history TYPE string  DEFAULT "";
DEFINE FIELD diff        ON history TYPE option<string>;
DEFINE FIELD session_id  ON history TYPE string  DEFAULT "";
DEFINE FIELD created_at  ON history TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_history_target ON history FIELDS target;
DEFINE INDEX idx_history_session ON history FIELDS session_id;

-- ============================================================
-- EDGES (RELATIONS)
-- ============================================================

DEFINE TABLE depends_on TYPE RELATION SCHEMAFULL;
DEFINE FIELD in    ON depends_on TYPE record<topic>;
DEFINE FIELD out   ON depends_on TYPE record<topic>;
DEFINE FIELD note  ON depends_on TYPE string DEFAULT "";
DEFINE FIELD created_at ON depends_on TYPE datetime DEFAULT time::now();

DEFINE TABLE contradicts TYPE RELATION SCHEMAFULL;
DEFINE FIELD in    ON contradicts TYPE record<topic>;
DEFINE FIELD out   ON contradicts TYPE record<topic>;
DEFINE FIELD note  ON contradicts TYPE string DEFAULT "";
DEFINE FIELD created_at ON contradicts TYPE datetime DEFAULT time::now();

DEFINE TABLE replaced_by TYPE RELATION SCHEMAFULL;
DEFINE FIELD in    ON replaced_by TYPE record<topic>;
DEFINE FIELD out   ON replaced_by TYPE record<topic>;
DEFINE FIELD note  ON replaced_by TYPE string DEFAULT "";
DEFINE FIELD created_at ON replaced_by TYPE datetime DEFAULT time::now();

DEFINE TABLE gotcha TYPE RELATION SCHEMAFULL;
DEFINE FIELD in    ON gotcha TYPE record<topic>;
DEFINE FIELD out   ON gotcha TYPE record<topic>;
DEFINE FIELD note  ON gotcha TYPE string DEFAULT "";
DEFINE FIELD severity ON gotcha TYPE string DEFAULT "medium";
DEFINE FIELD created_at ON gotcha TYPE datetime DEFAULT time::now();

DEFINE TABLE see_also TYPE RELATION SCHEMAFULL;
DEFINE FIELD in    ON see_also TYPE record<topic>;
DEFINE FIELD out   ON see_also TYPE record<topic>;
DEFINE FIELD note  ON see_also TYPE string DEFAULT "";
DEFINE FIELD created_at ON see_also TYPE datetime DEFAULT time::now();

DEFINE TABLE war_story TYPE RELATION SCHEMAFULL;
DEFINE FIELD in    ON war_story TYPE record<topic>;
DEFINE FIELD out   ON war_story TYPE record<topic>;
DEFINE FIELD note  ON war_story TYPE string DEFAULT "";
DEFINE FIELD created_at ON war_story TYPE datetime DEFAULT time::now();

DEFINE TABLE owns TYPE RELATION SCHEMAFULL;
DEFINE FIELD in    ON owns TYPE record<topic>;
DEFINE FIELD out   ON owns TYPE record<topic>;
DEFINE FIELD note  ON owns TYPE string DEFAULT "";
DEFINE FIELD created_at ON owns TYPE datetime DEFAULT time::now();
