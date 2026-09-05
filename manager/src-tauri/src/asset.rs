//! Local images the panel draws, handed to the webview as bytes.
//!
//! A WebView2 under Tauri cannot load `file://`, and the two ways around that are
//! not equivalent. Enabling the asset protocol would give the webview a *readable
//! directory*, and the smallest tree covering every pack is the data dir - which
//! also holds `token.txt`. A pack registered by hand can live anywhere on disk, so
//! that grant would have to widen to "wherever a pack happens to be" before it even
//! worked. Reading the file here and answering with a `data:` URL keeps the
//! webview's filesystem access at exactly nothing, which is what it has today, and
//! it is the seam `config_view::pack_manifest_file` already uses for text files:
//! Rust reads the bytes, the frontend receives a value.
//!
//! `img-src 'self' data:` is already in the CSP (`tauri.conf.json`), so nothing in
//! the app's security configuration changes to make this work.

use std::path::Path;

use tauri::{AppHandle, Manager};

use crate::config_edit;
use crate::host::Host;

/// Past this a portrait is not a portrait.
///
/// The list draws it at 32 CSS px and the subtitle dialog at 40; a file bigger than
/// this is a full-resolution artwork someone dropped in, and inlining it would put
/// megabytes of base64 into the DOM - a third larger than the file - for a
/// thumbnail. Refusing is not a degradation: the caller already has to draw the
/// glyph placeholder for packs with no portrait at all.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The registered pack's portrait as a `data:` URL, or `None` when there is not one
/// to draw.
///
/// Every way this can fail collapses into `None` on purpose - no such pack, no
/// `avatar`, the file is gone, an extension nothing here decodes, too large,
/// unreadable - because the frontend has exactly one thing to do about all of them:
/// draw the first-character glyph placeholder that `docs/voicepack-spec.md` defines.
/// A `Result` here would be an error message about a thumbnail.
///
/// Resolution goes through `config_edit::read_packs`, so `id` means here what it
/// means everywhere else in the panel and `avatar` arrives absolute - resolved
/// against the pack by the same `hydrate` the pack list itself is read through, or
/// against the data dir for an entry written before packs carried their own
/// portrait.
#[tauri::command]
pub async fn pack_avatar(app: AppHandle, id: String) -> Option<String> {
    let host = app.state::<Host>();
    let pack = config_edit::read_packs(&host).into_iter().find(|pack| pack.id == id)?;
    data_url(Path::new(&pack.avatar?))
}

/// The image at `path` as `data:image/<subtype>;base64,<bytes>`.
///
/// The extension decides the media type, against the same allow-list
/// `config_edit::import_avatar` enforces on the way in: a portrait this app accepted
/// is a portrait it can serve, and one it never accepted is not sniffed into
/// existence here.
fn data_url(path: &Path) -> Option<String> {
    let subtype = media_subtype(path)?;
    // Asked before reading, so an accidental 40 MB PNG is never held in memory.
    let size = std::fs::metadata(path).ok()?.len();
    if size == 0 || size > MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;

    const PREFIX: &str = "data:image/";
    const INFIX: &str = ";base64,";
    let mut url = String::with_capacity(
        PREFIX.len() + subtype.len() + INFIX.len() + bytes.len().div_ceil(3) * 4,
    );
    url.push_str(PREFIX);
    url.push_str(subtype);
    url.push_str(INFIX);
    base64_into(&bytes, &mut url);
    Some(url)
}

/// `image/*` subtype for an extension the panel accepts as a portrait, or `None`.
fn media_subtype(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpeg"),
        "webp" => Some("webp"),
        "bmp" => Some("bmp"),
        _ => None,
    }
}

/// Standard base64 (RFC 4648, padded), appended to `out`.
///
/// Written out rather than taken from a crate: this is the only base64 in the
/// manager, it is a table and a three-byte loop, and a direct dependency has to earn
/// its line in the manifest. `out` is pre-sized by the caller, so this allocates
/// nothing.
fn base64_into(bytes: &[u8], out: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut groups = bytes.chunks_exact(3);
    for group in &mut groups {
        let word =
            (u32::from(group[0]) << 16) | (u32::from(group[1]) << 8) | u32::from(group[2]);
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[((word >> shift) & 0x3f) as usize] as char);
        }
    }

    // One or two bytes left over are still written as a four-character group: the
    // missing sextets are zero-padded and the group is filled out with '='.
    let tail = groups.remainder();
    if tail.is_empty() {
        return;
    }
    let word = (u32::from(tail[0]) << 16) | (u32::from(tail.get(1).copied().unwrap_or(0)) << 8);
    out.push(ALPHABET[((word >> 18) & 0x3f) as usize] as char);
    out.push(ALPHABET[((word >> 12) & 0x3f) as usize] as char);
    out.push(if tail.len() == 2 {
        ALPHABET[((word >> 6) & 0x3f) as usize] as char
    } else {
        '='
    });
    out.push('=');
}

#[cfg(test)]
mod tests {
    use super::base64_into;

    /// RFC 4648 §10's vectors, which exist to catch exactly the bug this encoder can
    /// have: the two padded tails. The registered packs' portraits are both a
    /// multiple of three bytes long, so running the app proves the unpadded path
    /// only.
    #[test]
    fn matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            let mut out = String::new();
            base64_into(input.as_bytes(), &mut out);
            assert_eq!(out, expected, "base64 of {input:?}");
        }
    }

    /// Every byte value, so a wrong table entry cannot hide behind ASCII input.
    #[test]
    fn covers_the_whole_alphabet() {
        let bytes: Vec<u8> = (0..=255).collect();
        let mut out = String::new();
        base64_into(&bytes, &mut out);
        assert_eq!(
            out,
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7\
             PD0+P0BBQkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3\
             eHl6e3x9fn+AgYKDhIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmqq6ytrq+wsbKz\
             tLW2t7i5uru8vb6/wMHCw8TFxsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t/g4eLj5OXm5+jp6uvs7e7v\
             8PHy8/T19vf4+fr7/P3+/w=="
        );
    }
}
