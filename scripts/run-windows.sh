#!/usr/bin/env bash
# Windows 動作確認用スクリプト:
#   1) Windows ターゲット（x86_64-pc-windows-msvc）にクロスビルド
#   2) Whisky のボトルがなければ作成
#   3) Whisky（Wine）でアプリを起動
#
# 使い方: ./scripts/run-windows.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BOTTLE="thundoku"
# debug ビルド: release は gpui_windows のシェーダーコンパイル（fxc.exe）が
# xwin 環境に無く失敗するため（include! は debug でスキップされる）
EXE="target/x86_64-pc-windows-msvc/debug/thundoku-shelf.exe"

# 1) ビルド（Windows ターゲット）
#    cargo-xwin は llvm-lib を AR として使うため、Homebrew の llvm を PATH に含める
export PATH="/opt/homebrew/opt/llvm/bin:$PATH"
echo "==> Building for Windows (x86_64-pc-windows-msvc, debug)"
mise exec -- cargo xwin build --target x86_64-pc-windows-msvc -p thundoku-shelf

if [ ! -f "$EXE" ]; then
  echo "error: build output not found: $EXE" >&2
  exit 1
fi

# 2) ボトルがなければ作成
if ! whisky list 2>/dev/null | grep -q "$BOTTLE"; then
  echo "==> Creating Whisky bottle '$BOTTLE' (first run downloads the Windows toolkit)"
  whisky create "$BOTTLE"
fi

# 3) 起動
echo "==> Launching via Whisky ($EXE)"
whisky run "$BOTTLE" "$EXE"
