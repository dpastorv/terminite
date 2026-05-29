#!/usr/bin/env bash
# Thin entrypoint for the lounge-validation lab.
#   ./run.sh                         all experiments
#   ./run.sh e1                      one experiment
#   ./run.sh e4 --cadence 250,500    custom cadence sweep (ms)
# Orchestration lives in run.py (bash can't sanely coordinate multi-agent
# timing). Results land in runs/<timestamp>/summary.md.
set -euo pipefail
cd "$(dirname "$0")"
exec python3 run.py "$@"
