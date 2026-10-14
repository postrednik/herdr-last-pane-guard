#!/bin/bash
# Fixture inputs are JSON data and are never executed as shell code.
set -u
set -o pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd) || exit 1
project_root=$(CDPATH='' cd -- "$script_dir/.." && pwd) || exit 1
planner="$project_root/bin/plan.jq"
fixture_dir="$script_dir/fixtures"

if ! command -v jq >/dev/null 2>&1; then
    printf 'FAIL: jq 1.6 or later is required\n' >&2
    exit 1
fi
if [ ! -r "$planner" ]; then
    printf 'FAIL: cannot read bin/plan.jq\n' >&2
    exit 1
fi

umask 077
test_tmp=$(mktemp -d "${TMPDIR:-/tmp}/pane-guard-decision.XXXXXX") || exit 1
trap 'rm -rf -- "$test_tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

shopt -s nullglob
fixtures=("$fixture_dir/"*.json)
if [ "${#fixtures[@]}" -eq 0 ]; then
    printf 'FAIL: no decision fixtures found\n' >&2
    exit 1
fi

total=0
failed=0

report_failure() {
    failed=$((failed + 1))
    printf 'FAIL %s: %s\n' "$name" "$1" >&2
    if [ -s "$test_tmp/error" ]; then
        cat "$test_tmp/error" >&2
    fi
    printf '  expected: ' >&2
    jq -c '.expect' "$fixture" >&2
    printf '  actual: ' >&2
    if [ -s "$test_tmp/actual.json" ]; then
        cat "$test_tmp/actual.json" >&2
    else
        printf '<no output>\n' >&2
    fi
}

for fixture in "${fixtures[@]}"; do
    total=$((total + 1))
    name=${fixture##*/}
    : > "$test_tmp/actual.json"
    : > "$test_tmp/error"

    if ! jq -s -e '
        if length == 1 then
          .[0]
          | if (.input | type) == "object"
            and (.expect | type) == "object"
            and (.expect | has("action") and has("reason")) then
              .input
            else error("fixture requires input and expect objects with action/reason")
            end
        else error("fixture must contain exactly one JSON value")
        end
    ' "$fixture" > "$test_tmp/input.json" 2> "$test_tmp/error"; then
        report_failure 'invalid fixture'
        continue
    fi

    if ! jq -f "$planner" "$test_tmp/input.json" \
        > "$test_tmp/actual.json" 2> "$test_tmp/error"; then
        report_failure 'planner exited nonzero'
        continue
    fi

    # Require exactly one object and compare only fields specified by expect.
    if ! jq -s -e --slurpfile fixture "$fixture" '
        . as $actual
        | $fixture[0].expect as $expect
        | if ($actual | length) != 1 then false
          elif ($actual[0] | type) != "object" then false
          else all($expect | to_entries[];
            . as $field
            | ($actual[0] | has($field.key))
              and ($actual[0][$field.key] == $field.value))
          end
    ' "$test_tmp/actual.json" > /dev/null 2> "$test_tmp/error"; then
        report_failure 'expected fields differ or output is not exactly one object'
    fi
done

printf '%d/%d decision fixtures passed\n' "$((total - failed))" "$total"
if [ "$failed" -ne 0 ]; then
    exit 1
fi
