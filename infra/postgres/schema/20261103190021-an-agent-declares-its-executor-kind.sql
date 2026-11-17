-- 20261103190021-an-agent-declares-its-executor-kind.sql — factory
-- taxonomy T1 (backlog 3ff2ef60; design 4348b857, M1).
--
-- An executor is a person, a robot or an automaton, and there is no
-- fourth kind. A person is an employees row and an automaton an
-- `automation:*` id (automation_actors, which holds no agents row: the
-- agents id CHECK is ^agent-). An agents row is a robot unless it says
-- otherwise, and this column is where it says so: robot or automaton,
-- never person — an agent is not staff.
--
-- The column's default IS the backfill: every row registered before it
-- reads robot, which is what boss_core::actor::ActorId::executor_kind
-- gives a registered agent's id, and the pin
-- the_agents_executor_kind_column_defaults_to_what_the_parse_says_an_agent_is
-- (boss-jobs, agents/one_kind_tests.rs) holds the two equal. Schema only:
-- no row is written by hand here.
ALTER TABLE agents ADD COLUMN IF NOT EXISTS executor_kind TEXT NOT NULL DEFAULT 'robot'
    CONSTRAINT agents_executor_kind_check CHECK (executor_kind IN ('robot', 'automaton'));
