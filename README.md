# Thundoku Shelf Desktop

技術書典・BOOTH で購入した本の管理・閲覧のためのデスクトップアプリ。
Rust + [GPUI](https://github.com/zed-industries/zed)（+ gpui-component）で実装。
詳細な機能説明・注意点は [docs](docs/) を参照。

## 機能

- **本棚** — カードグリッド（仮想化）・リスト表示、サイト/タグ/イベント/読了/
  お気に入りフィルタ、検索、キーボード選択（`hjkl` / 矢印 / `Enter`）
- **リーダー** — 単一 / 見開き / スクロールの 3 モード、ズーム（ダブルクリックで
  2 倍 → 6 倍 → 解除）、慣性付きパン、読書進捗・閲覧履歴の記録、
  libwebp による高速デコード（詳細: [docs/features.md](docs/features.md)）
- **チェックリスト** — イベント・サークル管理、試し読み
- **同期** — 技術書典（直接ログイン）、Google Drive（.opfspack 双方向同期）、
  Zenn タグ
- **認証** — 技術書典サイト直接ログイン、Google OAuth（PKCE + ループバック）、
  トークンは macOS キーチェーン（keyring）に保存

## ビルド / 実行

依存: [mise](https://mise.jdx.dev/)、Rust 1.97+

```sh
mise install          # 依存ツールチェーン
mise run build        # ビルド
mise run run          # アプリ起動
mise run test         # 全テスト
mise run lint         # clippy
```

## ドキュメント

- [docs/features.md](docs/features.md) — 実装機能と注意点（ビューアー周り含む）
- [docs/database.md](docs/database.md) — DB スキーマ構成

## データ

データは `~/Library/Application Support/thundoku-shelf/` に保存されます
（SQLite + .opfspack + 表紙キャッシュ）。

## ライセンス

[MIT](LICENSE)
