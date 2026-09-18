#!/bin/bash
# macOS 用 .app バンドル作成（アイコン反映: CFBundleIconFile + リソース配置）
# 使い方: scripts/make-app-bundle.sh [--release]
set -e

cd "$(dirname "$0")/.."

TARGET="${1:-debug}"
if [ "$TARGET" = "release" ]; then
  BIN="target/release/thundoku-shelf"
  PROFILE="release"
else
  BIN="target/debug/thundoku-shelf"
  PROFILE="debug"
fi

if [ ! -x "$BIN" ]; then
  echo "binary not found: $BIN (run: mise exec -- cargo build -p thundoku-shelf)" >&2
  exit 1
fi

APP="target/$PROFILE/ThundokuShelf.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/thundoku-shelf"
cp crates/app/assets/app-icon/icon.icns "$APP/Contents/Resources/icon.icns"

# PDFium の動的ライブラリを同梱する。PDF レンダリングは実行時ロードで、
# 探索先は実行ファイルの隣と ../Frameworks（＝ Contents/Frameworks）。
# 無いと PDF の取り込みが「PDFium のライブラリをロードできませんでした」で失敗するので、
# 先に `mise run pdfium` で crates/core/ に取得しておくこと。
if [ -f crates/core/libpdfium.dylib ]; then
  mkdir -p "$APP/Contents/Frameworks"
  cp crates/core/libpdfium.dylib "$APP/Contents/Frameworks/libpdfium.dylib"
else
  echo "warning: crates/core/libpdfium.dylib がありません。PDF の取り込みは失敗します（mise run pdfium で取得）" >&2
fi

cat > "$APP/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>Thundoku Shelf</string>
  <key>CFBundleDisplayName</key>
  <string>Thundoku Shelf</string>
  <key>CFBundleIdentifier</key>
  <string>com.megablacklabel.thundoku-shelf</string>
  <key>CFBundleExecutable</key>
  <string>thundoku-shelf</string>
  <key>CFBundleIconFile</key>
  <string>icon</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

# arm64 は読み込むコードに有効な署名が必要なので、同梱した dylib とバンドルを
# ad-hoc で署名する（未署名の dylib は dyld に拒否される）。
if [ -f "$APP/Contents/Frameworks/libpdfium.dylib" ]; then
  codesign --force --sign - "$APP/Contents/Frameworks/libpdfium.dylib"
fi
codesign --force --sign - "$APP"

echo "created $APP"
