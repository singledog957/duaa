#!/usr/bin/env bash
set -euo pipefail

# One-click dependency installer for duaa.
# Modes:
# - fast   : deps for easy_sign.sh + cron workflow
# - stable : deps for Rust daemon build/run

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
MODE=""

log() {
  printf "[install] %s\n" "$*"
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

usage() {
  cat <<'EOF'
Usage: ./install.sh [--mode fast|stable]

Modes:
  fast    Install dependencies for bash+cron workflow only
  stable  Install dependencies for Rust stable daemon and build binary

If --mode is omitted, installer asks interactively.
EOF
}

select_mode() {
  if [[ -n "$MODE" ]]; then
    return
  fi

  echo "Select install mode:"
  echo "  1) fast   (bash + cron)"
  echo "  2) stable (rust daemon)"
  read -r -p "Enter choice [1/2]: " choice

  case "$choice" in
    1) MODE="fast" ;;
    2) MODE="stable" ;;
    *)
      echo "Invalid choice: $choice" >&2
      exit 1
      ;;
  esac
}

if [[ "${EUID}" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

install_rust() {
  if need_cmd rustc && need_cmd cargo; then
    log "Rust already installed: $(rustc --version)"
    return
  fi

  log "Installing Rust via rustup..."
  curl https://sh.rustup.rs -sSf | sh -s -- -y

  # shellcheck disable=SC1090
  source "$HOME/.cargo/env"
  log "Rust installed: $(rustc --version)"
}

install_with_apt() {
  log "Detected apt-based system"
  $SUDO apt-get update
  if [[ "$MODE" == "fast" ]]; then
    $SUDO apt-get install -y curl jq ca-certificates cron
  else
    $SUDO apt-get install -y \
      curl jq ca-certificates cron build-essential pkg-config libssl-dev
  fi
}

install_with_dnf() {
  log "Detected dnf-based system"
  if [[ "$MODE" == "fast" ]]; then
    $SUDO dnf install -y curl jq ca-certificates cronie
  else
    $SUDO dnf install -y \
      curl jq ca-certificates cronie gcc gcc-c++ make pkgconf-pkg-config openssl-devel
  fi
}

install_with_yum() {
  log "Detected yum-based system"
  if [[ "$MODE" == "fast" ]]; then
    $SUDO yum install -y curl jq ca-certificates cronie
  else
    $SUDO yum install -y \
      curl jq ca-certificates cronie gcc gcc-c++ make pkgconfig openssl-devel
  fi
}

install_with_pacman() {
  log "Detected pacman-based system"
  if [[ "$MODE" == "fast" ]]; then
    $SUDO pacman -Sy --noconfirm curl jq ca-certificates cronie
  else
    $SUDO pacman -Sy --noconfirm \
      curl jq ca-certificates cronie base-devel pkgconf openssl
  fi
}

install_with_zypper() {
  log "Detected zypper-based system"
  if [[ "$MODE" == "fast" ]]; then
    $SUDO zypper --non-interactive install curl jq ca-certificates cron
  else
    $SUDO zypper --non-interactive install \
      curl jq ca-certificates cron gcc gcc-c++ make pkg-config libopenssl-devel
  fi
}

install_with_brew() {
  log "Detected Homebrew"
  brew update
  if [[ "$MODE" == "fast" ]]; then
    brew install curl jq
  else
    brew install curl jq pkg-config openssl rust
  fi
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --mode)
        MODE="${2:-}"
        shift 2
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        echo "Unknown argument: $1" >&2
        usage
        exit 1
        ;;
    esac
  done

  if [[ -n "$MODE" && "$MODE" != "fast" && "$MODE" != "stable" ]]; then
    echo "Invalid mode: $MODE (expected: fast or stable)" >&2
    exit 1
  fi
}

main() {
  parse_args "$@"
  select_mode
  log "Selected mode: $MODE"

  log "Installing system dependencies..."

  if need_cmd apt-get; then
    install_with_apt
  elif need_cmd dnf; then
    install_with_dnf
  elif need_cmd yum; then
    install_with_yum
  elif need_cmd pacman; then
    install_with_pacman
  elif need_cmd zypper; then
    install_with_zypper
  elif need_cmd brew; then
    install_with_brew
  else
    if [[ "$MODE" == "fast" ]]; then
      log "Unsupported package manager. Please install manually: curl jq ca-certificates cron"
    else
      log "Unsupported package manager. Please install manually: curl jq ca-certificates cron gcc make pkg-config openssl headers"
    fi
    exit 1
  fi

  if [[ "$MODE" == "fast" ]]; then
    log "Fast mode install complete."
    log "Next: ./config.sh <student_id> && crontab -e"
    return
  fi

  install_rust

  # Ensure cargo is available in this shell even after fresh rustup install.
  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
  fi

  if need_cmd cargo; then
    log "All done. cargo version: $(cargo --version)"
  else
    log "cargo not found after installation"
    exit 1
  fi

  log "Building duaa in release mode..."
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --release
  log "Build complete: $ROOT_DIR/target/release/duaa"
}

main "$@"
