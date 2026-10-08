//! サンプル（デモ）モード: 実在の本を出さずに全画面を見せるための固定データ。
//!
//! `--demo` か `THUNDOKU_DEMO=1` で有効になり、**メモリ DB と一時ディレクトリだけ**を使う
//! （本物のデータディレクトリには書かない。判定は [`enabled`]、起動は
//! [`crate::app_state::AppState::init_demo`]）。ネットワークへ出ないための分岐は各所に
//! 散らしてあり、ここは「サンプルの中身」だけを持つ。
//!
//! 表紙は既存の表紙キャッシュ経路（`thumbnails/{site}_{id}_448.png`）に書き込むので、
//! 本棚側にサンプル専用の分岐は要らない。

use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::sync::LazyLock;

use gpui_kit::RenderImage;
use thundoku_core::db;
use thundoku_core::db::SqlitePool;
use thundoku_core::db::books::Book;
use thundoku_core::db::bookshelf::BookshelfItem;
use thundoku_core::db::checklist::{CheckedItem, TbfEvent};
use thundoku_core::db::notes::{PageNoteInput, SpreadSide};
use thundoku_core::db::progress::{ReadingProgress, ReadingState};
use thundoku_core::tbf::SITE_ID_TECHBOOKFEST;

/// サンプルの Google アカウントの `sub`。
///
/// [`seed`] が本の所有者（`books.owner_sub`）に書き、`AppState` のプロフィールにも同じ値を
/// 入れる。本棚はログイン中の `sub` に帰属する本だけを表示する（`owned_book_ids`）ため、
/// 両者が食い違うとサンプルが 1 冊も出ない。
pub const DEMO_OWNER_SUB: &str = "demo-owner";

/// `--demo` か `THUNDOKU_DEMO=1` で true（純関数）。
///
/// `args` は `std::env::args()`（先頭の実行ファイルパスを含む）。他の引数は見ない。
pub fn enabled(args: &[String], env: Option<&str>) -> bool {
    args.iter().any(|arg| arg == "--demo") || env == Some("1")
}

/// サンプルのデータディレクトリ（OS の一時領域）。
///
/// 本物のデータディレクトリ（[`crate::app_state::resolve_data_dir`]）とは別にし、
/// **本物のデータもログもここには書かない**。ログの出力先もこの下にする
/// （`crates/app/src/main.rs` の `log_data_dir`）ので、サンプル起動で
/// 本物の `logs/thundoku.log` を切り詰めてしまう事故が起きない。
pub fn data_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("thundoku-shelf-demo")
}

/// サンプルの本 1 冊（seed と [`DemoPageLoader`] が共有する唯一の出典）。
pub struct DemoBook {
    /// `books.id` / `bookshelf_items.database_id` と同じ値（`demo-01` 形式）。
    pub database_id: &'static str,
    pub title: &'static str,
    pub circle: &'static str,
    pub author: &'static str,
    pub tags: &'static [&'static str],
    pub page_count: u32,
    /// 本棚の状態チップ・履歴の表示に使う読書状態。
    pub reading_state: ReadingState,
    /// お気に入りの本か（本棚とローカル本の両方に印を付ける。[`seed`] を参照）。
    pub favorite: bool,
    /// 最初からダウンロード済みに見せるか（`false` = 未ダウンロード）。
    ///
    /// `false` の本は本棚（`bookshelf_items`）と表紙 PNG だけを持ち、ローカル本
    /// （`books`）を持たない（[`seed`]）。本棚カードの「未ダウンロード」表示は
    /// ローカル本の有無で決まるため、これで未ダウンロードの状態を再現できる。
    /// ダミーのダウンロード（[`promote_to_downloaded`]）が完了すると、`true` の本と
    /// 同じ形でローカル本・タグ・所有者が入る。
    pub downloaded: bool,
}

/// 架空のサンプル 50 冊（実在の本・サークル名に似せない）。
///
/// `database_id` は `bookshelf_items.database_id` と `books.id` の両方に使い、リーダーが
/// `demo::book(&book.id)` でサンプルページのローダーを引けるようにする（同じ文字列）。
/// 読書状態は未読 / 読書中 / 読了を混ぜ、`favorite` の 10 冊は [`seed`] が
/// 本棚とローカル本の両方へお気に入りを立てる。`downloaded: false` の 12 冊
/// （demo-13 / 15 / 18 / 22 / 24 / 27 / 31 / 33 / 39 / 41 / 45 / 50）は未ダウンロードで、
/// 本棚（と表紙 PNG）だけを持ち、ダミーのダウンロード（[`promote_to_downloaded`]）で
/// 取り込める（未読・進捗なし・履歴なし）。
pub const BOOKS: &[DemoBook] = &[
    DemoBook {
        database_id: "demo-01",
        title: "はじめての設計メモ",
        circle: "こもれび工房",
        author: "みなと",
        tags: &["設計", "入門"],
        page_count: 32,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-02",
        title: "小さな道具をつくる Rust",
        circle: "星屑ラボ",
        author: "なぎさ",
        tags: &["Rust", "CLI"],
        page_count: 48,
        reading_state: ReadingState::Read,
        favorite: true,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-03",
        title: "色と余白のレイアウト帳",
        circle: "しろくま出版",
        author: "あおい",
        tags: &["デザイン", "レイアウト"],
        page_count: 24,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-04",
        title: "手書きで学ぶデータベース",
        circle: "みかづき書房",
        author: "かえで",
        tags: &["データベース", "入門"],
        page_count: 40,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-05",
        title: "毎日の自動化レシピ",
        circle: "かたつむり開発室",
        author: "ゆうと",
        tags: &["自動化", "スクリプト"],
        page_count: 28,
        reading_state: ReadingState::Read,
        favorite: true,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-06",
        title: "テストの書き方ノート",
        circle: "あおぞら技術部",
        author: "さくら",
        tags: &["テスト", "品質"],
        page_count: 36,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-07",
        title: "小さな Web アプリの作り方",
        circle: "ひだまりソフト",
        author: "りく",
        tags: &["Web", "入門"],
        page_count: 52,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-08",
        title: "文字と組版のきほん",
        circle: "つばめ印刷所",
        author: "ひな",
        tags: &["組版", "フォント"],
        page_count: 30,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-09",
        title: "図解アルゴリズム散歩",
        circle: "もりのなか技術班",
        author: "いつき",
        tags: &["アルゴリズム", "図解"],
        page_count: 44,
        reading_state: ReadingState::Unread,
        favorite: true,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-10",
        title: "ふりかえりカード 100",
        circle: "なないろ工房",
        author: "みお",
        tags: &["チーム", "ふりかえり"],
        page_count: 20,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-11",
        title: "手を動かす HTTP のきほん",
        circle: "くじら通信社",
        author: "そうた",
        tags: &["Web", "入門"],
        page_count: 44,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-12",
        title: "シェル芸で進める日々の作業",
        circle: "みなとや",
        author: "かんな",
        tags: &["スクリプト", "自動化"],
        page_count: 60,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-13",
        title: "小さなキーボードを作る",
        circle: "そよかぜ電子",
        author: "はるか",
        tags: &["設計", "図解"],
        page_count: 88,
        reading_state: ReadingState::Unread,
        favorite: true,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-14",
        title: "SQL を読み解く練習帳",
        circle: "まほろば出版",
        author: "ゆかり",
        tags: &["データベース", "入門"],
        page_count: 72,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-15",
        title: "やさしい正規表現の教科書",
        circle: "こもれび工房",
        author: "みなと",
        tags: &["スクリプト", "入門"],
        page_count: 52,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-16",
        title: "コンテナで作る小さな開発環境",
        circle: "あおぞら技術部",
        author: "さくら",
        tags: &["設計", "自動化"],
        page_count: 96,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-17",
        title: "型から考えるアプリ設計",
        circle: "星屑ラボ",
        author: "なぎさ",
        tags: &["設計", "テスト"],
        page_count: 110,
        reading_state: ReadingState::Reading,
        favorite: true,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-18",
        title: "CSS グリッドの組み方帳",
        circle: "しろくま出版",
        author: "あおい",
        tags: &["Web", "レイアウト"],
        page_count: 64,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-19",
        title: "Git の困ったをほどく本",
        circle: "かたつむり開発室",
        author: "ゆうと",
        tags: &["自動化", "チーム"],
        page_count: 58,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-20",
        title: "小さなテンプレートエンジン",
        circle: "ひだまりソフト",
        author: "りく",
        tags: &["Web", "設計"],
        page_count: 76,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-21",
        title: "読んでわかる並行処理",
        circle: "もりのなか技術班",
        author: "いつき",
        tags: &["設計", "テスト"],
        page_count: 120,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-22",
        title: "コマンドラインツールの作法",
        circle: "つばめ印刷所",
        author: "ひな",
        tags: &["CLI", "設計"],
        page_count: 48,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-23",
        title: "はじめてのドメイン設計",
        circle: "みかづき書房",
        author: "かえで",
        tags: &["設計", "データベース"],
        page_count: 132,
        reading_state: ReadingState::Read,
        favorite: true,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-24",
        title: "色を決めるための小さな本",
        circle: "なないろ工房",
        author: "みお",
        tags: &["デザイン", "レイアウト"],
        page_count: 40,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-25",
        title: "ログから学ぶ運用のきほん",
        circle: "しろくま出版",
        author: "あおい",
        tags: &["品質", "自動化"],
        page_count: 84,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-26",
        title: "テストを書く前に読む本",
        circle: "あおぞら技術部",
        author: "さくら",
        tags: &["テスト", "品質"],
        page_count: 68,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-27",
        title: "小さな静的サイトを作る",
        circle: "ひだまりソフト",
        author: "りく",
        tags: &["Web", "組版"],
        page_count: 54,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-28",
        title: "文字コードの迷宮を歩く",
        circle: "つばめ印刷所",
        author: "ひな",
        tags: &["フォント", "入門"],
        page_count: 62,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-29",
        title: "関数型の考え方をなめる",
        circle: "くじら通信社",
        author: "そうた",
        tags: &["設計", "入門"],
        page_count: 90,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-30",
        title: "計測して速くする小さな技",
        circle: "星屑ラボ",
        author: "なぎさ",
        tags: &["品質", "Web"],
        page_count: 100,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-31",
        title: "アイコンを描く休日",
        circle: "そよかぜ電子",
        author: "はるか",
        tags: &["デザイン", "図解"],
        page_count: 36,
        reading_state: ReadingState::Unread,
        favorite: true,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-32",
        title: "小さなコンパイラを作ってみる",
        circle: "もりのなか技術班",
        author: "いつき",
        tags: &["設計", "CLI"],
        page_count: 150,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-33",
        title: "やさしい暗号のきほん",
        circle: "みなとや",
        author: "かんな",
        tags: &["入門", "品質"],
        page_count: 70,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-34",
        title: "データベース設計の落とし穴",
        circle: "まほろば出版",
        author: "ゆかり",
        tags: &["データベース", "設計"],
        page_count: 118,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-35",
        title: "パズルで覚えるアルゴリズム",
        circle: "こもれび工房",
        author: "みなと",
        tags: &["アルゴリズム", "図解"],
        page_count: 56,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-36",
        title: "小さなスクレイパを育てる",
        circle: "かたつむり開発室",
        author: "ゆうと",
        tags: &["スクリプト", "自動化"],
        page_count: 80,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-37",
        title: "README を書く技術",
        circle: "なないろ工房",
        author: "みお",
        tags: &["チーム", "設計"],
        page_count: 34,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-38",
        title: "ソケットから学ぶネットワーク",
        circle: "くじら通信社",
        author: "そうた",
        tags: &["Web", "入門"],
        page_count: 128,
        reading_state: ReadingState::Reading,
        favorite: true,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-39",
        title: "レイアウトの崩れを直す本",
        circle: "しろくま出版",
        author: "あおい",
        tags: &["Web", "レイアウト"],
        page_count: 66,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-40",
        title: "小さなバッチ処理の設計",
        circle: "あおぞら技術部",
        author: "さくら",
        tags: &["自動化", "設計"],
        page_count: 74,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-41",
        title: "かな入力から始める組版",
        circle: "つばめ印刷所",
        author: "ひな",
        tags: &["組版", "フォント"],
        page_count: 46,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-42",
        title: "テストデータの作り方",
        circle: "みかづき書房",
        author: "かえで",
        tags: &["テスト", "データベース"],
        page_count: 88,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-43",
        title: "小さな TODO アプリを設計する",
        circle: "ひだまりソフト",
        author: "りく",
        tags: &["Web", "設計"],
        page_count: 92,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-44",
        title: "読みやすいコードの整え方",
        circle: "星屑ラボ",
        author: "なぎさ",
        tags: &["品質", "チーム"],
        page_count: 72,
        reading_state: ReadingState::Read,
        favorite: true,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-45",
        title: "図でわかるデータ構造",
        circle: "もりのなか技術班",
        author: "いつき",
        tags: &["アルゴリズム", "図解"],
        page_count: 94,
        reading_state: ReadingState::Unread,
        favorite: false,
        downloaded: false,
    },
    DemoBook {
        database_id: "demo-46",
        title: "手書きフォントを作る",
        circle: "そよかぜ電子",
        author: "はるか",
        tags: &["フォント", "デザイン"],
        page_count: 60,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-47",
        title: "小さな検索エンジンを作る",
        circle: "みなとや",
        author: "かんな",
        tags: &["アルゴリズム", "設計"],
        page_count: 160,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-48",
        title: "はじめての依存関係管理",
        circle: "かたつむり開発室",
        author: "ゆうと",
        tags: &["設計", "CLI"],
        page_count: 50,
        reading_state: ReadingState::Read,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-49",
        title: "毎日使うショートカットの本",
        circle: "なないろ工房",
        author: "みお",
        tags: &["CLI", "自動化"],
        page_count: 30,
        reading_state: ReadingState::Reading,
        favorite: false,
        downloaded: true,
    },
    DemoBook {
        database_id: "demo-50",
        title: "ふりかえりを続けるコツ",
        circle: "こもれび工房",
        author: "みなと",
        tags: &["チーム", "ふりかえり"],
        page_count: 42,
        reading_state: ReadingState::Unread,
        favorite: true,
        downloaded: false,
    },
];

/// `database_id`（= `books.id`）でサンプルの本を引く。
pub fn book(database_id: &str) -> Option<&'static DemoBook> {
    BOOKS.iter().find(|book| book.database_id == database_id)
}

/// サンプルページの大きさ（A4 比: 1240 × 1.414 ≒ 1754）。
const PAGE_WIDTH: u32 = 1240;
const PAGE_HEIGHT: u32 = 1754;

/// 表紙の大きさ（既存の表紙キャッシュと同じ最大幅。縦長は既存のプレースホルダと同じ 3:4）。
const COVER_WIDTH: u32 = 448;
const COVER_HEIGHT: u32 = 597;

/// 進捗の固定値（読書中・読了の本だけ）。`(database_id, 現在ページ, 最終閲覧日時)`。
///
/// 読書状態は [`DemoBook::reading_state`] が正で、こちらは「どこまで読んだか」を
/// 決めるだけ（`progress_table_matches_the_reading_states` が両者の一致を固定する）。
const PROGRESS: &[(&str, i64, &str)] = &[
    ("demo-01", 12, "2026-09-06 21:35:00"),
    ("demo-02", 48, "2026-09-08 20:25:00"),
    ("demo-04", 7, "2026-09-03 19:20:00"),
    ("demo-05", 28, "2026-09-09 22:30:00"),
    ("demo-07", 52, "2026-09-07 20:05:00"),
    ("demo-08", 19, "2026-09-02 21:00:00"),
    ("demo-10", 20, "2026-09-01 20:15:00"),
    ("demo-12", 34, "2026-08-25 21:10:00"),
    ("demo-14", 72, "2026-08-22 20:50:00"),
    ("demo-16", 41, "2026-08-27 20:40:00"),
    ("demo-17", 52, "2026-09-01 21:20:00"),
    ("demo-19", 58, "2026-08-23 21:05:00"),
    ("demo-21", 63, "2026-08-29 22:05:00"),
    ("demo-23", 132, "2026-08-28 20:15:00"),
    ("demo-25", 30, "2026-09-02 20:10:00"),
    ("demo-26", 68, "2026-08-21 21:40:00"),
    ("demo-28", 25, "2026-08-24 21:45:00"),
    ("demo-30", 100, "2026-08-20 20:30:00"),
    ("demo-32", 77, "2026-09-03 21:30:00"),
    ("demo-34", 118, "2026-08-19 21:25:00"),
    ("demo-36", 44, "2026-08-31 20:25:00"),
    ("demo-37", 34, "2026-08-18 19:55:00"),
    ("demo-38", 59, "2026-09-04 21:55:00"),
    ("demo-40", 74, "2026-08-17 20:45:00"),
    ("demo-42", 33, "2026-08-26 19:50:00"),
    ("demo-44", 72, "2026-08-16 21:35:00"),
    ("demo-46", 28, "2026-09-05 20:35:00"),
    ("demo-47", 160, "2026-08-15 20:20:00"),
    ("demo-48", 50, "2026-09-06 19:45:00"),
    ("demo-49", 14, "2026-08-30 21:15:00"),
];

/// 付箋（`(database_id, ページ, メモ)`。ページは 1-indexed = 保存形式と同じ）。
const NOTES: &[(&str, i64, &str)] = &[
    ("demo-01", 5, "章の区切りが見やすい"),
    ("demo-02", 12, "この例は手元でも試したい"),
    ("demo-07", 30, "あとで読み返す"),
];

/// 閲覧履歴（`(id, database_id, 何日前, 開始 HH:MM, 長さ 分)`）。
///
/// 履歴画面（`view_history::list_daily`）と本棚の閲覧統計（`view_history::view_stats`）が
/// これを見る。日付は固定値にせず**起動日を基準に組み立てる**（[`session_times`]）ので、
/// 「今日 / 今週 / 今月」のどの絞り込みでも中身が出る。時刻は夜（19:30〜22:20 開始、
/// 最長でも 23:10 終了）に散らしてある。
const VIEW_SESSIONS: &[(&str, &str, u32, &str, u32)] = &[
    // 今日
    ("demo-view-1", "demo-01", 0, "20:10", 45),
    ("demo-view-2", "demo-02", 0, "21:20", 30),
    ("demo-view-3", "demo-07", 0, "22:05", 30),
    // 昨日
    ("demo-view-4", "demo-05", 1, "21:05", 40),
    ("demo-view-5", "demo-01", 1, "22:10", 30),
    // 2 日前
    ("demo-view-6", "demo-11", 2, "19:30", 50),
    ("demo-view-7", "demo-03", 2, "21:00", 25),
    // 3 日前
    ("demo-view-8", "demo-02", 3, "20:25", 30),
    ("demo-view-9", "demo-14", 3, "22:00", 70),
    // 4 日前
    ("demo-view-10", "demo-06", 4, "21:15", 20),
    ("demo-view-11", "demo-09", 4, "22:20", 30),
    // 5 日前
    ("demo-view-12", "demo-17", 5, "19:45", 85),
    ("demo-view-13", "demo-04", 5, "21:30", 25),
    // 6 日前
    ("demo-view-14", "demo-08", 6, "20:40", 40),
    ("demo-view-15", "demo-12", 6, "22:15", 30),
];

/// 技術書典のイベント（チェックリストの親。本棚のイベント名にも使う）。
const EVENT_ID: &str = "demo-tbf-20";
const EVENT_SLUG: &str = "demo-tbf-20";
const EVENT_NAME: &str = "技術書典20";
/// サンプルの購入日（本棚の並び順・イベントの開催日）。
const PURCHASED_AT: &str = "2026-09-12 10:00:00";
const EVENT_DATE: &str = "2026-09-12";

/// チェックリストの項目（`(id, サークル, スペース, タイトル, チェック済み)`）。
const CHECKLIST: &[(&str, &str, &str, &str, bool)] = &[
    (
        "demo-check-1",
        "こもれび工房",
        "東2 あ-12",
        "はじめての設計メモ",
        true,
    ),
    (
        "demo-check-2",
        "星屑ラボ",
        "東2 い-05",
        "小さな道具をつくる Rust",
        true,
    ),
    (
        "demo-check-3",
        "しろくま出版",
        "東3 う-21",
        "色と余白のレイアウト帳",
        false,
    ),
    (
        "demo-check-4",
        "もりのなか技術班",
        "東3 え-08",
        "図解アルゴリズム散歩",
        false,
    ),
];

/// お気に入りのサークル / 作者（本棚のチップにハートが付く）。
const FAVORITE_CIRCLES: &[&str] = &["こもれび工房", "星屑ラボ"];
const FAVORITE_AUTHORS: &[&str] = &["なぎさ"];

/// 固定データを 1 回だけ投入する（表紙 PNG の生成を含む）。
///
/// 同じ引数で 2 回呼んでも行は増えない（`upsert` 系だけを使う）。表紙は
/// `{thumbnails_dir}/{site_id}_{database_id}_448.png` に書く（既存の
/// [`crate::views::bookshelf::cover_cache_path`] と同じ規則）。
pub fn seed(pool: &SqlitePool, thumbnails_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(thumbnails_dir)?;
    // 本棚は「ログイン中の sub に帰属する本」だけを表示する（`owned_book_ids`）。
    // サンプルの本をデモのアカウントに付けておかないと 1 冊も出ない。
    let owner = demo_owner_token();
    db::checklist::upsert_event(pool, &event())?;
    for demo_book in BOOKS {
        seed_book(pool, thumbnails_dir, demo_book, owner.as_deref())?;
    }
    seed_checklist(pool, owner.as_deref())?;
    insert_view_sessions(pool)?;
    for name in FAVORITE_CIRCLES {
        db::favorites::set_favorite(
            pool,
            db::favorites::EntityKind::Circle,
            name,
            true,
            owner.as_deref(),
        )?;
    }
    for name in FAVORITE_AUTHORS {
        db::favorites::set_favorite(
            pool,
            db::favorites::EntityKind::Author,
            name,
            true,
            owner.as_deref(),
        )?;
    }
    Ok(())
}

/// 未ダウンロードのサンプル本をローカル本として取り込む（ダミーダウンロードの完了処理）。
///
/// サンプルモードのダウンロード（`crate::views::bookshelf` のダミー経路）が、本物の取り込みの
/// 代わりに呼ぶ。`seed` の「ダウンロード済み」と**同じ形**（`books` 行 + タグ + 所有者 +
/// お気に入り）で書く（[`write_local_book`]）ので、次の再読み込みから本棚カードが
/// 「ダウンロード済み」になり、リーダーも [`DemoPageLoader`] でサンプルページを開ける。
///
/// `upsert` 系だけを使うので**冪等**（同じ id で何度呼んでも行は増えない）。
/// サンプルに無い id は何もしない（ダミーの完了で起動を落とさない）。
pub fn promote_to_downloaded(pool: &SqlitePool, database_id: &str) -> anyhow::Result<()> {
    let Some(book) = book(database_id) else {
        return Ok(());
    };
    write_local_book(pool, book, demo_owner_token().as_deref())
}

/// デモのアカウントに帰属する印（暗号化済み `sub`）。
///
/// `AppState` の `secrets` と同じ keyring（サンプルはメモリバックエンド）を使うため、
/// `SecretStore::new()` で作っても同じ鍵が返る（`app_state::owner_token` と一致する）。
fn demo_owner_token() -> Option<String> {
    let key = thundoku_core::secrets::SecretStore::new().db_key().ok()?;
    Some(thundoku_core::owner::encrypt(&key, DEMO_OWNER_SUB))
}

/// 1 冊ぶんの固定データ（本棚・タグ・表紙は全冊、ローカル本・進捗・付箋はダウンロード済みだけ）。
fn seed_book(
    pool: &SqlitePool,
    thumbnails_dir: &Path,
    book: &DemoBook,
    owner: Option<&str>,
) -> anyhow::Result<()> {
    // 本棚（`bookshelf_items`）は未ダウンロードの本も持つ（カードの「未ダウンロード」表示は
    // ローカル本の有無で決まる）。タグは本棚（tags_json）とローカル本（book_tags）の両方に
    // 入れる。本棚カードはローカル本があればローカル本のタグを表示するため
    // （`BookshelfView::reload`）。
    db::bookshelf::upsert(pool, &shelf_item(book))?;
    let tags: Vec<String> = book.tags.iter().map(|tag| (*tag).to_string()).collect();
    db::bookshelf::update_tags(pool, SITE_ID_TECHBOOKFEST, book.database_id, &tags)?;
    // お気に入りの**本棚側**の印は、ローカル本が無くても立てる（本棚の絞り込みに使う）。
    // upsert は上書きしない（`bookshelf::upsert` の DO UPDATE にも `books::upsert` にも
    // `is_favorite` は入っていない = 同期で消えない扱い）ので、ここで明示的に立てる。
    // true を入れるだけなので 2 回 seed しても同じ結果になる。
    if book.favorite {
        db::bookshelf::set_favorite(pool, SITE_ID_TECHBOOKFEST, book.database_id, true)?;
    }

    // ローカル本と、それに紐づくもの（`books` への外部キーを持つ）は**ダウンロード済みの本だけ**。
    // 未ダウンロードの本はダミーのダウンロード（[`promote_to_downloaded`]）が完了したときに
    // 同じ形で入る。
    if book.downloaded {
        write_local_book(pool, book, owner)?;

        if let Some((_, current_page, last_read_at)) = PROGRESS
            .iter()
            .find(|(database_id, _, _)| *database_id == book.database_id)
        {
            let finished = book.reading_state == ReadingState::Read;
            db::progress::upsert(
                pool,
                &ReadingProgress {
                    book_id: book.database_id.to_string(),
                    // 既定表示コンテンツ（`''` = コンテンツ未指定。サンプルは 1 冊 1 コンテンツ）。
                    content_id: String::new(),
                    current_page: *current_page,
                    total_pages: Some(i64::from(book.page_count)),
                    finished_at: finished.then(|| (*last_read_at).to_string()),
                    last_read_at: (*last_read_at).to_string(),
                },
            )?;
            // ページ毎の閲覧記録（「よく読むページ」のスタッツ）。
            for page in 1..=(*current_page).min(3) {
                db::page_views::record_view(pool, book.database_id, "", page)?;
            }
        }

        for (database_id, page, memo) in NOTES
            .iter()
            .filter(|(database_id, _, _)| *database_id == book.database_id)
        {
            db::notes::upsert(
                pool,
                &PageNoteInput {
                    id: &format!("demo-note-{database_id}-{page}"),
                    book_id: database_id,
                    content_id: "",
                    page: *page,
                    memo,
                    // 見開きで付けた想定（開いたときに同じ側へ出す値）。
                    spread_side: Some(SpreadSide::Right),
                },
            )?;
        }
    }

    // 表紙は未ダウンロードの本にも作る（本棚カードに表紙を出す）。
    let cover = cover_png_for_seed(book)
        .ok_or_else(|| anyhow::anyhow!("サンプルの表紙を描けない: {}", book.database_id))?;
    let path = crate::views::bookshelf::cover_cache_path(
        thumbnails_dir,
        SITE_ID_TECHBOOKFEST,
        book.database_id,
    );
    std::fs::write(&path, &cover)?;
    // Drive の表紙バンドル用の共有キャッシュも作る（既存の `write_cover_cache` と同じ経路。
    // 作れなくても表示には影響しない）。
    //
    // **テストでは作らない**: ここは書いた PNG を読み直して 256px WebP に再エンコードするため
    // debug ビルドで 1 冊 150ms 前後かかり、`seed` を呼ぶテストが 1 件 10 秒以上遅くなる
    // （デモのテストは 10 件以上あるので全体で 1 分を超える）。この経路そのものは
    // `thundoku_core::thumbs` のテスト（`cache_shelf_cover_fills_the_share_table_immediately`）が
    // 押さえている。
    #[cfg(not(test))]
    if let Err(error) = thundoku_core::thumbs::cache_shelf_cover(
        pool,
        thumbnails_dir,
        SITE_ID_TECHBOOKFEST,
        book.database_id,
    ) {
        log::warn!(
            "サンプル表紙の共有キャッシュを作れない: {}: {error}",
            book.database_id
        );
    }
    Ok(())
}

/// ローカル本（`books`）と、それに紐づくもの（タグの `book_tags`・所有者・お気に入り）を書く。
///
/// [`seed`] の「ダウンロード済み」と、ダミーのダウンロードの完了（[`promote_to_downloaded`]）が
/// 共有する（どちらも同じ形で入る。片方だけ変わると「ダウンロード済み」の見た目が食い違う）。
/// `book_tags` などは `books` への外部キー（`foreign_keys(true)`）を持つので、必ず
/// `books::upsert` の後に書く。`upsert` 系だけを使うので冪等。
fn write_local_book(pool: &SqlitePool, book: &DemoBook, owner: Option<&str>) -> anyhow::Result<()> {
    // ローカル本。id は database_id と同じ（リーダーが `demo::book(&book.id)` で引く）。
    db::books::upsert(pool, &local_book(book))?;
    // upsert は owner_sub を書かない（同期が勝手に持ち主を変えないため）ので明示する。
    db::books::set_owner_sub(pool, book.database_id, owner.map(String::from))?;
    db::tags::set_for_book(
        pool,
        book.database_id,
        &book
            .tags
            .iter()
            .map(|tag| (*tag, "manual"))
            .collect::<Vec<_>>(),
    )?;
    // お気に入りの**ローカル本側**の印（本棚側は `seed_book` が立てる。取り込みでも同じ印が
    // 揃うように、ここでも `DemoBook::favorite` に従って立てる）。
    if book.favorite {
        db::books::set_favorite(pool, book.database_id, true)?;
    }
    Ok(())
}

/// 技術書典のイベント（チェックリストの親）。
fn event() -> TbfEvent {
    TbfEvent {
        id: EVENT_ID.to_string(),
        site_id: SITE_ID_TECHBOOKFEST.to_string(),
        slug: Some(EVENT_SLUG.to_string()),
        tbf_event_id: None,
        event_name: EVENT_NAME.to_string(),
        event_date: Some(EVENT_DATE.to_string()),
        event_start_date: Some(EVENT_DATE.to_string()),
        event_end_date: Some(EVENT_DATE.to_string()),
        event_format: "offline".to_string(),
        is_cancelled: 0,
        display_order: 0,
        is_featured: 0,
        // サンプルはネットワークへ出ない（技術書典からのポーリング対象にしない）
        poll_sync_enabled: 0,
        created_at: PURCHASED_AT.to_string(),
        updated_at: PURCHASED_AT.to_string(),
    }
}

/// チェックリストの項目（`checked_items`）。
fn seed_checklist(pool: &SqlitePool, owner: Option<&str>) -> anyhow::Result<()> {
    for (index, (id, circle, space, title, checked)) in CHECKLIST.iter().enumerate() {
        db::checklist::upsert_item(
            pool,
            &CheckedItem {
                id: (*id).to_string(),
                event_id: EVENT_ID.to_string(),
                circle_name: (*circle).to_string(),
                space_number: (*space).to_string(),
                memo: String::new(),
                is_checked: i64::from(*checked),
                sort_order: index as i64,
                tbf_circle_id: None,
                product_id: None,
                product_title: (*title).to_string(),
                thumbnail_url: None,
                thumbnail_data: None,
                price: Some(1000 + index as i64 * 500),
                is_purchased: i64::from(*checked),
                sample_fetch_attempted_at: None,
                created_at: PURCHASED_AT.to_string(),
            },
        )?;
    }
    db::checklist::attribute_owner(pool, EVENT_ID, owner)?;
    Ok(())
}

/// 閲覧履歴を入れる（履歴画面と本棚の閲覧統計がこれを見る）。
///
/// 日付は**起動日を基準に**組み立てる（[`VIEW_SESSIONS`] は「何日前」で持つ）。固定日付だと
/// 時間が経つほど「今日 / 今週 / 今月」の絞り込みが空になり、履歴画面が死んで見える。
/// 保存する時刻は既存の `view_history`（`start` / `end` の `CURRENT_TIMESTAMP`）と同じ
/// **UTC** に直してから入れる（`list_daily` は保存値を UTC としてローカルに直す）。
/// ここだけ SQL を直に書く（id が固定なので `ON CONFLICT(id)` で 2 回目は上書きになる）。
fn insert_view_sessions(pool: &SqlitePool) -> anyhow::Result<()> {
    let today = chrono::Local::now().date_naive();
    for (id, book_id, days_ago, start, minutes) in VIEW_SESSIONS {
        let Some((started_at, ended_at)) = session_times(today, *days_ago, start, *minutes) else {
            continue;
        };
        thundoku_core::db::block_on(async {
            sqlx::query(
                "INSERT INTO view_history (id, book_id, started_at, ended_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(id) DO UPDATE SET book_id = excluded.book_id, \
                   started_at = excluded.started_at, ended_at = excluded.ended_at",
            )
            .bind(*id)
            .bind(*book_id)
            .bind(started_at.as_str())
            .bind(ended_at.as_str())
            .execute(pool)
            .await
        })?;
    }
    Ok(())
}

/// 「何日前の HH:MM から N 分」を UTC の保存形式（`YYYY-MM-DD HH:MM:SS`）に直す。
///
/// 定義（[`VIEW_SESSIONS`]）が壊れているときは `None` を返し、その行は入れない
/// （サンプルの履歴のために起動を落とさない）。サマータイムの切り替えで存在しない時刻は
/// `earliest()` が寄せてくれる。
fn session_times(
    today: chrono::NaiveDate,
    days_ago: u32,
    start: &str,
    minutes: u32,
) -> Option<(String, String)> {
    let (hour, minute) = start.split_once(':')?;
    let date = today - chrono::Duration::days(i64::from(days_ago));
    let naive = date.and_hms_opt(hour.parse().ok()?, minute.parse().ok()?, 0)?;
    let started = naive
        .and_local_timezone(chrono::Local)
        .earliest()?
        .with_timezone(&chrono::Utc);
    let ended = started + chrono::Duration::minutes(i64::from(minutes));
    Some((
        started.format("%Y-%m-%d %H:%M:%S").to_string(),
        ended.format("%Y-%m-%d %H:%M:%S").to_string(),
    ))
}

/// 本棚（`bookshelf_items`）の行。
fn shelf_item(book: &DemoBook) -> BookshelfItem {
    BookshelfItem {
        site_id: SITE_ID_TECHBOOKFEST.to_string(),
        database_id: book.database_id.to_string(),
        title: book.title.to_string(),
        circle_name: book.circle.to_string(),
        // 技術書典の作品情報に作者名は無い（UI でも出さない。ローカル本側が持つ）
        author: String::new(),
        thumbnail_url: None,
        format: "PDF".to_string(),
        caused_at: Some(PURCHASED_AT.to_string()),
        event_name: Some(EVENT_NAME.to_string()),
        event_slug: Some(EVENT_SLUG.to_string()),
        event_id: Some(EVENT_ID.to_string()),
        file_name: Some(format!("{}.pdf", book.database_id)),
        download_url: None,
        download_options: None,
        is_downloadable: 1,
        is_checked: 1,
        is_purchased: 1,
        is_new: 0,
        is_active: 1,
        // お気に入りは upsert が上書きしないので、seed が後から `set_favorite` で立てる
        is_favorite: 0,
        is_hidden: 0,
        hidden_at: None,
        tags_json: None,
        synced_at: PURCHASED_AT.to_string(),
        created_at: PURCHASED_AT.to_string(),
        updated_at: PURCHASED_AT.to_string(),
        media_category: None,
        ai_type: None,
        // 技術書典は DRM の情報を返さない（「不明」として保存する）
        is_drm: thundoku_core::drm::DrmStatus::Unknown.as_db(),
        release_date: None,
        description: None,
        theme: None,
        maker_id: None,
        page_count: Some(i64::from(book.page_count)),
        age_rating: None,
        series_name: None,
    }
}

/// ローカル本（`books`）の行。`id` は `database_id` と同じにする（リーダーが引く）。
fn local_book(book: &DemoBook) -> Book {
    Book {
        id: book.database_id.to_string(),
        title: book.title.to_string(),
        author: book.author.to_string(),
        circle_name: book.circle.to_string(),
        purchase_date: Some(EVENT_DATE.to_string()),
        file_name: format!("{}.pdf", book.database_id),
        // 実体（pack）は無い。0 だと一覧で「空」に見えるため、ページ数から見た目の値を作る。
        file_size: i64::from(book.page_count) * 120_000,
        opfs_path: format!("demo/{}.pdf", book.database_id),
        cover_thumbnail: None,
        tbf_product_id: Some(book.database_id.to_string()),
        site_id: Some(SITE_ID_TECHBOOKFEST.to_string()),
        tags_fetched: 1,
        pack_id: None,
        // お気に入りは upsert が上書きしないので、seed が後から `set_favorite` で立てる
        is_favorite: 0,
        is_hidden: 0,
        created_at: PURCHASED_AT.to_string(),
        updated_at: PURCHASED_AT.to_string(),
        media_category: None,
        ai_type: None,
        is_drm: thundoku_core::drm::DrmStatus::Unknown.as_db(),
        release_date: None,
        description: None,
        theme: None,
        maker_id: None,
        page_count: Some(i64::from(book.page_count)),
        age_rating: None,
        series_name: None,
    }
}

/// 表紙の SVG をラスタライズして PNG バイト列にする（既存の表紙キャッシュと同じ 448px）。
fn cover_png(book: &DemoBook) -> Option<Vec<u8>> {
    let image = crate::views::bookshelf::rasterize_svg(&cover_svg(book))?;
    let size = image.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    let mut data = image.as_bytes(0)?.to_vec();
    // RenderImage は BGRA（`rasterize_svg` が R/B を入れ替えている）。PNG は RGBA なので戻す。
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let rgba = image::RgbaImage::from_raw(width, height, data)?;
    let mut out = std::io::Cursor::new(Vec::new());
    rgba.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// `seed` が表紙を書くときに使うバイト列。
///
/// 本番は毎回ラスタライズする（1 回だけなので数秒）。**テストはプロセス内で 1 度だけ
/// 描いて使い回す**: 50 枚のラスタライズは debug ビルドで 15 秒以上かかり、デモのテストが
/// 軒並み遅くなる（1 テストあたり 17〜38 秒）。表紙の見た目から作った指紋をディレクトリ名に
/// するので、テンプレート・配色・タイトルを変えたら別の場所に描き直され、古い絵は使われない。
#[cfg(not(test))]
fn cover_png_for_seed(book: &DemoBook) -> Option<Vec<u8>> {
    cover_png(book)
}

/// [`cover_png_for_seed`] のテスト版（プロセス内で 1 度だけ描く）。
#[cfg(test)]
fn cover_png_for_seed(book: &DemoBook) -> Option<Vec<u8>> {
    /// 見た目の指紋ごとに 1 度だけ 50 枚を描いて置く場所（プロセス内で共有）。
    static DIR: LazyLock<std::path::PathBuf> = LazyLock::new(|| {
        let fingerprint = hash(&BOOKS.iter().map(cover_svg).collect::<String>());
        let dir =
            std::env::temp_dir().join(format!("thundoku-shelf-demo-covers-{fingerprint:08x}"));
        let _ = std::fs::create_dir_all(&dir);
        for entry in BOOKS {
            let path = dir.join(format!("{}.png", entry.database_id));
            if path.exists() {
                continue;
            }
            if let Some(png) = cover_png(entry) {
                let _ = std::fs::write(&path, png);
            }
        }
        dir
    });
    std::fs::read(DIR.join(format!("{}.png", book.database_id))).ok()
}

/// タイトルの文字色（どのテンプレートも暗い色で描く＝淡い背景の上で読める）。
const TITLE_INK: &str = "#1f2937";
/// 作者・ページ数の文字色。
const META_INK: &str = "#374151";
/// 「サンプル」の印の文字色。
const FAINT_INK: &str = "#6b7280";

/// 表紙の SVG（テンプレートと配色は棚の中の位置から決まる）。
fn cover_svg(book: &DemoBook) -> String {
    let (accent, background) = cover_palette(book);
    cover_with_variant(book, cover_variant(book), accent, background)
}

/// 表紙のレイアウトテンプレートの数（[`cover_with_variant`] の分岐と 1 対 1）。
const COVER_VARIANTS: usize = 10;

/// 決定的なハッシュ（テンプレートと配色の選択に使う。乱数は使わない）。
fn hash(seed: &str) -> u32 {
    seed.bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(u32::from(byte))
    })
}

/// `BOOKS` の並びの中での位置（表紙の見た目をここから決める）。
fn cover_slot(book: &DemoBook) -> Option<usize> {
    BOOKS
        .iter()
        .position(|entry| entry.database_id == book.database_id)
}

/// この本に使うテンプレート（同じ本はいつも同じ表紙）。
///
/// `BOOKS` に居る本は**並び順**から決める: ハッシュだけで決めると 50 冊で
/// (テンプレート, 配色) が衝突して同じ表紙が 2 枚できる（実測: demo-09 と demo-45）。
fn cover_variant(book: &DemoBook) -> usize {
    let slot = cover_slot(book).unwrap_or_else(|| hash(book.database_id) as usize);
    slot % COVER_VARIANTS
}

/// この本に使う配色。
///
/// `variant = slot % 10` / `palette = slot * 7 % 16` は、7 が 16 と互いに素で
/// 10 と 16 の最小公倍数が 80 のため、`slot` が 80 未満（= サンプル 50 冊）では
/// **同じ組み合わせが 2 度出ない**。
fn cover_palette(book: &DemoBook) -> (&'static str, &'static str) {
    match cover_slot(book) {
        Some(slot) => PALETTE[(slot * 7) % PALETTE.len()],
        None => palette(book.database_id),
    }
}

/// 指定のテンプレートで 1 冊ぶんの表紙を描く（テストが全部のテンプレートを確かめる）。
fn cover_with_variant(book: &DemoBook, variant: usize, accent: &str, background: &str) -> String {
    match variant % COVER_VARIANTS {
        0 => cover_banner(book, accent, background),
        1 => cover_panel(book, accent, background),
        2 => cover_vertical(book, accent, background),
        3 => cover_bottom_band(book, accent, background),
        4 => cover_frame(book, accent, background),
        5 => cover_diagonal(book, accent, background),
        6 => cover_dots(book, accent, background),
        7 => cover_big_circle(book, accent, background),
        8 => cover_split(book, accent, background),
        9 => cover_slant_split(book, accent, background),
        _ => unreachable!("テンプレート番号は 0..{COVER_VARIANTS} の範囲"),
    }
}

/// 表紙・ページの SVG の外枠（テンプレートは本文だけを組み立てる）。
fn svg_document(width: u32, height: u32, body: &str) -> String {
    format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{height}' \
         viewBox='0 0 {width} {height}'>{body}</svg>"
    )
}

/// 1 行ぶんの `<text>`（`anchor` は `start` / `middle` / `end`、`weight` は `bold` か空）。
fn cover_text(
    content: &str,
    x: u32,
    y: u32,
    size: u32,
    fill: &str,
    anchor: &str,
    weight: &str,
) -> String {
    let anchor = if anchor == "start" {
        String::new()
    } else {
        format!(" text-anchor='{anchor}'")
    };
    let weight = if weight.is_empty() {
        String::new()
    } else {
        format!(" font-weight='{weight}'")
    };
    format!(
        "<text x='{x}' y='{y}' font-family='sans-serif' font-size='{size}'{weight}{anchor} \
         fill='{fill}'>{}</text>",
        escape_xml(content)
    )
}

/// タイトルを描く位置と大きさ（テンプレートごとに違う）。
struct TitleLayout {
    x: u32,
    first_y: u32,
    step: u32,
    size: u32,
    per_line: usize,
    max_lines: usize,
    anchor: &'static str,
}

/// タイトル本体の `<text>` 群（`wrap_chars` で割った行を縦に並べる）。
///
/// 文字色は [`TITLE_INK`] に固定する（テストがタイトル領域の暗い画素を数える）。
fn title_text(book: &DemoBook, layout: &TitleLayout) -> String {
    wrap_chars(book.title, layout.per_line, layout.max_lines)
        .iter()
        .enumerate()
        .map(|(index, line)| {
            cover_text(
                line,
                layout.x,
                layout.first_y + index as u32 * layout.step,
                layout.size,
                TITLE_INK,
                layout.anchor,
                "bold",
            )
        })
        .collect()
}

/// テンプレート 0: 上部の帯 + 左寄せタイトル（最初の 1 種類）。
fn cover_banner(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <rect width='{COVER_WIDTH}' height='116' fill='{accent}'/>\
         {circle}\
         <rect x='32' y='158' width='72' height='8' fill='{accent}'/>\
         {title}\
         {author}\
         {pages}\
         {sample}",
        circle = cover_text(book.circle, 32, 74, 28, "#ffffff", "start", "bold"),
        title = title_text(
            book,
            &TitleLayout {
                x: 32,
                first_y: 210,
                step: 48,
                size: 34,
                per_line: 9,
                max_lines: 3,
                anchor: "start"
            }
        ),
        author = cover_text(book.author, 32, 500, 22, META_INK, "start", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            32,
            540,
            22,
            META_INK,
            "start",
            "",
        ),
        sample = cover_text("サンプル", 32, 576, 16, FAINT_INK, "start", ""),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 1: 全面アクセント色 + 淡いパネルの中央寄せタイトル。
fn cover_panel(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{accent}'/>\
         {circle}\
         <rect x='36' y='150' width='376' height='280' rx='20' fill='{background}'/>\
         {title}\
         {author}\
         {pages}\
         {sample}",
        circle = cover_text(book.circle, 224, 78, 26, "#ffffff", "middle", "bold"),
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 228,
                step: 50,
                size: 36,
                per_line: 8,
                max_lines: 3,
                anchor: "middle"
            }
        ),
        author = cover_text(book.author, 224, 486, 22, "#ffffff", "middle", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            224,
            528,
            22,
            "#ffffff",
            "middle",
            "",
        ),
        sample = cover_text("サンプル", 224, 568, 16, "#ffffff", "middle", ""),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 2: 左の縦帯 + 縦書きタイトル（1 文字ずつ `<text>` を縦に並べる）。
///
/// usvg の `writing-mode` には頼らない（縦書きは自前で組む）。
fn cover_vertical(book: &DemoBook, accent: &str, background: &str) -> String {
    let mut circle = String::new();
    for (index, line) in wrap_chars(book.circle, 1, 7).iter().enumerate() {
        circle.push_str(&cover_text(
            line,
            40,
            74 + index as u32 * 30,
            24,
            "#ffffff",
            "start",
            "bold",
        ));
    }
    let mut title = String::new();
    // 縦書きの列は表紙の下端まで使える（作者・ページ数は右端に寄せてあるので重ならない）。
    for (index, line) in wrap_chars(book.title, 1, 14).iter().enumerate() {
        title.push_str(&cover_text(
            line,
            160,
            104 + index as u32 * 36,
            34,
            TITLE_INK,
            "start",
            "bold",
        ));
    }
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <rect width='108' height='{COVER_HEIGHT}' fill='{accent}'/>\
         {circle}{title}\
         {sample}\
         {author}\
         {pages}",
        sample = cover_text("サンプル", 436, 48, 16, FAINT_INK, "end", ""),
        author = cover_text(book.author, 436, 548, 20, META_INK, "end", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            436,
            576,
            20,
            META_INK,
            "end",
            "",
        ),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 3: 下部の大きな帯 + 中央寄せタイトル。
fn cover_bottom_band(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         {circle}\
         <rect x='188' y='108' width='72' height='6' fill='{accent}'/>\
         {title}\
         <rect y='370' width='{COVER_WIDTH}' height='227' fill='{accent}'/>\
         {author}\
         {pages}\
         {sample}",
        circle = cover_text(book.circle, 224, 86, 26, accent, "middle", "bold"),
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 170,
                step: 50,
                size: 36,
                per_line: 8,
                max_lines: 3,
                anchor: "middle"
            }
        ),
        author = cover_text(book.author, 224, 452, 24, "#ffffff", "middle", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            224,
            498,
            24,
            "#ffffff",
            "middle",
            "",
        ),
        sample = cover_text("サンプル", 224, 556, 16, "#ffffff", "middle", ""),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 4: 二重の枠線フレーム + 中央寄せタイトル。
fn cover_frame(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <rect x='14' y='14' width='420' height='569' fill='none' stroke='{accent}' \
         stroke-width='6'/>\
         <rect x='30' y='30' width='388' height='537' fill='none' stroke='{accent}' \
         stroke-width='2'/>\
         {circle}\
         {title}\
         <rect x='164' y='382' width='120' height='3' fill='{accent}'/>\
         {author}\
         {pages}\
         {sample}",
        circle = cover_text(book.circle, 224, 92, 24, accent, "middle", "bold"),
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 248,
                step: 48,
                size: 34,
                per_line: 8,
                max_lines: 3,
                anchor: "middle"
            }
        ),
        author = cover_text(book.author, 224, 446, 22, META_INK, "middle", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            224,
            486,
            22,
            META_INK,
            "middle",
            "",
        ),
        sample = cover_text("サンプル", 224, 540, 16, FAINT_INK, "middle", ""),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 5: 斜めのバンド + タイトル（サークル名はバンドの上に白抜き）。
fn cover_diagonal(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <polygon points='0,330 448,190 448,258 0,398' fill='{accent}'/>\
         <polygon points='0,424 448,284 448,300 0,440' fill='{accent}' fill-opacity='0.55'/>\
         {title}\
         {circle}\
         {author}\
         {pages}\
         {sample}",
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 100,
                step: 46,
                size: 32,
                per_line: 9,
                max_lines: 3,
                anchor: "middle"
            }
        ),
        circle = cover_text(book.circle, 32, 360, 24, "#ffffff", "start", "bold"),
        author = cover_text(book.author, 416, 520, 22, META_INK, "end", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            416,
            552,
            22,
            META_INK,
            "end",
            "",
        ),
        sample = cover_text("サンプル", 416, 582, 16, FAINT_INK, "end", ""),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 6: ドットのパターン背景 + 淡いパネルの中央寄せタイトル。
fn cover_dots(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<defs><pattern id='dots' width='36' height='36' patternUnits='userSpaceOnUse'>\
         <circle cx='18' cy='18' r='4' fill='{accent}' fill-opacity='0.3'/></pattern></defs>\
         <rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='url(#dots)'/>\
         {circle}\
         <rect x='28' y='140' width='392' height='260' rx='12' fill='#ffffff' \
         fill-opacity='0.94'/>\
         {title}\
         {author}\
         {pages}\
         {sample}",
        circle = cover_text(book.circle, 224, 76, 26, accent, "middle", "bold"),
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 218,
                step: 50,
                size: 36,
                per_line: 9,
                max_lines: 3,
                anchor: "middle"
            }
        ),
        author = cover_text(book.author, 224, 486, 22, META_INK, "middle", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            224,
            526,
            22,
            META_INK,
            "middle",
            "",
        ),
        sample = cover_text("サンプル", 224, 570, 16, FAINT_INK, "middle", ""),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 7: 大きな円モチーフ（輪の中にサークル名）+ 下にタイトル。
fn cover_big_circle(book: &DemoBook, accent: &str, background: &str) -> String {
    let mut circle = String::new();
    for (index, line) in wrap_chars(book.circle, 4, 2).iter().enumerate() {
        circle.push_str(&cover_text(
            line,
            224,
            196 + index as u32 * 40,
            26,
            TITLE_INK,
            "middle",
            "bold",
        ));
    }
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <circle cx='224' cy='212' r='150' fill='{accent}'/>\
         <circle cx='224' cy='212' r='104' fill='{background}'/>\
         {circle}\
         {title}\
         {author}\
         {pages}\
         {sample}",
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 430,
                step: 46,
                size: 32,
                per_line: 9,
                max_lines: 2,
                anchor: "middle"
            }
        ),
        author = cover_text(book.author, 224, 532, 20, META_INK, "middle", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            224,
            562,
            20,
            META_INK,
            "middle",
            "",
        ),
        sample = cover_text("サンプル", 224, 590, 14, FAINT_INK, "middle", ""),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 8: 上半分をアクセントで塗り、下に中央寄せタイトル。
fn cover_split(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <rect width='{COVER_WIDTH}' height='300' fill='{accent}'/>\
         {circle}\
         <rect x='174' y='152' width='100' height='4' fill='#ffffff' fill-opacity='0.75'/>\
         {sample}\
         {title}\
         {author}\
         {pages}",
        circle = cover_text(book.circle, 224, 128, 28, "#ffffff", "middle", "bold"),
        sample = cover_text("サンプル", 416, 276, 16, "#ffffff", "end", ""),
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 374,
                step: 48,
                size: 34,
                per_line: 8,
                max_lines: 3,
                anchor: "middle"
            }
        ),
        author = cover_text(book.author, 224, 526, 20, META_INK, "middle", ""),
        pages = cover_text(
            &format!("{} ページ", book.page_count),
            224,
            558,
            20,
            META_INK,
            "middle",
            "",
        ),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// テンプレート 9: 斜めに切ったアクセント帯 + 下に中央寄せタイトル。
fn cover_slant_split(book: &DemoBook, accent: &str, background: &str) -> String {
    let body = format!(
        "<rect width='{COVER_WIDTH}' height='{COVER_HEIGHT}' fill='{background}'/>\
         <polygon points='0,0 448,0 448,110 0,300' fill='{accent}'/>\
         {circle}\
         {sample}\
         {title}\
         {meta}",
        circle = cover_text(book.circle, 32, 84, 26, "#ffffff", "start", "bold"),
        sample = cover_text("サンプル", 416, 84, 16, "#ffffff", "end", ""),
        title = title_text(
            book,
            &TitleLayout {
                x: 224,
                first_y: 392,
                step: 48,
                size: 34,
                per_line: 8,
                max_lines: 3,
                anchor: "middle"
            }
        ),
        meta = cover_text(
            &format!("{} / {} ページ", book.author, book.page_count),
            224,
            548,
            22,
            META_INK,
            "middle",
            "",
        ),
    );
    svg_document(COVER_WIDTH, COVER_HEIGHT, &body)
}

/// サンプルページの SVG（「サンプル / {タイトル} / {サークル} / p.{n}」）。
fn page_svg(book: &DemoBook, page: u32) -> String {
    let (accent, _) = palette(book.database_id);
    let title: String = wrap_chars(book.title, 20, 2)
        .iter()
        .enumerate()
        .map(|(index, line)| {
            format!(
                "<text x='80' y='{}' font-family='sans-serif' font-size='52' font-weight='bold' \
                 fill='#1f2937'>{}</text>",
                300 + index * 70,
                escape_xml(line)
            )
        })
        .collect();
    format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='{PAGE_WIDTH}' height='{PAGE_HEIGHT}' \
         viewBox='0 0 {PAGE_WIDTH} {PAGE_HEIGHT}'>\
         <rect width='{PAGE_WIDTH}' height='{PAGE_HEIGHT}' fill='#ffffff'/>\
         <rect width='{PAGE_WIDTH}' height='140' fill='{accent}'/>\
         <text x='80' y='92' font-family='sans-serif' font-size='44' font-weight='bold' \
         fill='#ffffff'>サンプル</text>\
         <text x='1160' y='92' text-anchor='end' font-family='sans-serif' font-size='34' \
         fill='#ffffff'>{circle}</text>\
         {title}\
         <text x='80' y='470' font-family='sans-serif' font-size='34' \
         fill='#4b5563'>{author}</text>\
         <text x='620' y='1000' text-anchor='middle' font-family='sans-serif' font-size='220' \
         font-weight='bold' fill='#111827'>p.{page}</text>\
         <text x='620' y='1700' text-anchor='middle' font-family='sans-serif' font-size='36' \
         fill='#9ca3af'>{title_text} / {circle_text} / p.{page}</text>\
         </svg>",
        circle = escape_xml(book.circle),
        author = escape_xml(book.author),
        title_text = escape_xml(book.title),
        circle_text = escape_xml(book.circle),
    )
}

/// 表紙の配色（アクセント, 背景）の一覧。
///
/// タイトルは [`TITLE_INK`] の暗い色で描くので、背景はどれも淡い色にし、アクセントは
/// 白文字が読める濃さにする。アクセントは「全チャンネルが 100 未満」の色を避ける
/// （テストがタイトル領域の暗い画素を数えるとき、図形を文字と取り違えないように）。
const PALETTE: &[(&str, &str)] = &[
    ("#4338ca", "#eef2ff"),
    ("#be185d", "#fdf2f8"),
    ("#0f766e", "#f0fdfa"),
    ("#b45309", "#fffbeb"),
    ("#6d28d9", "#f5f3ff"),
    ("#0e7490", "#ecfeff"),
    ("#b91c1c", "#fef2f2"),
    ("#15803d", "#f0fdf4"),
    ("#7c2d12", "#fff7ed"),
    ("#1d4ed8", "#eff6ff"),
    ("#a21caf", "#fdf4ff"),
    ("#4d7c0f", "#f7fee7"),
    ("#9d174d", "#fce7f3"),
    ("#1e40af", "#dbeafe"),
    ("#047857", "#ecfdf5"),
    ("#78350f", "#fef3c7"),
];

/// ハッシュから配色を選ぶ（表紙以外（ページ SVG など）で使う）。
fn palette(seed: &str) -> (&'static str, &'static str) {
    PALETTE[(hash(seed) % PALETTE.len() as u32) as usize]
}

/// SVG の `<text>` は自動で折り返さないため、N 文字ずつに割る
/// （日本語のタイトルは空白が無いので文字数で切る）。
fn wrap_chars(text: &str, per_line: usize, max_lines: usize) -> Vec<String> {
    let per_line = per_line.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut lines: Vec<String> = chars
        .chunks(per_line)
        .take(max_lines)
        .map(|chunk| chunk.iter().collect())
        .collect();
    // 入りきらないぶんは末尾に記号を付けて「続きがある」ことを示す。
    if chars.len() > per_line * max_lines
        && let Some(last) = lines.last_mut()
    {
        last.push('…');
    }
    lines
}

/// SVG に埋め込む前に実体参照へ置き換える（`&` は最初に処理する）。
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

/// PageLoader 実装（サンプルページを SVG からその場で作る）。
///
/// `ImageViewer` 側は無改修で使える（[`crate::components::image_viewer::PageLoader`]）。
pub struct DemoPageLoader {
    book: &'static DemoBook,
}

impl DemoPageLoader {
    /// `database_id`（= `books.id`）で本を引く。サンプルに無ければ `None`。
    pub fn new(database_id: &str) -> Option<Self> {
        Some(Self {
            book: book(database_id)?,
        })
    }
}

impl crate::components::image_viewer::PageLoader for DemoPageLoader {
    fn page_count(&self) -> usize {
        self.book.page_count as usize
    }

    fn page_size(&self, index: usize) -> Option<(u32, u32)> {
        (index < self.page_count()).then_some((PAGE_WIDTH, PAGE_HEIGHT))
    }

    /// ページ画像を作る（背景スレッドから呼ばれる）。毎回 SVG から描く
    /// （サンプルのページは 1 枚ずつ作り、ディスクには残さない）。
    fn load(&self, index: usize) -> Result<Arc<RenderImage>, String> {
        if index >= self.page_count() {
            return Err(format!("サンプルのページ番号が範囲外です: {index}"));
        }
        crate::views::bookshelf::rasterize_svg(&page_svg(self.book, index as u32 + 1))
            .ok_or_else(|| format!("サンプルページを描画できません: {}", self.book.database_id))
    }

    /// サムネイル・表示用とも [`Self::load`] に委譲する（サンプルページは
    /// 要求されたときに作るだけで、別の経路を持たない）。
    fn load_thumb(&self, index: usize) -> Result<Arc<RenderImage>, String> {
        self.load(index)
    }

    fn load_for_display(
        &self,
        index: usize,
        _target_width: f32,
    ) -> Result<Arc<RenderImage>, String> {
        self.load(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::image_viewer::PageLoader as _;
    use thundoku_core::db;
    use thundoku_core::tbf::SITE_ID_TECHBOOKFEST;

    /// 表紙の書き出し先（テストごとに作り直す。テストは並列に走るので名前を分ける）。
    fn thumbnails_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "thundoku-shelf-demo-covers-{tag}-{}",
            std::process::id()
        ))
    }

    /// 行数の内訳（seed を 2 回呼んでも増えないことの比較に使う）。
    fn row_counts(pool: &SqlitePool) -> Vec<usize> {
        let shelf = db::bookshelf::list(pool, SITE_ID_TECHBOOKFEST).unwrap();
        let local = db::books::list(pool).unwrap();
        // お気に入り（本棚とローカル本の両方に立つ。2 回目でも消えないことの比較に使う）
        let favorite_shelf = shelf.iter().filter(|item| item.is_favorite != 0).count();
        let favorite_local = local.iter().filter(|book| book.is_favorite != 0).count();
        let tags: usize = db::tags::tag_names_by_book(pool)
            .unwrap()
            .values()
            .map(Vec::len)
            .sum();
        let progress: usize = BOOKS
            .iter()
            .filter(|book| {
                let row = db::progress::get(pool, book.database_id).unwrap();
                ReadingState::from_progress(row.as_ref()) != ReadingState::Unread
            })
            .count();
        let views: usize = BOOKS
            .iter()
            .map(|book| {
                db::page_views::for_book(pool, book.database_id)
                    .unwrap()
                    .len()
            })
            .sum();
        let notes: usize = BOOKS
            .iter()
            .map(|book| {
                db::notes::noted_pages(pool, book.database_id, "")
                    .unwrap()
                    .len()
            })
            .sum();
        let history = db::view_history::list_daily(pool).unwrap().len();
        let events = db::checklist::list_events(pool).unwrap();
        let checklist: usize = events
            .iter()
            .map(|event| db::checklist::list_items(pool, &event.id).unwrap().len())
            .sum();
        let circles = db::favorites::list_favorites(pool, db::favorites::EntityKind::Circle)
            .unwrap()
            .len();
        vec![
            shelf.len(),
            local.len(),
            favorite_shelf,
            favorite_local,
            tags,
            progress,
            views,
            notes,
            history,
            events.len(),
            checklist,
            circles,
        ]
    }

    /// ダウンロード済み（ローカル本を持つ）の冊数。
    fn downloaded_count() -> usize {
        BOOKS.iter().filter(|book| book.downloaded).count()
    }

    /// 未ダウンロード（本棚と表紙だけ）の id。
    fn undownloaded_ids() -> Vec<&'static str> {
        BOOKS
            .iter()
            .filter(|book| !book.downloaded)
            .map(|book| book.database_id)
            .collect()
    }

    #[test]
    fn enabled_only_with_the_flag_or_env() {
        let args = |extra: &[&str]| -> Vec<String> {
            std::iter::once("thundoku-shelf".to_string())
                .chain(extra.iter().map(|arg| arg.to_string()))
                .collect()
        };
        assert!(
            enabled(&args(&["--demo"]), None),
            "起動フラグで有効にならない"
        );
        assert!(enabled(&args(&[]), Some("1")), "環境変数で有効にならない");
        assert!(
            enabled(&args(&["--demo"]), Some("0")),
            "フラグがあれば環境変数の値は見ない"
        );
        assert!(!enabled(&args(&[]), None), "指定が無いのに有効になっている");
        assert!(
            !enabled(&args(&[]), Some("0")),
            "環境変数 0 で有効になっている"
        );
        assert!(
            !enabled(&args(&["--other"]), Some("")),
            "無関係な指定で有効になっている"
        );
    }

    /// サンプルの定義そのものの健全性（id の重複・引き当て）。
    #[test]
    fn books_are_looked_up_by_database_id() {
        assert_eq!(BOOKS.len(), 50, "サンプルの冊数が変わっている");
        let mut ids: Vec<&str> = BOOKS.iter().map(|book| book.database_id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), BOOKS.len(), "database_id が重複している");
        assert!(
            BOOKS.iter().all(|book| book.page_count > 0),
            "ページ数が 0 の本がある"
        );
        assert_eq!(book("demo-01").map(|book| book.title), Some(BOOKS[0].title));
        assert!(book("demo-99").is_none(), "無い id で本が引けている");
    }

    /// 未読以外の本には進捗の固定値があること（seed の前提）。
    #[test]
    fn progress_table_matches_the_reading_states() {
        for book in BOOKS {
            let has_progress = PROGRESS
                .iter()
                .any(|(database_id, _, _)| *database_id == book.database_id);
            assert_eq!(
                has_progress,
                book.reading_state != ReadingState::Unread,
                "進捗の固定値と読書状態が食い違っている: {}",
                book.database_id
            );
        }
    }

    /// seed が本棚・ローカル本・タグ・進捗・履歴・付箋・チェックリストを埋めること。
    #[test]
    fn seed_fills_the_demo_data() {
        let pool = db::test_pool();
        let dir = thumbnails_dir("fills");
        let _ = std::fs::remove_dir_all(&dir);
        seed(&pool, &dir).expect("seed できる");

        let shelf = db::bookshelf::list(&pool, SITE_ID_TECHBOOKFEST).unwrap();
        assert_eq!(shelf.len(), BOOKS.len(), "本棚のサンプルが揃っていない");
        assert_eq!(
            db::books::list(&pool).unwrap().len(),
            downloaded_count(),
            "ローカル本（ダウンロード済み）の冊数が違う"
        );
        assert!(
            db::bookshelf::list(&pool, "booth").unwrap().is_empty(),
            "サンプルは技術書典だけ（他サイトの行を作っている）"
        );

        // タグは本棚（tags_json）とローカル本（book_tags）の両方に入る
        let tags = db::tags::tag_names_by_book(&pool).unwrap();
        assert_eq!(
            tags.get("demo-01").map(Vec::len),
            Some(BOOKS[0].tags.len()),
            "ローカル本のタグが入っていない"
        );
        assert!(
            !db::bookshelf::tags_of(&shelf[0]).is_empty(),
            "本棚側のタグ（tags_json）が入っていない"
        );

        // 読書状態の内訳（未読 / 読書中 / 読了 を混ぜる）
        let states: Vec<ReadingState> = BOOKS
            .iter()
            .map(|book| {
                let row = db::progress::get(&pool, book.database_id).unwrap();
                ReadingState::from_progress(row.as_ref())
            })
            .collect();
        for (index, book) in BOOKS.iter().enumerate() {
            assert_eq!(
                states[index], book.reading_state,
                "読書状態が定義と違う: {}",
                book.database_id
            );
        }
        assert!(
            states.contains(&ReadingState::Unread)
                && states.contains(&ReadingState::Reading)
                && states.contains(&ReadingState::Read),
            "未読 / 読書中 / 読了 が混ざっていない"
        );

        // ページ毎の閲覧記録と閲覧履歴（日付は固定値）
        assert!(
            !db::page_views::for_book(&pool, "demo-01")
                .unwrap()
                .is_empty(),
            "ページ毎の閲覧記録が無い"
        );
        let days = db::view_history::list_daily(&pool).unwrap();
        assert!(!days.is_empty(), "閲覧履歴が無い（履歴画面が空になる）");
        assert_eq!(
            days[0].day,
            chrono::Local::now().format("%Y-%m-%d").to_string(),
            "閲覧履歴の先頭が今日でない: {:?}",
            days.first()
        );

        // 付箋（暗号化して保存される）
        let note = db::notes::get_for_page(&pool, "demo-01", "", 5)
            .unwrap()
            .expect("付箋が入っていない");
        assert!(!note.memo.is_empty(), "付箋のメモが空");
        assert!(note.is_active, "付箋が外れた状態で入っている");

        // 技術書典のイベントとチェックリスト
        let events = db::checklist::list_events(&pool).unwrap();
        assert_eq!(events.len(), 1, "イベントの数が違う");
        assert_eq!(events[0].site_id, SITE_ID_TECHBOOKFEST);
        let items = db::checklist::list_items(&pool, &events[0].id).unwrap();
        assert!(items.len() >= 3, "チェックリストの項目が少ない");
        assert!(
            items.iter().any(|item| item.is_checked != 0),
            "チェック済みの項目が無い（チェックリスト画面が全部未チェックになる）"
        );

        // お気に入り（サークル / 作者）
        assert!(
            !db::favorites::list_favorites(&pool, db::favorites::EntityKind::Circle)
                .unwrap()
                .is_empty(),
            "お気に入りのサークルが無い"
        );

        // お気に入りの本（本棚とローカル本の両方に立つ。upsert で消えないこと）
        let favorites: Vec<&str> = BOOKS
            .iter()
            .filter(|book| book.favorite)
            .map(|book| book.database_id)
            .collect();
        assert_eq!(favorites.len(), 10, "サンプルのお気に入りが 10 冊でない");
        assert_eq!(
            shelf.iter().filter(|item| item.is_favorite != 0).count(),
            10,
            "本棚の is_favorite が立っていない"
        );
        // ローカル本の印は**ダウンロード済み**のお気に入りだけに立つ
        // （未ダウンロードの本はまだローカル本を持たない）
        assert_eq!(
            db::books::list(&pool)
                .unwrap()
                .iter()
                .filter(|book| book.is_favorite != 0)
                .count(),
            favorites
                .iter()
                .filter(|database_id| book(database_id).is_some_and(|book| book.downloaded))
                .count(),
            "ローカル本の is_favorite が立っていない"
        );
        for database_id in &favorites {
            let item = shelf
                .iter()
                .find(|item| item.database_id == *database_id)
                .unwrap_or_else(|| panic!("お気に入りの本が本棚に無い: {database_id}"));
            assert_eq!(
                item.is_favorite, 1,
                "本棚のお気に入りが立っていない: {database_id}"
            );
        }

        // 所有者はデモの Google アカウント（本棚の所有者フィルタを通す）。
        // 未ダウンロードの本はローカル本が無いので所有者も無い（取り込み時に付く）。
        let key = thundoku_core::secrets::SecretStore::new().db_key().unwrap();
        for book in BOOKS {
            let owner = db::books::get_owner_sub(&pool, book.database_id).unwrap();
            if book.downloaded {
                assert!(
                    thundoku_core::owner::matches(&key, owner.as_deref(), Some(DEMO_OWNER_SUB)),
                    "サンプルの本がデモのアカウントに帰属していない: {}",
                    book.database_id
                );
            } else {
                assert!(
                    owner.is_none(),
                    "未ダウンロードの本に所有者が付いている: {}",
                    book.database_id
                );
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2 回 seed しても行が増えないこと（起動のたびに同じ画面になる）。
    #[test]
    fn seed_is_idempotent() {
        let pool = db::test_pool();
        let dir = thumbnails_dir("idempotent");
        let _ = std::fs::remove_dir_all(&dir);
        seed(&pool, &dir).expect("seed できる");
        let first = row_counts(&pool);
        assert!(
            first.iter().all(|count| *count > 0),
            "1 回目で空: {first:?}"
        );

        seed(&pool, &dir).expect("2 回目も seed できる");
        assert_eq!(first, row_counts(&pool), "2 回目で行が増えている");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未ダウンロードの本は本棚にだけ入り、ローカル本・進捗・履歴・付箋を持たないこと。
    ///
    /// 本棚の「未ダウンロード」表示はローカル本（`books` 行）の有無で決まる
    /// （`BookshelfView` の `local`）ので、`books` 行を入れないことがそのまま状態の再現になる。
    /// 逆に `reading_progress` / `page_views` / `view_history` / `book_tags` は `books` への
    /// 外部キーを持つため、未ダウンロードの本には入れられない（入れると seed が失敗する）。
    #[test]
    fn seed_leaves_twelve_books_undownloaded() {
        let pool = db::test_pool();
        let dir = thumbnails_dir("undownloaded");
        let _ = std::fs::remove_dir_all(&dir);
        seed(&pool, &dir).expect("seed できる");

        let undownloaded = undownloaded_ids();
        assert_eq!(
            undownloaded,
            vec![
                "demo-13", "demo-15", "demo-18", "demo-22", "demo-24", "demo-27", "demo-31",
                "demo-33", "demo-39", "demo-41", "demo-45", "demo-50",
            ],
            "未ダウンロードのサンプルが変わっている"
        );
        assert_eq!(downloaded_count(), BOOKS.len() - undownloaded.len());

        let shelf = db::bookshelf::list(&pool, SITE_ID_TECHBOOKFEST).unwrap();
        assert_eq!(shelf.len(), BOOKS.len(), "本棚は全冊入る");
        let local = db::books::list(&pool).unwrap();
        assert_eq!(local.len(), downloaded_count(), "ローカル本の数が違う");
        let tags = db::tags::tag_names_by_book(&pool).unwrap();

        for book in BOOKS {
            let has_local = local.iter().any(|row| row.id == book.database_id);
            assert_eq!(
                has_local, book.downloaded,
                "ダウンロード済みの印とローカル本の有無が食い違っている: {}",
                book.database_id
            );
            // 未ダウンロードの本も本棚カードには出る（タグと表紙 PNG も今までどおり付く）
            let item = shelf
                .iter()
                .find(|item| item.database_id == book.database_id)
                .unwrap_or_else(|| panic!("本棚に無い: {}", book.database_id));
            assert!(
                !db::bookshelf::tags_of(item).is_empty(),
                "本棚側のタグが無い: {}",
                book.database_id
            );
            let cover = crate::views::bookshelf::cover_cache_path(
                &dir,
                SITE_ID_TECHBOOKFEST,
                book.database_id,
            );
            assert!(cover.exists(), "表紙が無い: {}", cover.display());
            if book.downloaded {
                continue;
            }
            // ここから未ダウンロードの本だけ
            assert_eq!(
                book.reading_state,
                ReadingState::Unread,
                "未ダウンロードの本が未読でない: {}",
                book.database_id
            );
            assert!(
                db::progress::get(&pool, book.database_id)
                    .unwrap()
                    .is_none(),
                "未ダウンロードの本に進捗がある: {}",
                book.database_id
            );
            assert!(
                db::page_views::for_book(&pool, book.database_id)
                    .unwrap()
                    .is_empty(),
                "未ダウンロードの本にページ毎の閲覧記録がある: {}",
                book.database_id
            );
            assert_eq!(
                db::view_history::view_count(&pool, book.database_id).unwrap(),
                0,
                "未ダウンロードの本に閲覧履歴がある: {}",
                book.database_id
            );
            assert!(
                db::notes::noted_pages(&pool, book.database_id, "")
                    .unwrap()
                    .is_empty(),
                "未ダウンロードの本に付箋がある: {}",
                book.database_id
            );
            assert!(
                !tags.contains_key(book.database_id),
                "未ダウンロードの本にローカルタグがある: {}",
                book.database_id
            );
        }

        // 2 回 seed してもローカル本は増えない（起動のたびに同じ画面になる）
        seed(&pool, &dir).expect("2 回目も seed できる");
        assert_eq!(
            db::books::list(&pool).unwrap().len(),
            downloaded_count(),
            "2 回目でローカル本の数が変わっている"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ダミーダウンロードの完了（[`promote_to_downloaded`]）: 未ダウンロードの本を
    /// 「ダウンロード済み」と同じ形（ローカル本 + タグ + 所有者）にする。
    /// 2 回呼んでも増えず、知らない id では何もしない。
    #[test]
    fn promote_to_downloaded_writes_the_local_book_once() {
        let pool = db::test_pool();
        let dir = thumbnails_dir("promote");
        let _ = std::fs::remove_dir_all(&dir);
        seed(&pool, &dir).expect("seed できる");

        // お気に入りの未ダウンロード本（本棚の印とローカル本の印が揃うことまで見る）
        let id = "demo-13";
        let sample = book(id).expect("サンプルを引ける");
        assert!(!sample.downloaded, "demo-13 は未ダウンロードのはず");
        assert!(sample.favorite, "demo-13 はお気に入りのはず");
        assert!(
            db::books::get(&pool, id).unwrap().is_none(),
            "もうローカル本がある"
        );

        promote_to_downloaded(&pool, id).expect("取り込める");
        let local = db::books::get(&pool, id)
            .unwrap()
            .expect("ローカル本ができていない");
        assert_eq!(local.title, sample.title);
        assert_eq!(local.circle_name, sample.circle);
        assert_eq!(local.author, sample.author);
        assert_eq!(local.tbf_product_id.as_deref(), Some(id));
        assert_eq!(local.page_count, Some(i64::from(sample.page_count)));
        assert_eq!(
            local.is_favorite, 1,
            "お気に入りの印がローカル本に付いていない"
        );
        assert_eq!(
            db::tags::tag_names_by_book(&pool)
                .unwrap()
                .get(id)
                .map(Vec::len),
            Some(sample.tags.len()),
            "ローカルタグが入っていない"
        );
        let key = thundoku_core::secrets::SecretStore::new().db_key().unwrap();
        assert!(
            thundoku_core::owner::matches(
                &key,
                db::books::get_owner_sub(&pool, id).unwrap().as_deref(),
                Some(DEMO_OWNER_SUB),
            ),
            "所有者がデモのアカウントになっていない"
        );

        // 2 回目でも行は増えない（ダウンロードの押し直し・再起動でも同じ結果）
        let before = row_counts(&pool);
        promote_to_downloaded(&pool, id).expect("2 回目も取り込める");
        assert_eq!(before, row_counts(&pool), "2 回目で行が増えている");

        // 知らない id は何もしない（サンプル以外の id でも落とさない）
        promote_to_downloaded(&pool, "demo-99").expect("無いサンプルでも Ok");
        promote_to_downloaded(&pool, "not-a-sample").expect("サンプル以外でも Ok");
        assert_eq!(before, row_counts(&pool), "知らない id で行が変わっている");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配色が 12 組以上あり、50 冊がその多くを使い分けていること。
    #[test]
    fn covers_use_at_least_twelve_palettes() {
        let mut used: Vec<(&str, &str)> = BOOKS.iter().map(cover_palette).collect();
        used.sort_unstable();
        used.dedup();
        assert!(used.len() >= 12, "配色が 12 組未満: {}", used.len());
    }

    /// 50 冊の表紙が (テンプレート, 配色) で重複しないこと。
    ///
    /// ハッシュだけで決めると同じ見た目の表紙が 2 枚できて「全部同じに見える」原因になる
    /// （実測: demo-09 と demo-45 が同一スタイルだった）。並び順から決めると 50 冊では
    /// 衝突しない。
    #[test]
    fn no_two_covers_share_the_same_style() {
        let mut styles: Vec<(usize, &str)> = BOOKS
            .iter()
            .map(|book| (cover_variant(book), cover_palette(book).0))
            .collect();
        let total = styles.len();
        styles.sort_unstable();
        styles.dedup();
        assert_eq!(styles.len(), total, "表紙のスタイルが重複している");
    }

    /// 閲覧履歴が「今日」から始まり、5 日以上に散っていること。
    ///
    /// 履歴画面の既定（全項目）だけでなく「今日 / 今週 / 今月」の絞り込みでも中身が出る
    /// ように、日付は起動日を基準に組み立てる。
    #[test]
    fn view_history_starts_today_and_spreads_over_days() {
        let pool = db::test_pool();
        let dir = thumbnails_dir("history");
        let _ = std::fs::remove_dir_all(&dir);
        seed(&pool, &dir).expect("seed できる");

        let days = db::view_history::list_daily(&pool).unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert!(!days.is_empty(), "閲覧履歴が無い（履歴画面が空になる）");
        assert_eq!(
            days[0].day,
            today,
            "履歴の先頭が今日でない: {:?}",
            days.first()
        );
        assert!(days.len() >= 5, "履歴の日数が少ない: {}", days.len());
        assert!(
            days.iter().any(|day| day.day == today),
            "今日の履歴が無い（「今日」の絞り込みが空になる）"
        );

        let sessions: i64 = BOOKS
            .iter()
            .map(|book| db::view_history::view_count(&pool, book.database_id).unwrap())
            .sum();
        assert!(
            (12..=18).contains(&sessions),
            "履歴のセッション数が範囲外: {sessions}"
        );

        // 2 回 seed しても行は増えない（id は固定、日付は同じ日に揃う）
        seed(&pool, &dir).expect("2 回目も seed できる");
        let again: i64 = BOOKS
            .iter()
            .map(|book| db::view_history::view_count(&pool, book.database_id).unwrap())
            .sum();
        assert_eq!(sessions, again, "2 回目で履歴の行が増えている");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `HH:MM` を 0 時からの分に直す（定義の時間帯チェック用）。
    fn hhmm_minutes(text: &str) -> u32 {
        let (hour, minute) = text.split_once(':').expect("HH:MM の形");
        hour.parse::<u32>().expect("時") * 60 + minute.parse::<u32>().expect("分")
    }

    /// 履歴の定義そのもの（件数・日数・本の数・時間帯）の健全性。
    #[test]
    fn view_sessions_spread_over_recent_evenings() {
        assert!(
            (12..=18).contains(&VIEW_SESSIONS.len()),
            "セッション数が範囲外: {}",
            VIEW_SESSIONS.len()
        );

        let mut days: Vec<u32> = VIEW_SESSIONS.iter().map(|session| session.2).collect();
        days.sort_unstable();
        days.dedup();
        assert!((5..=7).contains(&days.len()), "日数が範囲外: {days:?}");
        assert_eq!(days.first().copied(), Some(0), "今日の履歴が無い");
        assert_eq!(days.last().copied(), Some(6), "最も古い日が 6 日前でない");

        let mut books: Vec<&str> = VIEW_SESSIONS.iter().map(|session| session.1).collect();
        books.sort_unstable();
        books.dedup();
        assert!(
            (10..=14).contains(&books.len()),
            "履歴に出す本の数が範囲外: {}",
            books.len()
        );

        let mut ids: Vec<&str> = VIEW_SESSIONS.iter().map(|session| session.0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), VIEW_SESSIONS.len(), "履歴の id が重複している");

        for (_, book_id, _, start, minutes) in VIEW_SESSIONS {
            assert!(
                BOOKS.iter().any(|book| book.database_id == *book_id),
                "サンプルに無い本の履歴: {book_id}"
            );
            let start_minutes = hhmm_minutes(start);
            assert!(
                (19 * 60..24 * 60).contains(&start_minutes),
                "開始時刻が夜でない: {start}"
            );
            assert!((15..=90).contains(minutes), "長さが範囲外: {minutes} 分");
            assert!(
                start_minutes + minutes <= 23 * 60 + 30,
                "終了が 23:30 を超える: {start} から {minutes} 分"
            );
        }
    }

    /// 各テンプレートがタイトルを描く領域 `(x, y, 幅, 高さ)`。
    ///
    /// テンプレートの座標と 1 対 1 に対応させる（タイトル以外の文字が入らない狭さにする）。
    const TITLE_AREAS: [(u32, u32, u32, u32); COVER_VARIANTS] = [
        (0, 180, 448, 160),  // 0: 上部帯 + 左寄せタイトル
        (36, 190, 376, 190), // 1: 全面色 + パネル中央
        (140, 80, 140, 440), // 2: 左縦帯 + 縦書きタイトル
        (0, 120, 448, 200),  // 3: 下部の帯 + 中央
        (30, 212, 388, 160), // 4: 二重の枠 + 中央
        (0, 64, 448, 140),   // 5: 斜めのバンド
        (28, 182, 392, 190), // 6: ドット + パネル中央
        (0, 396, 448, 90),   // 7: 大きな円 + タイトル
        (0, 336, 448, 150),  // 8: 上半分 + 中央
        (0, 352, 448, 150),  // 9: 斜めスプリット
    ];

    /// 指定した領域にある「暗い画素」（全チャンネル 100 未満）の数。
    fn count_dark_in(rgba: &image::RgbaImage, x: u32, y: u32, width: u32, height: u32) -> usize {
        rgba.enumerate_pixels()
            .filter(|(px, py, pixel)| {
                (x..x + width).contains(px)
                    && (y..y + height).contains(py)
                    && pixel.0[..3].iter().all(|channel| *channel < 100)
            })
            .count()
    }

    /// 1 冊ぶんの表紙を PNG から RGBA にする。
    fn cover_pixels(book: &DemoBook) -> image::RgbaImage {
        let bytes = cover_png(book).expect("表紙を描画できる");
        image::load_from_memory(&bytes)
            .expect("PNG として読める")
            .to_rgba8()
    }

    /// SVG のテキストノードを順につなぐ（折り返し・縦書きでも全文が復元できる）。
    fn texts_of(svg: &str) -> String {
        let mut out = String::new();
        let mut rest = svg;
        while let Some(open) = rest.find('>') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('<') else {
                break;
            };
            out.push_str(&rest[..close]);
            rest = &rest[close..];
        }
        out
    }

    /// 表紙のテンプレートが 8 種類以上あり、サンプルの 50 冊で全部使われていること。
    #[test]
    fn cover_templates_all_appear_in_the_shelf() {
        const {
            assert!(COVER_VARIANTS >= 8, "テンプレートが 8 種類未満");
        }
        let mut counts = vec![0usize; COVER_VARIANTS];
        for book in BOOKS {
            counts[cover_variant(book)] += 1;
        }
        for (variant, count) in counts.iter().enumerate() {
            assert!(*count > 0, "テンプレート {variant} が 1 冊も使われていない");
        }
        let most = counts.iter().copied().max().unwrap_or(0);
        assert!(
            most * 2 <= BOOKS.len(),
            "1 つのテンプレートに偏っている: {counts:?}"
        );
    }

    /// 同じ本からはいつも同じ表紙（テンプレートも配色も決定的）で、大きさも変わらないこと。
    #[test]
    fn cover_svg_is_deterministic() {
        for book in BOOKS {
            let svg = cover_svg(book);
            assert_eq!(
                svg,
                cover_svg(book),
                "同じ本の表紙が変わる: {}",
                book.database_id
            );
            assert!(
                svg.contains(&format!("width='{COVER_WIDTH}' height='{COVER_HEIGHT}'")),
                "表紙の大きさが {COVER_WIDTH}x{COVER_HEIGHT} でない: {}",
                book.database_id
            );
        }
    }

    /// テンプレートごとに 1 冊ずつ描いて、タイトルが暗い文字で出ていること。
    #[test]
    fn every_template_draws_the_title_in_dark_ink() {
        for (variant, area) in TITLE_AREAS.iter().enumerate().take(COVER_VARIANTS) {
            let book = BOOKS
                .iter()
                .find(|book| cover_variant(book) == variant)
                .unwrap_or_else(|| panic!("テンプレート {variant} のサンプルが無い"));
            let rgba = cover_pixels(book);
            assert_eq!(
                rgba.dimensions(),
                (COVER_WIDTH, COVER_HEIGHT),
                "テンプレート {variant} の大きさが違う"
            );
            let (x, y, width, height) = *area;
            let dark = count_dark_in(&rgba, x, y, width, height);
            assert!(
                dark > 100,
                "テンプレート {variant} のタイトルが描かれていない（暗い画素 {dark}）: {}",
                book.database_id
            );
        }
    }

    /// 10 種類のテンプレートは同じ本でも構造が違う（色違いの量産になっていない）。
    #[test]
    fn templates_render_different_structures() {
        let book = &BOOKS[0];
        let svgs: Vec<String> = (0..COVER_VARIANTS)
            .map(|variant| cover_with_variant(book, variant, "#4338ca", "#eef2ff"))
            .collect();
        for (variant, svg) in svgs.iter().enumerate() {
            for (other, other_svg) in svgs.iter().enumerate().skip(variant + 1) {
                assert_ne!(svg, other_svg, "テンプレート {variant} と {other} が同じ");
            }
        }
    }

    /// どのテンプレートもタイトル・サークル・作者・ページ数・「サンプル」を出すこと。
    #[test]
    fn every_template_has_the_required_texts() {
        let book = &BOOKS[0];
        for variant in 0..COVER_VARIANTS {
            let svg = cover_with_variant(book, variant, "#4338ca", "#eef2ff");
            let texts = texts_of(&svg);
            for needle in [book.title, book.circle, book.author, "サンプル"] {
                assert!(
                    texts.contains(needle),
                    "テンプレート {variant} に {needle} が無い: {texts}"
                );
            }
            assert!(
                texts.contains(&book.page_count.to_string()),
                "テンプレート {variant} にページ数が無い"
            );
        }
    }

    /// 表紙 PNG が既存のキャッシュ経路（448px）に書かれ、タイトル文字が描かれていること。
    #[test]
    fn seed_writes_a_448_cover_with_glyphs() {
        let pool = db::test_pool();
        let dir = thumbnails_dir("cover");
        let _ = std::fs::remove_dir_all(&dir);
        seed(&pool, &dir).expect("seed できる");

        let path = crate::views::bookshelf::cover_cache_path(&dir, SITE_ID_TECHBOOKFEST, "demo-01");
        assert!(path.exists(), "表紙キャッシュが無い: {}", path.display());
        let bytes = std::fs::read(&path).unwrap();
        let image = image::load_from_memory(&bytes).expect("PNG として読める");
        assert_eq!(image.width(), 448, "表紙の幅が違う");
        assert!(
            image.height() > 448,
            "表紙の高さが縦長でない: {}",
            image.height()
        );
        let rgba = image.to_rgba8();
        let has_glyph = rgba
            .pixels()
            .any(|pixel| pixel.0[..3].iter().all(|channel| *channel < 100));
        assert!(
            has_glyph,
            "タイトル文字（暗い画素）が 1 ピクセルも描かれていない"
        );
        // タイトルはテンプレートごとに位置が違う（TITLE_AREAS）ので、その領域に暗い画素が
        // あることまで見る（ページ数やフッターの文字だけで通ってしまわないように）。
        let variant = cover_variant(book("demo-01").expect("サンプルの本を引ける"));
        let (x, y, width, height) = TITLE_AREAS[variant];
        let dark_in_title_band = count_dark_in(&rgba, x, y, width, height);
        assert!(
            dark_in_title_band > 50,
            "タイトルが描かれていない（タイトルの帯の暗い画素が {dark_in_title_band}）"
        );

        // サンプルの冊数ぶん書かれている（キャッシュは 1 冊 1 ファイル）
        let written = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(written, BOOKS.len(), "表紙の数が冊数と違う");
        // 最後の 1 冊（demo-50）まで書かれていること（冊数を増やしたときの取りこぼし防止）
        let last = crate::views::bookshelf::cover_cache_path(&dir, SITE_ID_TECHBOOKFEST, "demo-50");
        assert!(
            last.exists(),
            "最後の表紙キャッシュが無い: {}",
            last.display()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// サンプルページが SVG から画像になり、ページ番号と本文が描かれること。
    #[test]
    fn demo_page_loader_draws_sample_pages() {
        let loader = DemoPageLoader::new("demo-01").expect("サンプルの本を引ける");
        let book = book("demo-01").unwrap();
        assert_eq!(loader.page_count(), book.page_count as usize);
        assert_eq!(
            loader.page_size(0),
            Some((PAGE_WIDTH, PAGE_HEIGHT)),
            "ページサイズが A4 比でない"
        );
        assert!(
            loader.page_size(loader.page_count()).is_none(),
            "範囲外のページサイズを返している"
        );

        let image = loader.load(0).expect("ページ画像を返す");
        let size = image.size(0);
        assert_eq!(
            (size.width.0, size.height.0),
            (PAGE_WIDTH as i32, PAGE_HEIGHT as i32)
        );
        let bytes = image.as_bytes(0).expect("フレームが無い");
        assert!(
            bytes
                .chunks_exact(4)
                .any(|pixel| pixel[..3].iter().all(|channel| *channel < 100)),
            "ページに文字が 1 ピクセルも描かれていない"
        );
        // 本文（タイトルは y = 300 / 370 の 2 行）と中央のページ番号（y = 1000、220px）の
        // 帯に、それぞれ暗い画素があることまで見る（枠や帯だけで通らないように）。
        let rgba = image::RgbaImage::from_raw(
            PAGE_WIDTH,
            PAGE_HEIGHT,
            bytes
                .chunks_exact(4)
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
                .collect(),
        )
        .expect("ページの画素を作れる");
        let dark_in = |top: u32, bottom: u32| {
            rgba.enumerate_pixels()
                .filter(|(_, y, pixel)| {
                    (top..=bottom).contains(y) && pixel.0[..3].iter().all(|channel| *channel < 100)
                })
                .count()
        };
        assert!(
            dark_in(250, 380) > 50,
            "ページ本文（タイトル）が描かれていない"
        );
        assert!(
            dark_in(800, 1010) > 500,
            "ページ番号（p.{{n}}）が描かれていない"
        );

        assert!(loader.load(loader.page_count()).is_err(), "範囲外はエラー");
        assert!(DemoPageLoader::new("demo-99").is_none(), "無い id で作れる");
    }
}
