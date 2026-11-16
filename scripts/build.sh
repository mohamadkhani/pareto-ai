#!/usr/bin/env bash
# Compatibility wrapper for the task runner. The source of truth is `just`
# (see Justfile at the repo root); this file exists for callers that still
# invoke the old script path and will be removed.
if [[ "${1:-}" == "--no-default-features" ]]; then
  exec just build-api
fi
exec just build
