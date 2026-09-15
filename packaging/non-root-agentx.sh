#!/bin/sh
set -eu

work=$(mktemp -d)
master_pid=
subagent_pid=
created_group=no
created_user=no
agentx_dir=/var/agentx
agentx_socket=$agentx_dir/master
agentx_dir_existed=no
remove_agentx_socket=no
subagent_user=agentx-ifstack
subagent_group=agentx-ifstack

fail() {
    printf 'Non-root AgentX scenario failed: %s\n' "$*" >&2
    exit 1
}

show_logs() {
    if [ -s "$work/snmpd.log" ]; then
        printf '%s\n' 'snmpd log:' >&2
        cat "$work/snmpd.log" >&2
    fi
    if [ -s "$work/subagent.log" ]; then
        printf '%s\n' 'agentx-ifstack log:' >&2
        cat "$work/subagent.log" >&2
    fi
    if [ -s "$work/snmpwalk.err" ]; then
        printf '%s\n' 'latest snmpwalk error:' >&2
        cat "$work/snmpwalk.err" >&2
    fi
}

stop_process() {
    process_pid=$1
    process_name=$2
    if [ -z "$process_pid" ]; then
        return
    fi

    if kill -0 "$process_pid" 2>/dev/null; then
        kill "$process_pid" 2>/dev/null || true
        remaining=5
        while kill -0 "$process_pid" 2>/dev/null && [ "$remaining" -gt 0 ]; do
            process_state=$(awk '{ print $3 }' "/proc/$process_pid/stat" \
                2>/dev/null || true)
            if [ "$process_state" = Z ]; then
                break
            fi
            sleep 1
            remaining=$((remaining - 1))
        done
        if kill -0 "$process_pid" 2>/dev/null && [ "$remaining" -eq 0 ]; then
            printf 'Forcing %s to stop after five seconds\n' "$process_name" >&2
            kill -KILL "$process_pid" 2>/dev/null || true
        fi
    fi
    wait "$process_pid" 2>/dev/null || true
}

cleanup() {
    status=$?
    cleanup_status=$status
    trap - EXIT HUP INT TERM
    set +e

    stop_process "$subagent_pid" agentx-ifstack
    stop_process "$master_pid" snmpd
    if [ "$remove_agentx_socket" = yes ]; then
        if ! rm -f "$agentx_socket"; then
            printf 'Could not remove %s during cleanup\n' "$agentx_socket" >&2
            cleanup_status=1
        fi
    fi
    if [ "$agentx_dir_existed" = no ] && [ ! -L "$agentx_dir" ] && \
        [ -d "$agentx_dir" ] && ! rmdir "$agentx_dir"; then
        printf 'Could not remove %s during cleanup\n' "$agentx_dir" >&2
        cleanup_status=1
    fi
    if [ "$created_user" = yes ] && \
        getent passwd "$subagent_user" >/dev/null 2>&1; then
        if ! userdel "$subagent_user"; then
            printf 'Could not remove user %s during cleanup\n' \
                "$subagent_user" >&2
            cleanup_status=1
        fi
    fi
    if [ "$created_group" = yes ] && \
        getent group "$subagent_group" >/dev/null 2>&1; then
        if ! groupdel "$subagent_group"; then
            printf 'Could not remove group %s during cleanup\n' \
                "$subagent_group" >&2
            cleanup_status=1
        fi
    fi
    if ! rm -rf "$work"; then
        printf 'Could not remove temporary directory %s\n' "$work" >&2
        cleanup_status=1
    fi
    exit "$cleanup_status"
}

trap cleanup EXIT
trap 'exit 1' HUP INT TERM

if [ -L "$agentx_dir" ]; then
    fail "$agentx_dir is a symbolic link"
fi
if [ -d "$agentx_dir" ]; then
    agentx_dir_existed=yes
elif [ -e "$agentx_dir" ]; then
    fail "$agentx_dir exists and is not a directory"
fi
if [ -e "$agentx_socket" ] || [ -L "$agentx_socket" ]; then
    fail "$agentx_socket already exists"
fi
remove_agentx_socket=yes

if MANWIDTH=120 MANPAGER=cat PAGER=cat man snmpd.conf \
    > "$work/snmpd.conf.man" 2> "$work/snmpd.conf.man.err"; then
    if col -b < "$work/snmpd.conf.man" > "$work/snmpd.conf.txt"; then
        if manual_section=$(awk '
            found && /^[[:space:]]*$/ { exit }
            /^[[:space:]]*agentXPerms[[:space:]]/ { found = 1 }
            found { print }
            END { if (!found) exit 1 }
        ' "$work/snmpd.conf.txt"); then
            printf '%s\n' 'Installed snmpd.conf agentXPerms section:'
            printf '%s\n' "$manual_section"
        else
            printf '%s\n' \
                'The installed snmpd.conf manual has no agentXPerms section.'
        fi
    else
        printf '%s\n' 'The installed snmpd.conf manual could not be rendered.'
    fi
else
    printf '%s\n' 'The installed snmpd.conf manual page is missing.'
    if [ -s "$work/snmpd.conf.man.err" ]; then
        cat "$work/snmpd.conf.man.err"
    fi
fi

cat > "$work/snmpd.conf" <<EOF
agentaddress udp:127.0.0.1:1161
rocommunity agentx-ifstack-test 127.0.0.1
master agentx
agentXSocket $agentx_socket
agentXPerms 0660 0755 root $subagent_group
EOF

if getent group "$subagent_group" >/dev/null 2>&1; then
    fail "group $subagent_group already exists"
fi
if getent passwd "$subagent_user" >/dev/null 2>&1; then
    fail "user $subagent_user already exists"
fi
groupadd --system "$subagent_group"
created_group=yes
nologin_shell=$(command -v nologin) || fail "nologin is not installed"
useradd --system --gid "$subagent_group" --no-create-home \
    --home-dir /nonexistent --shell "$nologin_shell" "$subagent_user"
created_user=yes
subagent_gid=$(id -g "$subagent_user")
[ "$(id -u "$subagent_user")" -ne 0 ] || fail "$subagent_user has UID 0"

mkdir "$work/persistent"
SNMP_PERSISTENT_DIR="$work/persistent" \
    snmpd -f -Lo -C -c "$work/snmpd.conf" -p "$work/snmpd.pid" \
    > "$work/snmpd.log" 2>&1 &
master_pid=$!

socket_ready=no
remaining=30
while [ "$remaining" -gt 0 ]; do
    if [ -S "$agentx_socket" ]; then
        socket_metadata=$(stat -c '%a:%u:%g' "$agentx_socket" 2>/dev/null || true)
        if [ "$socket_metadata" = "660:0:$subagent_gid" ]; then
            socket_ready=yes
            break
        fi
    fi
    if ! kill -0 "$master_pid" 2>/dev/null; then
        show_logs
        fail "snmpd exited before creating $agentx_socket"
    fi
    sleep 1
    remaining=$((remaining - 1))
done
if [ "$socket_ready" != yes ]; then
    show_logs
    fail "snmpd did not create $agentx_socket with mode 0660 and ownership root:$subagent_group within 30 seconds"
fi

[ "$(stat -c '%u' "$agentx_dir")" = 0 ] || \
    fail "$agentx_dir is not owned by root"
[ "$(stat -c '%g' "$agentx_dir")" = 0 ] || \
    fail "$agentx_dir is not owned by group root"
if ! setpriv --reuid "$subagent_user" --regid "$subagent_group" --init-groups \
    test -x "$agentx_dir"; then
    fail "$agentx_dir is not traversable by $subagent_user"
fi

grep -Eq '^socket[[:space:]]*=[[:space:]]*"/var/agentx/master"[[:space:]]*$' \
    /etc/agentx-ifstack.toml || \
    fail "the packaged configuration does not select /var/agentx/master"

table_oid=.1.3.6.1.2.1.31.1.2
row_prefix=$table_oid.1.3.
if ! snmpwalk -v2c -c agentx-ifstack-test -t 1 -r 0 -On \
    udp:127.0.0.1:1161 "$table_oid" > "$work/pre-subagent.walk" \
    2> "$work/snmpwalk.err"; then
    show_logs
    fail "the ifStackTable precondition walk failed"
fi
if grep -Fq "$table_oid." "$work/pre-subagent.walk"; then
    printf '%s\n' 'ifStackTable rows before the subagent started:' >&2
    cat "$work/pre-subagent.walk" >&2
    fail "the ifStackTable served rows before agentx-ifstack started"
fi

: > "$work/interface-indexes"
for ifindex_path in /sys/class/net/*/ifindex; do
    [ -f "$ifindex_path" ] || fail "the container has no network interfaces"
    interface_index=$(cat "$ifindex_path")
    case $interface_index in
        ''|*[!0-9]*) fail "$ifindex_path does not contain an interface index" ;;
    esac
    [ "$interface_index" -gt 0 ] || \
        fail "$ifindex_path does not contain a positive interface index"
    printf '%s\n' "$interface_index" >> "$work/interface-indexes"
done
sort -nu "$work/interface-indexes" > "$work/interface-indexes.sorted"
mv "$work/interface-indexes.sorted" "$work/interface-indexes"

setpriv --reuid "$subagent_user" --regid "$subagent_group" --init-groups \
    /usr/bin/agentx-ifstack > "$work/subagent.log" 2>&1 &
subagent_pid=$!

remaining=30
while [ "$remaining" -gt 0 ]; do
    if snmpwalk -v2c -c agentx-ifstack-test -t 1 -r 0 -On \
        udp:127.0.0.1:1161 "$table_oid" > "$work/register.walk" \
        2> "$work/snmpwalk.err" && \
        grep -Fq "$row_prefix" "$work/register.walk"; then
        break
    fi
    if ! kill -0 "$subagent_pid" 2>/dev/null; then
        show_logs
        fail "agentx-ifstack exited before it registered"
    fi
    if ! kill -0 "$master_pid" 2>/dev/null; then
        show_logs
        fail "snmpd exited while agentx-ifstack was registering"
    fi
    sleep 1
    remaining=$((remaining - 1))
done
if [ "$remaining" -eq 0 ]; then
    show_logs
    fail "agentx-ifstack did not register and publish rows after 30 attempts"
fi

if ! kill -0 "$subagent_pid" 2>/dev/null; then
    show_logs
    fail "agentx-ifstack exited after it published rows"
fi
if ! subagent_uids=$(awk '
    $1 == "Uid:" { print $2 ":" $3; found = 1 }
    END { if (!found) exit 1 }
' "/proc/$subagent_pid/status"); then
    show_logs
    fail "could not read the running agentx-ifstack user IDs"
fi
subagent_real_uid=${subagent_uids%%:*}
subagent_effective_uid=${subagent_uids#*:}
[ "$subagent_real_uid" -ne 0 ] || \
    fail "running agentx-ifstack has real UID 0"
[ "$subagent_effective_uid" -ne 0 ] || \
    fail "running agentx-ifstack has effective UID 0"

if ! snmpwalk -v2c -c agentx-ifstack-test -t 1 -r 0 -On \
    udp:127.0.0.1:1161 "$table_oid" > "$work/ifstack.walk" \
    2> "$work/snmpwalk.err"; then
    show_logs
    fail "the ifStackTable walk failed after registration"
fi
[ -s "$work/ifstack.walk" ] || fail "the ifStackTable walk returned no rows"
printf '%s\n' 'Non-root ifStackTable walk:'
cat "$work/ifstack.walk"

awk -v prefix="$row_prefix" -v expected_indexes="$work/interface-indexes" '
    function reject(message) {
        print "Non-root AgentX scenario failed: " message > "/dev/stderr"
        invalid = 1
    }

    BEGIN {
        while ((result = getline interface_index < expected_indexes) > 0) {
            if (interface_index !~ /^[1-9][0-9]*$/) {
                reject("invalid interface index from sysfs: " interface_index)
                continue
            }
            expected_interfaces[interface_index + 0] = 1
            expected_count++
        }
        close(expected_indexes)
        if (result < 0) {
            reject("could not read container interface indexes")
        }
        if (expected_count == 0) {
            reject("the container has no network interfaces")
        }
    }

    {
        oid = $1
        if (index(oid, prefix) != 1) {
            reject("walk returned a row outside ifStackStatus: " oid)
            next
        }
        suffix = substr(oid, length(prefix) + 1)
        components = split(suffix, indexes, ".")
        if (components != 2 || indexes[1] !~ /^[0-9]+$/ ||
                indexes[2] !~ /^[0-9]+$/) {
            reject("walk returned an invalid ifStackStatus instance: " oid)
            next
        }
        if ($2 != "=" || $3 != "INTEGER:" ||
                ($4 != "1" && $4 != "active(1)")) {
            reject("walk returned a non-active ifStackStatus value: " $0)
        }

        higher = indexes[1] + 0
        lower = indexes[2] + 0
        if (higher == 0 && lower == 0) {
            reject("walk returned the invalid boundary row 0.0")
        } else if (higher != 0 && lower != 0) {
            reject("container walk returned a non-boundary row: " suffix)
        } else if (higher == 0) {
            if (!(lower in expected_interfaces)) {
                reject("zero-higher row names an unknown interface: " lower)
            }
            zero_higher[lower] = 1
        } else {
            if (!(higher in expected_interfaces)) {
                reject("zero-lower row names an unknown interface: " higher)
            }
            zero_lower[higher] = 1
        }

        if (rows > 0 && (higher < previous_higher ||
                (higher == previous_higher && lower <= previous_lower))) {
            reject("ifStackStatus rows are not in RFC index order at " suffix)
        }
        previous_higher = higher
        previous_lower = lower
        rows++
    }

    END {
        if (rows == 0) {
            reject("the ifStackTable walk returned no rows")
        }
        for (index_value in expected_interfaces) {
            if (!(index_value in zero_higher)) {
                reject("missing zero-higher boundary row for interface " index_value)
            }
            if (!(index_value in zero_lower)) {
                reject("missing zero-lower boundary row for interface " index_value)
            }
        }
        if (invalid) {
            exit 1
        }
    }
' "$work/ifstack.walk"

printf '%s\n' 'Non-root AgentX scenario passed.'
