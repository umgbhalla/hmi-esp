#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
lock_file="$repo_root/vendor/sources.lock"
upstream_root="$repo_root/vendor/upstream"
failed=0

git_vendor() {
    git -c core.fsmonitor=false "$@"
}

while IFS='|' read -r name url commit directory license; do
    case "$name" in
        ''|'#'*) continue ;;
    esac

    checkout="$upstream_root/$directory"
    if [ ! -d "$checkout/.git" ]; then
        printf 'MISSING  %s\n' "$name"
        failed=1
        continue
    fi

    actual_url=$(git_vendor -C "$checkout" remote get-url origin 2>/dev/null || true)
    actual_commit=$(git_vendor -C "$checkout" rev-parse HEAD 2>/dev/null || true)
    dirty=$(git_vendor -C "$checkout" status --porcelain 2>/dev/null || true)

    if [ "$actual_url" != "$url" ]; then
        printf 'REMOTE   %s expected=%s actual=%s\n' "$name" "$url" "$actual_url"
        failed=1
    elif [ "$actual_commit" != "$commit" ]; then
        printf 'REVISION %s expected=%s actual=%s\n' "$name" "$commit" "$actual_commit"
        failed=1
    elif [ -n "$dirty" ]; then
        printf 'DIRTY    %s\n' "$name"
        failed=1
    else
        printf 'OK       %s %s %s\n' "$name" "$commit" "$license"
    fi
done < "$lock_file"

exit "$failed"
