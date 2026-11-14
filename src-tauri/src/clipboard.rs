//! Native clipboard access is needed because desktop webviews do not all expose
//! screenshot data through the browser Clipboard API.
use arboard::{Clipboard, Error as ClipboardError};
use serde::Serialize;
use tauri::AppHandle;

use crate::{ssh::upload_clipboard_image, terminal::TerminalRegistry};

const MAX_RGBA_BYTES: usize = 128 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 20 * 1024 * 1024;

#[derive(Serialize)]
#[serde(tag = "kind", content = "content", rename_all = "lowercase")]
pub enum ClipboardPaste {
    Text(String),
    Image(String),
    Empty,
}

pub fn paste_clipboard(
    app: &AppHandle,
    registry: &TerminalRegistry,
    server_id: &str,
    session_id: &str,
) -> Result<ClipboardPaste, String> {
    let mut clipboard = Clipboard::new().map_err(|err| format!("无法读取剪贴板：{err}"))?;
    let image_error = match clipboard.get_image() {
        Ok(image) => {
            if !registry.connected_to(session_id, server_id) {
                return Err("请先连接目标服务器".to_string());
            }
            let png = encode_png(&image)?;
            // Release the OS clipboard before starting network I/O.
            drop(clipboard);
            if !registry.connected_to(session_id, server_id) {
                return Err("请先连接目标服务器".to_string());
            }
            return upload_clipboard_image(app, server_id, &png).map(ClipboardPaste::Image);
        }
        Err(err) => err,
    };

    // Text must remain available during authentication, before the terminal's
    // ready marker (e.g. pasting a password or a second-factor code).
    match clipboard.get_text() {
        Ok(text) => Ok(ClipboardPaste::Text(text)),
        Err(ClipboardError::ContentNotAvailable)
            if matches!(image_error, ClipboardError::ContentNotAvailable) =>
        {
            Ok(ClipboardPaste::Empty)
        }
        Err(err) => Err(format!("无法读取剪贴板：{err}")),
    }
}

fn encode_png(image: &arboard::ImageData<'_>) -> Result<Vec<u8>, String> {
    let expected = image
        .width
        .checked_mul(image.height)
        .and_then(|n| n.checked_mul(4));
    if expected.is_none_or(|n| n == 0 || n > MAX_RGBA_BYTES) {
        return Err("剪贴板图片尺寸过大或无效".to_string());
    }
    if expected != Some(image.bytes.len()) {
        return Err("剪贴板图片数据不完整".to_string());
    }
    let width = u32::try_from(image.width).map_err(|_| "剪贴板图片尺寸过大或无效".to_string())?;
    let height = u32::try_from(image.height).map_err(|_| "剪贴板图片尺寸过大或无效".to_string())?;
    let mut data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut data, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|err| format!("截图编码失败：{err}"))?;
        writer
            .write_image_data(&image.bytes)
            .map_err(|err| format!("截图编码失败：{err}"))?;
    }
    if data.len() > MAX_PNG_BYTES {
        return Err("截图超过 20 MiB，请缩小截图后重试".to_string());
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn screenshot_png_preserves_pixels_and_alpha() {
        let pixels = vec![255, 0, 0, 255, 0, 128, 255, 100];
        let data = encode_png(&arboard::ImageData {
            width: 2,
            height: 1,
            bytes: Cow::Borrowed(&pixels),
        })
        .unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(data))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&decoded[..info.buffer_size()], pixels);
    }

    #[test]
    fn invalid_or_oversized_images_are_rejected_before_encoding() {
        for (width, height, bytes) in [
            (0, 1, vec![]),
            (usize::MAX, 2, vec![]),
            (10000, 10000, vec![]),
            (2, 1, vec![0; 4]),
        ] {
            assert!(encode_png(&arboard::ImageData {
                width,
                height,
                bytes: Cow::Owned(bytes),
            })
            .is_err());
        }
    }
}
