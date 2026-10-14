#!/bin/bash
set -u

test_script_dir=${BASH_SOURCE[0]%/*}
test_root=$(cd "$test_script_dir/.." && pwd) || exit 1
test_tmp=$(mktemp -d "$test_root/.lpg-config-tests.XXXXXX") || exit 1
trap 'rm -rf -- "$test_tmp"' EXIT

checks=0
failures=0
config_file=$test_tmp/config.env
stdout_file=$test_tmp/stdout
stderr_file=$test_tmp/stderr

check_equal() {
    checks=$((checks + 1))
    if [[ $2 != "$3" ]]; then
        failures=$((failures + 1))
        # Labels are fixed test descriptions; never print input or captured logs.
        printf 'FAIL: %s\n' "$1" >&2
    fi
}

check_config() {
    check_equal "$1: RESTORE_WORKSPACE" "$2" "$RESTORE_WORKSPACE"
    check_equal "$1: RESTORE_TAB" "$3" "$RESTORE_TAB"
    check_equal "$1: LOOP_GUARD_SECONDS" "$4" "$LOOP_GUARD_SECONDS"
}

check_silent() {
    check_equal "$1: no stderr" '' "$(< "$stderr_file")"
}

check_warning() {
    local warning_present=no
    [[ ! -s $stderr_file ]] || warning_present=yes
    check_equal "$1: warning present" yes "$warning_present"
}

load_case() {
    local case_name=$1 case_path=$2 expected_rc=${3:-0} actual_rc=0
    local saved_flags=$- saved_cwd=$PWD saved_traps
    saved_traps=$(trap -p)
    lpg_load_config "$case_path" > "$stdout_file" 2> "$stderr_file" || actual_rc=$?
    check_equal "$case_name: return status" "$expected_rc" "$actual_rc"
    check_equal "$case_name: no stdout" '' "$(< "$stdout_file")"
    check_equal "$case_name: shell flags preserved" "$saved_flags" "$-"
    check_equal "$case_name: working directory preserved" "$saved_cwd" "$PWD"
    check_equal "$case_name: traps preserved" "$saved_traps" "$(trap -p)"
}

source_flags=$-
source_cwd=$PWD
source_traps=$(trap -p)
# shellcheck source=bin/config.sh
. "$test_root/bin/config.sh"
check_equal 'source: shell flags preserved' "$source_flags" "$-"
check_equal 'source: working directory preserved' "$source_cwd" "$PWD"
check_equal 'source: traps preserved' "$source_traps" "$(trap -p)"

RESTORE_WORKSPACE=auto
RESTORE_TAB=off
LOOP_GUARD_SECONDS=99
load_case missing "$test_tmp/missing.env"
check_config missing ask ask 5
check_silent missing

: > "$config_file"
RESTORE_WORKSPACE=off
RESTORE_TAB=auto
LOOP_GUARD_SECONDS=99
load_case empty "$config_file"
check_config empty ask ask 5
check_silent empty

printf '\n \t\n# comment\n \t# comment\n' > "$config_file"
load_case comments "$config_file"
check_config comments ask ask 5
check_silent comments

for mode in ask auto off; do
    printf 'RESTORE_WORKSPACE=%s\nRESTORE_TAB=%s\n' "$mode" "$mode" > "$config_file"
    load_case "valid mode $mode" "$config_file"
    check_config "valid mode $mode" "$mode" "$mode" 5
    check_silent "valid mode $mode"
done

for number_case in 0:0 0000:0 5:5 0005:5 0008:8 0009:9 00010:10 2147483647:2147483647 0002147483647:2147483647; do
    number=${number_case%%:*}
    expected_number=${number_case#*:}
    printf 'LOOP_GUARD_SECONDS=%s\n' "$number" > "$config_file"
    load_case "valid number $number" "$config_file"
    check_config "valid number $number" ask ask "$expected_number"
    check_silent "valid number $number"
done

invalid_number_index=0
for number in '' -1 +1 1.0 0x10 1e3 '1 2' '1=2' 2147483648 2147483649 9999999999 10000000000; do
    invalid_number_index=$((invalid_number_index + 1))
    printf 'LOOP_GUARD_SECONDS=19\nLOOP_GUARD_SECONDS=%s\n' "$number" > "$config_file"
    load_case "invalid number $invalid_number_index" "$config_file"
    check_config "invalid number $invalid_number_index" ask ask 5
    check_warning "invalid number $invalid_number_index"
done

printf -v zero_digits '%02000d' 0
huge_digits=${zero_digits//0/9}
printf 'LOOP_GUARD_SECONDS=19\nLOOP_GUARD_SECONDS=%s\n' "$huge_digits" > "$config_file"
load_case 'huge digits' "$config_file"
check_config 'huge digits' ask ask 5
check_warning 'huge digits'
printf 'LOOP_GUARD_SECONDS=%s9\n' "$zero_digits" > "$config_file"
load_case 'huge zero prefix' "$config_file"
check_config 'huge zero prefix' ask ask 9
check_silent 'huge zero prefix'
printf 'LOOP_GUARD_SECONDS=%s\n' "$zero_digits" > "$config_file"
load_case 'huge zero value' "$config_file"
check_config 'huge zero value' ask ask 0
check_silent 'huge zero value'

invalid_mode_index=0
for mode in '' ASK Auto false '"auto"' "'off'" 'auto # comment' 'ask=auto' '\auto'; do
    invalid_mode_index=$((invalid_mode_index + 1))
    printf 'RESTORE_WORKSPACE=auto\nRESTORE_TAB=off\nRESTORE_WORKSPACE=%s\nRESTORE_TAB=%s\n' \
        "$mode" "$mode" > "$config_file"
    load_case "invalid mode $invalid_mode_index" "$config_file"
    check_config "invalid mode $invalid_mode_index" ask ask 5
    check_warning "invalid mode $invalid_mode_index"
done

printf 'RESTORE_WORKSPACE=off\nRESTORE_WORKSPACE=auto\nRESTORE_TAB=auto\nRESTORE_TAB=off\nLOOP_GUARD_SECONDS=8\nLOOP_GUARD_SECONDS=0009\n' > "$config_file"
load_case 'valid duplicates' "$config_file"
check_config 'valid duplicates' auto off 9
check_silent 'valid duplicates'

printf '\r\n \t# comment\r\n \tRESTORE_WORKSPACE \t= \tauto \t\r\nRESTORE_TAB = off\r\n LOOP_GUARD_SECONDS = 0005 \r' > "$config_file"
load_case 'CRLF whitespace and final line' "$config_file"
check_config 'CRLF whitespace and final line' auto off 5
check_silent 'CRLF whitespace and final line'
printf 'RESTORE_WORKSPACE=off' > "$config_file"
load_case 'no final newline' "$config_file"
check_config 'no final newline' off ask 5
check_silent 'no final newline'

export LPG_CONFIG_TEST_UNKNOWN=preserved
unset LPG_CONFIG_TEST_UNSET
saved_path=$PATH
printf 'LPG_CONFIG_TEST_UNKNOWN=changed\nLPG_CONFIG_TEST_UNSET=changed\nPATH=lpg-test-secret-path\n' > "$config_file"
load_case 'unknown keys' "$config_file"
check_config 'unknown keys' ask ask 5
check_warning 'unknown keys'
check_equal 'unknown key: existing exported variable preserved' preserved "$LPG_CONFIG_TEST_UNKNOWN"
check_equal 'unknown key: unset variable remains unset' '' "${LPG_CONFIG_TEST_UNSET+x}"
check_equal 'unknown key: PATH preserved' "$saved_path" "$PATH"
unknown_log=$(< "$stderr_file")
check_equal 'unknown key: no input leaked' no "$([[ $unknown_log == *lpg-test-secret* || $unknown_log == *LPG_CONFIG_TEST* ]] && printf yes || printf no)"

printf 'lpg-test-secret-malformed\nexport RESTORE_WORKSPACE=auto\n=private\nRESTORE_TAB auto\n' > "$config_file"
load_case 'malformed lines' "$config_file"
check_config 'malformed lines' ask ask 5
check_warning 'malformed lines'
malformed_log=$(< "$stderr_file")
check_equal 'malformed lines: no input leaked' no "$([[ $malformed_log == *lpg-test-secret* || $malformed_log == *private* || $malformed_log == *export* ]] && printf yes || printf no)"

marker=$test_tmp/executed
# Keep shell syntax literal so the loader, rather than this fixture, receives it.
# shellcheck disable=SC2016
printf 'RESTORE_WORKSPACE=auto\nRESTORE_TAB=off\nLOOP_GUARD_SECONDS=19\nRESTORE_WORKSPACE=lpg-test-secret-value$(printf executed > "%s")\nRESTORE_TAB=`printf executed > "%s"`\nLOOP_GUARD_SECONDS=1; printf executed > "%s"\nlpg-test-secret-key=$(printf executed > "%s")\nlpg-test-secret-line\n' \
    "$marker" "$marker" "$marker" "$marker" > "$config_file"
load_case 'malicious values' "$config_file"
check_config 'malicious values' ask ask 5
check_warning 'malicious values'
check_equal 'malicious values: never executed' no "$([[ -e $marker ]] && printf yes || printf no)"
malicious_log=$(< "$stderr_file")
check_equal 'malicious values: no secret or command leaked' no "$([[ $malicious_log == *lpg-test-secret* || $malicious_log == *executed* || $malicious_log == *printf* || $malicious_log == *"$test_tmp"* ]] && printf yes || printf no)"

mkdir "$test_tmp/lpg-test-secret-directory" || exit 1
RESTORE_WORKSPACE=auto
RESTORE_TAB=off
LOOP_GUARD_SECONDS=19
load_case 'directory I/O error' "$test_tmp/lpg-test-secret-directory" 1
check_config 'directory I/O error' ask ask 5
check_equal 'directory I/O error: generic diagnostic only' 'lpg: cannot read configuration file.' "$(< "$stderr_file")"

ln -s "$test_tmp/missing-target" "$test_tmp/dangling" || exit 1
load_case 'dangling symlink I/O error' "$test_tmp/dangling" 1
check_config 'dangling symlink I/O error' ask ask 5
check_equal 'dangling symlink I/O error: generic diagnostic only' 'lpg: cannot read configuration file.' "$(< "$stderr_file")"

mkfifo "$test_tmp/fifo" || exit 1
load_case 'FIFO I/O error' "$test_tmp/fifo" 1
check_config 'FIFO I/O error' ask ask 5
check_equal 'FIFO I/O error: generic diagnostic only' 'lpg: cannot read configuration file.' "$(< "$stderr_file")"

printf 'RESTORE_WORKSPACE=auto\n' > "$config_file"
descriptor_rc=0
(
    exec 3> "$test_tmp/descriptor"
    lpg_load_config "$config_file" > "$stdout_file" 2> "$stderr_file" || exit 1
    printf preserved >&3
) || descriptor_rc=$?
check_equal 'caller descriptor: load succeeds' 0 "$descriptor_rc"
check_equal 'caller descriptor: descriptor 3 preserved' preserved "$(< "$test_tmp/descriptor")"

printf 'config tests: %d checks, %d failures\n' "$checks" "$failures"
[[ $failures -eq 0 ]]
