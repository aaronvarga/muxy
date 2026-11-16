# Architecture review

Read-only review covered the native actor, normalization boundary, submission
tracking, captured context, additive protocol and desktop draft state. It found:

| Severity | Finding | Resolution |
| --- | --- | --- |
| High | Lost turn acknowledgments lacked native-history reconciliation | Native turn/item client IDs and Claude transcript UUIDs reconcile status; no uncertain operation is resent |
| High | Post-detach errors could drop recovery state and reopen input | Retain Prepared and blocked recovery once termination is attempted |
| High | Failed approval writes could remain locally unanswerable | Reconnect only the same verified engine and replay with a fresh request epoch |
| High | Rejected first prompts could retry Begin after startup | Retire provider-selection metadata on established ownership, retain composer independently; GPUI regression covers Submit retry and LeaveProvider |
| Medium | Later item hydration could race an active non-full snapshot | Require atomic full active-turn snapshot; reject unsupported hydration |
| Medium | Provider preference recency | Existing save path removes and appends its provider entry; retained |

The review verified the native source's atomic full resume-history/subscription
boundary and withdrew its initial full-snapshot duplicate-delta concern. Captured
spawn environment and revision-guarded composer clearing had no additional high
finding. Required checks are recorded in 04-quality-gates.md. Visual acceptance
is a separate outstanding gate because desktop automation was unavailable.
