# ログアウト仕様（技術書典・BOOTH）

## 表示と失敗時の扱い（2026-09-22 追記）

- ローカルの利用状態は**即時に**落とす（メモリ上のセッション / トークンを破棄する）。
- 永続値（DB の `*.session` / keyring のトークン）の**削除は結果を待ってから**知らせる。
  失敗したまま「ログアウトしました」と出すと、共有端末などで「消えた」と誤解させる。
  - 失敗時の文言: 「〜 からログアウトしました（端末に保存したセッション情報を削除できませんでした。再起動すると復元される可能性があります）」。
- 削除に失敗したら、**次回起動で復元しない印**（`<data_dir>/session-purge.pending`）を残す。
  DB / keyring とは別の障害領域（データディレクトリのファイル）に書くので、片方が壊れていても記録できる。
- 「メモリ上で使用停止」「端末の保存情報の削除」「サイト側の許可取り消し」は**別操作**として扱い、
  **成功した範囲だけ**を表示する。
- BOOTH / FANZA / DLsite / 技術書典 のセッションは keyring の鍵で暗号化して DB に置く（`crates/core/src/session_store.rs`）。
  鍵が無い環境では保存しない（平文へは戻さない）。
- 技術書典のログアウトは、上に加えて**ログインに使った内蔵ブラウザー（WebView）の保存データ**も
  消す（`clear_all_browsing_data`。セキュリティ評価 2026-09-25 の F05）。消せなかった場合は
  「ブラウザーに保存したログイン情報を削除できませんでした」を付けて知らせる（消えていないのに
  「消した」と見せない）。**消去を「呼べたか」しか分からない**（下の「WebView（ログインモーダル）の
  Cookie」を参照）。

## 起動時（印をどう消費するか・2026-09-26 修正）

- 印（`session-purge.pending`）がある起動では、**削除を再試行し、成功したときだけ**印を外す。
  失敗したら印を残し、その起動では保存済みトークン・プロフィールを**一切復元しない**
  （`crates/app/src/app_state.rs` の `clear_pending_logout`）。
- 以前は削除の成否を見ずに印を消していたため、keyring の削除に失敗した端末では**同じ起動で**
  保存済みトークンを読み戻していた（＝ログアウトしたのにログイン状態で起動する。
  セキュリティ評価 2026-09-25 の F04）。トークンだけでなくプロフィール（`sub`）の削除結果も
  判定に含める（`google::delete_saved_profile` が成否を返す）。
- `SecretStore::delete` は**エントリが無い場合も成功**として返す（`keyring::Error::NoEntry`）ので、
  消し忘れが無い通常の状態では印は外れる。印が残り続けるのは実際の削除失敗（ロック・権限拒否）
  だけ。

### 技術書典（2026-09-26 追記）

- 技術書典のセッションは BOOTH / FANZA / DLsite と同じ vault（保存時刻つきの暗号文・**7 日**）から
  起動時に復元する。7 日を過ぎた値は破棄して未ログインで起動する（＝再ログイン案内。
  ログイン必須のチェックリスト・試し読みは `tbf_logged_in` が false なので導線ごと閉じる）。
- 旧版は keyring（`thundoku_core::secrets::USER_TECHBOOKFEST`）に**期限なし**で置いていた。
  起動時に **vault が空なら一度だけ vault へ移行**し、keyring の値を消す
  （`SessionVault::adopt_legacy`）。
  - keyring の削除に失敗したら印を残す。印がある起動では**移行し直さない**（期限の管理外の値を
    再利用しない）うえで削除を再試行し、成功したときだけ印を外す。
  - vault に既に値があるときは移行しない（vault のほうが新しい）。keyring の旧値は消すだけ。
- ログアウトで消す対象は ①メモリ ②vault（DB）+ 旧 keyring ③サイト側 ④WebView の保存データ。

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
- **技術書典は非 incognito（永続）のまま**にする（SSO の自動再ログインが無いため）。その代わり、
  ログアウト時に `WebView::clear_all_browsing_data()` で保存データを消す（`views/mod.rs` の
  `clear_login_webview_data` → `tbf_login` 側のログイン WebView と同じ既定プロファイル）。
  - 消去は **WebView のインスタンスからしか呼べない**ため、ログアウト時は一時的な WebView を
    作って消去し、消去が走る猶予（3 秒）をおいてから破棄する。
  - wry は消去の**完了を知らせない**（完了ハンドラーを公開していない）ので、結果として扱えるのは
    「消去を呼べたか」だけ。生成に失敗した場合も「消去できていない」として利用者に伝える。
  - 消去対象は既定プロファイル（非 incognito の WebView が共有する exe ごとの保存領域）なので、
    incognito を使う BOOTH / FANZA / DLsite のログインには影響しない。

## UI 側の挙動（settings.rs）

- アカウントパネル・設定画面のログアウトボタンは `SettingsView::logout_tbf` / `logout_booth` を経由する
- サーバー側の成功/失敗に応じてトーストを出し分ける:
  - 成功: 「〜からログアウトしました（サイト側のセッションも破棄しました）」
  - 失敗: 「〜からログアウトしました（サイト側のセッションは残っています）」
- サーバー側失敗時も**ローカルクリアは実行**（アプリとしてはログアウト状態にする）
- 技術書典は上の 2 つに加えて、**端末の保存情報の削除に失敗した**場合（成功として見せない）と、
  **WebView の保存データを消せなかった**場合を文言に含める（`tbf_logout_message`）。
  WebView の生成は非同期（Windows）なので、結果が揃ってから 1 回だけ通知する。

## デバッグログ

- `logout_tbf: server logout ok/failed` / `logout_tbf: local cleared (経過時間)`
- `logout_tbf: WebView の保存データを消去できません: ...`（失敗時のみ）
- `webview: 保存データの消去を要求 ok/failed`
- `tbf session: keyring の旧保存値を vault へ移行しました`（移行時のみ）
- `booth logout(plaza): status=... location=...` / `booth logout(booth): status=...`
