//! pack の鍵（v3 = ランダムなルート鍵 PRK + ラップ）を UI とつなぐ配線。
//!
//! 解決順序・ラップの形式は core（`thundoku_core::pack_keys` / 仕様
//! `docs/spec/10-pack-keys.md` §4.1）が正で、ここは
//!
//! 1. **判定**（keyring に鍵があるか / bundle に `kind=passphrase` のラップがあるか）を
//!    背景スレッドで行い、
//! 2. パスフレーズが要るときだけ UI（[`PackKeyPrompt`] → 解錠ダイアログ）へ尋ね、
//! 3. 入力値を持って復号する（入力が違えば**再入力を促す**。`sub` ラップへ黙って
//!    落ちない — 仕様 §4.1 手順 3）
//!
//! という順序を組む。`PackKeyStore` の prompt コールバックは同期なので、
//! [`KeyContext::unlock`] / [`resolve_root_key`] は**背景スレッドからのみ**呼ぶ
//! （UI スレッドから呼ぶとモーダルの答えを待って固まる）。

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use gpui_kit::ReadGlobal as _;
use opfspack::{PackKeyBundle, PackRootKey, WrapKind, derive_owner_id};
use parking_lot::Mutex;
use thundoku_core::db::{self, SqlitePool};
use thundoku_core::drive::{DriveApi, DriveClient};
use thundoku_core::google::{GoogleClient, GoogleProfile};
use thundoku_core::import::{ImportError, pack_root_key_for_import};
use thundoku_core::pack_keys::{PackKeyStore, PackKeysError};
use thundoku_core::secrets::SecretStore;
use thundoku_core::tbf::UreqTransport;

use crate::app_state::AppState;

/// Drive の同期フォルダ（`thundoku-keys.json` を置く場所）の設定キー。
const DRIVE_FOLDER_KEY: &str = "drive.sync.folder_id";

/// パスフレーズが違ったときの案内（解錠ダイアログに出す）。
///
/// 「スキップ」の意味も一緒に書く（スキップだけが `sub` ラップへ落ちる経路）。
pub const WRONG_PASSPHRASE: &str = "パスフレーズが違います。もう一度入力してください（「スキップ」すると Google ログインの鍵で復元できる場合だけ解錠します）";

/// 解錠ダイアログの答えを待つ上限。
///
/// 答える相手（UI）が消えた場合に背景スレッドが永久に止まらないための天井。
const ASK_TIMEOUT: Duration = Duration::from_secs(300);

/// 解錠ダイアログの答え。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassphraseAnswer {
    /// 入力されたパスフレーズ
    Passphrase(String),
    /// 利用者が「スキップ」した（`sub` ラップで解錠できるときだけそれを使う）
    Skipped,
}

/// UI に出す解錠の要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassphraseRequest {
    /// 何のために鍵が要るか（「本を開く」「本の取り込み」「同期」など）
    pub purpose: String,
    /// 直前の入力が違ったときの案内（再入力のときだけ入る）
    pub error: Option<String>,
}

/// 背景スレッドから UI（解錠ダイアログ）へパスフレーズを尋ねる口。
///
/// 背景側は [`Self::ask`] で**ブロックして待つ**。UI は描画のたびに [`Self::pending`]
/// を見てダイアログを出し、[`Self::answer`] で答える（`Workspace` の監視タスクが
/// 要求の出現に気づいて再描画する）。同時に尋ねるのは 1 件だけ（2 件目は待つ）。
#[derive(Default)]
pub struct PackKeyPrompt {
    /// 同時に 1 件だけ尋ねるための門（2 件目の `ask` は 1 件目が終わるまで待つ）
    gate: Mutex<()>,
    /// 表示待ちの要求（背景 → UI）
    pending: Mutex<Option<PassphraseRequest>>,
    /// 答えを返す口（UI → 背景）
    reply: Mutex<Option<Sender<PassphraseAnswer>>>,
}

impl PackKeyPrompt {
    /// パスフレーズを尋ね、答えが来るまで**ブロックする**（背景スレッド専用）。
    ///
    /// `error` は直前の入力が違ったときの案内（再入力の促し）。
    pub fn ask(&self, purpose: &str, error: Option<&str>) -> PassphraseAnswer {
        let _gate = self.gate.lock();
        let (sender, receiver) = channel();
        *self.reply.lock() = Some(sender);
        *self.pending.lock() = Some(PassphraseRequest {
            purpose: purpose.to_string(),
            error: error.map(str::to_string),
        });
        // 答えが来ない（UI が答える前にアプリが終わった等）ときは解錠しない。
        // ここで勝手に `sub` ラップへ落とすと「パスフレーズを尋ねたのに黙って別経路」に
        // なるため、既定はスキップ（＝仕様 §4.1 手順 3 の「利用者がスキップした場合」）。
        let answer = receiver
            .recv_timeout(ASK_TIMEOUT)
            .unwrap_or(PassphraseAnswer::Skipped);
        *self.pending.lock() = None;
        *self.reply.lock() = None;
        answer
    }

    /// UI に出す要求（無ければ `None`）。描画のたびに読む。
    pub fn pending(&self) -> Option<PassphraseRequest> {
        self.pending.lock().clone()
    }

    /// UI からの答えを返す。答える相手がいなければ何もしない。
    pub fn answer(&self, answer: PassphraseAnswer) {
        *self.pending.lock() = None;
        let reply = self.reply.lock().take();
        if let Some(reply) = reply {
            let _ = reply.send(answer);
        }
    }
}

/// パスフレーズを尋ねるべきか（仕様 §4.1 手順 3）。
///
/// keyring に鍵が無く、bundle に `kind=passphrase` のラップがあるときだけ尋ねる。
/// `sub` ラップしか無ければ尋ねずにそれで解く（Google ログインだけで読める本を
/// 毎回尋ねないため）。
pub fn passphrase_prompt_needed(keyring_has_key: bool, bundle_has_passphrase: bool) -> bool {
    !keyring_has_key && bundle_has_passphrase
}

/// ログイン直後に「パスフレーズ未設定」の警告を出すか（純関数）。
///
/// 出すのは **この端末に守るべき鍵があり**、かつ Drive の bundle に
/// パスフレーズのラップが無いと **判定できた** ときだけ:
///
/// - 鍵がまだ無い（新規アカウント / 解決できていない）→ 守る対象が無いので出さない
/// - Drive 未設定 → 「Drive のバックアップから復元」の話ができないので出さない
/// - 判定できない（オフライン等で `None`）→ 出さない
/// - 既にパスフレーズがある → 出さない
///
/// 誤警告は「本当に危ない状態」の警告を無視させるので、**判定できないときは黙る**
/// 側に倒す（警告はログインのたびに出せるが、信用は一度で失われる）。
pub fn passphrase_notice_needed(
    has_local_key: bool,
    drive_configured: bool,
    has_passphrase: Option<bool>,
) -> bool {
    has_local_key && drive_configured && has_passphrase == Some(false)
}

/// 手元の材料から PRK を決める（仕様 §4.1 手順 1〜4 の判定）。
///
/// - `keyring` にあれば**何も尋ねない**
/// - bundle にパスフレーズラップがあれば `ask` を呼ぶ。入力が違えば `ask` を
///   もう一度呼ぶ（`sub` ラップへ**黙って落ちない**）
/// - `ask` が [`PassphraseAnswer::Skipped`] を返したときだけ `sub` ラップを使う
/// - 手元に鍵が無ければ `Ok(None)`（作るかどうかは呼び出し側の判断 — 仕様 §4.1 手順 5）
///
/// I/O は引数の材料だけで、UI（`ask`）はコールバックなので単体テストできる。
pub fn decide_root_key(
    keyring: Option<PackRootKey>,
    bundle: Option<&PackKeyBundle>,
    sub: &str,
    mut ask: impl FnMut(Option<&str>) -> PassphraseAnswer,
) -> Result<Option<PackRootKey>, PackKeysError> {
    if let Some(root) = keyring {
        return Ok(Some(root));
    }
    let Some(bundle) = bundle else {
        return Ok(None);
    };
    if !passphrase_prompt_needed(false, bundle.has_wrap(WrapKind::Passphrase)) {
        // `sub` ラップしか無い（＝ Google ログインだけで解ける）
        return Ok(Some(
            bundle
                .unwrap_with_sub(sub)
                .ok_or(PackKeysError::Unavailable)?,
        ));
    }
    let mut error: Option<String> = None;
    loop {
        match ask(error.as_deref()) {
            PassphraseAnswer::Passphrase(value) => {
                match bundle.unwrap_with_passphrase(&value) {
                    Some(root) => return Ok(Some(root)),
                    // 違うパスフレーズは sub ラップへ落とさず、もう一度尋ねる
                    None => error = Some(WRONG_PASSPHRASE.to_string()),
                }
            }
            PassphraseAnswer::Skipped => {
                return Ok(Some(
                    bundle
                        .unwrap_with_sub(sub)
                        .ok_or(PackKeysError::Unavailable)?,
                ));
            }
        }
    }
}

/// 端末の keyring にある PRK（ネットワークを触らない）。
///
/// 壊れた値・読み出し失敗は `None`（＝この端末に鍵が無い扱い）。理由はログに残す。
pub fn keyring_root_key(secrets: &SecretStore, sub: &str) -> Option<PackRootKey> {
    let owner_id = derive_owner_id(sub);
    let encoded = match secrets.load_pack_root_key(&owner_id) {
        Ok(value) => value?,
        Err(error) => {
            log::warn!("pack keys: keyring を読めない（owner_id={owner_id}）: {error}");
            return None;
        }
    };
    let root = PackRootKey::from_base64(&encoded);
    if root.is_none() {
        log::warn!("pack keys: keyring の値が PRK として読めない（owner_id={owner_id}）");
    }
    root
}

/// PRK を解決する（仕様 §4.1 の順序）。**背景スレッド専用**。
///
/// keyring → Drive の bundle（`thundoku-keys.json`）→ 必要ならパスフレーズ入力、の順。
/// 解けたら keyring に保存する（手順 4。次回起動では尋ねない）。
/// この端末にも Drive にも鍵が無ければ `Ok(None)`（＝新規作成は取り込み時）。
pub fn resolve_root_key(
    secrets: &SecretStore,
    drive: &mut dyn DriveApi,
    folder_id: &str,
    sub: &str,
    prompt: &PackKeyPrompt,
    purpose: &str,
) -> Result<Option<PackRootKey>, PackKeysError> {
    let owner_id = derive_owner_id(sub);
    // 1. keyring にあれば何も尋ねない
    let keyring = keyring_root_key(secrets, sub);
    // 2. bundle はモーダルを出す前に取り終える（尋ねている間 Drive を握らない）
    let bundle = if keyring.is_some() {
        None
    } else {
        thundoku_core::pack_keys::load_bundle(drive, folder_id, &owner_id)?
    };
    // 3〜4. パスフレーズを尋ねるかどうかは `decide_root_key` が決める（純関数）
    let Some(root) = decide_root_key(keyring, bundle.as_ref(), sub, |error| {
        prompt.ask(purpose, error)
    })?
    else {
        return Ok(None);
    };
    if let Err(error) = secrets.save_pack_root_key(&owner_id, &root.to_base64()) {
        // 保存できなくても今のセッションは使える（次回起動でまた尋ねる）。
        log::warn!("pack keys: keyring へ保存できない（次回起動でまた尋ねる）: {error}");
    }
    Ok(Some(root))
}

/// 鍵の解決に要る `AppState` の一部（背景スレッドへ move して使う）。
///
/// `AppState` は gpui の Global なのでスレッドへ持っていけない。背景タスクは
/// これ（`Arc` / `Clone` できる値だけ）を持つ。
#[derive(Clone)]
pub struct KeyContext {
    pub secrets: SecretStore,
    pub pool: SqlitePool,
    pub google: Arc<Mutex<Option<GoogleClient>>>,
    pub profile: Arc<Mutex<Option<GoogleProfile>>>,
    pub prompt: Arc<PackKeyPrompt>,
    /// 解決済み PRK の置き場（`AppState::pack_root_key` と同じ実体）
    pub slot: Arc<Mutex<Option<PackRootKey>>>,
}

impl KeyContext {
    pub fn from_state(state: &AppState) -> Self {
        Self {
            secrets: state.secrets.clone(),
            pool: state.db_pool.clone(),
            google: state.google.clone(),
            profile: state.google_profile.clone(),
            prompt: state.pack_key_prompt.clone(),
            slot: state.pack_root_key.clone(),
        }
    }

    /// このセッションで解決済みの PRK。
    pub fn cached(&self) -> Option<PackRootKey> {
        self.slot.lock().clone()
    }

    /// ログイン中のプロフィール（未ログインは `None`）。
    pub fn google_profile(&self) -> Option<GoogleProfile> {
        self.profile.lock().clone()
    }

    /// ログイン中の `sub`（未ログイン・`sub` が空なら `None`）。
    fn sub(&self) -> Option<String> {
        let profile = self.google_profile()?;
        let sub = profile.sub.trim().to_string();
        (!sub.is_empty()).then_some(sub)
    }

    /// keyring の鍵をメモリへ載せる（ネットワークを触らない。起動時・ログイン直後用）。
    pub fn load_keyring(&self) -> Option<PackRootKey> {
        let sub = self.sub()?;
        let root = keyring_root_key(&self.secrets, &sub)?;
        self.keep(&root);
        Some(root)
    }

    /// Drive の同期フォルダ id（未設定なら作る）。
    pub fn drive_folder(&self, drive: &mut dyn DriveApi) -> Result<String, String> {
        if let Some(id) = db::settings::get(&self.pool, DRIVE_FOLDER_KEY)
            .ok()
            .flatten()
        {
            return Ok(id);
        }
        let id = drive
            .create_folder("thundoku-shelf")
            .map_err(|error| error.to_string())?;
        let _ = db::settings::set(&self.pool, DRIVE_FOLDER_KEY, &id);
        Ok(id)
    }

    /// Drive クライアントを作る（ログイン中のアクセストークンを使う）。
    ///
    /// Google の Mutex はここで解放する（パスフレーズの入力待ちの間に握らない）。
    fn drive(&self) -> Result<DriveClient, String> {
        let mut google = self.google.lock();
        let client = google
            .as_mut()
            .ok_or_else(|| "Google にログインしてください".to_string())?;
        let token = client.access_token().map_err(|error| error.to_string())?;
        Ok(DriveClient::new(Box::new(UreqTransport::new()), token))
    }

    /// 解決できた PRK を `AppState` にも持たせる（読む側はここから取る）。
    fn keep(&self, root: &PackRootKey) {
        *self.slot.lock() = Some(root.clone());
    }

    /// ログイン中のアカウントの PRK を解決する（**背景スレッド専用**）。
    ///
    /// 未ログイン（平文 pack の経路）と Drive 未設定は `Ok(None)`。
    /// パスフレーズが要るときは解錠ダイアログが出る（答えが来るまでここでブロックする）。
    pub fn unlock(&self, purpose: &str) -> Result<Option<PackRootKey>, String> {
        let Some(sub) = self.sub() else {
            return Ok(None);
        };
        // このセッションで解決済みならそのまま使う（ログアウトで捨てているので
        // 別アカウントの鍵が残ることはない）。
        if let Some(root) = self.cached() {
            return Ok(Some(root));
        }
        // この端末の keyring にあれば何も尋ねない
        if let Some(root) = keyring_root_key(&self.secrets, &sub) {
            self.keep(&root);
            return Ok(Some(root));
        }
        let mut drive = self.drive()?;
        // フォルダ未設定 = 一度も同期していない = bundle は無い（鍵は取り込み時に作る）
        let Some(folder_id) = db::settings::get(&self.pool, DRIVE_FOLDER_KEY)
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        let root = resolve_root_key(
            &self.secrets,
            &mut drive,
            &folder_id,
            &sub,
            &self.prompt,
            purpose,
        )
        .map_err(|error| pack_keys_error_message(&error))?;
        if let Some(root) = &root {
            self.keep(root);
        }
        Ok(root)
    }

    /// 取り込みで使う PRK を用意する（仕様 §4.1 手順 5 / §5.1。**背景スレッド専用**）。
    ///
    /// - 未ログイン（`profile` = `None`）: 平文 pack（`Ok(None)`）
    /// - ログイン中: keyring → bundle → パスフレーズ、無ければ**新規作成**して
    ///   keyring に保存し、`sub` ラップを bundle に載せて Drive へ上げる
    ///   （アップロードに失敗しても取り込みは続ける。未アップロードの印が残り、
    ///   次の同期で再試行される — 仕様 §5.1）
    pub fn import_root_key(
        &self,
        profile: Option<&GoogleProfile>,
        purpose: &str,
    ) -> Result<Option<PackRootKey>, ImportError> {
        pack_root_key_for_import(profile, |sub| {
            // このセッションで解決済みなら Drive を触らない
            if self.sub().as_deref() == Some(sub)
                && let Some(root) = self.cached()
            {
                return Ok(root);
            }
            let mut drive = self.drive().map_err(ImportError::KeyStore)?;
            let folder_id = self
                .drive_folder(&mut drive)
                .map_err(ImportError::KeyStore)?;
            let store = PackKeyStore::new(&self.secrets, &self.pool, &folder_id);
            let root = ensure_import_root_key(&store, &mut drive, sub, &self.prompt, purpose)
                .map_err(ImportError::from)?;
            self.keep(&root);
            Ok(root)
        })
    }

    /// 未アップロードの鍵 bundle があれば上げ直す（仕様 §5.1。同期の最後に呼ぶ）。
    ///
    /// ログイン中のアカウントに pending が無ければ何もしない。**背景スレッド専用**。
    pub fn retry_pending_upload(&self) -> Result<bool, String> {
        let Some(sub) = self.sub() else {
            return Ok(false);
        };
        let Some(folder_id) = db::settings::get(&self.pool, DRIVE_FOLDER_KEY)
            .ok()
            .flatten()
        else {
            return Ok(false);
        };
        let mut drive = self.drive()?;
        let store = PackKeyStore::new(&self.secrets, &self.pool, &folder_id);
        store
            .retry_pending_upload(&mut drive, &sub)
            .map_err(|error| pack_keys_error_message(&error))
    }

    /// 未アップロードの鍵 bundle の `owner_id`（設定画面の警告に出す）。
    pub fn pending_owner(&self) -> Option<String> {
        PackKeyStore::new(&self.secrets, &self.pool, "")
            .pending_owner()
            .ok()
            .flatten()
    }

    /// パスフレーズを設定 / 変更する（仕様 §5.2。**背景スレッド専用**）。
    ///
    /// 鍵がまだ無ければ作る（`ensure`）。PRK は変えない（pack の再暗号化は不要）。
    pub fn set_passphrase(&self, passphrase: &str, purpose: &str) -> Result<(), String> {
        let sub = self
            .sub()
            .ok_or_else(|| "Google にログインしてください".to_string())?;
        // 鍵を用意する（無ければ作成。別端末のパスフレーズが要る場合はここで尋ねる）
        let root = self
            .import_root_key(self.google_profile().as_ref(), purpose)
            .map_err(|error| import_error_message(&error))?
            .ok_or_else(|| "本の鍵を用意できませんでした".to_string())?;
        let mut drive = self.drive()?;
        let folder_id = self.drive_folder(&mut drive)?;
        let store = PackKeyStore::new(&self.secrets, &self.pool, &folder_id);
        store
            .set_passphrase(&mut drive, &sub, &root, passphrase)
            .map_err(|error| pack_keys_error_message(&error))
    }

    /// パスフレーズラップを削除する（仕様 §5.2。**背景スレッド専用**）。
    /// 削除できたら `true`（bundle / ラップが無ければ `false`）。
    pub fn remove_passphrase(&self) -> Result<bool, String> {
        let sub = self
            .sub()
            .ok_or_else(|| "Google にログインしてください".to_string())?;
        let mut drive = self.drive()?;
        let folder_id = self.drive_folder(&mut drive)?;
        let store = PackKeyStore::new(&self.secrets, &self.pool, &folder_id);
        store
            .remove_passphrase(&mut drive, &sub)
            .map_err(|error| pack_keys_error_message(&error))
    }

    /// パスフレーズが設定されているか（Drive の bundle が正。設定画面の表示用）。
    /// **背景スレッド専用**（Drive を引く）。
    pub fn has_passphrase(&self) -> Result<bool, String> {
        let Some(sub) = self.sub() else {
            return Ok(false);
        };
        let Some(folder_id) = db::settings::get(&self.pool, DRIVE_FOLDER_KEY)
            .ok()
            .flatten()
        else {
            return Ok(false);
        };
        let mut drive = self.drive()?;
        let store = PackKeyStore::new(&self.secrets, &self.pool, &folder_id);
        store
            .has_passphrase(&mut drive, &sub)
            .map_err(|error| pack_keys_error_message(&error))
    }

    /// ログイン直後に「パスフレーズ未設定」の警告を出すか（**背景スレッド専用**）。
    ///
    /// 鍵の解決が終わった時点で呼ぶ（解決前に呼ぶと、解錠ダイアログを待っている間の
    /// 状態で判定してしまう）。判定できないときは `false`（[`passphrase_notice_needed`]）。
    pub fn needs_passphrase_notice(&self) -> bool {
        let Some(sub) = self.sub() else {
            return false;
        };
        // 鍵は「このセッションで解決済み」か「keyring にある」のどちらでもよい
        // （解決済みなら背景タスクが載せている）。
        let has_local_key =
            self.cached().is_some() || keyring_root_key(&self.secrets, &sub).is_some();
        let drive_configured = db::settings::get(&self.pool, DRIVE_FOLDER_KEY)
            .ok()
            .flatten()
            .is_some();
        let has_passphrase = match self.has_passphrase() {
            Ok(value) => Some(value),
            Err(error) => {
                // オフライン等。警告は出さない（誤警告を避ける）
                log::warn!("passphrase notice check failed: {error}");
                None
            }
        };
        passphrase_notice_needed(has_local_key, drive_configured, has_passphrase)
    }
}

/// 背景で PRK を解決するタスクを起こす（必要なら解錠ダイアログを出して待つ）。
///
/// UI スレッドはブロックしない。戻り値のタスクを `await` して結果を使う
/// （鍵が要る処理を鍵つきで始めたいときに使う）。
pub fn unlock_task<T: 'static>(
    cx: &gpui_kit::Context<T>,
    purpose: &str,
) -> gpui_kit::Task<Result<Option<PackRootKey>, String>> {
    let keys = KeyContext::from_state(AppState::global(cx));
    let purpose = purpose.to_string();
    cx.background_executor()
        .spawn(async move { keys.unlock(&purpose) })
}

/// 取り込みで使う PRK を用意する（仕様 §4.1 手順 5 / §5.1）。
///
/// - 鍵が無ければ `ensure` が新規作成し、`sub` ラップを bundle に載せて Drive へ上げる
///   （アップロード失敗でも取り込みは続ける。未アップロードの印が残り、同期で再試行される）
/// - パスフレーズが要る場合は `prompt`（UI）で尋ねる。**違う入力は `sub` ラップへ黙って
///   落とさず**、案内つきで再入力を促す（仕様 §4.1 手順 3）
pub fn ensure_import_root_key(
    store: &PackKeyStore<'_>,
    drive: &mut dyn DriveApi,
    sub: &str,
    prompt: &PackKeyPrompt,
    purpose: &str,
) -> Result<PackRootKey, PackKeysError> {
    let mut error: Option<String> = None;
    loop {
        let outcome = store.ensure(
            drive,
            sub,
            &mut || match prompt.ask(purpose, error.as_deref()) {
                PassphraseAnswer::Passphrase(value) => Some(value),
                PassphraseAnswer::Skipped => None,
            },
        );
        match outcome {
            Ok(root) => return Ok(root),
            Err(PackKeysError::PassphraseFailed) => error = Some(WRONG_PASSPHRASE.to_string()),
            Err(other) => return Err(other),
        }
    }
}

/// 鍵の解決エラーを利用者に伝わる文言にする（パスフレーズ違いは再入力を促す）。
pub fn pack_keys_error_message(error: &PackKeysError) -> String {
    match error {
        PackKeysError::PassphraseFailed => WRONG_PASSPHRASE.to_string(),
        other => other.to_string(),
    }
}

/// pack の読み出しエラーを利用者に伝わる文言にする。
///
/// v2 以前の pack は v3 の鍵階層では開けない（読めないのではなく**形式が違う**）ので、
/// 再取り込みを案内する（仕様 §6）。
pub fn pack_read_error_message(error: &opfspack::PackError) -> String {
    match error {
        opfspack::PackError::Version(version) if *version != opfspack::FORMAT_VERSION => format!(
            "この本は旧形式（pack v{version}）のため開けません。お手数ですがストアから取り込み直してください"
        ),
        opfspack::PackError::KeyRequired(_) => {
            "この本を復号する鍵がありません。Google にログインしてから、\
             設定画面の「本の鍵」でパスフレーズを入力して復元してください"
                .to_string()
        }
        other => other.to_string(),
    }
}

/// 取り込みの失敗を利用者に伝わる文言にする。
///
/// 鍵が無いときは**復元（パスフレーズ入力）への導線**を文面に含める。
pub fn import_error_message(error: &ImportError) -> String {
    match error {
        ImportError::IdentityKeyUnavailable => {
            "Google にログイン済みですが、本を復号する鍵が取得できません。\
             設定画面の「本の鍵」からパスフレーズを入力して復元してください\
             （鍵がどこにも無い場合は、ストアから取り込み直す必要があります）"
                .to_string()
        }
        other => other.to_string(),
    }
}

/// Drive 同期の失敗（利用者への文言 + 復元導線が要るか）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    /// 画面に出す文言（生の英語のエラーは出さない）
    pub message: String,
    /// 鍵の復元（パスフレーズ入力 / ログイン）へ誘導すべきか
    pub needs_unlock: bool,
}

impl SyncFailure {
    /// 同期以外の理由（ログイン不足・通信の失敗など）の失敗。
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            needs_unlock: false,
        }
    }

    /// 鍵の復元（解錠）へ誘導する失敗。
    pub fn unlock(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            needs_unlock: true,
        }
    }
}

impl From<String> for SyncFailure {
    fn from(message: String) -> Self {
        Self::message(message)
    }
}

/// 同期の失敗を利用者に伝わる形にする（純関数）。
///
/// - v2 以前の pack（`UnsupportedPackVersion`）は再取り込みを案内する（仕様 §6）
/// - 鍵が無い（`PackKeyRequired`）は復元導線を立てる
pub fn sync_failure(error: &thundoku_core::drive::sync::SyncError) -> SyncFailure {
    use thundoku_core::drive::sync::SyncError;
    match error {
        SyncError::UnsupportedPackVersion { pack_id, version } => SyncFailure {
            message: format!(
                "旧形式の本（pack v{version}）があるため同期できませんでした（{pack_id}）。\
                 お手数ですがストアから取り込み直してください"
            ),
            needs_unlock: false,
        },
        SyncError::PackKeyRequired(pack_id) => SyncFailure {
            message: format!(
                "この本を復号する鍵がないため同期できませんでした（{pack_id}）。\
                 設定画面の「本の鍵」からパスフレーズを入力して復元してください"
            ),
            needs_unlock: true,
        },
        other => SyncFailure {
            message: other.to_string(),
            needs_unlock: false,
        },
    }
}

/// この端末の鍵の状態（設定画面の表示）。
///
/// 鍵は「この端末（keyring）」と「Drive の bundle」の 2 箇所にあり、パスフレーズの
/// 有無で復旧できるかが変わる。表示はその組み合わせで決める（純関数）。
pub fn key_status_label(has_local_key: bool, has_passphrase: bool) -> &'static str {
    match (has_local_key, has_passphrase) {
        (true, true) => "この端末に鍵あり（パスフレーズで保護）",
        (true, false) => "この端末に鍵あり（パスフレーズ未設定）",
        (false, true) => "この端末に鍵なし（パスフレーズで復元できます）",
        (false, false) => "この端末に鍵なし",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opfspack::{PackRootKeyWrap, derive_owner_id};
    use std::collections::HashMap;
    use std::sync::mpsc::channel as std_channel;
    use thundoku_core::drive::{DriveError, DriveFile};

    /// テストで使う `sub` と時刻。
    const SUB: &str = "sub-test-1";
    const NOW: i64 = 1_790_000_000_000;
    /// テストのパスフレーズラップは反復回数を小さくする（既定は 600k 回で遅い）。
    /// `iterations` はラップに記録されるので復号側はこの値を使う（仕様 §3.2）。
    const TEST_ITERATIONS: u32 = 1_000;

    /// テスト用 Drive（core の `pack_keys` のテストと同じ形）。
    ///
    /// bundle の出し入れだけを再現する（`list_files` / `download` /
    /// `upload_multipart` / `delete`）。
    #[derive(Default)]
    struct FakeDrive {
        files: HashMap<String, (String, Vec<u8>)>,
        next_id: usize,
    }

    impl FakeDrive {
        fn new() -> Self {
            Self {
                files: HashMap::new(),
                next_id: 1,
            }
        }

        /// 名前 `name` でファイルを置く（アップロードの代わり）。
        fn seed(&mut self, name: &str, bytes: &[u8]) -> String {
            let id = format!("seeded-{name}-{}", self.next_id);
            self.next_id += 1;
            self.files
                .insert(id.clone(), (name.to_string(), bytes.to_vec()));
            id
        }

        /// その名前のファイル一覧（重複検出用）。
        fn named(&self, name: &str) -> Vec<&Vec<u8>> {
            self.files
                .values()
                .filter(|(file_name, _)| file_name == name)
                .map(|(_, bytes)| bytes)
                .collect()
        }
    }

    impl DriveApi for FakeDrive {
        fn list_files(&mut self, _folder_id: &str) -> Result<Vec<DriveFile>, DriveError> {
            Ok(self
                .files
                .iter()
                .map(|(id, (name, bytes))| DriveFile {
                    id: id.clone(),
                    name: name.clone(),
                    size: Some(bytes.len() as i64),
                    md5_checksum: None,
                    modified_time: None,
                })
                .collect())
        }

        fn download(&mut self, file_id: &str) -> Result<Vec<u8>, DriveError> {
            self.files
                .get(file_id)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| DriveError::Http(404, "not found".to_string()))
        }

        fn upload_multipart(
            &mut self,
            name: &str,
            _folder_id: &str,
            bytes: &[u8],
        ) -> Result<String, DriveError> {
            Ok(self.seed(name, bytes))
        }

        fn create_folder(&mut self, _name: &str) -> Result<String, DriveError> {
            Ok("folder-1".to_string())
        }

        fn delete(&mut self, file_id: &str) -> Result<(), DriveError> {
            self.files.remove(file_id);
            Ok(())
        }

        fn touch(&mut self, _file_id: &str) -> Result<(), DriveError> {
            Ok(())
        }
    }

    /// UI の代わりに解錠ダイアログへ答えるスレッドを立てる。
    ///
    /// 要求が現れるたびに `answers` を順に返し、見た要求を返す（`answers` を使い切るか
    /// 10 秒経ったら終わる）。
    fn answer_passphrases(
        prompt: Arc<PackKeyPrompt>,
        answers: Vec<PassphraseAnswer>,
    ) -> std::thread::JoinHandle<Vec<PassphraseRequest>> {
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            let mut index = 0;
            while index < answers.len() && std::time::Instant::now() < deadline {
                match prompt.pending() {
                    Some(request) => {
                        seen.push(request);
                        prompt.answer(answers[index].clone());
                        index += 1;
                    }
                    None => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            seen
        })
    }

    fn bundle_with_sub(root: &PackRootKey) -> PackKeyBundle {
        let owner_id = derive_owner_id(SUB);
        let mut bundle = PackKeyBundle::new(owner_id.clone(), NOW);
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(root, SUB, &owner_id, NOW));
        bundle
    }

    fn bundle_with_passphrase(root: &PackRootKey, passphrase: &str) -> PackKeyBundle {
        let owner_id = derive_owner_id(SUB);
        let mut bundle = PackKeyBundle::new(owner_id.clone(), NOW);
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
            root,
            passphrase,
            &owner_id,
            TEST_ITERATIONS,
            NOW,
        ));
        bundle
    }

    /// パスフレーズを尋ねるのは「keyring に鍵が無く、bundle にパスフレーズラップがある」
    /// ときだけ（仕様 §4.1 手順 3）。`sub` ラップしか無ければ尋ねない。
    #[test]
    fn passphrase_is_asked_only_when_needed() {
        assert!(
            passphrase_prompt_needed(false, true),
            "keyring に鍵が無くパスフレーズラップがあるときだけ尋ねる"
        );
        assert!(!passphrase_prompt_needed(true, true), "keyring の鍵を使う");
        assert!(
            !passphrase_prompt_needed(false, false),
            "sub ラップで解ける"
        );
        assert!(!passphrase_prompt_needed(true, false));
    }

    /// keyring に鍵があれば何も尋ねない（bundle も見ない）。
    #[test]
    fn keyring_key_skips_every_question() {
        let root = PackRootKey::generate();
        let expected = root.clone();
        let mut asked = 0;
        let resolved = decide_root_key(Some(root), None, SUB, |_| {
            asked += 1;
            PassphraseAnswer::Skipped
        })
        .expect("keyring の鍵を使える");
        assert_eq!(asked, 0, "尋ねない");
        assert_eq!(
            resolved.expect("鍵あり").as_bytes(),
            expected.as_bytes(),
            "keyring の PRK をそのまま使う"
        );
    }

    /// bundle が無ければ「鍵が無い」（新規作成は取り込み側の判断）。
    #[test]
    fn no_bundle_means_no_key() {
        let root = decide_root_key(None, None, SUB, |_| PassphraseAnswer::Skipped).unwrap();
        assert!(root.is_none());
    }

    /// `sub` ラップしか無ければ尋ねずに `sub` で解く（Google ログインだけで読める本）。
    #[test]
    fn sub_wrap_resolves_without_asking() {
        let root = PackRootKey::generate();
        let bundle = bundle_with_sub(&root);
        let mut asked = 0;
        let resolved = decide_root_key(None, Some(&bundle), SUB, |_| {
            asked += 1;
            PassphraseAnswer::Skipped
        })
        .unwrap()
        .expect("sub ラップで解ける");
        assert_eq!(asked, 0, "尋ねない");
        assert_eq!(resolved.as_bytes(), root.as_bytes());
    }

    /// パスフレーズラップがあれば尋ね、入力が正しければそれで解く。
    #[test]
    fn passphrase_unlocks_the_bundle() {
        let root = PackRootKey::generate();
        let bundle = bundle_with_passphrase(&root, "correct horse");
        let mut asked = 0;
        let resolved = decide_root_key(None, Some(&bundle), SUB, |error| {
            asked += 1;
            assert!(error.is_none(), "1 回目は案内を出さない");
            PassphraseAnswer::Passphrase("correct horse".to_string())
        })
        .unwrap()
        .expect("パスフレーズで解ける");
        assert_eq!(asked, 1);
        assert_eq!(resolved.as_bytes(), root.as_bytes());
    }

    /// 違うパスフレーズは `sub` ラップへ黙って落ちず、案内つきで再入力を促す。
    #[test]
    fn wrong_passphrase_asks_again_instead_of_falling_back() {
        let root = PackRootKey::generate();
        let owner_id = derive_owner_id(SUB);
        let mut bundle = bundle_with_passphrase(&root, "correct horse");
        // `sub` ラップも入れておく（黙って落ちるならここで成功してしまう並び）
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(&root, SUB, &owner_id, NOW));
        let mut attempts = Vec::new();
        let resolved = decide_root_key(None, Some(&bundle), SUB, |error| {
            attempts.push(error.map(str::to_string));
            if attempts.len() == 1 {
                PassphraseAnswer::Passphrase("wrong".to_string())
            } else {
                PassphraseAnswer::Passphrase("correct horse".to_string())
            }
        })
        .unwrap()
        .expect("正しいパスフレーズで解ける");
        assert_eq!(attempts.len(), 2, "違えばもう一度尋ねる");
        assert!(attempts[0].is_none());
        assert_eq!(
            attempts[1].as_deref(),
            Some(WRONG_PASSPHRASE),
            "2 回目は「違います」と伝える"
        );
        assert_eq!(resolved.as_bytes(), root.as_bytes());
    }

    /// 「スキップ」のときだけ `sub` ラップへ落ちる（仕様 §4.1 手順 3）。
    #[test]
    fn skipped_passphrase_falls_back_to_sub_wrap() {
        let root = PackRootKey::generate();
        let owner_id = derive_owner_id(SUB);
        let mut bundle = bundle_with_passphrase(&root, "correct horse");
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(&root, SUB, &owner_id, NOW));
        let resolved = decide_root_key(None, Some(&bundle), SUB, |_| PassphraseAnswer::Skipped)
            .unwrap()
            .expect("sub ラップで解ける");
        assert_eq!(resolved.as_bytes(), root.as_bytes());
    }

    /// スキップしたが `sub` ラップも無ければ「鍵が無い」（平文へは落ちない）。
    #[test]
    fn skipped_passphrase_without_sub_wrap_is_unavailable() {
        let root = PackRootKey::generate();
        let bundle = bundle_with_passphrase(&root, "correct horse");
        let error = decide_root_key(None, Some(&bundle), SUB, |_| PassphraseAnswer::Skipped)
            .expect_err("解ける材料が無い");
        assert!(matches!(error, PackKeysError::Unavailable));
    }

    /// 背景スレッドの `ask` は UI の答えで解け、UI が消えたら（sender が落ちたら）
    /// スキップ扱いになる（`sub` ラップへ黙って落ちるのを避ける既定）。
    #[test]
    fn prompt_round_trips_between_background_and_ui() {
        let prompt = Arc::new(PackKeyPrompt::default());
        let (tx, rx) = std_channel();
        let worker = {
            let prompt = prompt.clone();
            std::thread::spawn(move || {
                let answer = prompt.ask("本を開く", None);
                tx.send(answer).unwrap();
            })
        };
        // UI 側: 要求が見えるまで待ってから答える
        let request = loop {
            if let Some(request) = prompt.pending() {
                break request;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(request.purpose, "本を開く");
        assert!(request.error.is_none());
        prompt.answer(PassphraseAnswer::Passphrase("secret".to_string()));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            PassphraseAnswer::Passphrase("secret".to_string())
        );
        assert!(prompt.pending().is_none(), "答えたら要求は消える");
        worker.join().unwrap();
    }

    /// v2 以前の pack は「旧形式なので取り込み直す」と伝える（仕様 §6）。
    #[test]
    fn legacy_pack_errors_tell_the_user_to_reimport() {
        let message = pack_read_error_message(&opfspack::PackError::Version(2));
        assert!(message.contains("旧形式"), "{message}");
        assert!(message.contains("取り込み直"), "{message}");
        // 鍵が無いときは復元の導線を出す
        let message = pack_read_error_message(&opfspack::PackError::KeyRequired("p".to_string()));
        assert!(message.contains("パスフレーズ"), "{message}");
    }

    /// 同期の失敗は v2 pack と鍵なしを区別する（前者は再取り込み、後者は復元導線）。
    #[test]
    fn sync_failures_map_to_actionable_messages() {
        use thundoku_core::drive::sync::SyncError;
        let legacy = sync_failure(&SyncError::UnsupportedPackVersion {
            pack_id: "book-1".to_string(),
            version: 2,
        });
        assert!(legacy.message.contains("旧形式"), "{}", legacy.message);
        assert!(legacy.message.contains("取り込み直"), "{}", legacy.message);
        assert!(!legacy.needs_unlock, "再取り込みの案内なので解錠は要らない");

        let key = sync_failure(&SyncError::PackKeyRequired("book-2".to_string()));
        assert!(key.message.contains("パスフレーズ"), "{}", key.message);
        assert!(key.needs_unlock, "鍵の復元へ誘導する");
    }

    /// 端末の鍵の表示は「この端末の鍵」と「パスフレーズ」の組み合わせで決まる。
    #[test]
    fn key_status_label_describes_recovery_path() {
        assert!(key_status_label(true, true).contains("パスフレーズで保護"));
        assert!(key_status_label(false, true).contains("復元できます"));
        assert!(key_status_label(true, false).contains("パスフレーズ未設定"));
        assert_eq!(key_status_label(false, false), "この端末に鍵なし");
    }

    /// ログイン直後の警告は「守るべき鍵があり、パスフレーズが無いと分かった」ときだけ。
    ///
    /// 分からないとき（鍵が未解決・Drive 未設定・オフライン）に出すと誤警告になり、
    /// 本当に危ない状態の警告が無視されるようになる。
    #[test]
    fn passphrase_notice_only_when_the_answer_is_known() {
        // 鍵はあるがパスフレーズが無い = 警告する（本来の目的）
        assert!(passphrase_notice_needed(true, true, Some(false)));

        // パスフレーズがある / 無いと分かっている
        assert!(!passphrase_notice_needed(true, true, Some(true)));

        // まだ鍵が無い（新規アカウント・解決前）: 守る対象が無いので出さない
        assert!(!passphrase_notice_needed(false, true, Some(false)));

        // Drive 未設定: 「Drive のバックアップから復元」の話ができない
        assert!(!passphrase_notice_needed(true, false, Some(false)));

        // 判定できない（オフライン等）: 誤警告を出さない
        assert!(!passphrase_notice_needed(true, true, None));
        assert!(!passphrase_notice_needed(false, false, None));
    }

    /// 未ログインでは警告の判定をしない（鍵はアカウントごと。Drive も引かない）。
    #[gpui_kit::test]
    async fn passphrase_notice_check_is_off_without_a_google_login(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        // `key_env` は同じ `sub` の keyring を消すので直列化する（並列だと他のテストと競合）
        let _guard = KEYRING_SLOT.lock();
        let (_secrets, _pool) = key_env(cx);
        let keys = cx.read(|cx| KeyContext::from_state(AppState::global(cx)));
        assert!(
            !keys.needs_passphrase_notice(),
            "未ログインで警告を出す判定になっている"
        );
    }

    // ---- ここから下は Drive を伴う経路（偽 Drive を使う） ----

    /// メモリバックエンドの keyring はプロセス内で共有されるため、同じ `sub` を使う
    /// テストはこの Mutex で直列化する（並列だと保存と削除が競合する）。
    static KEYRING_SLOT: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// テスト用の keyring（メモリバックエンド）と DB。
    fn key_env(cx: &mut gpui_kit::TestAppContext) -> (SecretStore, SqlitePool) {
        if cx.update(|cx| cx.try_global::<AppState>().is_none()) {
            cx.update(AppState::init_test);
        }
        let (secrets, pool) = cx.update(|cx| {
            let state = AppState::global(cx);
            (state.secrets.clone(), state.db_pool.clone())
        });
        // 前のテストの値が残らないようにする（メモリバックエンドはプロセス内で共有）
        let _ = secrets.delete_pack_root_key(&derive_owner_id(SUB));
        (secrets, pool)
    }

    /// keyring に鍵が無く、Drive の bundle にパスフレーズラップがあるとき、
    /// パスフレーズを尋ねて解錠し、**keyring に保存する**（次回は尋ねない）。
    #[gpui_kit::test]
    async fn unlock_asks_for_the_passphrase_and_saves_the_key(cx: &mut gpui_kit::TestAppContext) {
        let _guard = KEYRING_SLOT.lock();
        let (secrets, pool) = key_env(cx);
        let root = PackRootKey::generate();
        let bundle = bundle_with_passphrase(&root, "correct horse");
        let mut drive = FakeDrive::new();
        drive.seed(
            thundoku_core::pack_keys::KEY_BUNDLE_NAME,
            &bundle.to_json().unwrap(),
        );
        let prompt = Arc::new(PackKeyPrompt::default());
        let responder = answer_passphrases(
            prompt.clone(),
            vec![PassphraseAnswer::Passphrase("correct horse".to_string())],
        );

        let resolved = resolve_root_key(&secrets, &mut drive, "folder-1", SUB, &prompt, "テスト")
            .expect("bundle があるので解錠できる")
            .expect("パスフレーズで復号できる");
        assert_eq!(resolved.as_bytes(), root.as_bytes());

        let seen = responder.join().unwrap();
        assert_eq!(seen.len(), 1, "1 回だけ尋ねる");
        assert_eq!(seen[0].purpose, "テスト", "何のための解錠かを伝える");
        assert!(seen[0].error.is_none(), "1 回目は案内を出さない");

        // keyring に保存されている（次回起動では尋ねずに済む）
        let saved = keyring_root_key(&secrets, SUB).expect("keyring に保存される");
        assert_eq!(saved.as_bytes(), root.as_bytes());

        // 2 回目は要求が出ない（尋ねない）
        let prompt = Arc::new(PackKeyPrompt::default());
        let resolved = resolve_root_key(&secrets, &mut drive, "folder-1", SUB, &prompt, "テスト")
            .unwrap()
            .expect("keyring から解ける");
        assert_eq!(resolved.as_bytes(), root.as_bytes());
        assert!(prompt.pending().is_none(), "keyring にあれば尋ねない");
        let _ = pool;
    }

    /// 違うパスフレーズは `sub` ラップへ黙って落ちず、案内つきで再入力を促す
    /// （bundle に両方のラップがあっても `sub` では解かない）。
    #[gpui_kit::test]
    async fn wrong_passphrase_asks_again_before_falling_back(cx: &mut gpui_kit::TestAppContext) {
        let _guard = KEYRING_SLOT.lock();
        let (secrets, _pool) = key_env(cx);
        let root = PackRootKey::generate();
        let owner_id = derive_owner_id(SUB);
        let mut bundle = bundle_with_passphrase(&root, "correct horse");
        // `sub` ラップも入れておく（黙って落ちるなら 1 回目で成功してしまう）
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(&root, SUB, &owner_id, NOW));
        let mut drive = FakeDrive::new();
        drive.seed(
            thundoku_core::pack_keys::KEY_BUNDLE_NAME,
            &bundle.to_json().unwrap(),
        );
        let prompt = Arc::new(PackKeyPrompt::default());
        let responder = answer_passphrases(
            prompt.clone(),
            vec![
                PassphraseAnswer::Passphrase("wrong".to_string()),
                PassphraseAnswer::Passphrase("correct horse".to_string()),
            ],
        );

        let resolved = resolve_root_key(&secrets, &mut drive, "folder-1", SUB, &prompt, "テスト")
            .unwrap()
            .expect("正しいパスフレーズで解ける");
        assert_eq!(resolved.as_bytes(), root.as_bytes());

        let seen = responder.join().unwrap();
        assert_eq!(seen.len(), 2, "違えばもう一度尋ねる");
        assert_eq!(
            seen[1].error.as_deref(),
            Some(WRONG_PASSPHRASE),
            "2 回目は「違います」と伝える"
        );
    }

    /// 取り込み: 鍵が無ければ新規作成して bundle を Drive へ上げ、keyring に保存する。
    /// bundle が残っていれば（別端末でも）そこから同じ鍵を復元できる。
    #[gpui_kit::test]
    async fn import_creates_and_uploads_the_key_bundle(cx: &mut gpui_kit::TestAppContext) {
        let _guard = KEYRING_SLOT.lock();
        let (secrets, pool) = key_env(cx);
        let prompt = Arc::new(PackKeyPrompt::default());
        let mut drive = FakeDrive::new();
        let store = PackKeyStore::new(&secrets, &pool, "folder-1");

        let created = ensure_import_root_key(&store, &mut drive, SUB, &prompt, "取り込み")
            .expect("鍵を作れる");
        let bundles = drive.named(thundoku_core::pack_keys::KEY_BUNDLE_NAME);
        assert_eq!(bundles.len(), 1, "bundle を Drive へ上げる");
        assert!(
            prompt.pending().is_none(),
            "鍵を作るだけならパスフレーズを尋ねない"
        );
        assert!(
            keyring_root_key(&secrets, SUB).is_some(),
            "keyring に保存する"
        );

        // 端末の keyring を失っても、Drive の bundle から同じ鍵を復元できる
        secrets.delete_pack_root_key(&derive_owner_id(SUB)).unwrap();
        let restored = ensure_import_root_key(&store, &mut drive, SUB, &prompt, "取り込み")
            .expect("bundle から復元できる");
        assert_eq!(
            restored.as_bytes(),
            created.as_bytes(),
            "bundle から同じ鍵が戻る"
        );
    }
}
