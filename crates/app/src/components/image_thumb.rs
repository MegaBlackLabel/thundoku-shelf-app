//! 画像バイト列を表示用の `RenderImage` に変換する（表紙とレポートの添付で共用）。

use std::sync::Arc;

use gpui_kit::RenderImage;

/// 画像を縮小（最大幅 `max_width` px）して BGRA の `RenderImage` に変換する。
/// 元画像のアスペクト比を保つ（クロップしない）。表示側（カード / リスト）で
/// `fit_cover_size` により枠内に比率のまま収める（FANZA 等はサムネの比率がバラバラのため）。
pub fn decode_and_resize(data: &[u8], max_width: u32) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory(data).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let resized = if w > max_width {
        let scale = max_width as f32 / w as f32;
        let nw = (w as f32 * scale).max(1.0) as u32;
        let nh = (h as f32 * scale).max(1.0) as u32;
        decoded.resize(nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        decoded
    };
    let mut rgba = resized.into_rgba8();
    // RenderImage は BGRA を期待するため R/B を入れ替える
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    Some(Arc::new(RenderImage::new([frame])))
}
