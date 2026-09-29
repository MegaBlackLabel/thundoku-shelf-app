#!/usr/bin/env bash
# ローカルの .app を Developer ID で署名する（キーチェーンの許可を毎回聞かれないように）。
#
# なぜ必要か:
#   ad-hoc 署名（`codesign --sign -`）は**ビルドのたびに署名が変わる**ため、macOS は
#   「別のアプリ」とみなし、キーチェーン（DB 暗号鍵・各ストアのトークン）の許可を
#   毎回聞いてくる。Developer ID で署名すると署名が安定し、一度「常に許可」すれば
#   以降は聞かれない（＝ CI の配布物と同じ状態になる）。
#
# なぜ一時キーチェーンか:
#   ログインキーチェーンの鍵を直接使うと、キーチェーンの partition list の都合で
#   `errSecInternalComponent` で失敗しやすい。CI と同じ手順（一時キーチェーン +
#   partition list 設定 + 中間証明書の投入）で確実に署名する。
#
# 使い方:
#   scripts/sign-local-app.sh [署名する .app のパス]
#     既定: target/debug/ThundokuShelf.app
#
# 必要なもの（THUNDOKU_P12 / THUNDOKU_P12_PW_FILE で差し替え可）:
#   ~/Documents/thundoku-ci/thundoku-ci.p12   … Developer ID の証明書 + 秘密鍵
#   ~/Documents/thundoku-ci/password.txt      … そのパスワード
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="${1:-$REPO/target/debug/ThundokuShelf.app}"
P12="${THUNDOKU_P12:-$HOME/Documents/thundoku-ci/thundoku-ci.p12}"
PW_FILE="${THUNDOKU_P12_PW_FILE:-$HOME/Documents/thundoku-ci/password.txt}"
# Apple の Developer ID G2 中間証明書（これが無いと identity が有効にならない）
G2_URL="https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer"
G2_SHA="f16cd3c54c7f83cea4bf1a3e6a0819c8aaa8e4a1528fd144715f350643d2df3a"

[ -d "$APP" ] || { echo "アプリがありません: $APP（先に scripts/make-app-bundle.sh を実行）" >&2; exit 1; }
[ -f "$P12" ] || { echo "証明書がありません: $P12" >&2; exit 1; }
[ -f "$PW_FILE" ] || { echo "パスワードファイルがありません: $PW_FILE" >&2; exit 1; }

WORK="$(mktemp -d)"
KC="$WORK/local.keychain-db"
KC_PW="$(openssl rand -hex 16)"
cleanup() {
  security delete-keychain "$KC" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

security create-keychain -p "$KC_PW" "$KC"
security set-keychain-settings -lut 900 "$KC"
security unlock-keychain -p "$KC_PW" "$KC"
security import "$P12" -k "$KC" -P "$(cat "$PW_FILE")" -T /usr/bin/codesign

# 中間証明書を入れる（鍵チェーンを完成させる）。login キーチェーンにも入れておくと、
# 次回以降この一時キーチェーンを作る手順でも identity が有効になる。
curl -fsSL -o "$WORK/DeveloperIDG2CA.cer" "$G2_URL"
echo "${G2_SHA}  $WORK/DeveloperIDG2CA.cer" | shasum -a 256 -c - >/dev/null
security import "$WORK/DeveloperIDG2CA.cer" -k "$KC"
security import "$WORK/DeveloperIDG2CA.cer" -k "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null || true

security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KC_PW" "$KC" >/dev/null

IDENTITY="$(security find-identity -v -p codesigning "$KC" | sed -n 's/.*"\(.*\)"/\1/p' | head -1)"
[ -n "$IDENTITY" ] || { echo "署名 ID が見つかりません（証明書か中間証明書が欠けています）" >&2; exit 1; }
echo "署名 ID: $IDENTITY"

# 同梱 dylib → .app の順に署名する（順序を逆にすると seal が合わない）
for target in "$APP/Contents/Frameworks/libpdfium.dylib" "$APP"; do
  [ -e "$target" ] || continue
  codesign --force --options runtime --keychain "$KC" --sign "$IDENTITY" "$target"
done
codesign --verify --strict "$APP"

echo "署名完了: $APP"
echo "（初回の起動でキーチェーンの許可を聞かれたら「常に許可」を押してください。以降は聞かれません）"
