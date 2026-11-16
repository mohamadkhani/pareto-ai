# M2 frontend assumptions (2026-11-16)

Source: sub-agent `ses_e1ea9368fffe7MUWHymKpO5a04` (M2 admin UI). These are
the places where `docs/specs/m2-providers-catalog.md` was silent and the UI
chose a shape; the alignment pass must reconcile each one with the M2 backend
(`ses_e1ea9a98affeFm0du9HPssp7i3`). Types live in
`web/src/app/catalog/catalog.types.ts`, calls in `catalog.service.ts`.

1. List envelopes `{providers:[...]}`, `{credentials:[...]}`, `{models:[...]}`,
   `{offerings:[...]}`; single entities bare; DELETE → 204.
2. `*_json` columns travel parsed (`headers: Record<string,string>`,
   `modalities_*: string[]`, `capabilities: string[]`) — unlike audit's raw
   `detail_json` string. `capabilities` is a free-form tag list.
3. DB 0/1 flags (`enabled`) travel as JSON booleans.
4. Credential reads return `has_secret: boolean` + `header_names: string[]`;
   header values are write-only; `label` is plain. The service copies fields
   explicitly so a server-echoed `secret` can never be sent back.
5. Credential status vocabulary `unknown | ok | error | cooldown`; test probe
   returns `{ok, status, last_tested, error}`.
6. PATCH sends changed fields only; blank/omitted `secret` keeps the stored one;
   `headers` on PATCH is a full replace, sent only when edited.
7. Manual balance is `balance_usd`; server records `balance_source='manual'`,
   `balance_updated_at/by`. `balance_low_threshold` typed but not in the UI.
8. Offerings embed `price: Price | null`; `PUT /api/offerings/{id}/price` sends
   `source:'manual'` + six nullable amounts, `currency` omitted (USD default).
9. Sync: `POST /api/sync/openrouter` returns the run;
   `GET /api/sync/openrouter/status` returns `{run: SyncRun | null}`; run
   statuses `running | success | stale-if-error | error`; `error: string | null`
   (spec stores detail in `detail_json`).
10. Errors use the M1 envelope `{"error":{code,message,request_id}}`;
    timestamps RFC-3339 UTC.
