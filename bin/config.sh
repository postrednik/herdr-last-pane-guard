#!/bin/bash

# Strict plain KEY=VALUE data: allow surrounding whitespace, blank lines, and
# full-line # comments. Quotes, expansion, and inline comments are not supported.
# Each call resets defaults. A missing file succeeds; a file I/O error returns 1.
# These three globals are results consumed by the caller.
# shellcheck disable=SC2034
lpg_load_config() {
    local lpg_config_file=${1-}
    local lpg_line='' lpg_key='' lpg_value='' lpg_number=''
    local lpg_max_seconds=2147483647

    RESTORE_WORKSPACE=ask
    RESTORE_TAB=ask
    LOOP_GUARD_SECONDS=5

    if [[ -z $lpg_config_file ]]; then
        printf 'lpg: cannot read configuration file.\n' >&2
        return 1
    fi
    if [[ ! -e $lpg_config_file && ! -L $lpg_config_file ]]; then
        return 0
    fi
    if [[ ! -f $lpg_config_file || ! -r $lpg_config_file ]]; then
        printf 'lpg: cannot read configuration file.\n' >&2
        return 1
    fi

    # Preserve stderr on a scoped descriptor so an open error cannot reveal the
    # supplied path. The group's redirections restore the caller's descriptors.
    if {
        while IFS= read -r lpg_line || [[ -n $lpg_line ]]; do
            lpg_line=${lpg_line#"${lpg_line%%[![:space:]]*}"}
            lpg_line=${lpg_line%"${lpg_line##*[![:space:]]}"}
            case $lpg_line in
                ''|\#*) continue ;;
                *=*)
                    lpg_key=${lpg_line%%=*}
                    lpg_value=${lpg_line#*=}
                    ;;
                *)
                    printf 'lpg: ignored malformed configuration line.\n' >&3
                    continue
                    ;;
            esac
            lpg_key=${lpg_key#"${lpg_key%%[![:space:]]*}"}
            lpg_key=${lpg_key%"${lpg_key##*[![:space:]]}"}
            lpg_value=${lpg_value#"${lpg_value%%[![:space:]]*}"}
            lpg_value=${lpg_value%"${lpg_value##*[![:space:]]}"}

            case $lpg_key in
                RESTORE_WORKSPACE)
                    case $lpg_value in
                        ask|auto|off) RESTORE_WORKSPACE=$lpg_value ;;
                        *)
                            RESTORE_WORKSPACE=ask
                            printf 'lpg: invalid RESTORE_WORKSPACE; using default.\n' >&3
                            ;;
                    esac
                    ;;
                RESTORE_TAB)
                    case $lpg_value in
                        ask|auto|off) RESTORE_TAB=$lpg_value ;;
                        *)
                            RESTORE_TAB=ask
                            printf 'lpg: invalid RESTORE_TAB; using default.\n' >&3
                            ;;
                    esac
                    ;;
                LOOP_GUARD_SECONDS)
                    LOOP_GUARD_SECONDS=5
                    case $lpg_value in
                        ''|*[!0123456789]*)
                            printf 'lpg: invalid LOOP_GUARD_SECONDS; using default.\n' >&3
                            ;;
                        *)
                            lpg_number=${lpg_value#"${lpg_value%%[!0]*}"}
                            lpg_number=${lpg_number:-0}
                            # Equal-length decimal text compares numerically without
                            # arithmetic, so even huge input cannot overflow.
                            # shellcheck disable=SC2071
                            if [[ ${#lpg_number} -gt ${#lpg_max_seconds} ]] ||
                                { [[ ${#lpg_number} -eq ${#lpg_max_seconds} ]] &&
                                  [[ $lpg_number > $lpg_max_seconds ]]; }; then
                                printf 'lpg: invalid LOOP_GUARD_SECONDS; using default.\n' >&3
                            else
                                LOOP_GUARD_SECONDS=$lpg_number
                            fi
                            ;;
                    esac
                    ;;
                *)
                    printf 'lpg: ignored unknown configuration key.\n' >&3
                    ;;
            esac
        done
    } 3>&2 2>/dev/null < "$lpg_config_file"; then
        return 0
    fi

    printf 'lpg: cannot read configuration file.\n' >&2
    return 1
}
