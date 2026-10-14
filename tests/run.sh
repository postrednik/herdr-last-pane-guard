#!/bin/bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
/bin/bash "$ROOT/tests/config.sh"
/bin/bash "$ROOT/tests/decision.sh"
/bin/bash "$ROOT/tests/runtime.sh"
