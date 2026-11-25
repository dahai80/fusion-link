# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **Placeholder** — written when `fusion-link/` was still empty. Update or replace once real code lands.

## Status

`fusion-link` is a new, empty project (no source, no build manifest, no git repo yet). Nothing here is authoritative except this note.

## Monorepo Context

This directory lives inside the Fusion monorepo (`/Users/dahai/fusion`), whose root `CLAUDE.md` is the authority for:

- Shared venv: `cd /Users/dahai/fusion && source .venv/bin/activate`
- Per-project git (each `fusion-*` dir is its own git repo; commit inside this directory, never at repo root)
- Python projects: `setuptools`, `pytest tests/`, `ruff check .`; new services must register ports in `architecture/port-registry.yaml` (`python scripts/check-ports.py`)
- Global constraints: English-only GitHub artifacts/comments/logs, 4-space-multiple indentation, no docstrings, logging required

If this project becomes a networked service, pick a port from `architecture/port-registry.yaml` and register it before writing server code.

## To Do When Code Lands

- [ ] `git init` inside `fusion-link/`
- [ ] Replace this file with real build/test/lint commands and architecture notes
- [ ] Add entry to root `/Users/dahai/fusion/CLAUDE.md` project list (correct layer: Core / Platform / Governance / Application / Domain / Shell)
