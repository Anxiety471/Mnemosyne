#!/bin/sh
# Install Drawer: curl -fsSL https://raw.githubusercontent.com/Anxiety471/drawer/main/install.sh | sh
#
# Downloads the prebuilt release for this machine and verifies its checksum.
# If there is no release for this platform yet, builds from source with cargo.
#
# Environment overrides:
#   DRAWER_PREFIX   install root, default ~/.local (binary in bin/, UI in share/drawer/ui)
#   DRAWER_VERSION  release tag to install, default latest
#   DRAWER_REF      branch used for the source fallback, default main
#   DRAWER_REPO     GitHub owner/repo, default Anxiety471/drawer
#   DRAWER_GITHUB_URL  release host, default https://github.com (for mirrors and testing)
#   DRAWER_SOURCE=1 skip the release download and build from source
set -eu

REPO="${DRAWER_REPO:-Anxiety471/drawer}"
VERSION="${DRAWER_VERSION:-latest}"
REF="${DRAWER_REF:-main}"
PREFIX="${DRAWER_PREFIX:-$HOME/.local}"
GITHUB_URL="${DRAWER_GITHUB_URL:-https://github.com}"
# rustup's own installer fetches from here; its .sha256 sidecars are the checksums.
RUSTUP_ROOT="${DRAWER_RUSTUP_ROOT:-https://static.rust-lang.org/rustup}"
case "$GITHUB_URL" in
  https://*) PROTO="=https" ;;
  *) PROTO="=http,https" ;; # only reachable through the DRAWER_GITHUB_URL override
esac

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
# Same options everywhere, so a flaky network retries instead of falling back to a
# multi-minute source build on the first hiccup.
CURL="curl --retry 3 --retry-delay 5 --retry-all-errors --connect-timeout 15"

detect_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os-$arch" in
    Linux-x86_64) echo x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) echo aarch64-unknown-linux-gnu ;;
    Darwin-x86_64) echo x86_64-apple-darwin ;;
    Darwin-arm64) echo aarch64-apple-darwin ;;
    *) echo "" ;;
  esac
}

sha256_check() {
  # $1 = checksum file line(s) for the archive, run from the archive's directory
  if have sha256sum; then
    sha256sum -c "$1" >/dev/null
  elif have shasum; then
    shasum -a 256 -c "$1" >/dev/null
  else
    say "no sha256sum or shasum found, skipping checksum"
    return 0
  fi
}

file_sha256() {
  # $1 = path
  if have sha256sum; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# A release name is predictable, so a missing archive is normal: no release for this
# platform yet, and the caller falls back to a source build.
fetch_optional() {
  # $1 = url, $2 = destination
  $CURL --proto "$PROTO" --tlsv1.2 -fsSL "$1" -o "$2" && return 0
  say "not available: $1"
  return 1
}

# A missing source archive or an unpackable one is a hard failure, not a fallback.
fetch_required() {
  # $1 = url, $2 = destination, $3 = what it is
  $CURL --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2" || die "could not download $3"
}

check_prefix() {
  # Never let a too-broad prefix turn the install into a recursive delete.
  case "$PREFIX" in
    ""|/|/root|/home|/usr|/usr/local|/etc|/var|/opt|/bin|/sbin|/tmp|/dev|/proc|/sys) die "DRAWER_PREFIX is too broad: $PREFIX" ;;
    */|*.|*..*) die "DRAWER_PREFIX must be a real directory path: $PREFIX" ;;
  esac
  # A relative prefix would land in whatever directory the installer happens to run from.
  case "$PREFIX" in
    /*) ;;
    *) die "DRAWER_PREFIX must be an absolute path: $PREFIX" ;;
  esac
  if [ ! -d "$PREFIX" ]; then
    say "creating $PREFIX"
    mkdir -p "$PREFIX" || die "cannot create $PREFIX"
  fi
}

install_tree() {
  # $1 = binary path, $2 = UI dist dir or empty
  check_prefix
  mkdir -p "$PREFIX/bin"
  cp "$1" "$PREFIX/bin/drawer"
  chmod 755 "$PREFIX/bin/drawer"
  if [ -n "$2" ] && [ -f "$2/index.html" ]; then
    mkdir -p "$PREFIX/share/drawer"
    rm -rf "$PREFIX/share/drawer/ui"
    cp -R "$2" "$PREFIX/share/drawer/ui"
  fi
}

install_release() {
  target=$1
  if [ "$VERSION" = latest ]; then
    base="$GITHUB_URL/$REPO/releases/latest/download"
  else
    base="$GITHUB_URL/$REPO/releases/download/$VERSION"
  fi
  archive="drawer-$target.tar.gz"
  say "downloading $archive ($VERSION)"
  fetch_optional "$base/$archive" "$tmp/$archive" || return 1
  fetch_optional "$base/SHA256SUMS" "$tmp/SHA256SUMS" \
    || die "no SHA256SUMS for $VERSION; refusing an unverified install"
  grep " $archive\$" "$tmp/SHA256SUMS" > "$tmp/check" || die "$archive is not listed in SHA256SUMS"
  (cd "$tmp" && sha256_check check) || die "checksum mismatch for $archive"
  tar -xzf "$tmp/$archive" -C "$tmp" || die "cannot unpack $archive"
  # A release for another platform can unpack to a differently named directory. Verify the
  # layout before installing, and fail loudly instead of reporting success with nothing copied.
  [ -f "$tmp/drawer-$target/drawer" ] || die "$archive does not contain drawer-$target/drawer"
  install_tree "$tmp/drawer-$target/drawer" "$tmp/drawer-$target/ui"
}

# rustup-init is downloaded and checksum-verified instead of piping a remote script
# into a shell. rust publishes one .sha256 sidecar per rustup-init binary, which is
# the same artifact `sh.rustup.rs` downloads.
install_rustup() {
  rust_arch=$(uname -m)
  case "$rust_arch" in
    x86_64 | amd64) rust_arch=x86_64 ;;
    aarch64 | arm64) rust_arch=aarch64 ;;
    *) die "no rustup-init is published for $(uname -m)" ;;
  esac
  rust_os=$(uname -s)
  case "$rust_os" in
    Linux) rust_os=unknown-linux-gnu ;;
    Darwin) rust_os=apple-darwin ;;
    *) die "no rustup-init is published for $rust_os" ;;
  esac
  triple="$rust_arch-$rust_os"
  url="$RUSTUP_ROOT/dist/$triple/rustup-init"
  say "downloading rustup-init for $triple"
  fetch_required "$url" "$tmp/rustup-init" "rustup-init"
  fetch_required "$url.sha256" "$tmp/rustup-init.sha256" "the rustup-init checksum"
  expected=$(awk 'NR == 1 {print $1}' "$tmp/rustup-init.sha256")
  case "$expected" in
    ????????????????????????????????????????????????????????????????) ;;
    *) die "the rustup-init checksum is malformed" ;;
  esac
  actual=$(file_sha256 "$tmp/rustup-init")
  [ "$actual" = "$expected" ] || die "rustup-init checksum mismatch (expected $expected, got $actual)"
  chmod 700 "$tmp/rustup-init"
  "$tmp/rustup-init" -y --profile minimal --default-toolchain stable || die "rustup-init failed"
}

install_source() {
  if ! have cargo; then
    say "cargo not found, installing Rust with rustup (minimal profile)"
    have curl || die "curl is required"
    install_rustup
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
  say "downloading source ($REF)"
  fetch_required "https://codeload.github.com/$REPO/tar.gz/refs/heads/$REF" "$tmp/src.tar.gz" "the Drawer source"
  mkdir "$tmp/src"
  tar -xzf "$tmp/src.tar.gz" -C "$tmp/src" --strip-components=1 || die "cannot unpack the source"
  ui_dist=""
  if have npm; then
    say "building the UI"
    (cd "$tmp/src/ui" && npm ci --no-audit --no-fund && npm run build)
    ui_dist="$tmp/src/ui/dist"
  else
    say "npm not found, skipping the UI (the API and MCP server still work)"
  fi
  say "building drawer with cargo (this takes a few minutes)"
  (cd "$tmp/src" && cargo build --release)
  install_tree "$tmp/src/target/release/drawer" "$ui_dist"
}

main() {
  have curl || die "curl is required"
  have tar || die "tar is required"
  # Reject a destructive prefix before any download or build starts.
  case "$PREFIX" in
    ""|/|/root|/home|/usr|/usr/local|/etc|/var|/opt|/bin|/sbin|/tmp|/dev|/proc|/sys) die "DRAWER_PREFIX is too broad: $PREFIX" ;;
    */|*.|*..*) die "DRAWER_PREFIX must be a real directory path: $PREFIX" ;;
  esac
  case "$PREFIX" in
    /*) ;;
    *) die "DRAWER_PREFIX must be an absolute path: $PREFIX" ;;
  esac
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/drawer-install.XXXXXX")
  chmod 700 "$tmp"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  target=$(detect_target)
  installed=""
  if [ -n "$target" ] && [ "${DRAWER_SOURCE:-0}" != 1 ]; then
    if install_release "$target"; then
      installed=release
    else
      say "no published release for $target, falling back to a source build (slow: several minutes)"
    fi
  fi
  if [ -z "$installed" ]; then
    install_source
  fi

  say "installed drawer to $PREFIX/bin/drawer"
  case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) say "add $PREFIX/bin to your PATH, e.g.: export PATH=\"$PREFIX/bin:\$PATH\"" ;;
  esac
  say "start it with: drawer serve   (then open http://127.0.0.1:8787)"
  say "data goes to ~/.drawer unless you pass --data-dir or set DRAWER_DATA_DIR"
}

main "$@"
