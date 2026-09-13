# 06. 同期・認証・Google Drive・通知・非機能

> `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> 本章は **各ストア同期の手順、認証情報の保護、Drive 双方向同期、通知の送出元、並列度と上限値**。読み手は AI（別言語での再実装・Web 版への移植を想定）。
> 事実にはアンカー付き。断定できない事項は章末の「不明点 / 推測」に分離してある。
> 情報源: crates/core/src/{tbf,booth,fanza,dlsite,drive,google,owner,secrets,single_instance}.rs, crates/app/src/app_state.rs

情報源:
- `crates/core/src/secrets.rs`, `crates/core/src/owner.rs`, `crates/core/src/single_instance.rs`, `crates/core/src/google.rs`
- `crates/core/src/drive/mod.rs`, `crates/core/src/drive/sync.rs`
- `crates/core/src/db/mod.rs`, `crates/core/src/db/books.rs`, `crates/core/src/db/backup.rs`, `crates/core/src/db/sync_state.rs`, `crates/core/src/db/schema.sql`
- `crates/core/src/import/mod.rs`, `crates/core/src/import/pdf.rs`, `crates/core/src/tbf/transport.rs`
- `crates/app/src/app_state.rs`, `crates/app/src/main.rs`, `crates/app/src/workspace.rs`, `crates/app/src/views/settings.rs`, `crates/app/src/views/bookshelf.rs`, `crates/app/src/views/google_login.rs`
- `docs/features.md`, `docs/account-switch.md`, `docs/logout.md`
- 第 7 章（ストア別詳細）の追加情報源: `crates/core/src/booth.rs`, `crates/core/src/dlsite/{sync,client,mod}.rs`, `crates/core/src/fanza/{sync,client,mod}.rs`, `crates/core/src/tbf/{mod,sync,queries}.rs`, `crates/app/src/views/booth_login.rs`, `crates/app/src/views/dlsite_login.rs`, `crates/app/src/views/fanza_login.rs`, `crates/app/src/views/tbf_login.rs`, `crates/app/src/views/auth.rs`, `crates/core/src/db/bookshelf.rs`, `crates/core/src/db/checklist.rs`, `docs/database.md`, `docs/import-patterns.md`
- 依存の実体: `gpui-kit` rev `84f57fdfcb4910623fb0bb7f795b077e249f9271`（`crates/app/Cargo.toml:17`、通知の自動消滅時間の出典）

---

## 1. 認証情報の保存先と保護（全プロバイダ横断）

### 1.1 保存先の一覧

| プロバイダ | 保存先 | キー | 保存される値 | 保護 |
|---|---|---|---|---|
| 技術書典 | OS keyring | service=`com.megablacklabel.thundoku-shelf`, user=`techbookfest` | `TbfSession` の JSON（cookies / xsrf_raw / xsrf_token） | OS 資格情報ストア（macOS キーチェーン / Windows Credential Manager）に平文相当で預ける。独自暗号化なし |
| Google | OS keyring | user=`google` | `OAuthTokens` の JSON（access_token / refresh_token / expires_at） | 同上 |
| Google Drive 用 DB 鍵 | OS keyring | user=`thundoku-shelf.db-key` | 32 byte 乱数の BASE64 文字列 | 同上 |
| BOOTH | アプリ DB（`app_settings`） | `booth.session` | `BoothSession` の JSON | **暗号化なし（平文 JSON）** |
| FANZA同人 | アプリ DB（`app_settings`） | `fanza.session` | `FanzaSession` の JSON | **暗号化なし（平文 JSON）** |
| DLsite | アプリ DB（`app_settings`） | `dlsite.session` | `DlsiteSession` の JSON | **暗号化なし（平文 JSON）** |

- keyring の service 名と user 名の定数: `crates/core/src/secrets.rs:9`（`SERVICE`）, `:10`（`USER_TECHBOOKFEST`）, `:11`（`USER_GOOGLE`）, `:13`（`USER_BOOTH` ※定義のみ・実際の BOOTH 保存は DB）, `:15`（`USER_DB_KEY`）。
- モジュール冒頭の宣言: 「OS keyring-backed secret storage (sessions, OAuth tokens). Passwords are never stored — only session cookies / tokens.」`crates/core/src/secrets.rs:1-2`。
- keyring 操作は `keyring::Entry::new(service, user)` の `set_password` / `get_password` / `delete_credential`。`NoEntry` は `None`（load）／成功扱い（delete）`crates/core/src/secrets.rs:37-66`。
- BOOTH / FANZA / DLsite を DB に置く理由（コード内コメント）: 「セッション Cookie は Windows Credential Manager の上限（2560 UTF-16 文字）を超えることがあるため、keyring ではなく DB に保存する」`crates/app/src/app_state.rs:187-190`, `:389-391`, `:415-417`, `:435-437`。
- DB 保存は `db::settings::set(&pool, "<name>.session", &json)` をそのまま呼ぶだけ（暗号化・難読化なし）`crates/app/src/app_state.rs:394-396`, `:419-421`, `:439-441`。
- 起動時の復元: keyring（技術書典・Google）と DB（BOOTH/FANZA/DLsite）から読み、`session.logged_in()` を満たすものだけを有効とする `crates/app/src/app_state.rs:151-215`。
- ログアウト時は keyring 削除／DB キー削除をバックグラウンドで実行 `crates/app/src/views/settings.rs:620-630`（技術書典）, `:655-666`（Google）, `:687-700`（BOOTH）, `:722-733`（FANZA）, `:739-750`（DLsite）。

### 1.2 `owner_sub` 暗号化用ローカル鍵（DB 鍵）

- 定数 `USER_DB_KEY = "thundoku-shelf.db-key"` `crates/core/src/secrets.rs:15`。
- 取得手順 `SecretStore::db_key()` `crates/core/src/secrets.rs:72-88`:
  1. keyring から読み込み → BASE64 デコード → 長さ 32 byte なら採用。
  2. 無い／壊れている場合は `OsRng` で 32 byte 生成 → BASE64 で keyring に保存して返す。
- 「起動時に無い場合は新規生成（既存の `owner_sub` は復号不能になるが許容）」（P1）`crates/core/src/secrets.rs:70-71`。

### 1.3 `owner_sub` の暗号化形式

- `books.owner_sub TEXT` 列（NULL = 未所属）`crates/core/src/db/schema.sql:50`、既存 DB へは `PRAGMA table_info('books')` 確認後に `ALTER TABLE books ADD COLUMN owner_sub TEXT` `crates/core/src/db/mod.rs:236-245`。
- 暗号化: AES-256-GCM。`IV(12 byte 乱数) || ciphertext || tag` を BASE64 にして保存 `crates/core/src/owner.rs:16-27`。
- 復号: 誤鍵・破損・非 UTF-8 は `None`。**未知の所有者は「未所属」扱いにせず非表示に留める**（P1）`crates/core/src/owner.rs:30-41`。
- 暗号化が必要な理由（モジュールコメント）: pack 鍵が `sub + pack_id`（PBKDF2 → HKDF）から導出され、`pack_id` は平文で `books` にあるため、平文 `sub` があると DB 保持者が全 pack 鍵を導出できてしまう `crates/core/src/owner.rs:1-6`。同趣旨が `docs/account-switch.md`「データモデル」節にも記載。
- 復号鍵は keyring 保持のため、**keyring を失うと `owner_sub` の復号・所有者復元ができず pack も読めない**（`docs/account-switch.md`「セキュリティ / リスク」節）。

---

## 2. Google アカウント（OAuth）

### 2.1 定数

| 定数 | 値 | アンカー |
|---|---|---|
| 認可エンドポイント | `https://accounts.google.com/o/oauth2/v2/auth` | `crates/core/src/google.rs:14` |
| トークンエンドポイント | `https://oauth2.googleapis.com/token` | `crates/core/src/google.rs:15` |
| userinfo | `https://www.googleapis.com/oauth2/v3/userinfo` | `crates/core/src/google.rs:16` |
| ループバック固定ポート | `DEFAULT_REDIRECT_PORT = 38387` | `crates/core/src/google.rs:17` |
| スコープ | `openid email https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/drive.file`（appdata は使わない） | `crates/core/src/google.rs:19` |
| リフレッシュ余裕 | `REFRESH_SKEW_SECONDS = 60` 秒 | `crates/core/src/google.rs:21` |
| コールバック待ちタイムアウト | 300 秒（5 分） | `crates/core/src/google.rs:151` |
| コールバックのポーリング間隔 | 100 ms（非ブロッキング accept） | `crates/core/src/google.rs:155-163` |
| コールバックの read タイムアウト | 120 秒 | `crates/core/src/google.rs:168` |
| コールバック受信バッファ | 4096 byte | `crates/core/src/google.rs:170` |
| `expires_in` 既定値 | 3600 秒（応答に無い場合） | `crates/core/src/google.rs:126-129` |
| 既定 client_id（ビルド時 `THUNDOKU_GOOGLE_CLIENT_ID` 未設定時） | `1054619943130-2kaqpgnm719bp8l8rslm8rkuvdhb945s.apps.googleusercontent.com` | `crates/app/src/app_state.rs:21-25` |
| 既定 client_secret | `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`（`THUNDOKU_GOOGLE_CLIENT_SECRET` で上書き可。デスクトップでは公開情報扱いとコメント） | `crates/app/src/app_state.rs:27-32` |

### 2.2 OAuth フロー（installed-app / PKCE S256 + ループバック受信）

1. ログインモーダル `GoogleLoginView::new` が `AppState.google` の `GoogleClient::begin_authorize()` を呼ぶ `crates/app/src/views/google_login.rs:44-48`。
2. `begin_authorize()`: ループバック listener を確保 → `redirect_uri = http://127.0.0.1:{port}` を組み立て → verifier / challenge / state を生成 → 認可 URL を返す `crates/core/src/google.rs:408-431`。
3. listener 確保 `bind_loopback()`: `SO_REUSEADDR` を設定し `127.0.0.1:38387` に bind、`listen(128)`。失敗時は `127.0.0.1:0`（動的ポート）へフォールバック（Google Cloud Console 登録の redirect_uri と一致させるため固定ポート優先）`crates/core/src/google.rs:239-266`。
4. PKCE 素材: verifier = 32 byte 乱数の base64url（パディング無し）、challenge = `base64url(SHA-256(verifier))`、state = 16 byte 乱数の base64url `crates/core/src/google.rs:67-86`。
5. 認可 URL のクエリ: `client_id`, `redirect_uri`, `response_type=code`, `scope`, `access_type=offline`, `prompt=consent`, `state`, `code_challenge`, `code_challenge_method=S256` `crates/core/src/google.rs:90-104`。
6. **アプリ内 WebView（gpui-wry）** に認可 URL を load して表示（システムブラウザは開かない）`crates/app/src/views/google_login.rs:1-7`, `:52-58`。※ `GoogleClient::authorize()`（システムブラウザを開く実装）も存在するが、アプリ経路は `begin_authorize`/`finish_authorize` `crates/core/src/google.rs:387-392`。
7. `finish_authorize()` が別スレッドで `receive_callback` を実行 `crates/app/src/views/google_login.rs:79-95`, `crates/core/src/google.rs:433-443`。
8. `receive_callback`: 非ブロッキング accept ループ。`cancel: AtomicBool` が立っていれば `GoogleError::Cancelled`、300 秒で `Auth("authorization timed out")`。1 接続のみ受けて HTTP リクエスト行の query を `percent_decode` し、`error` → `Auth`、`state` 不一致 → `Auth("state mismatch")`、`code` 無し → `Auth("missing code")`。応答は `HTTP/1.1 200 OK` + `text/html; charset=utf-8` の短文（成功時「認証完了。このタブを閉じてください。」）`crates/core/src/google.rs:142-237`。
9. `exchange_code`: `POST https://oauth2.googleapis.com/token`、`Content-Type: application/x-www-form-urlencoded`、form は `grant_type=authorization_code&code&redirect_uri&client_id&code_verifier`（`client_secret` が設定されていれば `&client_secret=` を追加）。HTTP ステータスが 2xx 以外は `GoogleError::Token` `crates/core/src/google.rs:446-488`。
10. `parse_token_response`: JSON の `error` を検査。`access_token` 必須（空文字不可）。`refresh_token` は任意。`expires_at = 現在時刻(Unix 秒) + expires_in` `crates/core/src/google.rs:105-140`。
11. `profile()`: userinfo に `Authorization: Bearer {token}` で GET（リダイレクト追跡 5）。2xx 以外は `Auth`。`sub` / `email` / `name` / `picture` を取り出し（欠落は空文字 / None）`crates/core/src/google.rs:537-575`。
12. トークンを keyring に JSON で保存（`USER_GOOGLE`）。refresh_token を含むため期限切れ後も自動リフレッシュ可 `crates/app/src/views/google_login.rs:112-125`。
13. グローバル状態更新: `google_profile = Some(profile)`, `google_logged_in = true`, `google_login_error = None`, `google_login_done = true`（`cx.emit` は RefCell 再入でパニックするため使わない）`crates/app/src/views/google_login.rs:127-140`。
14. Workspace の監視タスク（100 ms 間隔）が `google_login_done` を検知 → `show_auth=false`, 認証モーダル破棄, `drive.last_sync_at` が未設定なら Drive 有効化ダイアログを表示, 本棚を再フィルタ `crates/app/src/workspace.rs:170-216`。

### 2.3 トークンのリフレッシュとログイン状態

- `access_token()`: トークン未保持なら `GoogleError::NotAuthorized`。`now >= expires_at - 60` で `refresh_tokens()` `crates/core/src/google.rs:521-535`。
- `refresh_tokens()`: `grant_type=refresh_token&refresh_token&client_id`（+ secret）。応答に新しい refresh_token が無ければ**旧 refresh_token を保持**する `crates/core/src/google.rs:490-519`。
- 起動時: keyring の `USER_GOOGLE` JSON から `restore_tokens()`。`google_logged_in = client.has_tokens()`（プロフィール取得失敗でもログイン状態は維持）`crates/app/src/app_state.rs:165-186`, `crates/app/src/views/settings.rs:558-595`。
- ログアウト `logout_google`: `client.logout()`（トークンを `None` に）、`google_profile=None`、`google_logged_in=false`、`google_logout_done=true`（本棚再フィルタ）、keyring の `USER_GOOGLE` を非同期削除、トースト「Google からログアウトしました」`crates/app/src/views/settings.rs:652-676`。
- エラー種別 `GoogleError`: `Io` / `Auth` / `Token` / `Network` / `NoRefreshToken` / `NotAuthorized` / `Cancelled` `crates/core/src/google.rs:39-58`。

### 2.4 `owner_sub` によるデータ属性付けとアカウント切替

- 設計の正は `docs/account-switch.md`（「改訂版（採用）」節）。要点:
  - 各 pack は所有者（ある Google `sub` の暗号文、または NULL=未所属）を持つ。複数アカウントのデータは共存し、それぞれ属性付けされる。ユーザー向けのアカウント選択・切替 UI は作らない `docs/account-switch.md`「基本原則」。
  - **ログイン中は現在 sub のデータのみ表示、ログアウト中は未所属（NULL）のみ表示**。切替時にデータは削除しない `docs/account-switch.md`「基本原則」「所有者ごとの挙動」表。
- フィルタ実装:
  - `books::list_owner_subs` が `SELECT id, owner_sub FROM books` で全件取得 `crates/core/src/db/books.rs:280-285`。
  - `books::owned_book_ids(pool, key, current_sub)`: `Some(sub)` なら `decrypt(key, blob) == sub` の行、`None`（未ログイン）なら `owner_sub IS NULL` の行のみを集合で返す `crates/core/src/db/books.rs:293-311`。※ 暗号文は SQL で比較できないため**アプリ側で全件 + メモリフィルタ**（P2 `docs/account-switch.md`）。
  - UI 側ラッパ `bookshelf::owned_book_ids(state)`: ログイン中は `(sub, key)` で復号比較、**ログイン中で鍵が無い場合は空集合（＝何も表示しない）**、未ログインは NULL 判定 `crates/app/src/views/bookshelf.rs:599-613`。
  - 使用箇所: 本棚 `crates/app/src/views/bookshelf.rs:1338`、履歴 `crates/app/src/views/history.rs:219`、付箋 `crates/app/src/views/notes.rs:131`、Drive アップロード/バックアップ `crates/core/src/drive/sync.rs:216-222`、起動時の復元差分判定 `crates/app/src/workspace.rs:509-516`。
- owner の付与:
  - ダウンロード取り込み時、**ログイン中のみ** `books::set_owner_sub(db, book_id, encrypt(key, sub))` を記録（未ログイン時は付けない = NULL）`crates/app/src/views/bookshelf.rs:2742-2745`, `:3007-3015`, `:3126-3134`。
  - Drive からダウンロードした pack も現在 sub の所有として記録する（フォルダ分離前提で帰属を信頼）`crates/core/src/drive/sync.rs:286-289`。
- 重複抑止 `(source, owner)`（`source = site_id + tbf_product_id`）:
  - `books::find_by_source(pool, site_id, tbf_product_id)` が `(id, owner_sub)` を返し `crates/core/src/db/books.rs:315-329`。
  - `books::resolve_reuse_id(pool, key, site_id, ...)` は同一 owner の行 id のみ返す（別 owner / NULL は対象外、自動変換しない）`crates/core/src/db/books.rs:332-360`。
  - 再利用時は同じ book id のまま `books::upsert` で置き換えるため、book_id に紐づく進捗・タグ・閲覧履歴は維持される（`docs/account-switch.md` P5）。
- データが消えない理由（実装上の帰結）:
  1. 切替時も `books` 行と `owner_sub` を削除・書き換えしない（削除コードは存在しない）。表示だけが `owned_book_ids` で切り替わる `crates/app/src/views/bookshelf.rs:599-613`。
  2. keyring に保持するトークンは 1 アカウント分だけだが、他アカウントの `owner_sub` 暗号文は DB に残り、そのアカウントで再ログインすれば同じ DB 鍵で復号できる `crates/core/src/secrets.rs:72-88`, `crates/app/src/app_state.rs:165-186`。
  3. 非現在アカウントの pub は復元手段が無く、`owner_sub` 破損時はその pack を読めない（許容と明記）`docs/account-switch.md`「セキュリティ / リスク」。
- 初回起動時の既存データクリア（P4）: `app_settings['owner_sub_model.initialized']` が無ければ、`product_sample_pages` … `drive_sync_state` の 17 テーブルを DELETE し、`packs` / `thumbnails` ディレクトリを削除→再作成、`drive.last_sync_at` / `drive.sync.enabled` / `drive.sync.folder_id` を削除してからフラグを立てる `crates/core/src/db/mod.rs:399-457`。呼び出しは起動時 `crates/app/src/app_state.rs:148-150`。

---

## 3. Google Drive 同期

### 3.1 API クライアント（`crates/core/src/drive/mod.rs`）

| 操作 | HTTP | 実装アンカー |
|---|---|---|
| 一覧 `list_files(folder_id)` | `GET https://www.googleapis.com/drive/v3/files?q='{folder_id}' in parents and trashed=false&fields=nextPageToken,files(id,name,size,md5Checksum,modifiedTime)&pageSize=100&spaces=drive`（`pageToken` で全ページ走査、`nextPageToken` が無くなったら終了） | `crates/core/src/drive/mod.rs:11-13`, `:108-171` |
| ダウンロード `download(file_id)` | `GET .../files/{file_id}?alt=media` | `crates/core/src/drive/mod.rs:172-182` |
| アップロード `upload_multipart(name, folder_id, bytes)` | `POST https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart`、`Content-Type: multipart/related; boundary=thundoku_shelf_boundary`。metadata は `{"name","parents":[folder_id],"mimeType":"application/octet-stream","appProperties":{"app":"thundoku-shelf","packId":<拡張子を除いた name>}}`。応答 JSON の `id` を返す | `crates/core/src/drive/mod.rs:14`, `:184-233` |
| フォルダ作成 `create_folder(name)` | `POST .../files`、`{"name","mimeType":"application/vnd.google-apps.folder"}`。応答 `id` を返す | `crates/core/src/drive/mod.rs:235-257` |
| 更新日時更新 `touch(file_id)` | `PATCH .../files/{file_id}`、`{"modifiedTime": <現在 UTC の RFC3339>}` | `crates/core/src/drive/mod.rs:259-276` |
| 削除 `delete(file_id)` | `DELETE .../files/{file_id}` | `crates/core/src/drive/mod.rs:278-291` |
| 共通 | すべて `Authorization: Bearer {access_token}`。`redirects = 5`。`list_files` は 401/403 で「authorization failed」を返す | `crates/core/src/drive/mod.rs:74-106`, `:118-123` |

- `size` は Drive API が number / string のどちらでも返し得るため両対応 `crates/core/src/drive/mod.rs:16-22`。
- `DriveApi` trait（テストでモック差し替え可能）: `list_files` / `download` / `upload_multipart` / `create_folder` / `delete` / `touch` `crates/core/src/drive/mod.rs:48-63`。
- 同期フォルダは My Drive 直下の `thundoku-shelf/`（Web 版 appdata ではなくユーザー可視フォルダ）`docs/features.md:440-442`。フォルダ ID は初回同期時に `create_folder("thundoku-shelf")` で作成し `app_settings['drive.sync.folder_id']` に保存 `crates/app/src/views/settings.rs:799-808`。

### 3.2 双方向同期アルゴリズム（`drive::sync::sync`）

入力 `SyncRequest` のフィールド `crates/core/src/drive/sync.rs:177-198`: `pool`, `drive`, `packs_dir`（DL 先/UL 元）, `downloads_dir`（作業用）, `identity_sub: Option<&str>`（未ログインは None）, `owner_key: Option<&[u8;32]>`, `folder_id`, `db_path: Option<&Path>`（None なら DB バックアップ/復元をしない）。

手順:

1. `drive.list_files(folder_id)` で全ファイル取得、件数をログ `crates/core/src/drive/sync.rs:205-208`。
2. Drive 側ファイル名から pack_id を抽出（`{pack_id}.opfspack` のみ。空・`/`・`\`・`..`・`:`・制御文字・先頭 `.` を含む id は除外）。同名は**最初の 1 件を採用**（`HashMap::entry().or_insert`）`crates/core/src/drive/sync.rs:61-75`, `:225-231`。
3. `outcome.file_count = files.len()`、`outcome.total_bytes = Σ max(size,0)` `crates/core/src/drive/sync.rs:233-238`。
4. アップロード対象集合 `upload_ids = books::owned_book_ids(pool, key, Some(sub))`。未ログイン（`identity_sub = None`）は空集合で**何もアップロードしない** `crates/core/src/drive/sync.rs:216-222`。
5. **ダウンロード方向**（Drive の各 pack について）`crates/core/src/drive/sync.rs:242-296`:
   1. `sync_state::get(pool, pack_id)` を読む。
   2. `drive_md5` が空でなく `state.md5` と一致 → スキップ（`outcome.skipped`）。
   3. `local_changed = state があり、ローカル pack の mtime(秒) > state.last_synced_at("%Y-%m-%d %H:%M:%S")`（厳密に大。ファイル無し・metadata 取得失敗・mtime 取得失敗・日時パース失敗はすべて false = 未変更扱い）`crates/core/src/drive/sync.rs:84-104`。
   4. `drive.download(file.id)` で全バイト取得。
   5. `PackReader::open(&bytes)` が失敗 → `SyncError::InvalidPack`。
   6. いずれかのエントリに `IDENTITY_BOUND` フラグがあり、かつ未ログイン → `SyncError::IdentityRequired`（同期全体を中断）。
   7. 競合（`local_changed` かつローカル pack が存在）→ ローカルを `packs/{pack_id}.conflict-local.opfspack` にコピーし `outcome.conflicts` に追加。**Drive 側が勝つ**。
   8. `downloads_dir` と `packs_dir` を作成し、`downloads/{pack_id}.opfspack`（一時）と `packs/{pack_id}.opfspack`（本体）の両方に書き出す。
   9. `import_book` で DB 反映（後述）。
   10. ログイン中なら `books::set_owner_sub(pack_id, encrypt(key, sub))`。
   11. `sync_state::upsert(pack_id, drive_file_id, md5, modified_time, last_synced_at=now)`。`now` は UTC の `%Y-%m-%d %H:%M:%S` `crates/core/src/drive/sync.rs:77-79`。
6. **アップロード方向**（`books::list(pool)` の全行について）`crates/core/src/drive/sync.rs:298-360`:
   1. `pack_id = book.pack_id`（無ければ `book.id`）。空ならスキップ。
   2. `upload_ids` に含まれない本（未所属・他アカウント）はスキップ。
   3. `packs_dir/{pack_id}.opfspack` が無ければスキップ。
   4. `sync_state` が無ければアップロード。あれば「Drive 側に同じ `drive_file_id` がまだ存在」**かつ**「ローカル mtime > last_synced_at」のときだけアップロード（削除は伝播させない）。
   5. `upload_multipart` 実行後、`sync_state::upsert(md5 = md5(local bytes), modified_time = None, last_synced_at = now)`。
7. **DB バックアップ**（`db_path.is_some()` のとき）`crates/core/src/drive/sync.rs:362-390`:
   1. `db::backup::export_json(pool, Some(&upload_ids))` でテキストテーブルの JSON を生成。
   2. `md5(ローカル JSON)` と Drive の `thundoku-backup.json` の `md5Checksum` を比較。
   3. 不一致（または Drive にファイルが無い）→ 既存ファイルを `delete` してから `upload_multipart`。`outcome.database_backed_up = true`。
   4. 一致 → `drive.touch(file.id)` で `modifiedTime` だけ現在時刻に更新（バックアップの更新日時が古いままにならないように）。
- 戻り値 `SyncOutcome` `crates/core/src/drive/sync.rs:22-38`: `downloaded` / `uploaded` / `skipped` / `conflicts`（pack_id の Vec）, `file_count`（Drive フォルダ内の全ファイル数）, `total_bytes`（Σ size、bytes）, `database_backed_up`, `database_restored`。
- エラー型 `SyncError` `crates/core/src/drive/sync.rs:40-58`: `Drive` / `Io` / `Db` / `Pack` / `InvalidPack` / `IdentityRequired`。
- **削除は双方向とも伝播しない**（モジュールコメント `crates/core/src/drive/sync.rs:1-13`、`docs/features.md:447-450`）。
- 注意（実装依存）: ダウンロード方向は `HashMap` を反復するため処理順は不定 `crates/core/src/drive/sync.rs:225-231`。

### 3.3 ダウンロードした pack の DB 反映（`import_book`）

- `metadata.json` エントリを（ログイン中は `Identity{sub, pack_id}` 付きで）読み、`title` / `author` / `circleName` / `purchaseDate` を採用。読めなければ `title = pack_id`、他は空 `crates/core/src/drive/sync.rs:106-140`。
- `books::upsert` の固定値: `file_name = opfs_path = "{pack_id}.opfspack"`, `tags_fetched = 1`, `pack_id = Some(pack_id)`, `is_favorite = 0`, `is_hidden = 0`, `is_drm = 0`, `cover_thumbnail = None`, `tbf_product_id = None`, `site_id = None`, `created_at = updated_at = now` `crates/core/src/drive/sync.rs:143-176`。
- 続けて `import::rebuild_from_pack(pool, pack_id, pack_bytes, identity)` でドキュメント・コンテンツ・ページ行を再構築（失敗は `log::warn` のみで続行）`crates/core/src/drive/sync.rs:170-175`。

### 3.4 DB バックアップの中身（`db/backup.rs`）

- 目的（モジュールコメント）: 画像 base64 を含む DB ファイル全体（200MB 級）を上げるのは重いため、主要テーブルのテキストのみ JSON 化する。`thumbnail_data` / `image_data` と環境依存設定（`drive.*` / `api.last_sync_at` 等）は含めない `crates/core/src/db/backup.rs:1-8`。
- 対象テーブル（16 個、この順で処理。FK 参照元が先）: `books`, `bookshelf_items`, `checked_items`, `tbf_events`, `book_contents`, `content_formats`, `reading_progress`, `page_views`, `book_tags`, `favorite_tags`, `favorite_entities`, `imported_documents`, `document_images`, `book_first_events`, `zenn_tag_metadata`, `view_history` `crates/core/src/db/backup.rs:14-31`。
- 除外カラム: `thumbnail_data`, `image_data` `crates/core/src/db/backup.rs:33`。
- owner フィルタ（P3）: `book_ids` を渡すと `books` を絞り、関連テーブルも `book_id IN (...)` で連動して除外する `crates/core/src/db/backup.rs:40-56`, `:129-150`。
- 復元 `import_json`: 各テーブルを PK 競合時 `DO UPDATE` の UPSERT でマージ（**Drive 側優先**）。`INSERT ... ON CONFLICT DO UPDATE` は DELETE を伴わないため FK の `ON DELETE CASCADE` を発火させない `crates/core/src/db/backup.rs:57-63`, `:226-230`。PK 定義は `books:[id]`, `bookshelf_items:[site_id,database_id]`, `checked_items:[id]`, `tbf_events:[id]`, `book_contents:[content_id]`, `content_formats:[format_id]`, `reading_progress:[book_id,content_id]`, `page_views:[book_id,content_id,page_number]`, `book_tags:[id]`, `favorite_tags:[tag_name]`, `favorite_entities:[entity_kind,entity_name]`, `imported_documents:[id]`, `document_images:[id]`, `book_first_events:[site_id,database_id]`, `zenn_tag_metadata:[tag_name]`, `view_history:[id]` `crates/core/src/db/backup.rs:82-100`。
- Drive 上のファイル名: `const DB_BACKUP_NAME = "thundoku-backup.json"` `crates/core/src/drive/sync.rs:392`。
- 復元 API: `check_drive_backup(drive, folder_id)` → `DriveBackupInfo{file_id, md5, size, modified_time}`（無ければ `None`）`crates/core/src/drive/sync.rs:394-415`; `restore_drive_backup(drive, folder_id, pool)` はダウンロードして `backup::import_json` `crates/core/src/drive/sync.rs:417-432`。
- 差分判定 `backup_has_diff(pool, drive, folder_id, book_ids)`: Drive の JSON をダウンロードし、**Drive 側に存在するテーブル名だけ**に両者を正規化してから md5 比較（新テーブル追加で毎回復元確認が出るのを防ぐ）`crates/core/src/drive/sync.rs:434-487`。
- 同期状態テーブル `drive_sync_state(pack_id PK, drive_file_id, md5, modified_time, last_synced_at)` の get/upsert/list/delete `crates/core/src/db/sync_state.rs:6-55`。

### 3.5 同期のトリガーと OFF 条件

| トリガー / 条件 | 挙動 | アンカー |
|---|---|---|
| 設定画面「今すぐ同期」 | `SettingsView::sync_drive_now` を実行（Google 未ログインならボタン disabled） | `crates/app/src/views/settings.rs:769-830`, `:1853-1863` |
| Google ログイン直後 | `drive.last_sync_at` が未設定なら「Google Drive と同期しますか？」ダイアログ → 「同期する」で `drive.sync.enabled="true"` を保存し即同期 | `crates/app/src/workspace.rs:195-209`, `:1406-1451` |
| チェックリストのポーリング | 変化があったときだけ `sync_drive_now` を呼ぶ（無変化ならノーコスト） | `crates/app/src/workspace.rs:294-296` |
| ウィンドウ終了時 | バックアップ対象に変更があれば「アップロードして終了」を提示し、`db_path` 付きで同期してから終了 | `crates/app/src/workspace.rs:750-806`, `:1467-1472` |
| 起動時 | ログイン済み かつ `drive.sync.enabled` が `"true"`/`"1"` かつ `drive.sync.folder_id` があり、`backup_has_diff` が真のときだけ復元確認ダイアログ | `crates/app/src/workspace.rs:502-576`, `:1359-1364` |
| OFF スイッチ | 設定のトグルで `drive.sync.enabled` に `"true"`/`"false"` を保存（起動時読み込み時に `"true"` のみ有効） | `crates/app/src/views/settings.rs:754-766`, `:133` |
| 同期情報のクリア | `drive_sync_state` を全 DELETE し、`drive.sync.enabled` / `drive.sync.folder_id` / `drive.last_sync_at` / `drive.file_count` / `drive.total_bytes` を削除（次回は全ファイルが再送/再取得対象） | `crates/core/src/drive/sync.rs:489-510`, `crates/app/src/views/settings.rs:2478-2509` |
| 未ログイン | `sync_drive_now` は「Google にログインしてください」で失敗。同期エンジン側も `identity_sub=None` でアップロード対象ゼロ | `crates/app/src/views/settings.rs:792-795`, `crates/core/src/drive/sync.rs:216-222` |
| 暗号化 pack を未ログインで受信 | `SyncError::IdentityRequired`（UI はログイン誘導） | `crates/core/src/drive/sync.rs:268-271`, `crates/app/src/views/settings.rs:860-866` |

- 同期後に `drive.last_sync_at`（UTC `%Y-%m-%d %H:%M:%S`）/ `drive.file_count` / `drive.total_bytes` を保存し、設定画面に表示（ローカル時間に変換して表示）`crates/app/src/views/settings.rs:845-858`, `:1645-1663`。
- 同期完了トーストは「同期完了（DL n / UL n / スキップ n / 競合 n）[/ DB バックアップ]」`crates/app/src/views/settings.rs:868-882`。
- 同期は `drive_enabled` 設定に関わらず**手動実行は可能**（無効時もエンジンは動く）。無効化は「終了時アップロード」等の自動導線を止める意味を持つ `crates/app/src/views/settings.rs:754-766`（※意図の明記はコード上なし → 「推測」節）。

---
