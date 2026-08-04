# Pi development-workflow policy alignment

## Spec

Implemented `docs/01-align-pi-development-workflow-with-claude-code-policy.md` in the foreground session, without subagents. The work aligns the user-scoped Pi workflow extension and policy with the Claude development pipeline while preserving recovery of legacy workflow state.

## Changed behavior

- Added explicit policy precedence and Pi/Claude parity rules to `~/.pi/agent/AGENTS.md`.
- Added workflow state v4 and a red-first sequence: `red_testing -> implementing -> reviewing -> reporting`.
- Preserved v1-v3 active stages and stage sequences during migration.
- Enforced targeted failing-test evidence before implementation, full-green evidence before review, a named Reviewer weakness and review-gate evidence, and exact Reporter content before completion.
- Enforced Test Writer, Implementer, Reviewer, and Reporter child authority at both hook and execution boundaries.
- Changed the default and hard review cap to two rounds. Critical leftovers escalate; non-critical leftovers become follow-ups and require the report to warn that the work could use more review passes.
- Routed roles to supported models: Terra for Test Writer, Implementer, and Reporter; Sol for Orchestrator, Reviewer, and the legacy Planner.
- Persisted validated per-role model overrides.
- Added repository preflight and explicit dirty-path acknowledgement, plus parallel batch/worktree metadata and ownership constraints.
- Made exact Reporter output the system-of-record payload, with `docs/reports/` as the file fallback.
- Rewrote bundled role prompts, Orchestrator instructions, and README guidance around TDD order, recovery, authority, model routing, and worktree safety.

## Verification

- `node --test ~/.pi/agent/extensions/development-workflow/*.test.mjs ~/.pi/agent/extensions/subagent/tests/*.test.mjs` — 76/76 passed.
- `pi --list-models -e ~/.pi/agent/extensions/development-workflow/index.ts` — extension loaded; `openai-codex/gpt-5.6-sol` and `openai-codex/gpt-5.6-terra` are available.
- `pi --list-models -e ~/.pi/agent/extensions/subagent/index.ts` — extension loaded.
- Mutation checks deliberately removed the red-test, full-green, Reviewer-weakness, and Reporter gates; the regression suite caught all four mutations, and each mutation was reverted.
- Regression coverage includes successful red-first flow, legacy migration, disputed/failed gates, two-round escalation and deferred follow-ups, exact report persistence, dirty-tree acknowledgement, model validation, child authority, and worktree/batch metadata.
- Temporary test plan fixture is absent. The pre-existing `.pi/workplans/workflow-1785297865246-12414c.md` remains untouched.

## Review verdict

Self-review completed in the foreground as requested. No blocking findings remain; all configured gates pass.

## Follow-ups

None.
