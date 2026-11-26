-- 0021_request_log_target: durable target identity of a request (D64).
--
-- `request_logs` kept only the resolved offering id and the final error
-- class/detail; the credential that served the request and the provider's own
-- HTTP status / error code lived only in `telemetry_attempts`, which keeps the
-- newest 200 rows per offering and prunes the rest (D49). The request-log
-- viewer needs the credential name and the origin error status/code for every
-- row, so they are copied onto the durable record at settle time (D64).
-- `credential_id` is the credential that served the request (the final
-- attempt's credential). `upstream_status` and `provider_error_code` are the
-- origin error's provider status and code.
--
-- All three columns are nullable; rows written before this migration read as
-- unknown. `credential_id` references the credential table the same way the
-- rest of the request-log columns reference their tables (ON DELETE SET NULL),
-- so a deleted credential never blocks settlement and never cascades.

ALTER TABLE request_logs ADD COLUMN credential_id TEXT REFERENCES provider_credentials (id) ON DELETE SET NULL;

ALTER TABLE request_logs ADD COLUMN upstream_status INTEGER;

ALTER TABLE request_logs ADD COLUMN provider_error_code TEXT;
