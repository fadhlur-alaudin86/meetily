#!/usr/bin/env bash
# cleanup.sh - Free disk space by removing build artifacts and caches
#
# Usage:
#   ./scripts/cleanup.sh              # Interactive mode (asks before each step)
#   ./scripts/cleanup.sh --dry-run    # Show what would be deleted, without deleting
#   ./scripts/cleanup.sh --all        # Delete everything without prompting
#   ./scripts/cleanup.sh --rust       # Only clean Rust build artifacts (target/)
#   ./scripts/cleanup.sh --node       # Only clean Node/JS artifacts
#   ./scripts/cleanup.sh --debug      # Only remove debug target (keep release)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN=false
AUTO_ALL=false
MODE="interactive"

# ── Argument parsing ───────────────────────────────────────────────────────────
for arg in "$@"; do
    case "$arg" in
        --dry-run)   DRY_RUN=true ;;
        --all)       AUTO_ALL=true ;;
        --rust)      MODE="rust" ;;
        --node)      MODE="node" ;;
        --debug)     MODE="debug" ;;
        --help|-h)
            echo "Usage: $0 [--dry-run] [--all] [--rust] [--node] [--debug]"
            echo ""
            echo "  --dry-run   Show what would be deleted without actually deleting"
            echo "  --all       Delete all cleanable artifacts without prompting"
            echo "  --rust      Only clean Rust target/ directory"
            echo "  --node      Only clean node_modules, .next, out, tsconfig cache"
            echo "  --debug     Only remove debug build artifacts (keep release)"
            exit 0
            ;;
    esac
done

# ── Helpers ────────────────────────────────────────────────────────────────────
RED='\033[0;31m'
YELLOW='\033[1;33m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

hr() { printf '%s\n' "$(printf '─%.0s' {1..60})"; }

size_of() {
    local path="$1"
    if [ -e "$path" ]; then
        du -sh "$path" 2>/dev/null | awk '{print $1}'
    else
        echo "0"
    fi
}

remove() {
    local path="$1"
    local label="$2"
    local sz
    sz="$(size_of "$path")"

    if [ ! -e "$path" ]; then
        echo -e "  ${CYAN}skip${RESET}  $label (not found)"
        return
    fi

    if $DRY_RUN; then
        echo -e "  ${YELLOW}would remove${RESET}  $label  (${BOLD}${sz}${RESET})"
        return
    fi

    echo -e "  ${RED}removing${RESET}  $label  (${BOLD}${sz}${RESET}) ..."
    rm -rf "$path"
    echo -e "  ${GREEN}done${RESET}"
}

ask() {
    local label="$1"
    local sz="$2"
    if $AUTO_ALL; then
        return 0
    fi
    printf "  Remove %s (%s)? [y/N] " "$label" "$sz"
    read -r answer
    [[ "$answer" =~ ^[Yy]$ ]]
}

total_freed=0
record_freed() {
    local path="$1"
    if [ -e "$path" ]; then
        local bytes
        bytes="$(du -sb "$path" 2>/dev/null | awk '{print $1}')"
        total_freed=$(( total_freed + bytes ))
    fi
}

# ── Header ─────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}Meetily Project Cleanup${RESET}"
hr
echo -e "  Project root : ${CYAN}${REPO_ROOT}${RESET}"
echo -e "  Mode         : ${BOLD}${MODE}${RESET}$(${DRY_RUN} && echo ' (DRY RUN)' || true)"
echo -e "  Auto-confirm : ${BOLD}${AUTO_ALL}${RESET}"
hr

# ── Section 1: Rust / Cargo ────────────────────────────────────────────────────
rust_cleanup() {
    echo ""
    echo -e "${BOLD}[1] Rust build artifacts${RESET}"
    hr

    local target="${REPO_ROOT}/target"

    # debug build (incremental + deps are the biggest wins)
    local debug_sz
    debug_sz="$(size_of "${target}/debug")"
    echo -e "  target/debug size: ${BOLD}${debug_sz}${RESET}"

    if [ "$MODE" = "debug" ]; then
        record_freed "${target}/debug"
        if $DRY_RUN || ask "target/debug (entire debug build)" "$debug_sz"; then
            remove "${target}/debug" "target/debug"
        fi
        return
    fi

    # incremental compilation artifacts (safe to always delete)
    record_freed "${target}/debug/incremental"
    record_freed "${target}/release/incremental"
    if ask "debug+release incremental compilation caches" "$(size_of "${target}/debug/incremental") + $(size_of "${target}/release/incremental")"; then
        remove "${target}/debug/incremental"  "target/debug/incremental"
        remove "${target}/release/incremental" "target/release/incremental"
    fi

    # full debug tree
    record_freed "${target}/debug"
    if ask "entire debug build" "$(size_of "${target}/debug")"; then
        remove "${target}/debug" "target/debug"
    fi

    # release deps (compiled dependency objects; re-linkable from .rlib cache)
    record_freed "${target}/release/deps"
    if ask "release/deps (dep object files)" "$(size_of "${target}/release/deps")"; then
        remove "${target}/release/deps" "target/release/deps"
    fi

    # release build scripts output
    record_freed "${target}/release/build"
    if ask "release/build (build-script artifacts)" "$(size_of "${target}/release/build")"; then
        remove "${target}/release/build" "target/release/build"
    fi

    # tauri bundle artifacts (installers; regenerated by tauri build)
    record_freed "${target}/release/bundle"
    if ask "release/bundle (Tauri installer packages)" "$(size_of "${target}/release/bundle")"; then
        remove "${target}/release/bundle" "target/release/bundle"
    fi

    # entire release tree (nuclear option)
    record_freed "${target}/release"
    if ask "ENTIRE release build (nuclear - forces full rebuild)" "$(size_of "${target}/release")"; then
        remove "${target}/release" "target/release"
    fi

    # cargo check / clippy caches outside target
    local cargo_cache="${HOME}/.cargo/registry/src"
    local cargo_cache_sz
    cargo_cache_sz="$(size_of "${cargo_cache}")"
    echo ""
    echo -e "  ${YELLOW}Note:${RESET} Cargo registry source cache is at ${CYAN}${HOME}/.cargo/registry${RESET} (${cargo_cache_sz})"
    echo -e "        Run ${BOLD}cargo cache --autoclean${RESET} to clean it globally (affects all projects)."
}

# ── Section 2: Node / JS ───────────────────────────────────────────────────────
node_cleanup() {
    echo ""
    echo -e "${BOLD}[2] Node / JavaScript artifacts${RESET}"
    hr

    local frontend="${REPO_ROOT}/frontend"

    record_freed "${frontend}/node_modules"
    if ask "node_modules" "$(size_of "${frontend}/node_modules")"; then
        remove "${frontend}/node_modules" "frontend/node_modules"
    fi

    record_freed "${frontend}/.next"
    if ask ".next (Next.js build cache)" "$(size_of "${frontend}/.next")"; then
        remove "${frontend}/.next" "frontend/.next"
    fi

    record_freed "${frontend}/out"
    if ask "out/ (Next.js static export)" "$(size_of "${frontend}/out")"; then
        remove "${frontend}/out" "frontend/out"
    fi

    record_freed "${frontend}/tsconfig.tsbuildinfo"
    if ask "tsconfig.tsbuildinfo (TypeScript incremental cache)" "$(size_of "${frontend}/tsconfig.tsbuildinfo")"; then
        remove "${frontend}/tsconfig.tsbuildinfo" "frontend/tsconfig.tsbuildinfo"
    fi
}

# ── Section 3: Misc generated files ───────────────────────────────────────────
misc_cleanup() {
    echo ""
    echo -e "${BOLD}[3] Miscellaneous generated files${RESET}"
    hr

    # Tauri gen dir (regenerated by tauri build)
    record_freed "${REPO_ROOT}/frontend/src-tauri/gen"
    if ask "src-tauri/gen (Tauri auto-generated code)" "$(size_of "${REPO_ROOT}/frontend/src-tauri/gen")"; then
        remove "${REPO_ROOT}/frontend/src-tauri/gen" "frontend/src-tauri/gen"
    fi

    # llama-helper build target (separate workspace)
    record_freed "${REPO_ROOT}/llama-helper/target"
    if ask "llama-helper/target (llama-helper build artifacts)" "$(size_of "${REPO_ROOT}/llama-helper/target")"; then
        remove "${REPO_ROOT}/llama-helper/target" "llama-helper/target"
    fi

    # backend Python cache
    record_freed "${REPO_ROOT}/backend/__pycache__"
    if ask "backend/__pycache__ (Python bytecode cache)" "$(size_of "${REPO_ROOT}/backend/__pycache__")"; then
        remove "${REPO_ROOT}/backend/__pycache__" "backend/__pycache__"
    fi

    # Any stray *.pyc files
    if $DRY_RUN; then
        local count
        count="$(find "${REPO_ROOT}" -name "*.pyc" 2>/dev/null | wc -l)"
        echo -e "  ${YELLOW}would remove${RESET}  ${count} *.pyc files"
    else
        if ask "*.pyc files across project" "$(find "${REPO_ROOT}" -name "*.pyc" 2>/dev/null | wc -l) files"; then
            find "${REPO_ROOT}" -name "*.pyc" -delete
            echo -e "  ${GREEN}done${RESET}"
        fi
    fi

    # Windows build tool that ended up in repo
    local vs_exe="${REPO_ROOT}/frontend/vs_buildtools.exe"
    record_freed "${vs_exe}"
    if ask "vs_buildtools.exe (Windows build tool, not needed on Linux)" "$(size_of "${vs_exe}")"; then
        remove "${vs_exe}" "frontend/vs_buildtools.exe"
    fi
}

# ── Run based on mode ──────────────────────────────────────────────────────────
case "$MODE" in
    rust)
        rust_cleanup
        ;;
    node)
        node_cleanup
        ;;
    debug)
        rust_cleanup
        ;;
    interactive|*)
        rust_cleanup
        node_cleanup
        misc_cleanup
        ;;
esac

# ── Summary ────────────────────────────────────────────────────────────────────
echo ""
hr
if $DRY_RUN; then
    echo -e "${YELLOW}Dry-run complete. No files were deleted.${RESET}"
else
    freed_human="$(numfmt --to=iec-i --suffix=B ${total_freed} 2>/dev/null || echo "${total_freed} bytes")"
    echo -e "${GREEN}Cleanup complete.${RESET}"
    echo -e "Estimated space freed: ${BOLD}${freed_human}${RESET}"
fi
echo ""
