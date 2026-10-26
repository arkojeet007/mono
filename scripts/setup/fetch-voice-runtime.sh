#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0

# Place the sherpa-onnx prebuilt native archive this tree links against, and
# verify it against the hash pinned in crates/koru-voice-sherpa/src/artifacts.rs.
#
# **Why this exists.** `sherpa-onnx-sys`' build script downloads a matching
# prebuilt archive from GitHub whenever `SHERPA_ONNX_LIB_DIR` is unset, so an
# enabled build reaches the network and gets whatever the release page is
# serving that day. It also reads `SHERPA_ONNX_ARCHIVE_DIR` first, and copies
# from there instead of downloading. That is the seam this script fills: the
# same discipline `assets/fonts/*.ttf` follows through `koru.sh fonts`.
#
#   ./scripts/setup/fetch-voice-runtime.sh            this host's target
#   ./scripts/setup/fetch-voice-runtime.sh --all      all three shipped targets
#   ./scripts/setup/fetch-voice-runtime.sh --verify   check what is already here
#
# Exit status 3 means this host has nothing provisioned (Linux, for now): the
# script placed nothing, and that is not a failure.
#
# Then, for a benchmark or a release build:
#
#   export SHERPA_ONNX_ARCHIVE_DIR="$PWD/third-party/sherpa-onnx/1.13.6"
#   cargo run -p koru-voice-bench --features sherpa -- ...
#
# whisper.cpp needs no artifact: `whisper-rs-sys` vendors the sources inside its
# own .crate and builds them with cmake and bindgen. What it needs is CMake, a
# C++ toolchain and libclang on the machine, which this script does not install.

set -euo pipefail

VERSION="1.13.6"
BASE="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${VERSION}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DEST="${ROOT}/third-party/sherpa-onnx/${VERSION}"

# name<TAB>sha256<TAB>bytes — the same three columns as
# `koru_voice_sherpa::artifacts::NAMES`, and a test there fails if the file
# names drift from what sherpa-onnx-sys will look for.
#
# All three are upstream's TTS-free ("no-tts") archives: no espeak-ng, ucd or
# piper_phonemize, so no GPL-3.0 eSpeak NG in what links them
# (`third-party/patches/sherpa-onnx.md`).
ARTIFACTS=(
  "sherpa-onnx-v${VERSION}-win-x64-static-MT-Release-no-tts-lib.tar.bz2	6c746e0fabc52df40e185662faccb8dc03bcb1fd137394c3e9d3815e78ca9308	116816761"
  "sherpa-onnx-v${VERSION}-osx-arm64-static-no-tts-lib.tar.bz2	5df455ff1c262347dde37e5608d2a80ff948f9d65984709cf351c4d06edc931b	18717755"
  "sherpa-onnx-v${VERSION}-osx-x64-static-no-tts-lib.tar.bz2	adcc1de32d99410416a98d4f880ee2e2ef4343fad8b627e58d1cd1a5bd6d16b8	18413779"
)

host_artifact() {
  case "$(uname -s)-$(uname -m)" in
    MINGW*|MSYS*|CYGWIN*) echo "win-x64-static-MT-Release-no-tts-lib" ;;
    Darwin-arm64)         echo "osx-arm64-static-no-tts-lib" ;;
    Darwin-x86_64)        echo "osx-x64-static-no-tts-lib" ;;
    # Not provisioned yet rather than unsupported: Linux voice is its own rung,
    # gated by verifying the linux-x64 no-tts archive's licensing
    # (docs/features/linux/TECHNICAL.final.md, D4 and D6). Status 3 lets a
    # caller tell "nothing to place here" from a failure.
    Linux-x86_64)         return 3 ;;
    *)
      echo "This product ships windows-x86_64, macos-aarch64 and macos-x86_64." >&2
      echo "There is no sherpa-onnx artifact provisioned for $(uname -s)-$(uname -m)." >&2
      exit 1
      ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

size_of() {
  # BSD stat and GNU stat disagree about the flag, and getting this wrong makes
  # the cheap pre-check silently pass.
  stat -f%z "$1" 2>/dev/null || stat -c%s "$1"
}

# **Worktrees copy before they download.** A new worktree takes an archive from
# the main checkout or another worktree that has it, as fonts do, rather than
# downloading it again. Only a copy that matches the pin is taken, so a sibling
# holding a stale or locally built archive is passed over, not copied in to be
# refused.
sibling_artifact() {
  local name="$1" want_hash="$2" want_size="$3" line tree path
  git -C "${ROOT}" worktree list --porcelain 2>/dev/null | while IFS= read -r line; do
    case "${line}" in worktree\ *) ;; *) continue ;; esac
    tree="${line#worktree }"
    [ "$(cd "${tree}" 2>/dev/null && pwd)" = "${ROOT}" ] && continue
    path="${tree}/third-party/sherpa-onnx/${VERSION}/${name}"
    [ -f "${path}" ] || continue
    [ "$(size_of "${path}")" = "${want_size}" ] || continue
    if [ "$(sha256_of "${path}")" = "${want_hash}" ]; then
      printf '%s\n' "${path}"
      break
    fi
  done
}

# **Verified into place, never in place.** A download that is interrupted must
# not leave a file the build script will happily copy: it is written beside the
# target and renamed only after its hash matches.
place() {
  local name="$1" want_hash="$2" want_size="$3"
  local final="${DEST}/${name}"

  if [ ! -f "${final}" ] && [ "${VERIFY_ONLY}" = "0" ]; then
    local sibling
    sibling="$(sibling_artifact "${name}" "${want_hash}" "${want_size}")"
    if [ -n "${sibling}" ]; then
      mkdir -p "${DEST}"
      echo "[voice] copying ${name} from ${sibling%/*}"
      cp "${sibling}" "${final}.part"
      mv "${final}.part" "${final}"
    fi
  fi

  if [ -f "${final}" ]; then
    local have_hash
    have_hash="$(sha256_of "${final}")"
    if [ "${have_hash}" = "${want_hash}" ]; then
      echo "[voice] ${name} is already here and verified"
      return 0
    fi
    echo "[voice] ${name} is here and does NOT match the pinned hash" >&2
    echo "[voice]   want ${want_hash}" >&2
    echo "[voice]   have ${have_hash}" >&2
    echo "[voice] refusing to use it; delete it to re-fetch" >&2
    return 1
  fi

  if [ "${VERIFY_ONLY}" = "1" ]; then
    echo "[voice] ${name} is not here" >&2
    return 1
  fi

  mkdir -p "${DEST}"
  local part="${final}.part"
  echo "[voice] downloading ${name}"
  curl -fSL --retry 3 -o "${part}" "${BASE}/${name}"

  local have_size have_hash
  have_size="$(size_of "${part}")"
  if [ "${have_size}" != "${want_size}" ]; then
    rm -f "${part}"
    echo "[voice] ${name} is ${have_size} bytes and the pin says ${want_size}" >&2
    return 1
  fi
  have_hash="$(sha256_of "${part}")"
  if [ "${have_hash}" != "${want_hash}" ]; then
    rm -f "${part}"
    echo "[voice] ${name} does not match the pinned sha256" >&2
    echo "[voice]   want ${want_hash}" >&2
    echo "[voice]   have ${have_hash}" >&2
    return 1
  fi
  mv "${part}" "${final}"
  echo "[voice] placed ${final}"
}

WANT_ALL=0
VERIFY_ONLY=0
for arg in "$@"; do
  case "${arg}" in
    --all)    WANT_ALL=1 ;;
    --verify) VERIFY_ONLY=1 ;;
    *)
      echo "usage: $0 [--all] [--verify]" >&2
      exit 2
      ;;
  esac
done

provisioned=0
WANTED="$(host_artifact)" || provisioned=$?
if [ "${provisioned}" = "3" ] && [ "${WANT_ALL}" = "0" ]; then
  echo "voice runtime: not provisioned on this platform"
  exit 3
fi
if [ "${provisioned}" != "0" ] && [ "${provisioned}" != "3" ]; then
  exit "${provisioned}"
fi
status=0
for row in "${ARTIFACTS[@]}"; do
  name="$(printf '%s' "${row}" | cut -f1)"
  hash="$(printf '%s' "${row}" | cut -f2)"
  size="$(printf '%s' "${row}" | cut -f3)"
  if [ "${WANT_ALL}" = "0" ] && [[ "${name}" != *"${WANTED}"* ]]; then
    continue
  fi
  place "${name}" "${hash}" "${size}" || status=1
done

if [ "${status}" = "0" ]; then
  echo
  echo "[voice] now:  export SHERPA_ONNX_ARCHIVE_DIR=\"${DEST}\""
fi
exit "${status}"
