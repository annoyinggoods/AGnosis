#!/usr/bin/env bash
#
# Build the AGnosis web front end into a folder a static host can serve.
#
# The output folder holds three files: index.html, agnosis_web.js (the
# wasm-bindgen glue) and agnosis_web_bg.wasm. There is no server-side
# component and no network access: the page works from any static host.
#
# Usage:  ./build-web.sh [output-folder]
#         OUT_DIR=./dist ./build-web.sh
#         bash ./build-web.sh ./dist
#
# The toolchain: the wasm-bindgen CLI is preferred (its version must
# match the wasm-bindgen crate the module was built against — the crate
# is pinned in crates/agnosis-web/Cargo.toml, and the script reads the pin
# from there so the two cannot drift), and wasm-pack is the fallback
# (it installs the matching CLI itself, so it needs no version check).
# Neither is required to *use* the page, only to build it.
#
# wasm-opt is NOT needed: the script never calls it, and wasm-pack only
# uses it when binaryen happens to be installed.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"
out="${1:-${OUT_DIR:-${here}/dist}}"
# A relative `out` must not depend on the caller's cwd: the wasm-pack
# branch `cd`s into the crate before building, and a relative path
# landed the bundle in `crates/agnosis-web/crates/agnosis-web/<out>`
# (audit 2026-10-22 §P2-22). Absolutize once, here.
mkdir -p "${out}"
out="$(cd "${out}" && pwd)"
target="wasm32-unknown-unknown"

cd "${root}"

# The wasm-bindgen CLI must match the wasm-bindgen CRATE the module was
# built against, exactly: a CLI built for another version refuses the
# module with a schema-version error. The crate is pinned with `=`, so
# the expected version is read from the manifest and never drifts.
expected_bindgen="$(sed -n 's/^wasm-bindgen[[:space:]]*=[[:space:]]*"=\([^"]*\)".*$/\1/p' crates/agnosis-web/Cargo.toml | head -n 1)"
if [ -z "${expected_bindgen}" ]; then
  echo "cannot read the pinned wasm-bindgen version from crates/agnosis-web/Cargo.toml" >&2
  exit 1
fi
echo "==> the crate pins wasm-bindgen ${expected_bindgen}"

echo "==> cargo build --release --target ${target} -p agnosis-web"
cargo build --release --target "${target}" -p agnosis-web

wasm="${root}/target/${target}/release/agnosis_web.wasm"
if [ ! -f "${wasm}" ]; then
  echo "no wasm module at ${wasm}" >&2
  exit 1
fi

# The output folder is rebuilt from scratch: a stale file from an
# earlier build can never survive into the bundle, so two runs of this
# script produce the same folder.
rm -rf "${out}"
mkdir -p "${out}"

if command -v wasm-bindgen >/dev/null 2>&1; then
  bindgen_version="$(wasm-bindgen --version | awk '{print $2}')"
  echo "==> wasm-bindgen CLI ${bindgen_version}"
  if [ "${bindgen_version}" != "${expected_bindgen}" ]; then
    echo "the wasm-bindgen CLI is ${bindgen_version}, the crate is ${expected_bindgen}" >&2
    echo "the CLI refuses a module that was built against another version" >&2
    echo "install the matching CLI: cargo install wasm-bindgen-cli --version ${expected_bindgen}" >&2
    exit 1
  fi
  wasm-bindgen --target web --no-typescript --out-dir "${out}" "${wasm}"
elif command -v wasm-pack >/dev/null 2>&1; then
  echo "==> wasm-pack"
  # wasm-pack reads the wasm-bindgen version out of the project's
  # Cargo.lock and installs that CLI itself, so there is nothing to
  # check here — and no wasm-opt to install.
  ( cd "${here}" && wasm-pack build . --target web --out-dir "${out}" --release --no-typescript )
else
  echo "neither wasm-bindgen nor wasm-pack is installed" >&2
  echo "install one of them:" >&2
  echo "  cargo install wasm-bindgen-cli --version ${expected_bindgen}" >&2
  echo "  cargo install wasm-pack" >&2
  exit 1
fi

cp "${here}/index.html" "${out}/index.html"

# The bundle is exactly three files. A missing one is a broken bundle,
# so the script says so instead of claiming a success it cannot back.
for file in index.html agnosis_web.js agnosis_web_bg.wasm; do
  if [ ! -s "${out}/${file}" ]; then
    echo "the bundle is incomplete: ${out}/${file} is missing or empty" >&2
    ls -l "${out}" >&2
    exit 1
  fi
done

echo "==> ${out}"
ls -l "${out}"
