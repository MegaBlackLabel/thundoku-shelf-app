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
「pack ファイル 1 つが漏れた」場合の防御にしかならない。Drive ごと漏れる想定では
**パスフレーズラップの有無が強度を決める**。

| 漏れたもの | `sub` ラップのみ | パスフレーズラップあり |
|---|---|---|
| pack ファイル 1 つ | 解けない | 解けない |
| pack + `thundoku-keys.json` | **解ける**（`sub` を知っていれば） | 解けない（パスフレーズを知らなければ） |
| 上記 + `sub` | 解ける | 解けない |
| 端末の keyring | 解ける（PRK を直接取られる） | 解ける |

- `thundoku-keys.json` に `sub` は**入らない**（`owner_id` は `sub` のハッシュで一方向）。
  したがって「bundle だけ漏れた」場合は `sub` を別途入手しないと解けない。
- v2 との比較: v2 は「pack ファイル + `sub`」で解けた。v3 は**bundle が無ければ解けない**。
  これが移行の実利（＋後述のローテーション可能性）。

### 1.3 鍵材料を `sub` から離す実利（強度以外）

- **ローテーションできる**: パスフレーズの変更・`sub` ラップの削除は**ラップの作り直しだけ**で済む
  （v2 は全 pack の再暗号化が必要だった＝事実上不可能）。
- **失効できる**: 端末を手放す／共有端末で読んだ、という状況でパスフレーズを変えれば、
  過去に配った `sub` ラップ（+ 旧パスフレーズ）では解けなくなる。

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
| `wraps[].iterations` | number | `kdf` の反復回数。**復号側はこの値を使う**（実装定数を使わない） |
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
| `sub` 依存をやめる | `kind=sub` のラップを削除してアップロードする。以後はパスフレーズが唯一の経路（**実装状況**: `PackKeyBundle::remove_wrap` までは実装済みだが、core の `PackKeyStore` に公開操作はまだ無い＝現状は `sub` ラップが残る） |
| 別端末を切り離す | パスフレーズを変更する（その端末が保存している PRK では新しい pack を読めない） |

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

- 「この端末に鍵を保存しない（毎回パスフレーズを尋ねる）」設定（共有端末向け）。
- `sub` ラップを作らない運用を既定にするかどうかの設定化（既定は作る＝Google ログインだけで読める）。
- パスフレーズの代わりになる明示的な回復コード（現状は「Drive の bundle + パスフレーズ」が回復手段）。
- 鍵のローテーション（PRK そのものの入れ替え＝全 pack の再暗号化）。

## 10. 不明点

| 項目 | 現状 |
|---|---|
| パスフレーズの PBKDF2 反復回数 | `600_000` で確定（release 実測 ≒ 60ms。ローカルの解錠では体感できない。将来上げる場合はラップの `iterations` を上げて作り直すだけ＝ PRK も pack も変えない） |
| NFKC 正規化の必要性 | IME / OS による差（合成文字）を避けるために仕様に含めたが、実機での差は未検証。Rust 側は `unicode_normalization` で NFKC してから PBKDF2 に渡す（`crates/opfspack/src/keys.rs`。パスフレーズのラップ作成・復号の両方） |
| Drive のフォルダがアカウント別でない既知の制約 | `docs/spec/README.md` §4 の「Drive の保存先フォルダはアカウント別ではない」。bundle は `owner_id` で選別するので混在しても誤用しない設計だが、フォルダ分離は別件（§11.5 も参照） |
| Web 側の鍵の保持 | メモリのみか IndexedDB かは Web 側の判断（XSS リスクの tradeoff。本仕様はどちらも許容する） |

## 11. バックアップの暗号化（`thundoku-backup.json` v3 / R06）

> **状態: Rust 側は実装済み（2026-09-25）/ Web 版は未対応**。実装の正は
> `crates/opfspack/src/keys.rs`（鍵導出と封筒 `BackupEnvelope`）と
> `crates/core/src/db/backup.rs`（`DriveBackup` = v3/v2 の判別と復号）、同期への配線は
> `crates/core/src/drive/sync.rs`。

### 11.1 何を守るか（線引き）

| | 内容 |
|---|---|
| 守る | Drive に上げる**メタデータバックアップ**（本棚・進捗・履歴・付箋メモ。`crates/core/src/db/backup.rs` の `TABLES` のテキスト列） |
| **守らない** | **ローカルの SQLite（`thundoku-shelf.db`）**。SQLCipher 等は入れない（重い・既存の読み書き経路を総取り替えになる）。**ディスク暗号化（BitLocker / FileVault 等）と OS アカウント分離**を前提にする（`README.md`） |
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
