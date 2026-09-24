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
| pack のルート鍵（v3 / PRK） | OS keyring | user=`thundoku-shelf.pack-root-key:<owner_id>`（`owner_id` = `SHA-256("opfspack:v1:" + sub)` の hex） | 32 byte 乱数（PRK そのもの）の BASE64 文字列。Drive 側の `thundoku-keys.json` は `sub` / パスフレーズでラップした PRK | 同上。**PRK は `sub` から導出できない**（乱数）ので、これが漏れれば pack が解ける。`sub` / パスフレーズはここに置かない（§10 章） |
| BOOTH | アプリ DB（`app_settings`） | `booth.session` | `BoothSession` の JSON | **keyring の鍵で暗号化（AES-256-GCM）** |
| FANZA同人 | アプリ DB（`app_settings`） | `fanza.session` | `FanzaSession` の JSON | **keyring の鍵で暗号化（AES-256-GCM）** |
| DLsite | アプリ DB（`app_settings`） | `dlsite.session` | `DlsiteSession` の JSON | **keyring の鍵で暗号化（AES-256-GCM）** |

- keyring の service 名と user 名の定数: `crates/core/src/secrets.rs`（`SERVICE` / `USER_TECHBOOKFEST` / `USER_GOOGLE` / `USER_BOOTH` / `USER_DB_KEY` / `USER_SESSION_KEY`）。
- **pack のルート鍵（v3）はアカウントごとのスロットに入る**: user 名は `PACK_ROOT_KEY_PREFIX = "thundoku-shelf.pack-root-key:"` + `owner_id`（`pack_root_key_user(owner_id)`）、読み書きは `load_pack_root_key` / `save_pack_root_key` / `delete_pack_root_key`（`crates/core/src/secrets.rs:76-80`, `:205-218`）。鍵の解決順序と Drive 側のラップは `crates/core/src/pack_keys.rs`（§3.1 / §10 章）。
- **DB 保存の 3 ストアは keyring の鍵で暗号化する**: 鍵は `thundoku-shelf.session-key`（`USER_DB_KEY` とは別スロット＝別鍵）、値は AES-256-GCM（AAD に用途名と形式版）で `enc:v2:` + base64 として `app_settings` に入る。**復号できない値は未ログインとして破棄**し、平文へはフォールバックしない `crates/core/src/session_store.rs`。
- **保存期限は 7 日（`SESSION_MAX_AGE_SECONDS`）**: 保存時刻を**暗号文の中**（`Envelope { saved_at, session }`）に入れて復元時に判定し、期限切れ・未来の時刻（改ざん/時計ずれ）は行ごと破棄して再ログインを求める（DB を書き換えても期限は延ばせない）。形式版は v2 で、**v1 の値は接頭辞が違うため復号できず破棄**される `crates/core/src/session_store.rs`。
- keyring の鍵が取得できない環境では**セッションを保存しない**（平文で保存しない）。その場合、次回起動では再ログインが必要。
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
- 暗号化が必要な理由: `sub` は v3 でも **`owner_id`（keyring のスロット名・ラップの AAD）と `sub` ラップの KEK の材料**で、平文で持つと鍵の復号経路を DB ごと渡すのと同じ（v2 は `sub + pack_id` から pack 鍵を導出していた。`crates/core/src/owner.rs:1-6` のコメントは v2 の記述のまま）。同趣旨が `docs/account-switch.md`「データモデル」節にも記載。
- 復号鍵は keyring 保持のため、**keyring を失うと `owner_sub` の復号・所有者復元ができず pack も読めない**（`docs/account-switch.md`「セキュリティ / リスク」節）。
- **同じ列をアカウントに紐づく他テーブルにも持つ**: `bookshelf_items` / `checked_items` /
  `book_first_events` / `favorite_tags` / `favorite_entities`（値は同じ DB 鍵による暗号文）。
  追加は `db/mod.rs` のプログラム的マイグレーション（`ensure_column`、テーブル作成がすべて
  終わった後にまとめて実行）。`favorite_entities` は同関数内で後から CREATE されるため順序が要る。
- **`sub` はログに出さない**: v3 でも `owner_id` と `sub` ラップの KEK の材料（＝実質の復号経路）なので、
  ログには有無だけを書く（`google::profile_log_label`）。`crates/core/src/google.rs`。

---

## 2. Google アカウント（OAuth）

### 2.1 定数

| 定数 | 値 | アンカー |
|---|---|---|
| 認可エンドポイント | `https://accounts.google.com/o/oauth2/v2/auth` | `crates/core/src/google.rs:14` |
| トークンエンドポイント | `https://oauth2.googleapis.com/token` | `crates/core/src/google.rs:15` |
| userinfo | `https://www.googleapis.com/oauth2/v3/userinfo` | `crates/core/src/google.rs:16` |
| ループバック固定ポート | `DEFAULT_REDIRECT_PORT = 38387` | `crates/core/src/google.rs:17` |
| スコープ | `openid email https://www.googleapis.com/auth/drive.file`（**`drive.readonly` は要求しない**。appdata も使わない） | `crates/core/src/google.rs:18-25` |
| リフレッシュ余裕 | `REFRESH_SKEW_SECONDS = 60` 秒 | `crates/core/src/google.rs:21` |
| コールバック待ちタイムアウト | 300 秒（5 分） | `crates/core/src/google.rs:151` |
| コールバックのポーリング間隔 | 100 ms（非ブロッキング accept） | `crates/core/src/google.rs:155-163` |
| コールバックの read タイムアウト | 120 秒 | `crates/core/src/google.rs:168` |
| コールバック受信バッファ | 4096 byte | `crates/core/src/google.rs:170` |
| `expires_in` 既定値 | 3600 秒（応答に無い場合） | `crates/core/src/google.rs:126-129` |
| 既定 client_id（ビルド時 `THUNDOKU_GOOGLE_CLIENT_ID` 未設定時） | `1054619943130-2kaqpgnm719bp8l8rslm8rkuvdhb945s.apps.googleusercontent.com` | `crates/app/src/app_state.rs:21-25` |
| 既定 client_secret | コード内の `DEFAULT_GOOGLE_CLIENT_SECRET`（`THUNDOKU_GOOGLE_CLIENT_SECRET` で上書き可）。**デスクトップではクライアント種別が「デスクトップ アプリ」である前提**で扱う（§2.5）。実値は仕様書に転記しない | `crates/app/src/app_state.rs:27-32` |

### 2.2 OAuth フロー（installed-app / PKCE S256 + ループバック受信）

1. ログインモーダル `GoogleLoginView::new` が `AppState.google` の `GoogleClient::begin_authorize()` を呼ぶ `crates/app/src/views/google_login.rs`。
2. `begin_authorize()`: ループバック listener を確保 → `redirect_uri = http://127.0.0.1:{port}` を組み立て → verifier / challenge / state を生成 → 認可 URL を返す `crates/core/src/google.rs:408-431`。
3. listener 確保 `bind_loopback()`: `SO_REUSEADDR` を設定し `127.0.0.1:38387` に bind、`listen(128)`。失敗時は `127.0.0.1:0`（動的ポート）へフォールバック（Google Cloud Console 登録の redirect_uri と一致させるため固定ポート優先）`crates/core/src/google.rs:239-266`。
4. PKCE 素材: verifier = 32 byte 乱数の base64url（パディング無し）、challenge = `base64url(SHA-256(verifier))`、state = 16 byte 乱数の base64url `crates/core/src/google.rs:67-86`。
5. 認可 URL のクエリ: `client_id`, `redirect_uri`, `response_type=code`, `scope`, `access_type=offline`, `prompt=consent`, `state`, `code_challenge`, `code_challenge_method=S256` `crates/core/src/google.rs:90-104`。
6. **OS の既定ブラウザ**で認可 URL を開く（`google::open_browser` → `browser_command`）。アプリ内 WebView は使わない
   （RFC 8252 はネイティブアプリに外部ユーザーエージェントを求め、Google も埋め込み UA を拒否する）。
   ブラウザを開けなかった場合は URL を画面に出して手動で開いてもらう `crates/app/src/views/google_login.rs`, `crates/core/src/google.rs`（`open_browser`）。
   **Windows はコマンドライン経由（`cmd /C start`）も `explorer` も使わない**: `cmd` は `&` を
   コマンド区切り、`%XX` を環境変数として解釈するため認可 URL が最初の `&` で切れて渡り
   （実測: `cmd /C echo <URL>` は `?client_id=…` までしか出さない）、Google が 400
   `invalid_request`（「アクセスをブロック: 認証エラー」）を返す。`explorer <URL>` は
   エクスプローラーが開くだけで既定ブラウザが開かない（実測 2026-09-23）。**OS の API
   （`ShellExecuteW`）**で URL をそのままシェルへ渡す（`crates/core/src/google.rs` の
   `open_browser`、`windows-sys` 依存）。
   ※ `GoogleClient::authorize()` はブラウザを開いて最後まで実行する版で、アプリ経路は `begin_authorize`
   （URL を作る）→ ブラウザで開く → `wait_for_code`（ループバック受信）→ `complete_authorize`
   （トークン交換〜プロフィール）に分ける `crates/core/src/google.rs`。
7. `wait_for_code()` が別スレッドで `receive_callback` を実行する。**クライアントのロックは取らない**
   （待ちは最大 300 秒あり、保持すると UI 側の `google.lock()` が止まり、ログインモーダルの ✕ も
   効かなくなる）。ロックは `complete_authorize` のときだけ取る `crates/app/src/views/google_login.rs`, `crates/core/src/google.rs`。
8. `receive_callback`: 非ブロッキング accept ループ。`cancel: AtomicBool` が立っていれば `GoogleError::Cancelled`、300 秒で `Auth("authorization timed out")`。1 接続のみ受けて HTTP リクエスト行の query を `percent_decode` し、`error` → `Auth`、`state` 不一致 → `Auth("state mismatch")`、`code` 無し → `Auth("missing code")`。応答は `HTTP/1.1 200 OK` + `text/html; charset=utf-8` の短文（成功時「認証完了。このタブを閉じてください。」）`crates/core/src/google.rs:142-237`。
9. `exchange_code`: `POST https://oauth2.googleapis.com/token`、`Content-Type: application/x-www-form-urlencoded`、form は `grant_type=authorization_code&code&redirect_uri&client_id&code_verifier`（`client_secret` が設定されていれば `&client_secret=` を追加）。HTTP ステータスが 2xx 以外は `GoogleError::Token` `crates/core/src/google.rs:446-488`。
10. `parse_token_response`: JSON の `error` を検査。`access_token` 必須（空文字不可）。`refresh_token` は任意。`expires_at = 現在時刻(Unix 秒) + expires_in` `crates/core/src/google.rs:105-140`。
11. `profile()`: userinfo に `Authorization: Bearer {token}` で GET（リダイレクト追跡 5）。2xx 以外は `Auth`。`sub` / `email` / `name` / `picture` を取り出し（欠落は空文字 / None）`crates/core/src/google.rs:537-575`。
12. トークンを keyring に JSON で保存（`USER_GOOGLE`）。refresh_token を含むため期限切れ後も自動リフレッシュ可 `crates/app/src/views/google_login.rs`。
13. グローバル状態更新: `google_profile = Some(profile)`, `google_logged_in = true`, `google_login_error = None`, `google_login_done = true`（`cx.emit` は RefCell 再入でパニックするため使わない）`crates/app/src/views/google_login.rs`。
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

### 2.5 OAuth クライアントの前提（公開前の確認事項 / C-01）

デスクトップアプリのバイナリに埋め込んだ値は利用者から隠せないため、`client_secret` を
「秘密」として扱うことはできない（公開クライアントとして設計する）。したがって
**Google Cloud 側の設定がどうなっているかで安全性の評価が変わる**。ここはコードだけでは
決められないので、公開前に次を確認する。

| 確認項目 | 期待する状態 | 違反していた場合の対応 |
|---|---|---|
| クライアント種別 | **デスクトップ アプリ**（インストール型）として登録されている | Web アプリ等の機密クライアントを共用しているなら、デスクトップ用を別途作成して分離する |
| Web 版との共用 | Web 版（thundoku-web）と `client_id` / `client_secret` を共用していない | 共用しているなら Web 側の `client_secret` を失効・再発行し、公開履歴を確認する |
| リダイレクト URI | ループバック（`http://127.0.0.1`）が許可されている | 未登録だとフォールバック（動的ポート）で認可できない |
| 要求スコープ | `openid email` + `drive.file` のみ | `drive.readonly` を要求していたら最小権限に戻す（§2.1） |
| 同意画面 | アプリ名・サポート連絡先・プライバシーポリシーが設定されている | 未設定だと利用者に警告が出る |

注: `client_secret` を環境変数化・難読化しても**配布バイナリ内の秘密にはならない**
（ビルド時に埋め込まれる）。保護になるのは「クライアント種別とスコープを正しく設定すること」
であって、文字列を隠すことではない。

---

## 3. Google Drive 同期

### 3.1 API クライアント（`crates/core/src/drive/mod.rs`）

| 操作 | HTTP | 実装アンカー |
|---|---|---|
| 一覧 `list_files(folder_id)` | `GET https://www.googleapis.com/drive/v3/files?q='{folder_id}' in parents and trashed=false&fields=nextPageToken,files(id,name,size,md5Checksum,modifiedTime)&pageSize=100&spaces=drive`（`pageToken` で全ページ走査、`nextPageToken` が無くなったら終了） | `crates/core/src/drive/mod.rs:11-13`, `:133-195` |
| ダウンロード `download(file_id)` | `GET .../files/{file_id}?alt=media`、`redirects = 5`。実体は `download_with_progress`（進捗コールバックは `\|_,_\| true`） | `crates/core/src/drive/mod.rs:197-199` |
| 進捗つき取得 `download_with_progress(file_id, on_progress)` | `GET .../files/{file_id}?alt=media` を **`Transport::send_download`** で取得（上限 2 GiB + 進捗 + キャンセル）。トレイトの既定実装は `download` に委譲して完了時に 1 回通知するだけ（テストのモック用）で、`DriveClient` が上書きする | `crates/core/src/drive/mod.rs:63-74`（既定実装）, `:201-234`（本番） |
| アップロード `upload_multipart(name, folder_id, bytes)` | `POST https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart`（`DRIVE_UPLOAD_URL`）、`Content-Type: multipart/related; boundary=thundoku_shelf_boundary`（`MULTIPART_BOUNDARY`）。metadata は `{"name","parents":[folder_id],"mimeType":"application/octet-stream","appProperties":{"app":"thundoku-shelf","packId":<拡張子を除いた name>}}`。応答 JSON の `id` を返す | `crates/core/src/drive/mod.rs:12`, `:14`, `:236-285` |
| フォルダ作成 `create_folder(name)` | `POST .../files`、`{"name","mimeType":"application/vnd.google-apps.folder"}`。応答 `id` を返す | `crates/core/src/drive/mod.rs:287-309` |
| 更新日時更新 `touch(file_id)` | `PATCH .../files/{file_id}`、`{"modifiedTime": <現在 UTC の RFC3339>}` | `crates/core/src/drive/mod.rs:311-328` |
| 削除 `delete(file_id)` | `DELETE .../files/{file_id}` | `crates/core/src/drive/mod.rs:330-341` |
| 共通 | すべて `Authorization: Bearer {access_token}`。`redirects = 5`。`list_files` は 401/403 で「authorization failed」を返す | `crates/core/src/drive/mod.rs:103-130`, `:146-149` |

- `size` は Drive API が number / string のどちらでも返し得るため両対応 `crates/core/src/drive/mod.rs:16-22`。
- `DriveApi` trait（テストでモック差し替え可能）: `list_files` / `download` / `download_with_progress` / `upload_multipart` / `create_folder` / `delete` / `touch` `crates/core/src/drive/mod.rs:54-88`。
- **取得は「保存できたのに戻せない」状態を解消した**: 以前は通常 API（`Transport::send`）を通っていたため応答本文が `MAX_API_BODY_BYTES`（**16 MiB**）で打ち切られ、16 MiB 超の pack / DB JSON を取得できなかった。現在は `DriveClient::download_with_progress` が `Transport::send_download` を使うので、上限は `MAX_DOWNLOAD_BODY_BYTES`（**2 GiB**）になり、進捗コールバックとキャンセルも効く `crates/core/src/drive/mod.rs:201-234`, `crates/core/src/tbf/transport.rs:129-133`。
- `DriveError::Cancelled` を追加（進捗コールバックが `false` を返したとき。**部分的な本文は返さない**）。`From<TbfError>` は `TbfError::Cancelled` だけ `DriveError::Cancelled` に写し、他は `Network` にする `crates/core/src/drive/mod.rs:41-42`, `:45-51`。
- **Content-Length が無い応答でも進捗・キャンセルは効く**: `read_body_with_progress` が `total = 0` のとき **1 MiB 刻み**で通知し（`UNKNOWN_TOTAL_STEP`）、読み切ったら最後のサイズを一度通知する `crates/core/src/tbf/transport.rs:175-227`（`:182`, `:207`, `:224-226`）。
- 同期エンジン（`drive/sync.rs`）は `drive.download(...)` を呼ぶため（pack = `:254`、DB バックアップ復元 = `:483`、差分検査 = `:548`）、**pack も DB JSON もこの 2 GiB 経路を通る**。現状 `download_with_progress` を直接呼ぶのはテストのみで、進捗表示・キャンセルは API としては用意されているが UI からは未使用 `crates/core/src/drive/sync.rs:254`, `:483`, `:548`, `crates/core/src/drive/mod.rs:414-432`。
- 同期フォルダは My Drive 直下の `thundoku-shelf/`（Web 版 appdata ではなくユーザー可視フォルダ）`docs/features.md:440-442`。フォルダ ID は初回同期時に `create_folder("thundoku-shelf")` で作成し `app_settings['drive.sync.folder_id']` に保存 `crates/app/src/views/settings.rs:799-808`。
- **鍵 bundle `thundoku-keys.json`（v3）**: 同じフォルダに、`sub` / パスフレーズで**ラップした** PRK を置く（ファイル形式・ラップの計算は `docs/spec/10-pack-keys.md` §3.2、実装は `crates/core/src/pack_keys.rs`）。
  - 取得 `pack_keys::load_bundle(drive, folder_id, owner_id)`: 名前で一覧から探してダウンロードし、**`owner_id` が一致しない bundle は使わない**（`PackKeysError::OwnerMismatch`。黙って上書きすると相手の鍵を失うため）。無ければ `None`。
  - 保存 `pack_keys::upload_bundle`: **新しいファイルを上げてから**同名の旧ファイルを消す（先に消すと途中で失敗したときに鍵を失う。`thundoku-backup.json` と同じ順序）。
  - **未アップロードの印**は `app_settings['drive.pack_keys.pending']`（値は `owner_id`）。取り込み中のアップロード失敗は取り込みを止めず、印を残して**同期の最後に再試行**する（`retry_pending_upload`。§3.2 手順 8）。この間その鍵は端末にしか無い＝端末故障で復元不能なので、警告ログを出す。
  - keyring に PRK が無い / 未ログインのときは何もしない（再試行は `false`）。`crates/core/src/pack_keys.rs:23`, `:29`, `:214-245`, `:268-325`

### 3.2 双方向同期アルゴリズム（`drive::sync::sync`）

入力 `SyncRequest` のフィールド `crates/core/src/drive/sync.rs:176-198`: `pool`, `drive`, `packs_dir`（DL 先/UL 元）, `downloads_dir`（作業用）, `identity_sub: Option<&str>`（未ログインは None）, `pack_root_key: Option<&PackRootKey>`（v3 の PRK。未ログインは None。冊ごとの pack 鍵は `derive_pack_key(pack_id)` で導出）, `owner_key: Option<&[u8;32]>`, `folder_id`, `db_path: Option<&Path>`（None なら DB バックアップ/復元をしない）。

手順:

1. `drive.list_files(folder_id)` で全ファイル取得、件数をログ `crates/core/src/drive/sync.rs:211-213`。
2. Drive 側ファイル名から pack_id を抽出（`{pack_id}.opfspack` のみ。空・`/`・`\`・`..`・`:`・制御文字・先頭 `.` を含む id は除外）。同名は**最初の 1 件を採用**（`HashMap::entry().or_insert`）`crates/core/src/drive/sync.rs:62-65`, `:225-231`。
3. `outcome.file_count = files.len()`、`outcome.total_bytes = Σ max(size,0)` `crates/core/src/drive/sync.rs:232-237`。
4. アップロード対象集合 `upload_ids = books::owned_book_ids(pool, key, Some(sub))`。未ログイン（`identity_sub = None`）は空集合で**何もアップロードしない** `crates/core/src/drive/sync.rs:215-222`。
5. **ダウンロード方向**（Drive の各 pack について）`crates/core/src/drive/sync.rs:239-311`:
   1. `sync_state::get(pool, pack_id)` を読む。
   2. `drive_md5` が空でなく `state.md5` と一致 → スキップ（`outcome.skipped`）`crates/core/src/drive/sync.rs:241-250`。
   3. `local_changed = state があり、ローカル pack の mtime(秒) > state.last_synced_at("%Y-%m-%d %H:%M:%S")`（厳密に大。ファイル無し・metadata 取得失敗・mtime 取得失敗・日時パース失敗はすべて false = 未変更扱い）`crates/core/src/drive/sync.rs:73-93`。
   4. `drive.download(file.id)` で全バイト取得（実体は `Transport::send_download` の 2 GiB + 進捗/キャンセル経路。§3.1）`crates/core/src/drive/sync.rs:254`。
   5. `PackReader::open(&bytes)` が失敗 → **`PackError::Version` は `SyncError::UnsupportedPackVersion`**（v2 以前の pack。ストアからの**再取り込み**を案内）、それ以外は `SyncError::InvalidPack` `crates/core/src/drive/sync.rs:255-263`。
   6. ヘッダの `ENCRYPTED` が立ち、`pack_root_key` が無い → `SyncError::PackKeyRequired`（同期全体を中断）。**平文として読む・平文で上書きする経路は無い**（fail-closed）`crates/core/src/drive/sync.rs:265-268`。
   7. 競合（`local_changed` かつローカル pack が存在）→ ローカルを `packs/{pack_id}.conflict-local.opfspack` にコピーし `outcome.conflicts` に追加。**Drive 側が勝つ**。
   8. `downloads_dir` と `packs_dir` を作成し、`downloads/{pack_id}.opfspack`（一時）と `packs/{pack_id}.opfspack`（本体）の両方に書き出す。
   9. `import_book` で DB 反映（後述）。
   10. ログイン中なら `books::set_owner_sub(pack_id, encrypt(key, sub))`。
   11. `sync_state::upsert(pack_id, drive_file_id, md5, modified_time, last_synced_at=now)`。`now` は UTC の `%Y-%m-%d %H:%M:%S` `crates/core/src/drive/sync.rs:67-69`。
6. **アップロード方向**（`books::list(pool)` の全行について）`crates/core/src/drive/sync.rs:313-363`:
   1. `pack_id = book.pack_id`（無ければ `book.id`）。空ならスキップ。
   2. `upload_ids` に含まれない本（未所属・他アカウント）はスキップ。
   3. `packs_dir/{pack_id}.opfspack` が無ければスキップ。
   4. `sync_state` が無ければアップロード。あれば「Drive 側に同じ `drive_file_id` がまだ存在」**かつ**「ローカル mtime > last_synced_at」のときだけアップロード（削除は伝播させない）。
   5. `upload_multipart` 実行後、`sync_state::upsert(md5 = md5(local bytes), modified_time = None, last_synced_at = now)`。
7. **DB バックアップ**（`db_path.is_some()` のとき）`crates/core/src/drive/sync.rs:365-425`:
   1. `db::backup::export_json(pool, Some(&upload_ids))` でテキストテーブルの JSON を生成。
   2. `md5(ローカル JSON)` と Drive の `thundoku-backup.json` の `md5Checksum` を比較。
   3. 不一致（または Drive にファイルが無い）→ **先に新しいバックアップを上げてから**旧ファイルを `delete`。`outcome.database_backed_up = true`。
   4. 一致 → `drive.touch(file.id)` で `modifiedTime` だけ現在時刻に更新（バックアップの更新日時が古いままにならないように）。
   5. どちらの場合も、いま書き出した JSON の**正規形 md5 を `app_settings['drive.backup.md5']` に保存**する（起動時チェックの基準値。アップロードした場合も md5 一致でスキップした場合も Drive 上の内容はこのエクスポートと一致するため）`crates/core/src/drive/sync.rs:408-412`。
8. **鍵 bundle の再試行**（`identity_sub` があるとき）`crates/core/src/drive/sync.rs:428-440`: `pack_keys::PackKeyStore::retry_pending_upload` を呼び、`drive.pack_keys.pending` が自分の `owner_id` なら `thundoku-keys.json` を上げ直して印を消す。**失敗しても同期全体は成功**として扱う（印は残り、次の同期で再試行。§3.1）。
- 戻り値 `SyncOutcome` `crates/core/src/drive/sync.rs:26-38`: `downloaded` / `uploaded` / `skipped` / `conflicts`（pack_id の Vec）, `file_count`（Drive フォルダ内の全ファイル数）, `total_bytes`（Σ size、bytes）, `database_backed_up`, `database_restored`。
- エラー型 `SyncError` `crates/core/src/drive/sync.rs:42-60`: `Drive` / `Io` / `Db` / `Pack` / `InvalidPack` / **`PackKeyRequired`**（暗号化 pack だが PRK が無い。fail-closed） / **`UnsupportedPackVersion { pack_id, version }`**（v2 以前の pack。再取り込みを案内）。
- **削除は双方向とも伝播しない**（モジュールコメント `crates/core/src/drive/sync.rs:1-12`、`docs/features.md:447-450`）。
- 注意（実装依存）: ダウンロード方向は `HashMap` を反復するため処理順は不定 `crates/core/src/drive/sync.rs:225-231`。

### 3.3 ダウンロードした pack の DB 反映（`import_book`）

- `metadata.json` エントリを（`pack_root_key` があれば `PackKey = PRK.derive_pack_key(pack_id)` を渡して）読み、`title` / `author` / `circleName` / `purchaseDate` を採用。鍵が無い・読めない場合は `title = pack_id`、他は空（`read_entry` の失敗は握って続行）`crates/core/src/drive/sync.rs:107-109`, `:95-131`。
- `books::upsert` の固定値: `file_name = opfs_path = "{pack_id}.opfspack"`, `tags_fetched = 1`, `pack_id = Some(pack_id)`, `is_favorite = 0`, `is_hidden = 0`, `is_drm = 0`, `cover_thumbnail = None`, `tbf_product_id = None`, `site_id = None`, `created_at = updated_at = now` `crates/core/src/drive/sync.rs:133-166`。
- 続けて `import::rebuild_from_pack(pool, pack_id, pack_bytes, root_key)` でドキュメント・コンテンツ・ページ行を再構築（失敗は `log::warn` のみで続行）`crates/core/src/drive/sync.rs:168-175`。

### 3.4 DB バックアップの中身（`db/backup.rs`）

- 目的（モジュールコメント）: 画像 base64 を含む DB ファイル全体（200MB 級）を上げるのは重いため、主要テーブルのテキストのみ JSON 化する。`thumbnail_data` / `image_data` と環境依存設定（`drive.*` / `api.last_sync_at` 等）は含めない `crates/core/src/db/backup.rs:1-8`。
- 対象テーブル（16 個、この順で処理。FK 参照元が先）: `books`, `bookshelf_items`, `checked_items`, `tbf_events`, `book_contents`, `content_formats`, `reading_progress`, `page_views`, `book_tags`, `favorite_tags`, `favorite_entities`, `imported_documents`, `document_images`, `book_first_events`, `zenn_tag_metadata`, `view_history` `crates/core/src/db/backup.rs:14-31`。
- 除外カラム: `thumbnail_data`, `image_data`, `extracted_text`（`document_images` の
  ページ本文。暗号化 pack から取り出した平文で、バックアップに載ると暗号化の意味が
  失われる。アプリはこの列を読まない）`crates/core/src/db/backup.rs`。
- owner フィルタ（P3）: `book_ids` を渡すと `books` を絞り、関連テーブルも `book_id IN (...)` で連動して除外する。加えて `OwnerFilter { key, sub }` を渡すと、本に紐づかない
  `OWNER_SCOPED_TABLES`（`bookshelf_items` / `checked_items` / `book_first_events` /
  `favorite_tags` / `favorite_entities`）を `owner_sub` の復号比較で絞る `crates/core/src/db/backup.rs`。
  - `book_first_events` は `bookshelf_items` の子（FK）なので、親と同じ規則で絞らないと
    復元時に FK 違反で全体がロールバックする。
  - 復元 `import_json` は `books.id` / `books.pack_id` が安全な id でなければ**復元を中止**する
    （`pack_path::is_safe_id`。保存領域外を指す pack パスを作らせない）。
- 復元 `import_json`: 各テーブルを PK 競合時 `DO UPDATE` の UPSERT でマージ（**Drive 側優先**）。`INSERT ... ON CONFLICT DO UPDATE` は DELETE を伴わないため FK の `ON DELETE CASCADE` を発火させない `crates/core/src/db/backup.rs:163-190`, `:318-355`。PK 定義は `books:[id]`, `bookshelf_items:[site_id,database_id]`, `checked_items:[id]`, `tbf_events:[id]`, `book_contents:[content_id]`, `content_formats:[format_id]`, `reading_progress:[book_id,content_id]`, `page_views:[book_id,content_id,page_number]`, `book_tags:[id]`, `favorite_tags:[tag_name]`, `favorite_entities:[entity_kind,entity_name]`, `imported_documents:[id]`, `document_images:[id]`, `book_first_events:[site_id,database_id]`, `zenn_tag_metadata:[tag_name]`, `view_history:[id]` `crates/core/src/db/backup.rs:204-226`。
- Drive 上のファイル名: `const DB_BACKUP_NAME = "thundoku-backup.json"` `crates/core/src/drive/sync.rs:445`。
- 復元 API: `check_drive_backup(drive, folder_id)` → `DriveBackupInfo{file_id, md5, size, modified_time}`（無ければ `None`）`crates/core/src/drive/sync.rs:458-475`; `restore_drive_backup(drive, folder_id, pool)` はダウンロードして `backup::import_json` `crates/core/src/drive/sync.rs:476-518`。
- 差分判定 `inspect_drive_backup(pool, drive, folder_id, book_ids, baseline_md5)` → `Option<BackupStatus{info, drive_changed, local_differs}>` `crates/core/src/drive/sync.rs:537-568`:
  - 比較は両側を `backup::canonicalize_json` の正規形にしてから md5 で行う。正規形は「Drive 側に存在するテーブルだけに絞る」「揮発列（`books.updated_at` / `bookshelf_items.synced_at,updated_at` / `tbf_events.updated_at`）を落とす」「行を PK 順に並べる」`crates/core/src/db/backup.rs:41-129`。新テーブル追加・同期のたびに書き換わる時刻・行の物理順で毎回復元確認が出るのを防ぐ。
  - `drive_changed` = Drive 側の正規形 md5 が基準値 `app_settings['drive.backup.md5']`（最後にアップロードした内容）と違う。基準値が無ければ判定不能として false。
  - `should_offer_restore()` = `drive_changed && local_differs`。**ローカル側だけが進んだ場合は復元確認を出さない**（出すと新しいローカルを古いバックアップで上書きしてしまう）`crates/core/src/drive/sync.rs:521-535`。
- `restore_drive_backup` は復元後に**同じ内容を基準値として保存**する（ローカルに Drive に無い行が残っていると差分は消えないため、更新しないと次回起動でまた復元を促し続ける）`crates/core/src/drive/sync.rs:495-498`。
- 同期状態テーブル `drive_sync_state(pack_id PK, drive_file_id, md5, modified_time, last_synced_at)` の get/upsert/list/delete `crates/core/src/db/sync_state.rs:6-55`。

### 3.5 同期のトリガーと OFF 条件

| トリガー / 条件 | 挙動 | アンカー |
|---|---|---|
| 設定画面「今すぐ同期」 | `SettingsView::sync_drive_now` を実行（Google 未ログインならボタン disabled） | `crates/app/src/views/settings.rs:769-830`, `:1853-1863` |
| Google ログイン直後 | `drive.last_sync_at` が未設定なら「Google Drive と同期しますか？」ダイアログ → 「同期する」で `drive.sync.enabled="true"` を保存し即同期 | `crates/app/src/workspace.rs:195-209`, `:1406-1451` |
| チェックリストのポーリング | 変化があったときだけ `sync_drive_now` を呼ぶ（無変化ならノーコスト） | `crates/app/src/workspace.rs:294-296` |
| ウィンドウ終了時 | バックアップ対象に変更があれば「アップロードして終了」を提示し、`db_path` 付きで同期してから終了 | `crates/app/src/workspace.rs:750-806`, `:1467-1472` |
| 起動時 | ログイン済み かつ `drive.sync.enabled` が `"true"`/`"1"` かつ `drive.sync.folder_id` があり、`inspect_drive_backup` が「Drive 側が最後のアップロードから動いた」と判定したときだけ復元確認ダイアログ（ローカル側だけが進んだ場合は出さない） | `crates/app/src/workspace.rs:582-673`, `:1359-1364` |
| OFF スイッチ | 設定のトグルで `drive.sync.enabled` に `"true"`/`"false"` を保存（起動時読み込み時に `"true"` のみ有効） | `crates/app/src/views/settings.rs:754-766`, `:133` |
| 同期情報のクリア | `drive_sync_state` を全 DELETE し、`drive.sync.enabled` / `drive.sync.folder_id` / `drive.last_sync_at` / `drive.file_count` / `drive.total_bytes` / `drive.backup.md5` を削除（次回は全ファイルが再送/再取得対象） | `crates/core/src/drive/sync.rs:569-583`, `crates/app/src/views/settings.rs:2478-2509` |
| 未ログイン | `sync_drive_now` は「Google にログインしてください」で失敗。同期エンジン側も `identity_sub=None` でアップロード対象ゼロ | `crates/app/src/views/settings.rs:792-795`, `crates/core/src/drive/sync.rs:215-222` |
| 暗号化 pack だが鍵が無い | `SyncError::PackKeyRequired`（同期全体を中断。**平文として読む・平文で上書きする経路は無い**）。UI はログインとパスフレーズによる鍵の復元を案内する | `crates/core/src/drive/sync.rs:265-268` |
| v2 以前の pack を取得 | `SyncError::UnsupportedPackVersion { pack_id, version }`（`PackReader::open` の `Version` を写す）。**ストアからの取り込み直し**を案内する | `crates/core/src/drive/sync.rs:255-263` |
| 鍵 bundle が未アップロード | 同期の最後に `retry_pending_upload` が上げ直す（失敗しても同期は成功扱い。印は残る） | `crates/core/src/drive/sync.rs:428-440`, `crates/core/src/pack_keys.rs:214-245` |

- 同期後に `drive.last_sync_at`（UTC `%Y-%m-%d %H:%M:%S`）/ `drive.file_count` / `drive.total_bytes` を保存し、設定画面に表示（ローカル時間に変換して表示）`crates/app/src/views/settings.rs:845-858`, `:1645-1663`。
- 同期完了トーストは「同期完了（DL n / UL n / スキップ n / 競合 n）[/ DB バックアップ]」`crates/app/src/views/settings.rs:868-882`。
- 同期は `drive_enabled` 設定に関わらず**手動実行は可能**（無効時もエンジンは動く）。無効化は「終了時アップロード」等の自動導線を止める意味を持つ `crates/app/src/views/settings.rs:754-766`（※意図の明記はコード上なし → 「推測」節）。

---
