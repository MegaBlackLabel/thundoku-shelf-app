# ログアウト仕様（技術書典・BOOTH）

アプリのログアウトは「**サーバー側セッションの破棄** + ローカルクリア」の 2 段階。
サイト側のセッションが残っていると「ログアウトしても再ログインできる」ように見えるため、
各サイトの正規のログアウト API を叩く（2026-08-27 調査・実装済み）。

## 技術書典（techbookfest.org）

- **Web 版と同じ GraphQL ミューテーション**:
  - `POST https://techbookfest.org/api/graphql?operationName=LogoutUserMutation`
  - クエリ: `mutation LogoutUserMutation($input: LogoutUserInput!) { logoutUser(input: $input) { clientMutationId user { id email } } }`
  - Cookie + `X-XSRF-TOKEN` ヘッダー（ログイン時に吸収済みの CSRF）を送る
- 実装: `crates/core/src/tbf/mod.rs` の `TbfClient::logout()`
- 注意: `POST https://techbookfest.org/user/signout`（HTTP エンドポイント）は **200 を返すがログアウトを実行しない**（疑似成功）— 使わないこと

## BOOTH（booth.pm）

2 段階（plaza = pixiv アカウント + booth.pm アプリ）：

1. **plaza（pixiv アカウント）**:
   - `POST https://accounts.booth.pm/users/sign_out` + `_method=delete` + `X-CSRF-Token`
   - 失敗しても続行（booth.pm 側だけでもログアウトとして機能する）
2. **booth.pm（Rails アプリ）**:
   - `POST https://booth.pm/users/sign_out` + `_method=delete` + `X-CSRF-Token` + `Referer/Origin`
   - 成功時は **204 No Content**（確認済み）

- **CSRF トークンの取得**: ログイン済みで `GET https://booth.pm/ja` → `<meta name="csrf-token" content="...">`
- 実装: `crates/core/src/booth.rs` の `BoothClient::logout()`
- 注意点:
  - `https://accounts.booth.pm/users/sign_out` を **DELETE メソッド単体**で叩くと 422（CSRF 不足）
  - `https://booth.pm/logout` は 404

## WebView（ログインモーダル）の Cookie

- **BOOTH ログイン用 WebView は `with_incognito(true)`（non-persistent ストア）を使う**（`booth_login.rs`）
- 永続ストア（`WKWebsiteDataStore.default`）だと pixiv/booth のセッションがアプリ再起動をまたいで残り、
  ログアウト後にログイン WebView を開くと pixiv の SSO で自動再ログインされ「ログアウトが効かない」ように見える
- incognito 化により: ログイン WebView を閉じると Cookie も破棄 → 次のログインはクリーンな状態から
- ログイン完了時の Cookie 収集は `booth.pm` + `accounts.booth.pm` の両方から（`_plaza_session_*` はログアウトに必要）

## UI 側の挙動（settings.rs）

- アカウントパネル・設定画面のログアウトボタンは `SettingsView::logout_tbf` / `logout_booth` を経由する
- サーバー側の成功/失敗に応じてトーストを出し分ける:
  - 成功: 「〜からログアウトしました（サイト側のセッションも破棄しました）」
  - 失敗: 「〜からログアウトしました（サイト側のセッションは残っています）」
- サーバー側失敗時も**ローカルクリアは実行**（アプリとしてはログアウト状態にする）

## デバッグログ

- `logout_tbf: server logout ok/failed`
- `booth logout(plaza): status=... location=...` / `booth logout(booth): status=...`
