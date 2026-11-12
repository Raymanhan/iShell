//! Terminal character encodings. Remote shells usually speak UTF-8, but older
//! systems and some network devices use GBK, Big5, Shift_JIS and the like:
//! output is decoded and typed input encoded with the server's encoding.

use encoding_rs::{Encoding, UTF_8};

pub const DEFAULT: &str = "UTF-8";

/// The canonical name for an encoding label (`gbk` -> `GBK`), which the
/// frontend's `TextDecoder` also accepts.
pub fn normalize(label: &str) -> Result<String, String> {
    let label = label.trim();
    if label.is_empty() {
        return Ok(DEFAULT.into());
    }
    let encoding = Encoding::for_label(label.as_bytes())
        .ok_or_else(|| format!("不支持的终端编码：{label}"))?;
    // UTF-16 cannot carry a terminal byte stream, and the replacement
    // encoding decodes everything to one character.
    if encoding.output_encoding() != encoding || encoding == encoding_rs::REPLACEMENT {
        return Err(format!("不支持的终端编码：{label}"));
    }
    Ok(encoding.name().into())
}

pub fn lookup(label: &str) -> &'static Encoding {
    Encoding::for_label(label.as_bytes()).unwrap_or(UTF_8)
}

/// Text typed into the terminal, as bytes in the remote encoding. Characters
/// the encoding lacks become `?` rather than HTML entities.
pub fn encode(encoding: &'static Encoding, text: &str) -> Vec<u8> {
    if encoding == UTF_8 {
        return text.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut buffer = [0u8; 4];
    for ch in text.chars() {
        let (bytes, _, unmappable) = encoding.encode(ch.encode_utf8(&mut buffer));
        if unmappable {
            out.push(b'?');
        } else {
            out.extend_from_slice(&bytes);
        }
    }
    out
}

pub fn decode(encoding: &'static Encoding, bytes: &[u8]) -> String {
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_normalize_to_names_the_frontend_decodes() {
        assert_eq!(normalize("gbk").unwrap(), "GBK");
        assert_eq!(normalize(" utf8 ").unwrap(), "UTF-8");
        assert_eq!(normalize("").unwrap(), "UTF-8");
        assert_eq!(normalize("big5").unwrap(), "Big5");
        assert_eq!(normalize("sjis").unwrap(), "Shift_JIS");
        assert!(normalize("utf-16le").is_err());
        assert!(normalize("klingon").is_err());
        // The names the frontend offers (src/features/shell/encodings.ts).
        for name in [
            "UTF-8",
            "GBK",
            "gb18030",
            "Big5",
            "Shift_JIS",
            "EUC-JP",
            "EUC-KR",
            "windows-1252",
            "ISO-8859-2",
            "windows-1251",
            "KOI8-R",
        ] {
            assert_eq!(normalize(name).unwrap(), name);
        }
    }

    #[test]
    fn input_and_output_round_trip_in_the_remote_encoding() {
        let gbk = lookup("GBK");
        assert_eq!(
            encode(gbk, "中文 ls"),
            [0xd6, 0xd0, 0xce, 0xc4, b' ', b'l', b's']
        );
        assert_eq!(decode(gbk, &[0xd6, 0xd0, 0xce, 0xc4]), "中文");
        // Not in GBK: replaced, not turned into an HTML entity.
        assert_eq!(encode(gbk, "a😀b"), b"a?b");
        assert_eq!(encode(lookup("UTF-8"), "é"), "é".as_bytes());
    }
}
