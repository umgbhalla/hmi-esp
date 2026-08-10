#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
lock_file="$repo_root/vendor/sources.lock"
upstream_root="$repo_root/vendor/upstream"

mkdir -p "$upstream_root"

git_vendor() {
    git -c core.fsmonitor=false "$@"
}

while IFS='|' read -r name url commit directory license; do
    case "$name" in
        ''|'#'*) continue ;;
    esac

    checkout="$upstream_root/$directory"
    if [ ! -d "$checkout/.git" ]; then
        printf 'CLONE    %s\n' "$name"
        mkdir -p "$checkout"
        git_vendor -C "$checkout" init --quiet
        git_vendor -C "$checkout" remote add origin "$url"
    else
        dirty=$(git_vendor -C "$checkout" status --porcelain)
        if [ -n "$dirty" ]; then
            printf 'Refusing to replace dirty checkout: %s\n' "$checkout" >&2
            exit 1
        fi

        actual_url=$(git_vendor -C "$checkout" remote get-url origin)
        if [ "$actual_url" != "$url" ]; then
            printf 'Remote mismatch for %s: %s\n' "$name" "$actual_url" >&2
            exit 1
        fi
    fi

    if ! git_vendor -C "$checkout" cat-file -e "$commit^{commit}" 2>/dev/null; then
        printf 'FETCH    %s %s\n' "$name" "$commit"
        git_vendor -C "$checkout" fetch --depth 1 origin "$commit"
    fi

    git_vendor -C "$checkout" checkout --quiet --detach "$commit"
    printf 'PINNED   %s %s %s\n' "$name" "$commit" "$license"
done < "$lock_file"

"$script_dir/vendor-check.sh"
