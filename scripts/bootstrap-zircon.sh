#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FUCHSIA_DIR="${ROOT}/third-party/fuchsia"

mkdir -p "${ROOT}/third-party"

if [[ ! -x "${FUCHSIA_DIR}/.jiri_root/bin/jiri" ]]; then
  echo "[hirsel-os] Bootstrapping jiri into ${FUCHSIA_DIR}..."
  TMP_BOOTSTRAP="$(mktemp)"
  trap 'rm -f "${TMP_BOOTSTRAP}"' EXIT
  curl -fsSL "https://fuchsia.googlesource.com/jiri/+/HEAD/scripts/bootstrap_jiri?format=TEXT" \
    | base64 --decode > "${TMP_BOOTSTRAP}"
  bash "${TMP_BOOTSTRAP}" "${FUCHSIA_DIR}"
fi

cd "${FUCHSIA_DIR}"

if [[ ! -f "${FUCHSIA_DIR}/.jiri_manifest" ]]; then
  echo "[hirsel-os] Importing Fuchsia integration/flower manifest..."
  .jiri_root/bin/jiri import -name=integration flower https://fuchsia.googlesource.com/integration
fi

echo "[hirsel-os] Running jiri update (this fetches Zircon + prebuilt clang/lld/gn/ninja/rustc/qemu)..."
.jiri_root/bin/jiri update -fetch-packages-timeout=180 -hook-timeout=30

echo "[hirsel-os] Zircon/Fuchsia source tree ready at ${FUCHSIA_DIR}"
