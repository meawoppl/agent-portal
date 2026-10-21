# Runs as a separate launchd job, never as a child of the job being unloaded.
# All paths are positional arguments, not interpolated shell source.
ctl=$1
domain=$2
target=$3
plist=$4
status=$5
helper=$6
printf '%s\n' 'Reloading service definition.' > "$status"
# bootout may report an absent job: bootstrap is still the recovery action.
"$ctl" bootout "$target"
attempt=0
while [ "$attempt" -lt 30 ]; do
    if "$ctl" bootstrap "$domain" "$plist"; then
        if "$ctl" kickstart "$target"; then
            printf '%s\n' 'Service definition reloaded and start requested.' > "$status"
            "$ctl" remove "$helper"
            exit 0
        fi
        break
    fi
    attempt=$((attempt + 1))
    sleep 1
done
printf '%s\n' 'RESTART FAILED: inspect restart.log and run agent-portal service start.' > "$status"
exit 1
