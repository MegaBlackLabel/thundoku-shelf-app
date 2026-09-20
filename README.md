# Thundoku Shelf

技術書典・BOOTH・FANZA同人・DLsite で購入した同人誌を、デスクトップで管理・閲覧するアプリです。

> **これはアルファ版です。** 開発中で、予告なく仕様が変わったり、データの互換性が壊れたりすることがあります。大切なデータは Google Drive へのバックアップを併用してください。

> **対象は購入済み・DRM の無い（非DRM）同人誌のみ**です。購入していないコンテンツや、DRM で保護されたコンテンツは取り込めません。

## ダウンロードとインストール

[Releases](https://github.com/MegaBlackLabel/thundoku-shelf-app/releases) からお使いの OS の zip をダウンロードして、展開して起動してください（インストーラーはありません）。

ファイル名の `x.y.z` はバージョンです（例: `thundoku-shelf-0.2.4-aarch64-macos.zip`）。

| OS | ファイル |
|---|---|
| Windows (x64) | `thundoku-shelf-x.y.z-x86_64-windows.zip` |
| macOS (Apple Silicon) | `thundoku-shelf-x.y.z-aarch64-macos.zip` |
| macOS (Intel) | `thundoku-shelf-x.y.z-x86_64-macos.zip` |

- **Windows**: 展開したフォルダに `pdfium.dll`（PDF 表示用）が入っています。**exe と同じフォルダに置いたまま**使ってください。初回起動時に SmartScreen の警告が出たら「詳細情報」→「実行」で進めます。

### macOS の初回起動（Important）

> [!IMPORTANT]
> **macOS は初回だけ Gatekeeper の解除が必要です。** このアプリは Apple の公証を受けていないため、ダウンロードしたままだと起動できません（現在 Developer ID を申請中で、取得でき次第この手順は不要になります）。

macOS の zip を展開すると `ThundokuShelf.app` が出てきます。Applications に移してから、次の手順で起動してください。

1. アプリをダブルクリック → 「開発元を検証できません」と表示されたら **「完了」** を押す
2. **システム設定 →「プライバシーとセキュリティ」** を開き、「セキュリティ」の欄に出ている **「このまま開く」** を押す → 認証 → **「開く」**
   - 「このまま開く」は 1 の失敗から**約 1 時間以内**しか表示されません
3. 次回以降は普通のアプリと同じように起動できます

> [!NOTE]
> **macOS 15 (Sequoia) 以降では、右クリック →「開く」は Gatekeeper の回避として機能しません**（メニューには出ますが効きません）。上の 1〜2 を使ってください。

うまくいかない場合は、ターミナルで隔離属性を外してから開きます:

```sh
xattr -dr com.apple.quarantine /Applications/ThundokuShelf.app
open /Applications/ThundokuShelf.app
```

最初から隔離させない方法もあります（ブラウザではなくターミナルで落とすと隔離属性が付かず、Gatekeeper の判定自体が走りません。`x.y.z` と `arch` は実際の値に置き換え。`~/Applications` なら管理者権限は不要、`/Applications` でも構いません）:

```sh
mkdir -p ~/Applications
curl -fL -o /tmp/t.zip https://github.com/MegaBlackLabel/thundoku-shelf-app/releases/download/vx.y.z/thundoku-shelf-x.y.z-arch-macos.zip
shasum -a 256 /tmp/t.zip   # 下のコマンドが出す digest と一致することを確認
ditto -x -k /tmp/t.zip ~/Applications/
open ~/Applications/ThundokuShelf.app
```

整合性の確認（`vx.y.z` は実際のタグ）:

```sh
curl -s https://api.github.com/repos/MegaBlackLabel/thundoku-shelf-app/releases/tags/vx.y.z | grep -o 'sha256:[0-9a-f]\{64\}'
```

## 使い方

1. 起動すると本棚が開きます（初回は空です）
2. **サイドバー下部のアカウント**から、購入元のストアにログインします
3. 本棚の**同期**ボタンで購入本を取り込みます（表紙や書籍ファイルのダウンロードが始まります）
4. 本をクリックするとビューアーで開きます。右クリックメニューからお気に入り・タグ・付箋などを操作できます

## 主な機能

- **本棚** — カード / リスト表示の切り替え（選択した表示形式は次回起動時も復元）、ストア・タグ・イベント・読書状態（未読 / 読んでいる途中 / 読了）・お気に入りでの絞り込み、検索、並び替え（購入日 / 発売日 / タイトル / 最終閲覧日 / 閲覧回数 / 閲覧時間 / サイズ）、キーボード操作（矢印 / `hjkl` / `Enter`）
- **ビューアー** — 単一ページ / 見開き / スクロールの 3 モード。ダブルクリックでズーム（2x → 6x → 解除）、ドラッグで移動。見開きの綴じ方向（右綴じ / 左綴じ）は、既定がサイトごと（技術書典は左綴じ、FANZA同人 / DLsite は右綴じ）で、ビューアで変えると**その本だけ**に効きます。通常のホイールは 1 ノッチ = 1 ページ送りで、向きは設定画面の「ビューア共通設定」で変更できます
- **付箋** — ページ単位のメモ。付箋画面から一覧して、そのページで開き直せます
- **閲覧履歴・読書統計** — いつ・何回・どれだけ読んだかを記録します
- **チェックリスト** — 技術書典のイベント・サークルの確認（技術書典ログイン中のみ利用可能）
- **ダウンロード管理** — 未取得の本だけを取り込み、進捗を表示。進行中の転送は、右クリックメニューの「ダウンロード中止」または `Backspace` で中止できます
- **ストアからの取り込み** — 技術書典 / BOOTH / FANZA同人 / DLsite に対応（ストアごとにログインして同期）
- **自動タグ生成** — 取り込んだ本の情報からタグを生成（技術書典専用）
- **Google Drive バックアップ** — Google でログインすると、本棚の DB と画像を Google Drive にバックアップ・同期できます（複数端末での共有も可）
- **レポート（GitHub Issue）** — GitHub でログインすると、サイドバーの「設定」の上に「レポート」が出ます。不具合や要望をテンプレート付きの画面からそのまま Issue として送信できます（画像の添付もできます）

## データの保存場所

購入履歴・書籍ファイル・表紙などは、すべて**お使いの PC の中**に保存されます。

| OS | 場所 |
|---|---|
| Windows | `%APPDATA%\thundoku-shelf\` |
| macOS | `~/Library/Application Support/thundoku-shelf/` |

- アプリが外部と通信するのは、**各ストアへのログイン・同期**（技術書典 / BOOTH / FANZA同人 / DLsite）、**Google Drive へのバックアップ**、**GitHub へのレポート送信**のときです（ほかに、タグ生成のために Zenn のタグ一覧を参照します）。アクセス解析・広告・利用状況の送信はありません
- 保存先は設定画面から変更できます（バックアップの対象にもなります）

## 困ったときは

- **不具合・要望の報告**: アプリの「レポート」機能（GitHub ログインが必要）から送るか、[Issues](https://github.com/MegaBlackLabel/thundoku-shelf-app/issues/new/choose) へどうぞ。Issue テンプレートを用意しています
- **バージョンの確認**: 「このアプリについて」画面の下部に出ています
- **機能の詳細と既知の制約**: [docs/features.md](docs/features.md)

## 既知の制約

- **アルファ版です**。仕様変更やデータの非互換が起こりえます（バックアップの併用を推奨）
- 対象は購入済み・非DRM の同人誌のみです（DRM 付きのコンテンツは取り込めません）
- Google ログインはバックアップ用です。**書籍を読むのにログインは不要**です
- 「レポート」の画像添付は、投稿先リポジトリへの書き込み権限が必要です。権限が無い場合は本文のみで送信できます
- レポートの投稿先はこのアプリのリポジトリ（`MegaBlackLabel/thundoku-shelf-app`）に固定です
- macOS 版は署名・公証を行っていません（**Developer ID を申請中**）。初回起動の手順は「[macOS の初回起動](#macos-の初回起動important)」を参照

## 開発者向け

実装の詳細・設計メモ・DB スキーマは [docs/](docs/) にあります。ビルドやテストはリポジトリの `.mise.toml` のタスク（`mise run build` / `mise run test` / `mise run lint`）を使ってください。

## ライセンス

アプリ本体は [MIT](LICENSE)。

同梱・依存しているソフトウェア（アプリ本体 / Lucide アイコン・PDFium などの同梱アセット /
Rust の依存クレート）のライセンス表記と全文は、アプリ内の「説明」画面の一番下の
**「ライセンスについて」**から確認できます（[`Cargo.lock`](Cargo.lock) から生成した一覧を
同梱しています。更新は `mise run licenses`）。
