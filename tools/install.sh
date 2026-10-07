#!/bin/sh
# srvm install front door: run the official dist installer with its checksum
# verification actually enforced.
#
# The generated installer (cargo-dist 0.30.2) verifies the downloaded archive
# with `sha256sum` and, when that command is missing, prints "skipping sha256
# checksum verification" and installs anyway. Stock macOS has no `sha256sum`, so
# a tampered download would install there with no warning worth the name. This
# wrapper supplies a `sha256sum` built from `shasum` or `openssl`, so the
# installer's own comparison runs and a mismatch aborts the install, then runs
# the official installer unchanged: there is still exactly one install path.
#
# Usage:
#   curl -sSfL https://raw.githubusercontent.com/thecont1/srvm/main/tools/install.sh | sh
#   curl -sSfL https://raw.githubusercontent.com/thecont1/srvm/main/tools/install.sh | sh -s -- --version v0.1.0-rc.1
#
# SRVM_INSTALLER_URL overrides where the official installer is fetched from,
# which is how the release workflow exercises this wrapper without publishing.
set -eu

installer_url="${SRVM_INSTALLER_URL:-https://github.com/thecont1/srvm/releases/latest/download/srvm-installer.sh}"
shim_dir="$(mktemp -d)"
trap 'rm -rf "$shim_dir"' EXIT
supplied=""

if command -v sha256sum >/dev/null 2>&1; then
    : # the installer verifies on its own
elif command -v shasum >/dev/null 2>&1; then
    printf '#!/bin/sh\nexec shasum -a 256 "$@"\n' > "$shim_dir/sha256sum"
    supplied="shasum"
elif command -v openssl >/dev/null 2>&1; then
    cat > "$shim_dir/sha256sum" <<'SHIM'
#!/bin/sh
# Just enough of sha256sum for the installer: it calls `sha256sum -b FILE` and
# reads the first field of the output.
for arg in "$@"; do
    [ "$arg" = "-b" ] || file="$arg"
done
hash="$(openssl dgst -sha256 "$file" | awk '{print $NF}')" || exit 1
printf '%s  %s\n' "$hash" "$file"
SHIM
    supplied="openssl"
else
    echo "srvm: refusing to install — none of sha256sum, shasum or openssl is available to verify the download" >&2
    exit 1
fi

if [ -n "$supplied" ]; then
    chmod +x "$shim_dir/sha256sum"
    PATH="$shim_dir:$PATH"
    export PATH
    echo "srvm: verifying the download with sha256sum supplied from $supplied (the installer skips its own check without one)" >&2
fi

curl --proto '=https' --tlsv1.2 -LsSf "$installer_url" | sh -s -- "$@"