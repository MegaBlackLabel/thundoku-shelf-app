//! Fantia（`fantia.jp`）のセッション、ショップ購入（商品）のページ解析と同期。
//!
//! ログインは**アプリ内 WebView** でユーザーが行う（reCAPTCHA と 2 段階認証があるため
//! 自動ログインはできない。`crates/app/src/views/fantia_login.rs`）。
//! 購入の取り込みはショップの購入履歴ページを読む（[`sync`]）。

pub mod client;
pub mod shop;
pub mod sync;
