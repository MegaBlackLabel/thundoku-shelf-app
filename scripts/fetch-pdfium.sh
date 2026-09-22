#!/usr/bin/env bash
# PDFium の動的ライブラリを取得して crates/core/ に置く。
#
# PDF レンダリングは PDFium を**実行時ロード**する（静的リンクしない。prebuilt の
# static ライブラリは macOS で FPDF_FORMFILL を欠きリンクできない）。探索先は
# ビルド時の `CARGO_MANIFEST_DIR`（= crates/core）と実行ファイルの隣なので、
# テスト・`cargo run` のどちらもこの配置で足りる（CWD は探索しない）。
# 無い場合、PDF を描画するテストはスキップされる。
#
# ライブラリはリポジトリにコミットしない（.gitignore）。取得元は pdfium-render の
# pdfium_7881 バインディングに対応する chromium/7881 で、SHA256 を検証する。
#
# 使い方: mise run pdfium  /  bash scripts/fetch-pdfium.sh
set -euo pipefail

VERSION=7881
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    ASSET=mac-arm64
    SHA=52e94ca5aa8847934330daf3f8150c190682c5ca93831468794f8b90d4392e40
    LIB=lib/libpdfium.dylib
    DEST=libpdfium.dylib
    ;;
  Darwin-x86_64)
    ASSET=mac-x64
    SHA=6dedf83990e0e3d6b7c93c9e7589c5a126b0ae14b7464d76120cff7a26afb18b
    LIB=lib/libpdfium.dylib
    DEST=libpdfium.dylib
    ;;
  Linux-x86_64)
    ASSET=linux-x64
    SHA=1470e21b8b4a3b4ad7f85684e2da11d94f3b69a86d81dee11b9b6709d927ac1d
    LIB=lib/libpdfium.so
    DEST=libpdfium.so
    ;;
  MINGW*|MSYS*|CYGWIN*)
    ASSET=win-x64
    SHA=73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08
    LIB=bin/pdfium.dll
    DEST=pdfium.dll
    ;;
  *)
    echo "未対応のプラットフォームです: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

TMP="$(mktemp -d)"
TGZ="$TMP/pdfium.tgz"

curl -fsSL -o "$TGZ" \
  "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/${VERSION}/pdfium-${ASSET}.tgz"

# sha256sum は Linux、shasum は macOS。どちらでも検証する。
if command -v sha256sum > /dev/null 2>&1; then
  echo "${SHA}  ${TGZ}" | sha256sum -c -
else
  echo "${SHA}  ${TGZ}" | shasum -a 256 -c -
fi

tar -xzf "$TGZ" -C "$TMP" "$LIB"
mkdir -p "$ROOT/crates/core"
cp "$TMP/$LIB" "$ROOT/crates/core/$DEST"
rm -f "$TGZ"

echo "配置しました: crates/core/$DEST"
