# Documentation index

| File | What it is |
| --- | --- |
| [ENVIRONMENT.md](ENVIRONMENT.md) | The single reference for every environment variable the service reads; checked against the code in CI. |
| [RUNBOOK.md](RUNBOOK.md) | Operator runbook: symptoms, diagnosis and remediation for recurring failure modes. |
| [KEEPER_STATE_MACHINE.md](KEEPER_STATE_MACHINE.md) | The keeper's per-key failure state machine, with constants named as in the code. |
| [branch-protection.md](branch-protection.md) | Intended branch-protection settings for `main`. **Not currently applied** — see the note at the top of the file. |
| [RISK_MECHANICS.md](RISK_MECHANICS.md) | Design reference for the external liquidation/ADL/insurance-fund contracts. Nothing in it is implemented in this service. |
| [REWRITE_PLAN.md](REWRITE_PLAN.md) | Historical: the plan for the Axum/Tokio rewrite now on `main` (formerly `plan.md` at the repo root). Not a specification. |
| [ORACLE_UPDATE_IMPLEMENTATION_PLAN.md](ORACLE_UPDATE_IMPLEMENTATION_PLAN.md) | Historical: plan for the retired Cloudflare Worker design. Not a specification. |

Plan documents carry a `Status:` line at the top (`implemented`, `superseded by …`
or `abandoned`, with the date it was last reviewed). Read anything marked
historical as background only — what shipped is recorded in
[CHANGELOG.md](../CHANGELOG.md) and the code.
