# Google アカウント切替の設計

> **現行の採用仕様のみ**を載せている。旧仕様（1 アカウント = 1 プロファイル / 2 択の
> 初期化・ログアウト、および不採用になった Plan B 案）は 2026-09-14 にこのファイルから
> 削除した（経緯が必要なら git の履歴を参照）。実装は「# 改訂版（採用）」と
> 「# 実装上の確定事項」の 2 節が正。

# 改訂版（採用）: データ保持＆属性付け・ログイン切替モデル

> 上記の旧仕様（1 アカウント = 1 プロファイル / 2 択の初期化・ログアウト、および Plan B 案）は
> **不採用**。本節が現行の採用仕様。

## 基本原則

- ローカルの各本（pack）は**所有者**を持つ：ある Google `sub`、または**未所属（NULL）**。
- **複数アカウントのデータは共存**でき、それぞれ**属性付け**される。
  - ※ これは**内部実装**（DB の属性付け）。**ユーザには表立って出さない**。
    アカウント切替は「ログインし直すだけでそのアカウントのデータが見える」だけ。
    専用のアカウント選択・切替 UI は作らない。
- **未所属 pack はそのまま保持**（再暗号化・再DLしない）。未暗号化のまま。**表示・閲覧は
  ログアウト中のみ**（ログイン中は非表示）。
- Google アカウントの切替 = **表示・復号可能データの切替**。データは**削除しない**。
- **ログイン中は現在ログイン中のアカウントのデータのみ**、**ログアウト中は未所属（未暗号化）の
  データのみ**が読める（keyring にトークンは 1 つだけ）。

## データモデル

`books.owner_sub`（**暗号化済み**の所有者 sub、NULL = 未所属・未暗号化）

- **暗号化は必須**：平文 sub を DB に置くと、`owner_id`（keyring のスロット名・ラップの AAD）と
  `sub` ラップの KEK（`PBKDF2(sub + APP_SALT)`）を導出でき、鍵 bundle と pack を入手していれば
  復号経路を渡すことになる（旧 v2 は `pack key = PBKDF2(sub + APP_SALT) → HKDF(salt = pack_id)`
  を直接導出できた）。→ keyring のローカル鍵（例 `thundoku-shelf.db-key`）で AES 暗号化して保存。
- `NULL` = 未ログイン時 DL（未暗号化 pack）→ **そのまま保有**
- `値` = その sub で暗号化された pack（所有者）

> 注: `owner_sub`（pack の暗号化所有者）と、旧仕様の `drive.sub`（アカウント切替の検知用、
> `app_settings`）は**別物**。採用版では `drive.sub` による検知自体を使わない（下記）。

### 他のテーブルの所有者（`owner_sub`）

本に紐づかない、または `book_id` から辿れないユーザーデータも同じ所有者列を持つ
（`books` と同型の暗号文（keyring の DB 鍵で AES-256-GCM）をそのまま入れる）:

| テーブル | 何のデータか | 属性付けのタイミング |
|---|---|---|
| `bookshelf_items` | 各ストアの購入一覧（タイトル・購入日時・`download_url` 等） | 同期（BOOTH / DLsite / FANZA / 技術書典）の完了時に、書き込んだ site の未所属行へ |
| `checked_items` | 技術書典のチェックリスト（メモ・価格・購入状態） | チェックリスト同期の完了時に、そのイベントの未所属行へ |
| `book_first_events` | 本ごとの初出イベント（`bookshelf_items` の子） | `bookshelf_items` と同じ（FK を保つため同じ絞り込みが要る） |
| `favorite_tags` | タグのお気に入り | 登録（ハート）時に現在の sub で付与 |
| `favorite_entities` | サークル / 作者のお気に入り | 同上 |

- 属性付けは「**未所属（NULL）の行だけ**を埋める」。既に所有者が付いた行は書き換えない
  （A の購入一覧が B の同期で B のものに化けて B のバックアップに混ざるのを防ぐ）。
- 列の追加は `db/mod.rs` のプログラム的マイグレーション（`ensure_column`）で行う
  （`favorite_entities` は同関数内で後から CREATE されるため、**テーブル作成がすべて
  終わった後**にまとめて追加する）。

## 所有者ごとの挙動

| 所有者 | 生成されるタイミング | 表示 | 読める | Drive アップロード |
|---|---|---|---|---|
| `NULL`（未所属） | **ログイン中でない**時の DL | **ログアウト中のみ表示** | そのときだけ読める（未暗号化） | ❌ しない |
| `= 現在 sub` | そのアカウントでログイン中の DL | 表示 | 読める | ✅ する |
| `≠ 現在 sub` | 他のアカウントで DL | 非表示 | 読めない（キーが無い） | ❌ しない |

## ログイン・切替・ログアウト

- **DL の挙動**：ログイン中（sub S）だと `owner_sub = S` で暗号化。**ログイン中でない**と
  `owner_sub = NULL` で未暗号化。
- **ログイン**：未所属 pack は**そのまま**（何も書き換えず、再暗号化・再DLしない）。
  現在 sub のデータ**のみ表示**（未所属 pack は非表示）。
  - ※ 未所属しか持ってない場合、ログインすると**本棚は空になり得る**が、これも仕様
    （ユーザには表立って出さない）。
  - **重複は `(source, owner)` で抑止**（source = `site_id + tbf_product_id`）：
    - 再DLの owner が**既存行と同じ**（同一 source・同一 owner）→ **既存行を更新**（同じ book id を使い、
      進捗・タグ・履歴維持。pack を現在 owner で再ビルド）。
    - 再DLの owner が**既存行と違う**（別アカウント、または未所属 NULL）→ **別の行**を追加
      （各アカウントのライブラリ + 未所属）。
      - ※ 未所属（NULL）の本をログイン中に再DLしても、**NULL 行は更新されない**（未所属→所属の
        自動変換はしない）。所属行が別途追加され、両方が共存する（両方管理できる）。
  - （実装）`(source, owner)` の重複抑止は**実装済み**。`books::find_by_source`
    （`site_id + tbf_product_id` で既存行を取得）と `books::resolve_reuse_id`（owner を復号比較し、
    同一 owner の行 id のみ返す。別 owner / 未所属 NULL は対象外）で再利用 id を決め、
    `download_item` から各取り込み関数へ `reuse_book_id` として渡す
    （PDF / EPUB / ZIP / 画像の全経路。画像は `import_image_bytes` にも配線済み）。
    再利用時は同じ book id のまま `books::upsert` で置き換えるため、book_id 紐付けの
    進捗・タグ・閲覧履歴は維持される。
- **フィルタ（表示・アップロード判定）**：`owner_sub` を復号して比較するため、ログイン時に
  `book_id → sub` の**メモリキャッシュ**を作る（起動時1回。本棚300件での毎復号を避ける）。
- **切替（別アカウントでログイン）**：前アカウントのデータは**削除しない**・属性付けたまま
  保持・**非表示**。現在アカウントのデータ**のみ表示**（未所属は非表示）。**2 択（初期化/ログアウト）は廃止**。
- **ログアウト**：Google ログアウトのみ。表示は**未所属（未暗号化）の本だけ**。sub 付きの本は
  **非表示**（読もうとしたらログイン誘導）。

## Drive 同期（所有者連動）

- `drive.sync.folder_id` は**単一キー**（アカウント別ではない）。
  **既知の制約**: 複数 Google アカウントを使う場合、Drive の保存先はアカウント間で共通に
  なっており、バックアップは同一フォルダに集まる。データ本体は `owner_sub` で属性付けされて
  いるため混ざらないが、フォルダを分けたい場合は `drive.sync.folder_id.{sub}` への
  切り替え（＋既存値の移行）が必要。
  切替時は前アカウントのフォルダに触らない。
- **アップロード**：`owner_sub == 現在 sub` のみ。NULL・他 sub は上げない。
- **ダウンロード**：現在 sub のフォルダから（自分の pack だけ）。インポート時は
  `owner_sub = 現在sub` をセットする。
  - **所有者確認（metadata プローブ）は行わない**。`folder_id` でフォルダが
    アカウントごとに分離されている前提で帰属を信頼する（複数アカウント対応は内部実装）。
- **DB バックアップ**（`thundoku-backup.json`）は **owner == 現在 sub のデータだけ**にフィルタして
  現在 sub の Drive にアップロード → **真にアカウントごとに独立**（他アカウント・未所属のメタは混ぜない）。
- **現在 sub は起動直後から必要**：アップロードの所有者フィルタだけでなく、起動時の復元確認
  （ローカルと Drive バックアップの差分判定）も「現在 sub の本」を範囲にする必要がある。
  しかし `sub` はプロフィール（`userinfo`）でしか取れず、起動時にネットワーク取得する前提には
  できない。そこでプロフィールは**取得時に keyring（`google-profile`）へ保存**し、起動時に
  復元する（`AppState::init_with_data_dir` → `restore_google_profile`）。ログアウト・
  トークン失効では控えも削除する。
  - 保存済みプロフィールが無い（アップグレード直後など）場合は起動時に `userinfo` を
    取りに行き、その場で保存する（`Workspace::restore_google_profile`）。
  - **`現在 sub` が分からないときは差分比較をしない**：Drive 側は所有者で絞られているため、
    ローカル全件と突き合わせると内容が同じでも「差分あり」になり、起動のたびに復元確認が
    出る（しかも古い Drive の内容でローカルを上書きできてしまう）。判定できないときは
    スキップする（`Workspace::backup_owner_ids`）。終了時のアップロードも owner 不明なら
    行わない（`sync()` の `can_backup_db`。無言でスキップせず警告ログを出す）。
- **復元確認は「Drive 側が動いた」ときだけ**: 「ローカルと Drive が違う」だけでは方向が
  分からず、ローカル側だけが進んだ場合（他端末を起動していない場合）にも出てしまう。
  最後にアップロードした内容の正規形 md5 を `app_settings['drive.backup.md5']` に保存し、
  Drive 側の md5 がそれと違うときだけ復元を提案する（`inspect_drive_backup` /
  `BackupStatus::should_offer_restore`）。比較は揮発列（`tbf_events.updated_at` 等）を
  落として PK 順に並べた正規形で行う（`db::backup::canonicalize_json`）。アップロード後と
  復元後は基準値を更新する（更新しないと次回起動で同じ差分を再提示し続ける）。

## セキュリティ / リスク

- `owner_sub` は暗号化して保存。**DB だけでは pack キーを導出できない**（v3 の鍵材料は乱数）。
- **`sub` はログに出さない**: v3 でも `sub` は `owner_id`（keyring のスロット名とラップの AAD）と
  `sub` ラップの KEK の材料で、実質の復号経路。ログ（Windows は `%TEMP%/thundoku-shelf/thundoku.log`
  に既定 debug レベルで残る）へ書くと、鍵 bundle と pack を入手した第三者に復号材料を渡すことに
  なる。`google::profile_log_label` は有無だけを返し、値は含めない。
- **v3 の鍵は乱数ルート鍵（PRK）+ ラップ**（`docs/spec/10-pack-keys.md`）: PRK は keyring と
  Drive の `thundoku-keys.json`（`sub` / 任意のパスフレーズでラップ）に置く。したがって
  **`sub` を知っているだけでは pack は解けない**（ラップの入った bundle が要る）。パスフレーズを
  設定していれば、bundle が漏れてもパスフレーズを知らない相手には解けない。`sub` ラップは
  仕様上は削除できる（`PackKeyBundle::remove_wrap`）が、core の `PackKeyStore` に公開操作はまだ無い。
  **v2 の pack は読めない**（`unsupported pack version`。ストアから取り込み直す）。
  （`docs/spec/03-import-and-pack.md` §4.5）
- `owner_sub` が**唯一の所有者記録**（pack ファイルには sub が無い）。
  - **現在ログイン中のアカウント**の sub は保存済みプロフィール（keyring `google-profile`）から
    復元できるため、`owner_sub` 破損でも pack は復号できる。トークン（`USER_GOOGLE`）自体には
    sub が含まれないため、プロフィールは取得時に保存する（[`Drive 同期（所有者連動）`] を参照）。
  - **非現在アカウント**の `owner_sub` は復元手段がなく、破損・消失でその pack は読めない
    （データ損失。許容）。
- **keyring（DB-key・Google トークン）を失うと**、`owner_sub` の復号・所有者復元ができなくなり、
  属性・pack が読めない（ログイン状態も失われる）。

## 既存データ

- **開発版のため、既存データは削除して新モデルで開始する**（マイグレーション・
  `owner_sub` のバックフィルは行わない）。既存データは初回起動時にクリアする。

## 不採用になった旧仕様（対応表）

| 旧仕様 | 判定 |
|---|---|
| 1 アカウント = 1 プロファイル / 切替 = 2 択（初期化・ログアウト） | ❌ 不採用（データ保持＆属性付け・ログイン切替へ） |
| ログイン中、未所属 pack は「未 DL 扱い」→ 再DLでサブキー付きへ差し替え | ❌ 不採用（そのまま保持） |
| `books.identity_sub` を平文で持つ | ❌ 不採用（`books.owner_sub` を暗号化して持つ） |
| Drive アップロード = 現在 sub の pack のみ（Plan B の仕様③） | ✅ 踏襲（表現が `owner_sub` に統一） |
| 設定のローカルデータ削除 = 未ログイン分も全部削除（仕様④） | ✅ 踏襲（`delete_all_data` は全削除・サブキー無関係を維持） |

---

# 実装上の確定事項（おすすめ方針）

上記の採用仕様を TDD で実装する際に、仕様から一意に決まらない部分をここで確定する。
（コードで ad-hoc に決めさせないためのメモ。）

## P1. `owner_sub` の暗号化形式と DB-key のライフサイクル

- **暗号化形式**：AES-256-GCM。`IV（12byte）|| ciphertext || tag` を BASE64 にして
  `books.owner_sub`（TEXT）に保存。`NULL` = 未所属（未暗号化）。
- **DB-key**：keyring の `thundoku-shelf.db-key`（ランダム 32 byte。初回使用時に生成）。
  - **起動時に無い場合 → 新規生成**する（dev 版・既存データ削除予定なので、古い `owner_sub` は復号不能になるが許容）。
  - 復号不能な `owner_sub` は「未知の所有者」として**未所属（NULL）扱いにはしない**（表示は非表示に留める）。

## P2. 暗号化された `owner_sub` は SQL でフィルタできない

- `owner == 現在sub` を SQL で絞れない（暗号化のため）。**フィルタはアプリ側で行う**。
  - ログイン時に `books::list()` で全件ロード → 各 `owner_sub` を復号 → `book_id → sub` のメモリキャッシュ構築。
  - その後の**表示・アップロード・バックアップ**のフィルタは、この**メモリキャッシュで判定**する
    （`book_id → sub` が現在 sub or NULL扱い かどうか）。
  - `books::list` 等は owner 列で SQL フィルタせず、**全件 + メモリフィルタ**に変更する。

## P3. バックアップの owner フィルタは複数テーブルに波及

- `backup::export_json` の owner フィルタは、`books` を絞るだけでなく、**絞った book_id 集合に連動して
  関連テーブルも除外**する（`reading_progress` / `book_tags` / `view_history` / `page_views` 等）。
  - 実装：`books` の owner フィルタで出た book_id 集合をキーに、関連テーブルの行も絞って出力。
- **本に紐づかないテーブルは `owner_sub` 列で絞る**（`backup::OWNER_SCOPED_TABLES` =
  `bookshelf_items` / `checked_items` / `book_first_events` / `favorite_tags` /
  `favorite_entities`）。呼び出し側は `OwnerFilter { key, sub }` を渡し、`owner_sub`
  （暗号文）を復号して現在の sub と比較する（P2 と同じ理由で SQL では絞れない）。
  - `book_first_events` は `bookshelf_items` の子（FK）なので、親と同じ規則で絞らないと
    復元時に FK 違反で全体がロールバックする。
  - 公開メタ（`tbf_events` / `zenn_tag_metadata` / `sites`）は対象外（誰のものでもない）。

## P4. 「初回起動時の既存データクリア」のトリガー

- `app_settings` に `owner_sub_model.initialized` フラグを持つ。
  - **初回起動（フラグ無し）**：既存データを全削除 → フラグをセット。
  - **2 回目以降（フラグ有り）**：触らない。

## P5. `(source, owner)` 重複抑止の book id 決定

- **新規**（その source の既存行が無い / owner が一致しない）：新しい book id（UUID）を割り当て、
  `identity.pack_id` に使う。
- **既存更新**（source + owner 一致）：**既存の book id** を `identity.pack_id` にして更新。
- `book_id_for(Some(identity))` が `identity.pack_id` を返す仕組みと噛み合わせる。
- **未所属（NULL）行は更新しない**（自動変換なし）。所属行を別途追加する。
- **実装**: `download_item` が `books::resolve_reuse_id`（`site_id + tbf_product_id` の lookup +
  owner 復号比較）で `reuse_book_id` を求め、各取り込み関数（`import_image_bytes` を含む）へ渡す。
  `import::book_id_for` は `reuse_book_id` を最優先、無ければ `identity.pack_id`、最後に新規 UUID。

## P6. id はファイルパスになるため復元時に検証する

- `books.id` / `books.pack_id` は `packs/{id}.opfspack` のファイル名になる。Drive の JSON
  バックアップから復元できるため、復元時に検証しないと `../other/book` のような id で
  保存領域の外を読み書き・削除できてしまう（CWE-22）。
- 検証は `pack_path` モジュールに集約する（`is_safe_id` / `pack_path` / `conflict_backup_path`）。
  - `is_safe_id` が拒否するもの: 空 / 長すぎる / パス区切り（`/` `\`）/ 親参照（`..`）/ `:` /
    制御文字 / 先頭 `.` / パスとして 1 成分でない（絶対パス・Windows プレフィックス）。
  - `pack_path` は更に「解決後の親が保存領域そのもの」であることを確認する。
- 復元（`backup::import_json`）は不正な `books.id` / `pack_id` を見つけたら**復元を中止**する
  （行だけ捨てると子テーブルの FK 違反で全体がロールバックするため。利用者にエラーを見せる）。
- pack を触る経路（取り込み・閲覧・改名・削除・Drive 同期・表紙読み込み）は
  **すべて `pack_path` を通す**。

## P7. 認証付き送信の送信先は許可リストで検証する

- セッション Cookie / XSRF を付けたリクエストは、**送信直前**に `download_url::check` で
  `https` + 許可ホスト（+ 必要なパス）を検証する。
  - `bookshelf_items.download_url` は Drive の JSON バックアップから復元でき、改変した
    バックアップを復元させると外部ホストへ Cookie が送られる。
  - 302 を手動追跡する経路（DLsite / FANZA）は `Location` も検証する（任意ホストへの転送を防ぐ）。
- 許可リストは Cookie のスコープに合わせる: BOOTH は `booth.pm` + `/downloadables/`、
  DLsite は `*.dlsite.com`、FANZA は `*.dmm.co.jp`、技術書典は `techbookfest.org`（解決）と
  `techbookfest.org/api/product-dlc/` + `storage.googleapis.com/tbf-tokyo-product-dlc/`（本体）。
- 検証に失敗したら**リクエストを送らずにエラー**を返す（`BlockedUrl`）。

