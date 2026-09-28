# 10. pack の鍵（v3 = ルート鍵 + ラップ）

> **状態: Rust 側は実装済み（`crates/opfspack` = Phase A / `crates/core` = Phase B）/ Web 版は未対応**。
> 実装の正は `crates/opfspack/src/keys.rs` と `crates/core/src/pack_keys.rs`、pack 形式と鍵導出の
> 記述は `docs/spec/03-import-and-pack.md` §4.5・§6.2。**読み出しも v3 のみ**で、v2 の pack は
> `unsupported pack version` として拒否され、再取り込みが要る。
> **残り**: Web 版（`thundoku-shelf` モノレポ / `packages/opfspack`）の対応（§7 のチェックリスト）と、
> アプリ側の配線（パスフレーズの入力 UI など。本章は core / opfspack の仕様まで）。
> **§11（メタデータバックアップの暗号化）も Rust 側は実装済み**（2026-09-25 / R06）。
> `thundoku-backup.json` は PRK がある限り**暗号化された封筒（v3）**になり、v2 の平文も読める。
>
> **決定（2026-09-24）**: 方式は **C（`sub` ラップ + 任意のパスフレーズラップ）**。
> α 版のため**後方互換は切る**。**パスフレーズラップも今回の実装に含める**。
> Web 版は本仕様を見て後から対応する。
>
> **決定（2026-09-25 / R06）**: Drive に上げる**メタデータバックアップも暗号化する**（§11）。
> ローカルの SQLite は対象外（ディスク暗号化 + OS アカウント分離が前提）。v2 の平文
> バックアップは**読めるまま残す**（既存の控えを読めなくしない）。
>
> 背景: 旧実装（v2）は `sub` から鍵を導出していたため、**`sub` を知る相手は pack を復号できる**。
> 本仕様は鍵材料を乱数化し、導出可能なのは「ラップを解ける要因」だけにする。

## 0. この章の読み方

- 数値・文字列・バイト列は**バイト一致が要件**（Rust 版と TS 版で同じ pack を読めること）。
- 実装アンカーは `crates/opfspack/src/keys.rs`（ラップ・bundle・PRK）と `crates/core/src/pack_keys.rs`
  （keyring / Drive の解決と保管）。pack 本体の数値（header / version / 上限）は
  `docs/spec/03-import-and-pack.md` §4 を参照。
- 断定的に書いていない事項は §10「不明点」に分離してある。

## 1. 目的と脅威モデル

### 1.1 守るもの / 守らないもの

| | 内容 |
|---|---|
| 守る | `.opfspack` の中身（ページ画像・メタデータ）。**鍵材料は乱数**で、`sub` からは導出できない |
| 守らない | 端末の OS アカウントを奪われて keyring を読まれる場合（PRK そのものが取られる）／ブラウザの XSS（Web 実装が鍵を保持する）／Drive アカウントごと奪われる場合（既存端末の keyring が無ければ復号はできないが、鍵の更新はできない） |

### 1.2 どの漏洩で何が解けるか（正直な強度）

`sub` ラップは**利便性（Google ログインだけで読める）のための経路**であり、単体では
「pack ファイル 1 つが漏れた」場合の防御にしかならない。

**現行の `set_passphrase` はパスフレーズラップを追加するだけで `sub` ラップを消さない**
（`crates/core/src/pack_keys.rs` の `set_passphrase` は `bundle.upsert_wrap(...)` のみ）。
したがって Drive ごと漏れる想定（`thundoku-keys.json` + `sub`）では、**パスフレーズの有無は
強度に影響しない**。パスフレーズは「Google に依存しない**復元手段**」であって、追加の防御では
ない（**追加防御にしたい場合は「パスフレーズ必須モード」をオンにする**＝ `sub` ラップを削除する。
§5.2.1 / 2026-09-26 実装済み。オンにすると下表の最終列になる）。

| 漏れたもの | `sub` ラップのみ | パスフレーズラップあり（現行＝`sub` ラップ併存） | `sub` ラップ無し（必須モード §5.2.1） |
|---|---|---|---|
| pack ファイル 1 つ | 解けない（bundle が要る） | 解けない | 解けない |
| pack + `thundoku-keys.json` | **解ける**（`sub` を知っていれば） | **解ける**（`sub` ラップが残っている） | 解けない（パスフレーズが要る） |
| 上記 + `sub` | 解ける | **解ける** | 解けない |
| 端末の keyring | 解ける（PRK を直接取られる） | 解ける | 解ける |

- `thundoku-keys.json` に `sub` は**入らない**（`owner_id` は `sub` のハッシュで一方向）。
  したがって「bundle だけ漏れた」場合は `sub` を別途入手しないと解けない。
- v2 との比較: v2 は「pack ファイル + `sub`」で解けた。v3 は**bundle が無ければ解けない**。
  これが移行の実利（＋後述のローテーション可能性）。

### 1.3 鍵材料を `sub` から離す実利（強度以外）

- **ローテーションできる**: パスフレーズの変更・`sub` ラップの削除は**ラップの作り直しだけ**で済む
  （v2 は `sub` 由来の鍵で pack 自身を暗号化していたため、全 pack の再暗号化が必要＝事実上不可能だった）。
- **失効は限定的**: パスフレーズを変えても**`sub` ラップが残っている限り `sub` を知る相手は解ける**
  （現行の `set_passphrase` は `sub` ラップを消さない）。`sub` ラップを消す運用
  （§5.2.1 の「パスフレーズ必須モード」）にしたときだけ、`sub` を知る相手を締め出せる。
  **PRK そのものが漏れた場合はローテーションでは無効化できない**（新しい PRK で全 pack を
  再暗号化する必要があり、現状は未実装 — §9）。

## 2. 鍵階層と定数

```
PRK (pack root key, 32B 乱数・アカウントごとに 1 個)
  └─ pack_key(pack_id) = HKDF-SHA256(ikm = PRK, salt = UTF8(pack_id), info = HKDF_INFO, 32B)
       └─ エントリ = AES-256-GCM(key = pack_key, iv = 12B 乱数, AAD なし)

PRK の保管 = ラップ（Wrapping）= 以下で AES-256-GCM した 32B
  KEK_sub        = PBKDF2-SHA256(password = UTF8(sub) ‖ APP_SALT, salt = APP_SALT, 100_000, 32B)
  KEK_passphrase = PBKDF2-SHA256(password = NFKC(passphrase) の UTF-8, salt = ランダム 16B, iterations, 32B)
  ラップの AAD    = UTF8("thundoku-pack-root:1:" + owner_id)
```

| 定数 | 値 | 備考 |
|---|---|---|
| `APP_SALT` | `b"opfspack-v1-identity-salt-2024"` | v2 と同一（`sub` ラップの KEK は v2 の master key そのもの） |
| `PBKDF2_ITERATIONS`（sub） | `100_000` | 同上 |
| `PBKDF2_ITERATIONS`（passphrase） | `600_000` | ラップごとに `iterations` を記録するので将来上げられる。**実測: release ビルドで 1 回 ≒ 60ms**（debug は ≒ 3.4s。2026-09-24 計測） |
| `MIN_PASSPHRASE_CHARS` | `12` | パスフレーズの最低文字数。強度は反復回数より**長さ**に強く効くため、短いものは設定時に拒否する（`crates/core/src/pack_keys.rs`） |
| `HKDF_INFO` | `b"opfspack-entry-key"` | v2 と同一 |
| `owner_id` | `SHA-256("opfspack:v1:" + sub)` の小文字 hex | 既存 `derive_owner_id` と同一 |
| ラップの AAD | `"thundoku-pack-root:1:" + owner_id` | 別アカウントのラップへの差し替えを検出する |

- 既存の `thundoku-shelf.db-key` / `thundoku-shelf.session-key` とは**別の鍵**（用途分離を維持）。
- PRK は**アカウントごと**（`owner_id` ごと）。アカウント切替時はそれぞれの PRK を使う。
- 実装側の定数名（`crates/opfspack/src/`）: `APP_SALT` / `SUB_WRAP_ITERATIONS` / `PASSPHRASE_WRAP_ITERATIONS`（`lib.rs:46-51`）、`HKDF_INFO`（`crypto.rs:16`）、`KEY_BUNDLE_FORMAT_VERSION` / `WRAP_AAD_PREFIX` / `PASSPHRASE_SALT_LEN` / `WRAP_CIPHERTEXT_LEN`（`keys.rs:24-40`）。

## 3. ラップの保存形式

### 3.1 置き場所

| 場所 | 内容 |
|---|---|
| 端末（デスクトップ） | OS keyring。service = 既存の `SERVICE`、user = **`thundoku-shelf.pack-root-key:<owner_id>`**、値 = PRK の base64 |
| Drive | ファイル **`thundoku-keys.json`**（`thundoku-backup.json` と同じフォルダ）。下記の bundle |

### 3.2 `thundoku-keys.json`

```json
{
  "format_version": 1,
  "owner_id": "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0",
  "created_at": 1790000000000,
  "updated_at": 1790000000000,
  "wraps": [
    {
      "kind": "sub",
      "kdf": "pbkdf2-sha256",
      "iterations": 100000,
      "salt": "b3Bmc3BhY2stdjEtaWRlbnRpdHktc2FsdC0yMDI0",
      "nonce": "AAECAwQFBgcICQoL",
      "ciphertext": "nHt3APH/39gU7sCBwhoBh7xDgqgW9a...="
    },
    {
      "kind": "passphrase",
      "kdf": "pbkdf2-sha256",
      "iterations": 600000,
      "salt": "<base64 16B 乱数>",
      "nonce": "<base64 12B 乱数>",
      "ciphertext": "<base64 48B = 32B PRK + 16B tag>"
    }
  ]
}
```

| フィールド | 型 | 規則 |
|---|---|---|
| `format_version` | number | `1` 固定。上がったら読めない版として拒否する |
| `owner_id` | string | `derive_owner_id(sub)`。アプリは自分の `owner_id` と一致する bundle だけを使う |
| `created_at` / `updated_at` | number | Unix ミリ秒。`updated_at` はラップを触るたびに更新（同期の比較用） |
| `wraps[]` | array | 1 個以上。`kind` の重複は禁止（同じ `kind` は置換する） |
| `wraps[].kind` | string | `"sub"` または `"passphrase"` |
| `wraps[].kdf` | string | `"pbkdf2-sha256"` のみ（将来 `argon2id` を許す余地） |
| `wraps[].iterations` | number | `kdf` の反復回数。**復号側はこの値を使う**（実装定数を使わない）。0 と `MAX_PASSPHRASE_ITERATIONS`（1000 万）超は**PBKDF2 を走らせる前**に拒否する（鍵ファイルは同期先からも来るため、書き換えられた値で復元中の端末の CPU を焼かせない。セキュリティ評価 F06） |
| `wraps[].salt` | string (base64) | `kdf` の salt。`kind=sub` は `APP_SALT` のバイト列（固定） |
| `wraps[].nonce` | string (base64) | AES-GCM の 12B IV |
| `wraps[].ciphertext` | string (base64) | `AES-256-GCM(KEK)(PRK)` = 32B + 16B tag = **48B 固定** |

- `kind=sub` の KEK: `PBKDF2-SHA256(password = UTF8(sub) ‖ APP_SALT, salt = APP_SALT, iterations = 100_000)`
  （v2 の `master_key` と同式。Web 版の既存 `deriveMasterKey` をそのまま使える）。
- `kind=passphrase` の KEK: `PBKDF2-SHA256(password = NFKC(passphrase) の UTF-8, salt = wraps[].salt, iterations = wraps[].iterations)`。
- ラップの AAD（両 kind 共通）: `UTF8("thundoku-pack-root:1:" + owner_id)`。**AAD を付け忘れると復号できない**。
- base64 は標準アルファベット + padding（`Buffer`/`btoa` 互換）。base64url は使わない。

## 4. 解決順序（PRK をどう得るか）

### 4.1 デスクトップ

0. **未ログイン（Google のプロフィールが無い）は鍵を用意できないので、取り込みを失敗させる**
   （`ImportError::LoginRequired`。平文 pack を作らない — セキュリティ評価 F03）。UI は
   ダウンロードを始める前にログインを促す（`crates/app/src/views/bookshelf.rs` の
   `require_import_login`）。
1. keyring に `thundoku-shelf.pack-root-key:<owner_id>` があれば**それを使う**（何も尋ねない）。
2. 無ければ Drive から `thundoku-keys.json` を取得し、`owner_id` 一致の bundle を選ぶ。
3. `kind=passphrase` がある場合はパスフレーズを尋ねる。入力があればそれで復号し、
   失敗したら**エラー**（`sub` ラップへ黙って落ちない）。利用者が「スキップ」した場合のみ
   `kind=sub` を使う。
4. 復号できたら keyring に保存する（既定）。
5. bundle が無い／どのラップも解けない場合:
   - まだ 1 冊も暗号化 pack を作っていない → **新規作成**（§5.1）。
   - 既に pack がある → **鍵が無い**として取り込みを失敗させる（平文に落とさない。現行の
     `ImportError::IdentityKeyUnavailable` の位置づけを継承）。

### 4.2 Web

keyring が無いので 2 → 3 の順。`sub` は既存の認証セッション（`googleProfile.sub`）から得る。
復号した PRK は**メモリのみ**に置く（保存するなら IndexedDB。XSS で抜かれる前提で扱うこと）。

## 5. 生成・配布・ローテーション

### 5.1 生成

- 暗号化 pack を作る直前に PRK を用意する（無ければ `OsRng` で 32B 生成 → keyring 保存）。
- `owner_id` を計算し、`kind=sub` のラップを作って bundle を Drive へアップロードする。
- **アップロードに失敗しても取り込みは続行し、未アップロードの印を残して次の同期で再試行する**
  （ネットワークが無いと取り込めない、という事態を避ける）。ただしその間、鍵はその端末に
  しか無い＝端末故障で復元不能なので、設定画面とログに警告を出す。
- bundle が既にあれば `wraps` をマージする（同じ `kind` は置換、`created_at` は既存値を保持、
  `updated_at` を更新）。

### 5.2 ローテーション（pack の再暗号化は不要）

| 操作 | 手順 |
|---|---|
| パスフレーズを設定 | `kind=passphrase` のラップを追加（新しい salt / iterations）。PRK は変えない |
| パスフレーズを変更 | 同 kind を置換（`passphrase` ラップを作り直す） |
| `sub` 依存をやめる（**パスフレーズ必須モード**） | `kind=sub` のラップを削除してアップロードする（`PackKeyStore::enable_passphrase_only`）。以後はパスフレーズが唯一の経路になる。**実装済み**（2026-09-26 / セキュリティ評価 F01）。戻すのは `disable_passphrase_only`（PRK が要る＝パスフレーズか keyring で本人確認できることが前提）。**既定はオフ**で、設定画面「本の鍵」から明示的にオンにする |
| 別端末を切り離す | パスフレーズを変更する（その端末が保存している PRK では新しい pack を読めない） |

### 5.2.1 パスフレーズ必須モード（`sub` ラップを消す。セキュリティ評価 F01）

| | 内容 |
|---|---|
| 何をする | bundle から `kind=sub` のラップを削除し、（必要なら）`kind=passphrase` を upsert する（**1 回の書き込み**。Drive に中途半端な状態を残さない） |
| パスフレーズを尋ねる条件 | **まだパスフレーズが設定されていないときだけ**。既に設定済みなら `enable_passphrase_only(..., None)` で**既存のラップをそのまま使う**（値を尋ね直さない。必要なのは `sub` ラップを消すことだけ）。値も既存のラップも無ければ `PassphraseRequired` で断る（必須にした瞬間に解錠手段が無くなるため） |
| 何が変わる | Google ログイン（`sub`）だけでは PRK に戻せない。Drive の `thundoku-keys.json` と `sub` を**両方**奪われても本は復号できない（§1.2 の表の最終列） |
| 解錠 | パスフレーズを尋ねる（`decide_root_key`）。**「スキップ」では復元しない**（`PackKeysError::PassphraseRequired`。`sub` へ戻る経路が無い） |
| 代償 | 新端末で Google ログインだけでは読めない（Web 版の「ログインだけで読める」経路も止まる）。**パスフレーズを忘れると復元できない** |
| 既定 | **オフ**。設定画面「本の鍵」の「必須にする」で明示的にオンにする（パスフレーズ入力欄の値を使う）。オンのときは「必須を解除する」が出る |
| 端末の keyring | **残る**（この端末では PRK をそのまま使うので、端末を奪われた場合は §1.2 の最終行どおり解ける）。他の端末を切り離すには、その端末でログアウト/鍵を消すか、パスフレーズを変更する |
| 既に漏れた鍵 | 無効化できない。それには**鍵のローテーション**（PRK の入れ替え＋全 pack の再暗号化）が要る＝未実装（§9） |

実装は `crates/core/src/pack_keys.rs`（`enable_passphrase_only` / `disable_passphrase_only` / `has_sub_wrap`）、
UI は `crates/app/src/views/settings.rs`（「本の鍵」カード）、解錠の判断は
`crates/app/src/pack_keys.rs` の `decide_root_key`。テストは同ファイルの
`passphrase_only_bundle_never_falls_back_to_sub` と `crates/core/src/pack_keys.rs` の
`enable_passphrase_only_removes_the_sub_wrap`。

### 5.3 復旧

- keyring を失っても、**Drive の bundle + `sub`（または パスフレーズ）** で復旧できる。
- bundle を失っても、PRK を持つ端末が 1 台あれば再アップロードで復旧できる。
- 両方を失った場合、pack は**復号不能**（鍵は 32B 乱数で総当たり不可）。この場合でも
  ストアから再ダウンロードして取り込み直せる（付箋・履歴は DB 側の別問題）。

## 6. pack v3 の形式差分

| 項目 | v2（旧・読めない） | v3（現行） |
|---|---|---|
| `FORMAT_VERSION`（header） | `2` | `3` |
| 鍵スケジュール | `sub` → master → `HKDF(master, pack_id)` | `HKDF(PRK, pack_id)` |
| header / entry の `ENCRYPTED` | あり | あり（同じ） |
| entry の `IDENTITY_BOUND` | `ENCRYPTED` と必ず同時に立つ | **立てない**（v3 で立っていれば `Corrupted`） |
| エントリ暗号 | AES-256-GCM / 12B IV / AAD なし | 同じ |
| pack に埋め込む識別子 | なし（`sub` は鍵材料として外から渡す） | なし（鍵材料も識別子も持たない） |
| 読み出し | v2 のみ | **v3 のみ**（v2 は `Version(2)` で拒否＝再取り込みを案内） |
| 書き出し | v2 | **v3 のみ** |

- **header version で鍵スケジュールを選ぶ**（フラグでは選ばない）。v3 の `IDENTITY_BOUND` は
  定義済みビットではないので、立っていたら壊れた pack として拒否する（実装では
  `entry_flags::KNOWN_MASK` に含めず、`Corrupted("unknown entry flags …")` になる）。
- 別アカウントの pack を開こうとした場合の挙動は v2/v3 で同じ（AES-GCM の認証失敗 →
  `Corrupted("decryption failed: …")`）。
- v2 の pack は開けない（`PackError::Version(2)`）。利用者には「旧形式のため**再取り込みが必要**」と
  案内する（取り込み直せば v3 で保存される）。

## 7. Web 実装チェックリスト（`thundoku-shelf` モノレポ）

- [ ] `packages/opfspack/src/auth/identity-key.ts`: 既存 `deriveMasterKey(sub)` を
      **`deriveSubWrapKek(sub)` として再利用**（式は同じ。用途名を変えて誤用を防ぐ）。
- [ ] `unwrapPackRoot({ salt, iterations, nonce, ciphertext }, kek, ownerId)` を追加
      （Web Crypto: `crypto.subtle.deriveBits({ name: "PBKDF2", salt, iterations, hash: "SHA-256" }, ...)`
      → `crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce, additionalData: UTF8("thundoku-pack-root:1:" + ownerId) }, ...)`）。
- [ ] `derivePackKey(prk, packId)` を追加（`HKDF`: `importKey("raw", prk, "HKDF")` →
      `deriveBits({ name: "HKDF", hash: "SHA-256", salt: UTF8(packId), info: UTF8("opfspack-entry-key") }, ..., 256)`）。
- [ ] `thundoku-keys.json` の取得（Drive の同じフォルダ）・`owner_id` 照合・`format_version` 検査・
      `kind` 選択・パスフレーズ入力 UI（**NFKC 正規化**してから UTF-8 化）。
- [ ] パスフレーズが設定されている場合は**先に**そちらを試し、失敗時に `sub` ラップへ
      黙って落ちない（デスクトップと同じ規則）。
- [ ] `reader.ts`: **header version 3 のみ**対応。v3 の `IDENTITY_BOUND` は不正として弾く。
      v2 は `unsupported version` として拒否する（移行は再取り込み）。
- [ ] `builder.ts`: 常に v3 を書き出す。`identityBinding` 引数の扱いを version 3 ベースに変更。
- [ ] PRK・パスフレーズをログ / URL / `localStorage` に置かない。IndexedDB に置く場合は
      XSS 前提のリスクをコメントで明記する。
- [ ] §8 のテストベクタをテストとして固定する（KDF とラップの両方）。

## 8. テストベクタ（実装間の一致確認）

**KDF**（§2 の式で計算。独立実装（Python / WebCrypto）で検算済み）

| 入力 | 期待値 |
|---|---|
| `owner_id("test-sub")` | `6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0` |
| `KEK_sub = PBKDF2(sub="test-sub")` | `e0aaeaedbc447f52d089dc7f7422eb4575edb21c67fe954c006e9a1c9131bd73` |
| `pack_key v2 = HKDF(KEK_sub, pack_id="test-pack")` | `1ac84c9cbe517c58667657c7536b5a7a90e3e0d9a3d584732bae2e345ed97d29` |
| `pack_key v3 = HKDF(PRK, pack_id="test-pack")`、`PRK = 00 01 … 1f` | `9c65c8705e14eacc536cf438b3ff2fa58d399ffa50b10c451d543b2c058f7cd6` |

**ラップ**（`PRK = 00 01 … 1f` / `KEK = KEK_sub("test-sub")` / `nonce = 00 01 … 0b` / AAD は §2 の式）

| 項目 | 期待値 |
|---|---|
| `nonce` (hex) | `000102030405060708090a0b` |
| `ciphertext` (hex) | `9c7b7700f1ffdfd814eec081c21a0187bc4382a816f5a48918a789a5f485bd1b428c7b8cffa54ccdacf74a0ae0e094fa` |
| `AAD` (UTF-8) | `thundoku-pack-root:1:6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0` |

- PBKDF2 / HKDF の検算用（実装の前提そのものの確認）: PBKDF2-HMAC-SHA256(`"password"`, `"salt"`, 1 回, 32B)
  = `120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b`／RFC 5869 Test Case 1 の OKM。
- **Rust 側はこの表をテストで固定している**: `crates/opfspack/tests/keys_v3.rs`（`pack_key v3` と `sub` ラップの暗号文）・`crates/opfspack/tests/interop.rs`（`owner_id`）。値を書き換えるときは仕様書と実装を同時に直す。`pack_key v2` の行は**廃止した旧方式の参考値**（v2 の pack は読めない）。

## 9. 未実装・将来のオプション（現時点で実装しない）

（パスフレーズ必須モードは 2026-09-26 に実装済み。§5.2.1）

- 「この端末に鍵を保存しない（毎回パスフレーズを尋ねる）」設定（共有端末向け）。
- パスフレーズの代わりになる明示的な回復コード（現状は「Drive の bundle + パスフレーズ」が回復手段）。
- 鍵のローテーション（PRK そのものの入れ替え＝全 pack の再暗号化）。漏洩が疑われる場合に
  現行でできるのは「鍵（keyring と bundle）を消して新しい PRK を作り、本を取り込み直す」運用
  だけ（古い `.opfspack` は開けなくなる）。

## 10. 不明点

| 項目 | 現状 |
|---|---|
| パスフレーズの PBKDF2 反復回数 | `600_000` で確定（release 実測 ≒ 60ms。ローカルの解錠では体感できない。将来上げる場合はラップの `iterations` を上げて作り直すだけ＝ PRK も pack も変えない） |
| NFKC 正規化の必要性 | IME / OS による差（合成文字）を避けるために仕様に含めたが、実機での差は未検証。Rust 側は `unicode_normalization` で NFKC してから PBKDF2 に渡す（`crates/opfspack/src/keys.rs`。パスフレーズのラップ作成・復号の両方） |
| Drive のフォルダがアカウント別でない既知の制約 | `docs/spec/README.md` §4 の「Drive の保存先フォルダはアカウント別ではない」。bundle は `owner_id` で選別するので混在しても誤用しない設計だが、フォルダ分離は別件（§11.5 も参照） |
| Web 側の鍵の保持 | メモリのみか IndexedDB かは Web 側の判断（XSS リスクの tradeoff。本仕様はどちらも許容する） |

## 11. バックアップの暗号化（`thundoku-backup.json` v3 / R06）

> **状態: Rust 側は実装済み（2026-09-25）/ Web 版は未対応**。実装の正は
> `crates/opfspack/src/keys.rs`（鍵導出と封筒 `SealedEnvelope` + ラベル
> `BACKUP_LABEL` / `THUMBS_LABEL`）と
> `crates/core/src/db/backup.rs`（`DriveBackup` = v3/v2 の判別と復号）、同期への配線は
> `crates/core/src/drive/sync.rs`。同じ封筒を**表紙バンドル**（§11.8）でも使う。

### 11.1 何を守るか（線引き）

| | 内容 |
|---|---|
| 守る | Drive に上げる**メタデータバックアップ**（本棚・進捗・履歴・付箋メモ。`crates/core/src/db/backup.rs` の `TABLES` のテキスト列） |
| **守らない（DB 全体）** | **ローカルの SQLite（`thundoku-shelf.db`）全体**。SQLCipher 等は入れない（重い・既存の読み書き経路を総取り替えになる）。**ただし機密列（`document_text.text_content` / `token_analysis` の `token`・`pos`・`base_form`・`reading` / `page_notes.memo`）だけは keyring の DB 鍵で暗号化する**（2026-09-26 / セキュリティ評価 F02。`crates/core/src/db/column_crypto.rs`、保存形式は `docs/spec/02-data-model.md` §1.11）。書誌・進捗・履歴などの他の列は平文のまま。**ディスク暗号化（BitLocker / FileVault 等）と OS アカウント分離**を前提にする（`README.md`） |
| 守らない | 端末の OS アカウントを奪われて keyring を読まれる場合（PRK そのものが取られる。§1.2 と同じ） |
| 対象外 | 画像（`thumbnail_data` / `image_data`）とページ本文（`extracted_text`）は**そもそもバックアップに含めない**（`docs/spec/06-sync-auth-drive.md` §3.4） |

鍵は §2 の PRK から導出する。**新しい鍵管理を増やさない**（keyring と `thundoku-keys.json` の
既存の仕組みをそのまま使う）。

### 11.2 鍵導出

```text
backup_cipher_key = HKDF-SHA256(ikm = PRK, salt = b"thundoku-backup:v1", info = b"thundoku-backup-key",  32B)
backup_hash_key   = HKDF-SHA256(ikm = PRK, salt = b"thundoku-backup:v1", info = b"thundoku-backup-hash", 32B)
```

| 定数 | 値 |
|---|---|
| salt | `b"thundoku-backup:v1"` |
| info（暗号鍵） | `b"thundoku-backup-key"` |
| info（HMAC 鍵） | `b"thundoku-backup-hash"` |
| 封筒の AAD | `UTF8("thundoku-backup:3:" + owner_id)`（`owner_id` = `derive_owner_id(sub)`） |

- 実装: `PackRootKey::derive_backup_cipher_key()` / `derive_backup_hash_key()`
  （`crates/opfspack/src/keys.rs`）。2 本に分けるのは用途分離のため（同じ PRK から導出するが、
  暗号鍵を HMAC に流用しない）。
- pack 鍵（`HKDF_INFO` = `opfspack-entry-key`）とは salt も info も違う。
- 封筒の `format_version`（3）を AAD に含めるので、**同じ平文でも版が上がれば別の暗号文**になる。

### 11.3 封筒の形式

```json
{
  "format_version": 3,
  "owner_id": "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0",
  "encryption": {
    "alg": "aes-256-gcm",
    "kdf": "hkdf-sha256",
    "nonce": "<base64 12B 乱数>",
    "ciphertext": "<base64 平文 + 16B タグ>"
  },
  "content_hmac": "<hex 64>"
}
```

| フィールド | 型 | 規則 |
|---|---|---|
| `format_version` | number | `3` 固定。**これ以外は読めない版として拒否する**（平文として扱わない） |
| `owner_id` | string | `derive_owner_id(sub)`（§2）。keyring のスロット選択と AAD に使う |
| `encryption.alg` | string | `"aes-256-gcm"` のみ |
| `encryption.kdf` | string | `"hkdf-sha256"` のみ（`nonce` は乱数なので salt は持たない） |
| `encryption.nonce` | string (base64) | AES-GCM の 12B IV。**毎回乱数**（固定しない） |
| `encryption.ciphertext` | string (base64) | `AES-256-GCM(backup_cipher_key, nonce, AAD)(平文)` = 平文長 + 16B（GCM タグ） |
| `content_hmac` | string (hex 64) | `HMAC-SHA256(backup_hash_key, UTF8(平文))` を小文字 hex |

- **平文は現行のバックアップ JSON そのもの**（`db::backup::export_json` の出力。
  いまの `format_version` は 2）。封筒は**外側だけ**で、中身の形式は変えない。
- base64 は標準アルファベット + padding（§3.2 と同じ。base64url は使わない）。
- **`content_hmac` の主目的は変更検知**。`nonce` が乱数なので**暗号文（＝ファイルの md5）は
  毎回変わる** — 暗号文の md5 を「変わったか」の判定に使うと毎回無駄なアップロードになる。
  平文に対する HMAC なら**同じ内容なら同じ値**になる。
- `content_hmac` は**復号時にも再計算して照合する**（平文と封筒の組が食い違う改変を検出する。
  合わなければ平文を返さない）。
- **`content_hmac` の入力は平文のバイト列そのもの**（正規化・再直列化をしない）。
  `export_json` はテーブル順・SELECT 順が決定的なので、DB の内容が同じなら同じ HMAC になる。
  揮発列（`books.updated_at` / `bookshelf_items.synced_at` 等）だけが動いた場合は HMAC も変わる
  ＝「アップロード要否」の判定は **v2（生の md5 比較）と同じ粒度**。正規化した比較（揮発列を落とし、
  行を PK 順に並べる）は**起動時の復元提案**（`local_differs`）が従来どおり行う（§11.5）。

### 11.4 読み出しと互換

- ファイルの判別は `db::backup::DriveBackup::parse`:
  - `encryption`（オブジェクト）があれば**封筒として扱う**。`format_version` が 3 でなければ
    `対応していないバックアップ形式です（v{n}）` として**拒否**する（暗号文を平文として取り込まない）
  - `encryption` が無ければ従来の平文バックアップ（v2 / 版なし）として読む＝**既存の控えを読めなくしない**
- 復号に使う鍵は keyring の `thundoku-shelf.pack-root-key:<owner_id>`（§3.1 と同じスロット）。
  スロットは**封筒の `owner_id` で選ぶ**。
- core には UI が無いのでパスフレーズを尋ねられない。解錠はアプリの `PackKeyStore` が行い、
  成功すれば keyring に入る（§4.1）。したがって**keyring に無い鍵では復号しない**。
  - 復元（`restore_drive_backup`）: `SyncError::BackupKeyRequired` で失敗し、**1 行も取り込まない**
  - 起動時の差分判定（`inspect_drive_backup`）: `drive_changed` だけ判定して `local_differs` は
    false＝**復元確認を出さない**（復元しても失敗するため）
- パスフレーズのみの構成（keyring に PRK を置かない）でも、`PackKeyStore::unlock` が成功すれば
  keyring に保存されるので、その後の起動では復号できる。

### 11.5 同期での扱い（`crates/core/src/drive/sync.rs`）

| 局面 | v3（PRK あり） | v2（PRK なし） |
|---|---|---|
| アップロード | 封筒（`content_hmac` を基準値に保存） | **平文のまま + 警告ログ**（鍵が無いだけで利用者の唯一の控えを失わないため。§11.1） |
| アップロード要否 | 基準値 `app_settings['drive.backup.md5']`（= 前回上げた `content_hmac`）と今回の封筒の `content_hmac` を比較。Drive 側にファイルが無ければ上げ直す | 従来どおり書き出したバイト列の md5 と Drive の `md5Checksum` を比較 |
| 復元提案 `drive_changed` | 基準値と Drive の封筒の `content_hmac` を比較 | 基準値と Drive の平文の正規形 md5 を比較（従来どおり） |
| 復元提案 `local_differs` | 復号した平文とローカルを**正規形**で比較（従来と同じ基準） | 同左 |
| 復元 | keyring の PRK で復号 → `import_json`。基準値に封筒の `content_hmac` を保存 | そのまま `import_json`。基準値に正規形 md5 を保存 |

- `app_settings['drive.backup.md5']` の**値の意味は形式で変わる**（v3 = `content_hmac` /
  v2 = 正規形 md5）。形式が入れ替わった直後（例: 相手が v3 で上げ、こちらがまだ v2 の基準値を
  持っている）は `drive_changed` が true になり得る。無駄な確認を防ぐのは `local_differs` の
  判定（内容の比較）で、**内容が同じなら復元確認は出ない**。
- 既知の挙動（v3 で変わる点）:
  - 他端末が上げた封筒でも**内容が同じなら自分の控えを上げ直さない**（相手のファイルを無駄に
    上書きしない）。内容が違う場合も、ローカルの内容が動いた時点で自分の控えを上げる。
  - **Drive のフォルダはアカウント別ではない**（§10）。別アカウントの封筒は、その `owner_id` の
    鍵がこの端末の keyring にある場合にだけ復号できる（v2 の平文は誰でも読めていたので、
    これは緩和であって新たな穴ではない）。
  - PRK が無い端末で同期すると、Drive 上の**暗号化された控えが平文で置き換わる**（同じ内容でも
    `content_hmac` の比較は成立しないため上の表の v2 経路に落ち、md5 が違うのでアップロードする）。
    設計判断として許容する（鍵が無いだけでバックアップを止めない）。鍵を用意できれば次の同期で
    封筒に戻る。

### 11.6 テストベクタ（実装間の一致確認）

**鍵導出**（PRK = `00 01 … 1f`。独立実装（WebCrypto）で検算済み）

| 入力 | 期待値 |
|---|---|
| `derive_backup_cipher_key()` | `514e03cce7c0aa82128b07af6ed7cbd1b2e7a30262f811d39201f21fd4faba8e` |
| `derive_backup_hash_key()` | `840d69fba8e9af9525b86e4c0dfa74403ecfcaa8743a6ed4f7576322c81df043` |

**封筒**（`PRK = 00 01 … 1f` / `owner_id = derive_owner_id("test-sub")` /
`nonce = 0b 0a 09 08 07 06 05 04 03 02 01 00` /
平文 = `{"format_version":3,"tables":{}}`（UTF-8））

| 項目 | 期待値 |
|---|---|
| `AAD` (UTF-8) | `thundoku-backup:3:6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0` |
| `nonce` (hex) | `0b0a09080706050403020100` |
| `ciphertext` (hex) | `a89bac086476371010c84a0f976bee1d8e83650f48c9c2c411b5bbcf54df7cdea7185fdbd89c979adeb6404b869a38b3` |
| `content_hmac` (hex) | `1d43363600f7a97ed4357e385917178206398a86b2bb0b0512c67d412007488b` |

- 平文は封筒ベクタのための**例**（実際の平文は `export_json` の出力で `format_version` は 2）。
- Rust 側はこの表を `crates/opfspack/tests/backup_envelope.rs` で固定し、`content_hmac` が
  同じ平文で不変・暗号文は毎回変わることも同じファイルで固定している。値を書き換えるときは
  仕様書と実装を同時に直す。

### 11.7 Web 実装チェックリスト（`thundoku-shelf` モノレポ）

- [ ] `deriveBackupKey(prk)`: HKDF で 2 本（`info` は `thundoku-backup-key` / `thundoku-backup-hash`、
  `salt` は `thundoku-backup:v1`）。§11.2 の式と一致させる。
- [ ] 封筒の `seal` / `open`: `crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce,
  additionalData: UTF8("thundoku-backup:3:" + ownerId) }, ...)` と `HMAC`（`crypto.subtle.sign`）。
  `content_hmac` は**平文のバイト列そのもの**に対して計算する（JSON を再直列化しない。
  キー順や空白が違うと値が変わり、無駄なアップロードになる）。
- [ ] 読むときは `encryption` の有無で v3 / v2 を判別する。未知の `format_version` は**エラー**
  （平文として扱わない）。
- [ ] アップロード要否は `content_hmac` の比較。暗号文の md5 / ファイルサイズで判定しない。
- [ ] PRK が無いときのフォールバック（平文で書く + 警告）と、その結果 Drive 上に平文の
   バックアップが載り得ることを UI / ログで利用者に伝える。

### 11.8 表紙バンドル（`thundoku-thumbs.json` / 2026-09-28）

> **状態: デスクトップ側は実装済み / Web 版は未対応**。実装の正は
> `crates/opfspack/src/keys.rs`（ラベル `THUMBS_LABEL`）と `crates/core/src/thumbs.rs`
> （平文の組み立て・エンコード）、配線は `crates/core/src/drive/sync.rs`
> （`backup_thumbnails`）と `crates/core/src/db/thumbs.rs`（派生キャッシュ）。
> コメント付きのサンプル（データはダミー）は `docs/thundoku-thumbs.jsonc`。

#### 何のために

Web 版の本棚が**未ダウンロードの本の表紙**を出せるようにする。`bookshelf_items.thumbnail_url`
（外部 URL）はバックアップ JSON に元から入っているが、URL をそのまま読ませる方法は
サイトごとの規則（FANZA の原寸置換・TBF の `/api/image/` など）やホットリンク・参照元の
消滅に弱い。**デスクトップが取得済みの画像そのもの**を 1 ファイルにまとめて共有フォルダへ置く。

#### 形式

| 項目 | 値 |
|---|---|
| ファイル名 | `thundoku-thumbs.json`（`thundoku-backup.json` と同じフォルダ） |
| 封筒 | §11.3 と同じ（`aes-256-gcm` / `hkdf-sha256`）。**ラベルだけ別**（AAD は `thundoku-thumbs:1:<owner_id>`、`format_version` は 1） |
| 鍵 | `thumbs_cipher_key` / `thumbs_hash_key`（§11.2 の式の `salt` / `info` を `thundoku-thumbs:*` に差し替え） |
| 平文 | `{"entries":[…],"format_version":1}` |
| 対象 | 本棚（`bookshelf_items`）の表紙と、チェックリスト（`checked_items.thumbnail_data`）のサムネイル |
| 平文で上げるフォールバック | **無い**。PRK が無い端末では上げない（表紙は蔵書そのものを晒し、かつ再取得できる派生データなので、DB バックアップとは判断が違う） |

平文の 1 件（kind ごとに識別子の形が違う。Web 側に文字列を分割させない）:

| kind | フィールド |
|---|---|
| `shelf` | `site_id`, `database_id`, `mime`, `width`, `height`, `sha256`, `data`(base64) |
| `checklist` | `item_id`, `mime`, `width`, `height`, `sha256`, `data`(base64) |

- `entries` は `(kind, item_key)` 順に固定。**平文に時刻や mtime を入れない**（入れると
  `content_hmac` が毎回変わり、毎回アップロードになる）。キー順は `serde_json` の既定（辞書順）。

#### 画像

| 対象 | 変換 | 根拠 |
|---|---|---|
| 本棚 | 448px キャッシュ PNG → **256px WebP lossy q80** | 実測（実データ 618 枚）: 合計 9.5MB / 平均 15.5KB / p95 24KB / 最大 41KB。448px PNG のままなら 169MB、448px WebP でも 21.8MB |
| チェックリスト | `checked_items.thumbnail_data` の 256px JPEG を**そのまま** | 保存時点で目的の寸法・形式（`crates/app/src/views/checklist.rs` の `thumbnail_jpeg_base64`） |

- 1 枚が 64 KiB を超えたら q60 で作り直し、256 KiB を超える 1 枚は載せない（病的な画像対策）。
- 平文が 48 MiB を超えたら警告ログ（**分割は未実装**。上げるのは上げる）。
- 本棚の取得元は `<data_dir>/thumbnails/{site_id}_{database_id}_448.png` **だけ**。
  pack 内の `thumbnail.webp` へのフォールバックは未実装（本棚を開けば `fetch_remote_covers` が
  全カード分を取得するので、通常は埋まる）。

#### 変更検知と同期

- 基準値は `app_settings['drive.thumbs.hash']`（封筒の `content_hmac`）。`drive.backup.md5` と同じ流儀。
- 1 回の同期で新しくエンコードするのは 100 枚まで（起動時・終了時にも同期が走るため）。
  残りは次の同期で載る。`thumbnail_share` に入った分は再エンコードしない。
- **表紙を 1 枚も作れないときは既存のバンドルを残す**（表紙キャッシュの無い端末が Web 側の
  表紙を全部消すのを防ぐ。削除は伝播させない、という pack と同じ方針）。
- 失敗しても同期全体は失敗させない。`drive.thumbs.failed` を立てて設定画面に警告を出し、
  成功したら消す。中止（利用者操作）だけはそのまま伝播する。
- `drive.sync.books`（pack の送受信）を OFF にしても止めない（失うものが別。§11.5 と同じ理屈）。
- 同期の前に `SyncPhase::Upload` で中止を判定する（DB バックアップと同じく書き出す前が区切り）。

#### ローカルの派生キャッシュ

`thumbnail_share(kind, item_key, mime, width, height, sha256, bytes, source_mtime, source_size, updated_at)`
（PK = `(kind, item_key)`）。`item_key` は本棚 `{site_id}:{database_id}` / チェックリスト
`checked_items.id`。

- **DB バックアップの対象外**（`db::backup::TABLES` の許可リストに入れない）。復元先で作り直せる。
- 書き込み点は 3 つ: 表紙を取得した直後（`write_cover_cache` → `core::thumbs::cache_shelf_cover`）、
  チェックリストのサムネイル保存直後（`cache_checklist_thumbnail`）、同期時の埋め戻し
  （足りない分だけを上限まで）。
- 所有者フィルタは親テーブル（`bookshelf_items` / `checked_items`）の `owner_sub` を復号して
  求めたキー集合で行う（§11.5 と同じ。他アカウントの表紙を混ぜない）。親が消えたキーの行は
  上げない（同期のたびに書き直すので Drive 側に孤児は残らない）。

#### テストベクタ

**鍵導出**（PRK = `00 01 … 1f`。HKDF-SHA256 と AES-256-GCM を Python（hashlib / pycryptodome）で
独立実装して検算済み。同じ手順で §11.6 の値も再現できることを確認している）

| 入力 | 期待値 |
|---|---|
| `derive_thumbs_cipher_key()` | `c6bcc317a110145e9c2ab08f079195fd43790f37a54a5217a8ac58e050cea8e1` |
| `derive_thumbs_hash_key()` | `43ea28b23b2dab950943cf6a97ee9266560b2ca1781eaee08fa28e9f8b168d25` |

**封筒**（`PRK = 00 01 … 1f` / `owner_id = derive_owner_id("test-sub")` /
`nonce = 0b 0a 09 08 07 06 05 04 03 02 01 00` / 平文 = `{"format_version":1,"entries":[]}`）

| 項目 | 期待値 |
|---|---|
| `AAD` (UTF-8) | `thundoku-thumbs:1:6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0` |
| `nonce` (hex) | `0b0a09080706050403020100` |
| `ciphertext` (hex) | `61697d8103eb4a2fc6440c6230179c9ca86081b45f26bdb39a34cfd01d7bef36609fc48cd14c8cf74c6c5e9b65b1c4c091` |
| `content_hmac` (hex) | `46fba895ee29f2dcdddd05c9e7ee8e0f690d8722f6b337744ecc8d87f4e6e7e6` |

固定は `crates/opfspack/tests/thumbs_envelope.rs`。`format_version` が違うので
**表紙の封筒をバックアップとして読むことはできない**（逆も同じ。AAD のラベルと鍵も別）。
既存のバックアップ側のベクタ（§11.6）はバイト単位で不変であることが回帰条件
（`crates/opfspack/tests/backup_envelope.rs`）。

#### Drive 上のファイル属性（探すための情報）

Web は「同期フォルダの一覧 → 名前一致」でファイルを見つける。一覧から得られる属性は次の通り
（アップロードの実装は `crates/core/src/drive/mod.rs` の `multipart_wrapper` と
`crates/core/src/drive/sync.rs` の `backup_thumbnails`）:

| 項目 | 値 |
|---|---|
| 名前 | `thundoku-thumbs.json`（完全一致。`sync.rs` の `THUMBS_NAME`） |
| 親 | 同期フォルダ（My Drive 直下 `thundoku-shelf/`。`SYNC_FOLDER_NAME`） |
| `mimeType` | `application/octet-stream`（**`application/json` ではない**ので、mime で絞り込まない） |
| `appProperties.app` | `thundoku-shelf` |
| `appProperties.packId` | `thundoku-thumbs.json`（`.opfspack` で終わらないので名前がそのまま入る） |
| サイズ | 平文 + 16B タグを base64 した長さ + 封筒のフィールド。実データ 612 枚で平文 17.3 MB（`entries` 次第で増減。約 1.33 倍が base64） |

- 一覧クエリの例（`folderId` は同期フォルダの id。`docs/spec/06-sync-auth-drive.md` の `list_files` と同じ形）:
  `q = '<folderId>' in parents and trashed=false and name='thundoku-thumbs.json'`、
  `fields = nextPageToken,files(id,name,size,md5Checksum,modifiedTime)`、`pageSize=100`・`pageToken` で全ページ。
  `appProperties` を絞り込みに使うなら `and appProperties has { key='packId' and value='thundoku-thumbs.json' }`。
- **同名が 2 つ以上あり得る**。デスクトップは「新しい方を上げてから旧い方を消す」ので、削除に失敗すると
  そのまま残る（`sync.rs` は警告だけで続行）。デスクトップ自身は一覧の最初の一致を使うが、
  **Web は `modifiedTime` が新しい方を採る**こと（古い方を読むと 1 世代前の表紙が出る）。
- `drive.file` スコープでは「自分が作成した / Picker で許可された」ファイルしか列挙できない。
  この制約の扱いは「前提（Web 側で確認が要る）」を参照。

#### 暗号処理（WebCrypto の具体形）

`thundoku-backup.json` と**同じ PRK**を使い、**ラベルだけ別**（この節の「形式」表）。
Rust の実装（`hkdf` crate / `aes-gcm` crate）とバイト一致させる:

| 段階 | Rust の式 | WebCrypto |
|---|---|---|
| 暗号鍵 | `HKDF-SHA256(ikm = PRK, salt = b"thundoku-thumbs:v1", info = b"thundoku-thumbs-key")` 32B | `importKey("raw", prk, "HKDF", false, ["deriveBits"])` → `deriveBits({ name: "HKDF", hash: "SHA-256", salt: UTF8("thundoku-thumbs:v1"), info: UTF8("thundoku-thumbs-key") }, key, 256)` |
| HMAC 鍵 | 同式で `info = b"thundoku-thumbs-hash"` | 同上（`info` だけ差し替え） |
| 復号 | `AES-256-GCM(key = cipherKey, nonce, AAD)(ciphertext)` | `importKey("raw", cipherKey, "AES-GCM", false, ["decrypt"])` → `decrypt({ name: "AES-GCM", iv: nonceBytes, additionalData: UTF8("thundoku-thumbs:1:" + ownerId), tagLength: 128 }, key, ciphertextBytes)` |
| 平文の改変検出 | `HMAC-SHA256(hashKey, 平文)` を `content_hmac`（小文字 hex）と比較 | `importKey("raw", hashKey, { name: "HMAC", hash: "SHA-256" }, false, ["sign"])` → `sign("HMAC", key, plaintextBytes)` → 32B を hex 化して比較 |

- **`ciphertext` は「平文 + 16B の GCM タグ」が連結された形**（WebCrypto の `decrypt` はこの形を
  前提にするので、タグを別扱いしない）。
- **AAD は文字列の UTF-8 バイト列**。`owner_id` は封筒の `owner_id`（＝自分と一致することを確認した値）を使う。
- **HKDF の `salt` は必須**（backup と違って thumbs は salt を持つ）。WebCrypto の `"HKDF"` は
  extract + expand で、Rust の `Hkdf::new(Some(salt), ikm)` と同じ結果になる。
- `nonce` は 12B、`iv` にそのバイト列をそのまま渡す。base64 は**標準アルファベット + padding**
  （base64url ではない）。`Uint8Array.from(atob(s), (c) => c.charCodeAt(0))` で足りる。
- `content_hmac` は**必ず自分で計算して照合**する（封筒の値を信じない）。照合は定数時間比較が望ましい。
- 鍵の導出結果にテストベクタがあるので、**移植直後に TS のテストで突き合わせる**（後述の「受け入れ確認」）。

#### PRK の解決（Web）

表紙バンドルの復号に使う PRK は **DB バックアップと同一の経路**で解く（§3.2 / §4.2 / §5.2.1）。
Web は OS keyring を持たないので、`thundoku-keys.json`（同じフォルダ）から毎回解く:

1. `thundoku-keys.json` を一覧から名前で探して取得（無ければ表紙は出せない → 退化経路）。
2. `owner_id` が自分と一致する bundle だけを使う（不一致は `OwnerMismatch` 相当として無視）。
3. `kind = "sub"` のラップ: `KEK_sub = PBKDF2-SHA256(password = UTF8(sub) ‖ APP_SALT, salt = APP_SALT, iterations = 100_000, 32B)`。
   `sub` は Google の ID トークンの `sub` claim（`googleProfile.sub`）。
   `APP_SALT = b"opfspack-v1-identity-salt-2024"`。**`sub` を UTF8 にしてから `APP_SALT` を後置連結**する
   （区切り文字は入れない）。
4. `AES-256-GCM(KEK)(wraps[].ciphertext)` を AAD `UTF8("thundoku-pack-root:1:" + owner_id)` で復号すると PRK 32B（`ciphertext` は 48B = 32B + 16B タグ）。
5. パスフレーズラップがある場合は**先にパスフレーズを試す**（§4.2）。`NFKC` 正規化してから UTF-8 化する。
6. **パスフレーズ必須モード**（`kind = "sub"` のラップが無い bundle）では `sub` からは戻せない。
   パスフレーズを尋ね、「スキップ」では**復号しない**（`sub` へ落ちる経路が無い。§5.2.1）。
7. PRK が取れない（未ログイン・bundle が無い・利用者がスキップ）なら、表紙は出さず
   `thumbnail_url` → プレースホルダへ退化する。**メタデータだけは出せる**ので本棚は壊さない。

#### 取得・キャッシュ・再取得の判定

**`modifiedTime` は「変わった」の判定に使えない**（内容が同じでも進む）。`sync.rs` の
`backup_thumbnails` は、`content_hmac` が前回と同じときも `drive.touch` で
`modifiedTime` だけ現在時刻に更新する（DB バックアップと同じ流儀）。したがって:

| 変化 | `modifiedTime` | ファイルのバイト列 | `content_hmac` |
|---|---|---|---|
| 内容が変わった（上げ直し） | 進む | 変わる（`nonce` が乱数なので毎回別物） | 変わる |
| 内容が同じ（`touch` のみ） | **進む** | 変わらない | 変わらない |
| デスクトップが同期していない | 変わらない | 変わらない | 変わらない |

- **`md5Checksum` / サイズで「内容が変わった」を判定しない**（どちらも `nonce` と再エンコードで動く）。
  ただし**サイズは平文長の関数**なので、「前回と同じサイズ」は同一内容の強いヒントになる（判定の正は `content_hmac`）。
- 推奨の手順（無駄なダウンロードを抑えつつ正しさを優先する）:
  1. 一覧で `name` 一致（複数あれば `modifiedTime` が新しい方）を探す。
  2. 前回保存した `modifiedTime` と同じなら**何もしない**（ただし `touch` で進むので、これは
     「変化が無かった」の十分条件でしかない）。
  3. 進んでいたらダウンロードして復号し、平文から `content_hmac` を計算する。
  4. 前回保存した `content_hmac` と同じなら、**画像のデコード結果をそのまま使い回す**
     （17 MB を落とした無駄は戻らないが、デコードと再描画はしない）。
- **キャッシュの格納先は OPFS / IndexedDB**（`localStorage` は 5 MB 級で入らない。平文 17 MB +
  base64 のデコード結果を考えると IndexedDB / OPFS が前提）。
  - 粗い単位: `content_hmac` をキーに**平文 JSON をそのまま**保存する（再ダウンロードしても復号を省ける）。
  - 細かい単位: `{kind}:{item_key}:{sha256}` をキーに**デコード済み画像**（Blob / ImageBitmap）を保存する。
    こちらは世代をまたいで再利用できる（`sha256` が同じ = 同じ画像）。
  - **`content_hmac` が変わると平文全体が作り直される**ので、古い世代のキャッシュは
    適当なタイミングで捨てる（直近 1〜2 世代を残す程度で十分）。
- ダウンロードは 17 MB 級。**モバイル回線では重い**ので、
  「初回は表紙を遅延してでもメタデータを先に描く」「再取得はアプリ起動時 / 明示操作時だけ」のように
  頻度を絞る（デスクトップの同期は起動時と終了時にも走るため、頻繁に叩くと無駄が出る）。

#### 表示の実装（性能と寿命）

- **全部を一度にデコードしない**。612 枚 / 平文 17.3 MB を一括で `Blob` 化するとモバイルで落ちる。
  本棚のカードは `IntersectionObserver` で**可視になった分だけ**デコードする。
- `data`（base64）→ `Uint8Array` → `Blob` → `URL.createObjectURL`。**base64 のデコードは
  メインスレッドを止めうる**ので、大きい画像は Worker に逃がすか、可視カードぶんに限る。
- **オブジェクト URL の寿命**: 1 枚につき 1 本。カードが画面外へ出たら `revokeObjectURL` する。
  スクロールで出し入れするなら上限つき LRU（可視枚数 + 数枚。デスクトップ側のページ一覧は
  120 枚上限＝`docs/spec/04-ui.md` の `MAX_PAGE_THUMBS`）を目安にする。
- `createImageBitmap` を使う場合は、不要になったら `bitmap.close()` を呼ぶ。
- `width` / `height` を使って**画像の到着前に枠を確保**する（`aspect-ratio` / `padding-top`）。
  これが無いと画像の到着ごとにカードがガタつく（表紙の縦横比は本ごとに違う）。
- デコードに失敗した entry（壊れた画像・未知の `mime`）は、その 1 枚だけプレースホルダに落とす。

#### エラー時の分岐（Web がどの状態で何を出すか）

| 状況 | 判定 | Web の動作 |
|---|---|---|
| 同期フォルダにファイルが無い | 一覧で見つからない | 全カードを `thumbnail_url` → プレースホルダ。エラー扱いにしない（正常系） |
| 一覧は取れるがファイルが読めない（権限・オフライン） | HTTP エラー | キャッシュがあればそれを使う。無ければ退化経路 |
| 封筒に `encryption` が無い | 形式違い | **平文として扱わない**（thumbs は必ず封筒）。退化経路 |
| `format_version` ≠ 1 | 読めない版 | エラー。平文として扱わない（§11.4 と同じ方針） |
| `owner_id` が自分と違う | 別アカウントの封筒 | 使わない（自分の表紙ではない）。退化経路 |
| 復号失敗・`content_hmac` 不一致 | 鍵違い / 改変 | 使わない。キャッシュも捨てる。退化経路 |
| `entries` に未知の `kind` | 前方互換 | **その 1 件だけ捨てる**（他の entry は使う） |
| `entries` が空 | 正常系 | 全カードを退化経路（表紙キャッシュがまだ無い端末が上げた結果） |
| ある本の entry が無い | 正常系（部分集合） | その本だけ `thumbnail_url` → プレースホルダ |
| PRK が取れない | 未ログイン / bundle 無し / スキップ | 表紙は出さない。メタデータだけ出す（退化経路） |
| `data` の base64 が壊れている | 破損 | その 1 枚だけプレースホルダ |

「退化経路」= `bookshelf_items.thumbnail_url` を `<img>` で直読み → 失敗ならプレースホルダ。
**表紙が出ないだけで本棚は壊さない**のが原則。

#### Web 実装チェックリスト（§11.7 に追加）

- [ ] `deriveThumbsKey(prk)`: HKDF で 2 本（`info` = `thundoku-thumbs-key` /
  `thundoku-thumbs-hash`、`salt` = `thundoku-thumbs:v1`）。§11.2 の式と一致させる。
  **移植直後にテストベクタ（後述）と突き合わせる**。
- [ ] 封筒の AAD は `UTF8("thundoku-thumbs:1:" + ownerId)`、`format_version` は 1。
- [ ] `ciphertext` を「平文 + 16B タグ」として `AES-GCM`（`tagLength: 128`）で復号し、
  平文の `HMAC-SHA256` を計算して `content_hmac` と照合する（照合しない実装にしない）。
- [ ] `entries` を `kind` で分岐（`shelf` = `site_id` + `database_id` / `checklist` = `item_id`）。
  未知の `kind` は 1 件だけ捨て、未知のキーは無視する（前方互換）。
- [ ] `data` は base64 をデコードして `Blob` → `URL.createObjectURL` で表示する（CORS 不要）。
  **可視カードだけ**デコードし、不要になった URL は `revokeObjectURL` する。
- [ ] 保存は `content_hmac` をキーに OPFS / IndexedDB へ。再取得の判定は
  「`modifiedTime` が同じなら何もしない」→「変わっていたら落として `content_hmac` を比較」の順。
  **`md5Checksum` / サイズを内容の判定に使わない**（`touch` と乱数 `nonce` で動く）。
- [ ] 同名ファイルが複数あるときは `modifiedTime` が新しい方を採る。
- [ ] ファイルが無い・PRK が無いときは `thumbnail_url` の直読み → それも失敗ならプレースホルダへ
  退化する（表紙が出ないだけで本棚は壊さない）。
- [ ] 同期フォルダへの到達（`drive.file` スコープ）を実機で確認する（「前提」の項目）。

#### 平文の読み方（Web が守ること）

- **書き込まない**。`thundoku-thumbs.json` の所有者はデスクトップ（毎回全体を書き直す）。
  Web は読み取り専用で扱う（書き換えると次回の同期で戻るか、`content_hmac` の比較が壊れる）。
- **`entries` は部分集合**。表紙キャッシュがまだ無い本・アカウントに紐づかない本は入らない。
  1 冊も入っていない（`entries` が空・ファイルが無い）状態も正常系として扱う。
- **未知のキーは無視**する（前方互換）。未知の `kind` の entry はその 1 件だけ捨てる。
  `format_version` が 1 以外なら**エラー**（平文として扱わない。§11.4 と同じ方針）。
- **`data` の base64** は STANDARD アルファベット + padding（`Buffer.toString("base64")` /
  `btoa` 互換。base64url ではない）。
- `width` / `height` は**エンコード後の**寸法（縦横比の確保に使える。本棚の行の高さを
  先に決めると画像の到着でガタつかない）。
- `sha256` は `data` をデコードしたバイト列の SHA-256（小文字 hex）。省略可能な最適化に
  だけ使い、**値の検証は必須ではない**（`content_hmac` が封筒全体を守っている）。
- どの entry がどの本かは、バックアップ JSON の表と**この 2 つのキーで結合**する:

| kind | entry のキー | 結合先 |
|---|---|---|
| `shelf` | `site_id` + `database_id` | `bookshelf_items` の PK（`site_id`, `database_id`） |
| `checklist` | `item_id` | `checked_items.id` |

形の例（`data` は実際には base64 の画像。ここでは省略）:

```json
{"entries":[{"kind":"shelf","site_id":"booth","database_id":"1141786","mime":"image/webp",
 "width":256,"height":364,"sha256":"735cbaa2…","data":"<base64>"}],"format_version":1}
```

実測の目安: 実データ 612 枚で **17.3 MB**（1 枚 ≒ 28 KB、base64 と JSON を含む）。

#### Web の実装手順

1. バックアップ JSON（`thundoku-backup.json`）を読める状態にする（§11.7）。PRK の解決も同じ手順
   （`thundoku-keys.json` → `owner_id` 照合 → `sub` ラップ / パスフレーズ。上の「PRK の解決（Web）」）。
2. 同期フォルダの一覧から `thundoku-thumbs.json` を探す（**無ければ**手順 8 の退化経路へ）。同名が複数あれば
   `modifiedTime` が新しい方を採る。
3. 前回保存した `modifiedTime` と同じなら、**ダウンロードも復号もしない**（キャッシュを使う）。
4. 変わっていたらダウンロードし、封筒として `open(prk, ownerId)` する。AAD・鍵はこの節の値
   （`thundoku-thumbs:1` / `thundoku-thumbs:v1` / `-key` / `-hash`）。`format_version` は 1。
   平文の `HMAC-SHA256` を計算して `content_hmac` と照合する（照合しない実装にしない）。
5. 平文の `entries` を `kind` ごとの Map（`shelf` = `site_id:database_id` /
   `checklist` = `item_id`）にする。未知のキーは無視、未知の `kind` は 1 件だけ捨てる。
6. `content_hmac` をキーにして OPFS / IndexedDB へ保存する。前回と同じ `content_hmac` なら
   **デコード結果を使い回す**（落としたバイト列は無駄になるが、デコードと再描画はしない）。
7. 表示は `data` を base64 デコード → `Blob` → `URL.createObjectURL`（CORS 不要）。
   **可視カードだけ**デコードし、不要になった URL は `revokeObjectURL` する。
8. entry が無い本は `thumbnail_url` を `<img>` で直読み → それも失敗したら
   プレースホルダへ退化する（**表紙が出ないだけで本棚は壊さない**）。

#### 受け入れ確認（TS で検算する）

実装の前に、**`crates/opfspack/tests/thumbs_envelope.rs` と同じベクタを TS のテストとして書く**
（Rust ⇔ TS のバイト一致が唯一の合格条件。値を書き換えるときは仕様書・Rust テスト・TS テストを同時に直す）。

| 入力 | 期待値 |
|---|---|
| `deriveThumbsCipherKey(PRK)`（PRK = `00 01 … 1f`） | `c6bcc317a110145e9c2ab08f079195fd43790f37a54a5217a8ac58e050cea8e1` |
| `deriveThumbsHashKey(PRK)` | `43ea28b23b2dab950943cf6a97ee9266560b2ca1781eaee08fa28e9f8b168d25` |
| `ownerId("test-sub")` | `6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0` |
| 平文 `{"format_version":1,"entries":[]}` / `nonce = 0b0a09080706050403020100` のときの `ciphertext` | `61697d8103eb4a2fc6440c6230179c9ca86081b45f26bdb39a34cfd01d7bef36609fc48cd14c8cf74c6c5e9b65b1c4c091` |
| 同・`content_hmac` | `46fba895ee29f2dcdddd05c9e7ee8e0f690d8722f6b337744ecc8d87f4e6e7e6` |
| 同・AAD (UTF-8) | `thundoku-thumbs:1:6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0` |

確認すること:

- [ ] 鍵 2 本が hex 一致（HKDF の `salt` / `info` の取り違えをここで潰す）。
- [ ] 固定 `nonce` で封をして `ciphertext` / `content_hmac` が hex 一致
      （＝復号が通るだけでなく、**暗号文のバイト列まで同じ**）。
- [ ] 同じ平文で `nonce` を変えると `ciphertext` は変わり、`content_hmac` は変わらない。
- [ ] 表紙の封筒をバックアップとして開けない（`format_version` と AAD で弾かれる）。
- [ ] `owner_id` を変えると復号できない。
- [ ] 平文を 1 バイト改変すると `content_hmac` の照合で落ちる。

平文の形と実装者向けの注記は `docs/thundoku-thumbs.jsonc`（コメント付きサンプル。データはダミー）に
まとめてある。実ファイルはこの `.jsonc` ではなく、Drive 上のコメント無し JSON である。

#### 前提（Web 側で確認が要る）

- 同期フォルダ（My Drive 直下の `thundoku-shelf/`）へ Web 側の `drive.file` で到達できること。
  別クライアント（別 OAuth クライアント ID）から見るには、**同じ Google Cloud プロジェクト**で
  クライアントを作る（Picker の `setAppId` にプロジェクト番号を渡す）か、Picker でフォルダを
  開いて利用者に許可させる必要がある。ここが満たせないと表紙バンドルは読めない
  （＝本棚のメタデータだけは出せる）。
- 復号に PRK が要る点は DB バックアップと同じ（`sub` ラップ、必要ならパスフレーズ。
  `docs/spec/10-pack-keys.md` §4.2 / §11.4）。PRK が取れないときは手順 8 の退化経路に落ちる。
- **Web はこのファイルを書かない**（所有者はデスクトップ。書き換えても次の同期で戻り、
  `content_hmac` の比較も壊れる）。
