default:
    @just --list

# Build all crates (excluding GPUI app which needs platform libs)
build:
    cargo build --workspace --exclude rift-app

# Lint with zero warnings policy
lint:
    cargo clippy --workspace --exclude rift-app -- -D warnings

# Format all code
fmt:
    cargo fmt --all

# Check formatting without modifying
fmt-check:
    cargo fmt --all -- --check

# Run all tests
test:
    cargo test --workspace --exclude rift-app

# Full CI check (format + lint + test)
ci: fmt-check lint test

# Create an isolated headless worktree for an agent (off develop, own target, no
# GPU build). Pass an issue number to flip it to In Progress on the board.
agent-worktree branch issue="":
    #!/usr/bin/env bash
    set -euo pipefail
    git worktree add ../rift-worktrees/{{replace(branch, "/", "-")}} -b {{branch}} develop
    echo "Worktree ready: ../rift-worktrees/{{replace(branch, "/", "-")}} (branch {{branch}})"
    if [ -n "{{issue}}" ]; then
      scripts/set-issue-status.sh "{{issue}}" "In Progress"
    fi
    echo "Verify there headless: just lint && just test"

# Remove an agent worktree after merge
agent-worktree-rm branch:
    git worktree remove ../rift-worktrees/{{replace(branch, "/", "-")}}

# Open an interactive claude reviewer for a branch in its own tmux pane, with a
# fresh context. It writes its verdict to .claude/review-<branch>.md and stays
# interactive for follow-up. Run from the main checkout, inside tmux.
review-pane branch:
    #!/usr/bin/env bash
    set -euo pipefail
    branch="{{branch}}"
    dashed="${branch//\//-}"
    wt="../rift-worktrees/$dashed"
    if [ -z "${TMUX:-}" ]; then
      echo "review-pane: must run inside a tmux session" >&2; exit 1
    fi
    if [ ! -d "$wt" ]; then
      echo "review-pane: no worktree at $wt (create it with 'just agent-worktree $branch')" >&2; exit 1
    fi
    wt_abs=$(cd "$wt" && pwd)
    mkdir -p .claude
    verdict="$(pwd)/.claude/review-$dashed.md"
    panefile="$(pwd)/.claude/review-$dashed.pane"
    prompt="Review the git branch '$branch' for the rift project; you are in its worktree. Inspect the diff with 'git diff develop...HEAD' and judge correctness, architecture-rule compliance (see CLAUDE.md: agent-agnostic core, no .unwrap() in libs, crate boundaries, no clone() to satisfy the borrow checker) and test coverage. Write your verdict to $verdict as markdown whose first line is 'VERDICT: APPROVE' or 'VERDICT: REQUEST_CHANGES', followed by the findings. Then summarize for me and stay available for follow-up."
    pane=$(tmux split-window -h -P -F '#{pane_id}' -c "$wt_abs" "command claude")
    tmux select-pane -t "$pane" -T "review:$branch"
    echo "$pane" > "$panefile"
    # Wait for claude to replace the launching shell before sending the prompt.
    for _ in $(seq 1 30); do
      cur=$(tmux display -p -t "$pane" '#{pane_current_command}')
      if [ "$cur" != "bash" ] && [ "$cur" != "sh" ]; then break; fi
      sleep 0.5
    done
    sleep 1
    tmux send-keys -t "$pane" -l "$prompt"
    tmux send-keys -t "$pane" Enter
    echo "review-pane: launched $pane reviewing $branch; verdict -> $verdict"

# Tear down a branch's review pane and verdict/sidecar files (best-effort).
review-pane-rm branch:
    #!/usr/bin/env bash
    set -euo pipefail
    branch="{{branch}}"
    dashed="${branch//\//-}"
    panefile=".claude/review-$dashed.pane"
    if [ -n "${TMUX:-}" ] && [ -f "$panefile" ]; then
      tmux kill-pane -t "$(cat "$panefile")" 2>/dev/null || true
    fi
    rm -f "$panefile" ".claude/review-$dashed.md"

# Wait for a PR's checks to finish. Green only when every check is COMPLETED and
# passing; an empty or still-running rollup keeps waiting (bounded). Exit 0=green,
# 1=a check failed, 2=timeout.
pr-wait n interval="30":
    #!/usr/bin/env bash
    set -euo pipefail
    pr="{{n}}"
    interval="{{interval}}"
    max_attempts=60
    for i in $(seq 1 "$max_attempts"); do
      roll=$(gh pr view "$pr" --json statusCheckRollup --jq '.statusCheckRollup')
      total=$(jq 'length' <<<"$roll")
      pending=$(jq '[.[] | select(.status != "COMPLETED")] | length' <<<"$roll")
      failed=$(jq -r '[.[] | select(.status == "COMPLETED" and (.conclusion | test("SUCCESS|NEUTRAL|SKIPPED") | not)) | .name] | join(",")' <<<"$roll")
      echo "[pr-wait $i] checks=$total pending=$pending failed=[$failed]" >&2
      if [ "$total" -gt 0 ] && [ "$pending" -eq 0 ]; then
        if [ -z "$failed" ]; then echo "GREEN"; exit 0; fi
        echo "FAILED: $failed" >&2; exit 1
      fi
      sleep "$interval"
    done
    echo "TIMEOUT: checks did not finish after $max_attempts attempts" >&2
    exit 2

# Squash-merge a green PR, then clean up its worktree/branch and ff-sync develop.
# Remote-only: refreshes the branch server-side when behind (no force-push), never
# touches local state before the merge lands. Run from the main checkout on develop.
pr-merge n:
    #!/usr/bin/env bash
    set -euo pipefail
    pr="{{n}}"
    repo=$(gh repo view --json nameWithOwner --jq '.nameWithOwner')
    branch=$(gh pr view "$pr" --json headRefName --jq '.headRefName')
    wt="../rift-worktrees/${branch//\//-}"

    # 1. Wait for green, refreshing the branch while it is behind develop.
    updates=0
    while :; do
      if ! just pr-wait "$pr"; then
        echo "pr-merge: checks not green, aborting" >&2; exit 1
      fi
      state=$(gh pr view "$pr" --json mergeStateStatus --jq '.mergeStateStatus')
      echo "pr-merge: mergeStateStatus=$state" >&2
      case "$state" in
        BEHIND)
          updates=$((updates + 1))
          if [ "$updates" -gt 5 ]; then
            echo "pr-merge: still behind after $updates updates, aborting" >&2; exit 1
          fi
          old=$(gh pr view "$pr" --json headRefOid --jq '.headRefOid')
          echo "pr-merge: behind develop, updating branch (update $updates)" >&2
          gh api -X PUT "repos/$repo/pulls/$pr/update-branch" >/dev/null
          for _ in $(seq 1 20); do
            sleep 3
            new=$(gh pr view "$pr" --json headRefOid --jq '.headRefOid')
            [ "$new" != "$old" ] && break
          done
          ;;
        CLEAN|UNSTABLE|HAS_HOOKS|MERGEABLE)
          break ;;
        BLOCKED)
          echo "pr-merge: BLOCKED — required checks or protection not satisfied" >&2; exit 1 ;;
        DIRTY)
          echo "pr-merge: DIRTY — merge conflicts, resolve manually" >&2; exit 1 ;;
        *)
          echo "pr-merge: unexpected mergeStateStatus $state, aborting" >&2; exit 1 ;;
      esac
    done

    # 2. Remote squash-merge (no --delete-branch: it would fail on the live worktree
    #    and muddy the exit code; the branch is cleaned up explicitly below).
    gh pr merge "$pr" --squash

    # 3. Close the review pane (best-effort), then clean up worktree and refs.
    just review-pane-rm "$branch" 2>/dev/null || true
    if [ -d "$wt" ]; then
      git worktree remove "$wt"
    fi
    git branch -D "$branch" 2>/dev/null || true
    gh api -X DELETE "repos/$repo/git/refs/heads/$branch" >/dev/null 2>&1 || true

    # 4. Fast-forward local develop when run from the main checkout.
    if [ "$(git rev-parse --abbrev-ref HEAD)" = "develop" ]; then
      git fetch origin --prune
      git merge --ff-only origin/develop
    fi

    echo "pr-merge: merged #$pr"

# Build daemon release binary for Linux (musl)
release-daemon:
    cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl

# Run daemon locally
run-daemon *ARGS:
    cargo run -p rift-daemon -- {{ARGS}}

# SSH config (overridable via env)
export RIFT_SSH_HOST := env("RIFT_SSH_HOST", "127.0.0.1")
export RIFT_SSH_USER := env("RIFT_SSH_USER", "developer")
export RIFT_SSH_PORT := env("RIFT_SSH_PORT", "22")
export RIFT_SSH_KEY := env("RIFT_SSH_KEY", home_directory() / ".ssh" / "id_rsa")
windows_ssh_key := env("RIFT_WINDOWS_SSH_KEY", "C:\\Users\\skrischer\\.ssh\\id_rsa")
windows_exe := "target/x86_64-pc-windows-gnu/debug/rift.exe"

dev:
    WAYLAND_DISPLAY="" \
    RUST_LOG=rift=debug,rift_ssh=debug \
    RIFT_SSH_HOST="{{RIFT_SSH_HOST}}" \
    RIFT_SSH_USER="{{RIFT_SSH_USER}}" \
    RIFT_SSH_PORT="{{RIFT_SSH_PORT}}" \
    RIFT_SSH_KEY="{{RIFT_SSH_KEY}}" \
    cargo run -p rift-app

# Watch for changes: lint then rebuild (requires cargo-watch)
dev-watch:
    WAYLAND_DISPLAY="" \
    RUST_LOG=rift=debug,rift_ssh=debug \
    RIFT_SSH_HOST="{{RIFT_SSH_HOST}}" \
    RIFT_SSH_USER="{{RIFT_SSH_USER}}" \
    RIFT_SSH_PORT="{{RIFT_SSH_PORT}}" \
    RIFT_SSH_KEY="{{RIFT_SSH_KEY}}" \
    cargo watch -x 'clippy --workspace -- -D warnings' -x 'run -p rift-app'

# Build and run native Windows .exe (cross-compiled via MinGW)
dev-windows:
    cargo build -p rift-app --target x86_64-pc-windows-gnu
    -taskkill.exe /F /IM rift.exe 2>/dev/null
    export WSLENV="RUST_LOG:RIFT_SSH_HOST:RIFT_SSH_USER:RIFT_SSH_PORT:RIFT_SSH_KEY" && \
    export RUST_LOG=rift=debug,rift_ssh=debug && \
    export RIFT_SSH_HOST="{{RIFT_SSH_HOST}}" && \
    export RIFT_SSH_USER="{{RIFT_SSH_USER}}" && \
    export RIFT_SSH_PORT="{{RIFT_SSH_PORT}}" && \
    export RIFT_SSH_KEY="{{windows_ssh_key}}" && \
    {{windows_exe}}

# Watch for changes and rebuild+run Windows .exe (requires cargo-watch)
dev-windows-watch:
    cargo watch -s 'just dev-windows'

# Build Windows .exe without running
build-windows:
    cargo build -p rift-app --target x86_64-pc-windows-gnu

# Check licenses (requires cargo-deny)
deny:
    cargo deny check licenses
