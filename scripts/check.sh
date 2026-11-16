#!/usr/bin/env bash
# Compatibility wrapper for the task runner. The source of truth is `just`
# (see Justfile at the repo root); this file exists for callers that still
# invoke the old script path and will be removed.
exec just check
