# CEP Warn Mode Coverage Report

> **POINT-IN-TIME REPORT — v1.3.1-alpha baseline.** This report was captured
> against the v1.3.1-alpha catalog, which included `session.envelope` and
> agent-visible `observe.audit` / `observe.audit_replay`. Those methods have
> since been **removed**; the current agent catalog has **6 methods** (no
> `session.envelope`, no audit CEP methods). The tables below are preserved as
> the historical v1.3.1-alpha baseline and are **not** a current-state report.

Current v1.3.1-alpha coverage summary.

## §1 Methods Exercised

| Method | Warn Surface | Sample Types | Effect Warning |
|--------|-------------|--------------|----------------|
| observe.workspace | overreach | success + overreach | yes |
| mutate.create | overreach | success + overreach | yes |
| mutate.edit | overreach | success + overreach | yes |
| mutate.batch | overreach | success + overreach | yes |
| fetch.web | overreach | success + overreach | yes |
| exec.command_run | overreach / partial observation | success + overreach | yes |
| session.envelope | overreach | success + overreach | yes |
| session.todo_write | success-only (no effect warn surface) | success only | ok (no overreach sample expected) |
| observe.audit | success-only (no effect warn surface) | success only | ok (no overreach sample expected) |
| observe.audit_replay | success-only (no effect warn surface) | success only | ok (no overreach sample expected) |

## §2 Leak Count

**N=0** — Clean baseline. All overreach-capable methods produced `effect_warning`
in Warn mode audit metadata. No `fix-now` items identified.

## §3 Triage

| Method | Warn Surface | Effect Warning | Triage |
|--------|-------------|----------------|--------|
| observe.workspace | overreach | yes — `effect_warning` in audit metadata | accept-alpha |
| mutate.create | overreach | yes — `effect_warning` in audit metadata | accept-alpha |
| mutate.edit | overreach | yes — `effect_warning` in audit metadata | accept-alpha |
| mutate.batch | overreach | yes — `effect_warning` in audit metadata | accept-alpha |
| fetch.web | overreach | yes — `effect_warning` in audit metadata | accept-alpha |
| exec.command_run | overreach / partial observation | yes — `effect_warning` in audit metadata | accept-alpha |
| session.envelope | overreach | yes — `effect_warning` in audit metadata | accept-alpha |
| session.todo_write | success-only | n/a | accept-alpha |
| observe.audit | success-only | n/a | accept-alpha |
| observe.audit_replay | success-only | n/a | accept-alpha |
