//! Clipboard image staging for paste-to-send.
//!
//! Pasting reads the Wayland clipboard, validates the offered bytes as a PNG
//! or JPEG image, and stages an immutable copy in the media cache, which the
//! regular quota pruning reclaims if the paste is never sent. Sending reads
//! the staged copy back through the same validation so a file swapped between
//! staging and sending cannot smuggle another payload into an upload.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use tracing::warn;

use crate::assets::{self, MAX_IMAGE_BYTES};

static PASTE_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Clipboard access behind a seam so tests never depend on outside state.
pub(crate) trait ClipboardBackend: Send + Sync {
    fn offered_types(&self) -> Result<Vec<String>>;
    fn read_bytes(&self, mime_type: &str) -> Result<Vec<u8>>;
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Live Wayland clipboard access through `wl-paste`.
pub(crate) struct SystemClipboard;

#[cfg_attr(coverage_nightly, coverage(off))]
impl ClipboardBackend for SystemClipboard {
    fn offered_types(&self) -> Result<Vec<String>> {
        let output = run_paste_command(&["--list-types"])?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn read_bytes(&self, mime_type: &str) -> Result<Vec<u8>> {
        let mut child = Command::new("wl-paste")
            .arg("--type")
            .arg(mime_type)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting wl-paste to read the clipboard")?;
        let mut bytes = Vec::new();
        {
            let mut stdout = child.stdout.take().context("clipboard output is missing")?;
            std::io::Read::by_ref(&mut stdout)
                .take(MAX_IMAGE_BYTES + 1)
                .read_to_end(&mut bytes)
                .context("reading clipboard image bytes")?;
        }
        let _ = child.wait();
        Ok(bytes)
    }

    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn run_paste_command(args: &[&str]) -> Result<std::process::Output> {
    let mut child = Command::new("wl-paste")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting wl-paste to inspect the clipboard")?;
    // MIME listings are tiny, so draining first cannot block on a full pipe.
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        std::io::Read::read_to_end(&mut pipe, &mut stdout)?;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("wl-paste reported no clipboard selection");
            }
            return Ok(std::process::Output {
                status,
                stdout,
                stderr: Vec::new(),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out inspecting the clipboard");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod fake {
    use super::ClipboardBackend;
    use anyhow::{Result, anyhow, bail};
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    /// Scripted clipboard for tests.
    #[derive(Debug, Default)]
    pub(crate) struct FakeClipboard {
        script: StdMutex<FakeClipboardScript>,
    }

    /// Downcasts a shared clipboard backend to the test double.
    pub(crate) fn test_clipboard(
        clipboard: &std::sync::Arc<dyn ClipboardBackend>,
    ) -> &FakeClipboard {
        clipboard
            .as_any()
            .downcast_ref::<FakeClipboard>()
            .expect("test clipboard backend is always the fake")
    }

    #[derive(Debug, Default)]
    struct FakeClipboardScript {
        types: Vec<String>,
        bytes: HashMap<String, Vec<u8>>,
        list_error: Option<String>,
        read_error: Option<String>,
    }

    impl FakeClipboard {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn script_image(&self, mime_type: &str, bytes: Vec<u8>) {
            let mut script = self.script.lock().unwrap();
            script.types = vec![mime_type.to_owned()];
            script.bytes.insert(mime_type.to_owned(), bytes);
        }

        pub(crate) fn script_bytes(&self, mime_type: &str, bytes: Vec<u8>) {
            self.script
                .lock()
                .unwrap()
                .bytes
                .insert(mime_type.to_owned(), bytes);
        }

        pub(crate) fn script_types(&self, types: Vec<String>) {
            self.script.lock().unwrap().types = types;
        }

        pub(crate) fn fail_list(&self, message: &str) {
            self.script.lock().unwrap().list_error = Some(message.to_owned());
        }

        pub(crate) fn fail_read(&self, message: &str) {
            self.script.lock().unwrap().read_error = Some(message.to_owned());
        }
    }

    impl ClipboardBackend for FakeClipboard {
        fn offered_types(&self) -> Result<Vec<String>> {
            let script = self.script.lock().unwrap();
            if let Some(error) = script.list_error.clone() {
                bail!("{error}");
            }
            Ok(script.types.clone())
        }

        fn read_bytes(&self, mime_type: &str) -> Result<Vec<u8>> {
            let script = self.script.lock().unwrap();
            if let Some(error) = script.read_error.clone() {
                bail!("{error}");
            }
            script
                .bytes
                .get(mime_type)
                .cloned()
                .ok_or_else(|| anyhow!("clipboard no longer offers {mime_type}"))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
}

/// Preferred clipboard image type with its staged file extension.
pub(crate) fn pick_image_mime(offered: &[String]) -> Option<(&'static str, &'static str)> {
    for (mime_type, extension) in [("image/png", ".png"), ("image/jpeg", ".jpg")] {
        let advertised = offered.iter().any(|candidate| {
            candidate
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case(mime_type)
        });
        if advertised {
            return Some((mime_type, extension));
        }
    }
    None
}

/// Sniffs pasted bytes back to a supported image type.
pub(crate) fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// Validates clipboard bytes before they are staged or uploaded.
pub(crate) fn validate_pasted_bytes(bytes: &[u8], mime_type: &str) -> Result<()> {
    ensure!(!bytes.is_empty(), "clipboard image is empty");
    ensure!(
        bytes.len() as u64 <= MAX_IMAGE_BYTES,
        "pasted image exceeds the 25 MB limit"
    );
    ensure!(
        sniff_image_mime(bytes) == Some(mime_type),
        "clipboard image is not a valid {mime_type} file"
    );
    Ok(())
}

/// Reads staged image dimensions without a decoder dependency.
pub(crate) fn pasted_image_dimensions(bytes: &[u8], mime_type: &str) -> Result<(u32, u32)> {
    match mime_type {
        "image/png" => png_dimensions(bytes),
        "image/jpeg" => jpeg_dimensions(bytes),
        _ => bail!("unsupported pasted image type: {mime_type}"),
    }
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    ensure!(bytes.len() >= 24, "truncated PNG header");
    ensure!(
        bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "not a PNG file"
    );
    ensure!(&bytes[12..16] == b"IHDR", "PNG is missing its IHDR chunk");
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    ensure!(width > 0 && height > 0, "PNG has zero dimensions");
    Ok((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    ensure!(
        bytes.len() > 2 && bytes[0] == 0xFF && bytes[1] == 0xD8,
        "not a JPEG file"
    );
    // Frame headers always precede the entropy-coded scan, so walking
    // segments in order finds the dimensions before any scan data could
    // masquerade as markers.
    let mut cursor = 2;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] != 0xFF {
            cursor += 1;
            continue;
        }
        let marker = bytes[cursor + 1];
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            cursor += 2;
            continue;
        }
        // Start-of-frame markers except C4/C8/CC (tables) carry dimensions as
        // length(2) + precision(1) + height(2) + width(2).
        if matches!(marker, 0xC0..=0xCF) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            ensure!(cursor + 9 <= bytes.len(), "truncated JPEG frame header");
            let height = u32::from(u16::from_be_bytes([bytes[cursor + 5], bytes[cursor + 6]]));
            let width = u32::from(u16::from_be_bytes([bytes[cursor + 7], bytes[cursor + 8]]));
            ensure!(width > 0 && height > 0, "JPEG has zero dimensions");
            return Ok((width, height));
        }
        if marker == 0xFF {
            cursor += 1;
            continue;
        }
        ensure!(cursor + 3 < bytes.len(), "truncated JPEG segment");
        let length = u16::from_be_bytes([bytes[cursor + 2], bytes[cursor + 3]]) as usize;
        ensure!(length >= 2, "corrupt JPEG segment");
        cursor += 2 + length;
    }
    bail!("JPEG has no frame header")
}

/// Writes validated bytes to a uniquely named staged file.
pub(crate) fn stage_pasted_image(
    media_dir: &Path,
    bytes: &[u8],
    extension: &str,
) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = PASTE_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let destination = media_dir.join(format!(
        "paste-{nanos}-{sequence}{}{extension}",
        std::process::id()
    ));
    assets::write_private_bytes(&destination, bytes)?;
    Ok(destination)
}

/// Resolves a shell-supplied staged path back into the media cache, rejecting
/// absolute escapes, symlinks, and missing files.
pub(crate) fn resolve_staged_image(media_dir: &Path, path: &str) -> Result<PathBuf> {
    ensure!(!path.is_empty(), "staged image path is empty");
    let media_dir = media_dir
        .canonicalize()
        .context("media cache is unavailable")?;
    let staged = Path::new(path)
        .canonicalize()
        .context("staged image is not available")?;
    ensure!(
        staged.starts_with(&media_dir),
        "staged image is outside the media cache"
    );
    Ok(staged)
}

/// Outcome of one clipboard paste attempt.
pub(crate) enum PasteOutcome {
    Empty,
    Pasted {
        path: PathBuf,
        width: u32,
        height: u32,
        mime_type: String,
    },
}

/// Reads an image off the clipboard into a staged file. Clipboard and
/// mid-flight failures resolve to [`PasteOutcome::Empty`] so ordinary text
/// pastes keep working; only an offered image that fails validation errors.
pub(crate) fn paste_image_from_clipboard(
    clipboard: &dyn ClipboardBackend,
    media_dir: &Path,
) -> Result<PasteOutcome> {
    let offered = match clipboard.offered_types() {
        Ok(types) => types,
        Err(error) => {
            warn!(%error, "could not inspect clipboard contents");
            return Ok(PasteOutcome::Empty);
        }
    };
    let Some((mime_type, extension)) = pick_image_mime(&offered) else {
        return Ok(PasteOutcome::Empty);
    };
    let bytes = match clipboard.read_bytes(mime_type) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(%error, mime_type, "clipboard image vanished before it could be read");
            return Ok(PasteOutcome::Empty);
        }
    };
    validate_pasted_bytes(&bytes, mime_type)?;
    let (width, height) = pasted_image_dimensions(&bytes, mime_type)?;
    let path = stage_pasted_image(media_dir, &bytes, extension)?;
    Ok(PasteOutcome::Pasted {
        path,
        width,
        height,
        mime_type: mime_type.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes
    }

    fn jpeg_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F'];
        bytes.extend_from_slice(&[0xFF, 0xC0, 0, 11, 8]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[1, 0, 0xFF, 0xD9]);
        bytes
    }

    #[test]
    fn clipboard_mime_prefers_png_over_jpeg() {
        assert_eq!(
            pick_image_mime(&["text/plain".to_owned(), "image/jpeg".to_owned()]),
            Some(("image/jpeg", ".jpg"))
        );
        assert_eq!(
            pick_image_mime(&[
                "image/jpeg".to_owned(),
                "image/png;charset=binary".to_owned()
            ]),
            Some(("image/png", ".png"))
        );
        assert_eq!(
            pick_image_mime(&["IMAGE/PNG".to_owned()]),
            Some(("image/png", ".png"))
        );
        assert_eq!(pick_image_mime(&["text/plain".to_owned()]), None);
        assert_eq!(pick_image_mime(&[]), None);
    }

    #[test]
    fn pasted_bytes_require_matching_magic_and_size() {
        let png = png_bytes(2, 3);
        validate_pasted_bytes(&png, "image/png").unwrap();
        validate_pasted_bytes(&jpeg_bytes(4, 5), "image/jpeg").unwrap();
        assert!(validate_pasted_bytes(&[], "image/png").is_err());
        assert!(validate_pasted_bytes(&png, "image/jpeg").is_err());
        assert!(validate_pasted_bytes(&png, "image/gif").is_err());
        assert!(
            validate_pasted_bytes(
                &vec![0xFF; usize::try_from(MAX_IMAGE_BYTES).unwrap() + 1],
                "image/jpeg",
            )
            .is_err()
        );
    }

    #[test]
    fn pasted_png_dimensions_read_ihdr() {
        assert_eq!(
            pasted_image_dimensions(&png_bytes(2, 3), "image/png").unwrap(),
            (2, 3)
        );
        assert!(pasted_image_dimensions(&png_bytes(2, 3)[..10], "image/png").is_err());
        assert!(pasted_image_dimensions(&[0u8; 24], "image/png").is_err());
        let mut missing_ihdr = png_bytes(2, 3);
        missing_ihdr[12..16].copy_from_slice(b"IDAT");
        assert!(pasted_image_dimensions(&missing_ihdr, "image/png").is_err());
        assert!(pasted_image_dimensions(&png_bytes(0, 3), "image/png").is_err());
        assert!(pasted_image_dimensions(&png_bytes(2, 3), "image/gif").is_err());
    }

    #[test]
    fn pasted_jpeg_dimensions_scan_to_frame_header() {
        assert_eq!(
            pasted_image_dimensions(&jpeg_bytes(4, 5), "image/jpeg").unwrap(),
            (4, 5)
        );
        let mut progressive = jpeg_bytes(6, 7);
        progressive[9] = 0xC2;
        assert_eq!(
            pasted_image_dimensions(&progressive, "image/jpeg").unwrap(),
            (6, 7)
        );
        assert!(pasted_image_dimensions(&[0u8; 8], "image/jpeg").is_err());
        assert!(pasted_image_dimensions(&[0xFF, 0xD8, 0xFF], "image/jpeg").is_err());
        let mut truncated_sof = jpeg_bytes(4, 5);
        truncated_sof.truncate(12);
        assert!(pasted_image_dimensions(&truncated_sof, "image/jpeg").is_err());
        assert!(pasted_image_dimensions(&jpeg_bytes(0, 5), "image/jpeg").is_err());
        assert!(pasted_image_dimensions(&[0xFF, 0xD8, 0xFF, 0xD9], "image/jpeg").is_err());
    }

    #[test]
    fn pasted_jpeg_dimensions_skip_padding_and_reject_corruption() {
        assert!(pasted_image_dimensions(&[0xFF, 0xD8, 0x00, 0xFF, 0xD9], "image/jpeg").is_err());
        assert!(
            pasted_image_dimensions(&[0xFF, 0xD8, 0xFF, 0xFF, 0xFF, 0xD9], "image/jpeg").is_err()
        );
        assert!(
            pasted_image_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01], "image/jpeg").is_err()
        );
        assert!(pasted_image_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00], "image/jpeg").is_err());
    }

    #[test]
    fn staged_images_resolve_only_inside_the_media_cache() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media");
        std::fs::create_dir(&media_dir).unwrap();
        let first = stage_pasted_image(&media_dir, &png_bytes(1, 1), ".png").unwrap();
        let second = stage_pasted_image(&media_dir, &png_bytes(1, 1), ".png").unwrap();
        assert_ne!(first, second);
        assert_eq!(
            resolve_staged_image(&media_dir, first.to_string_lossy().as_ref()).unwrap(),
            first.canonicalize().unwrap()
        );
        assert!(resolve_staged_image(&media_dir, "").is_err());
        let outside = directory.path().join("outside.png");
        std::fs::write(&outside, b"x").unwrap();
        assert!(resolve_staged_image(&media_dir, outside.to_string_lossy().as_ref()).is_err());
        assert!(
            resolve_staged_image(
                &media_dir,
                media_dir.join("missing.png").to_string_lossy().as_ref()
            )
            .is_err()
        );
        assert!(resolve_staged_image(&directory.path().join("absent"), "paste-1.png").is_err());
    }

    #[test]
    fn pasting_without_an_image_reports_empty() {
        let directory = tempfile::tempdir().unwrap();
        let clipboard = fake::FakeClipboard::new();
        assert!(matches!(
            paste_image_from_clipboard(&clipboard, directory.path()).unwrap(),
            PasteOutcome::Empty
        ));
        clipboard.script_types(vec!["text/plain".to_owned()]);
        assert!(matches!(
            paste_image_from_clipboard(&clipboard, directory.path()).unwrap(),
            PasteOutcome::Empty
        ));
        clipboard.fail_list("wayland is unavailable");
        assert!(matches!(
            paste_image_from_clipboard(&clipboard, directory.path()).unwrap(),
            PasteOutcome::Empty
        ));
    }

    #[test]
    fn pasting_an_image_stages_validated_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let clipboard = fake::FakeClipboard::new();
        clipboard.script_image("image/png", png_bytes(2, 3));
        let outcome = paste_image_from_clipboard(&clipboard, directory.path()).unwrap();
        assert!(matches!(
            &outcome,
            PasteOutcome::Pasted {
                width: 2,
                height: 3,
                ..
            }
        ));
        assert!(matches!(
            &outcome,
            PasteOutcome::Pasted { mime_type, .. } if mime_type == "image/png"
        ));
        let mut staged = std::fs::read_dir(directory.path()).unwrap();
        let entry = staged.next().unwrap().unwrap();
        assert!(staged.next().is_none());
        assert_eq!(std::fs::read(entry.path()).unwrap(), png_bytes(2, 3));
        clipboard.script_types(vec!["image/jpeg".to_owned()]);
        assert!(matches!(
            paste_image_from_clipboard(&clipboard, directory.path()).unwrap(),
            PasteOutcome::Empty
        ));
        clipboard.fail_read("selection disappeared");
        clipboard.script_image("image/png", png_bytes(2, 3));
        assert!(matches!(
            paste_image_from_clipboard(&clipboard, directory.path()).unwrap(),
            PasteOutcome::Empty
        ));
    }

    #[test]
    fn pasting_prefers_png_over_jpeg() {
        let directory = tempfile::tempdir().unwrap();
        let clipboard = fake::FakeClipboard::new();
        clipboard.script_types(vec!["image/jpeg".to_owned(), "image/png".to_owned()]);
        clipboard.script_bytes("image/jpeg", jpeg_bytes(4, 5));
        clipboard.script_bytes("image/png", png_bytes(6, 7));
        let outcome = paste_image_from_clipboard(&clipboard, directory.path()).unwrap();
        assert!(matches!(
            &outcome,
            PasteOutcome::Pasted {
                width: 6,
                height: 7,
                ..
            }
        ));
        assert!(matches!(
            &outcome,
            PasteOutcome::Pasted { mime_type, .. } if mime_type == "image/png"
        ));
    }

    #[test]
    fn pasting_rejects_invalid_and_oversized_images() {
        let directory = tempfile::tempdir().unwrap();
        let clipboard = fake::FakeClipboard::new();
        clipboard.script_image("image/png", b"not an image".to_vec());
        assert!(paste_image_from_clipboard(&clipboard, directory.path()).is_err());
        clipboard.script_image(
            "image/png",
            vec![0xFF; usize::try_from(MAX_IMAGE_BYTES).unwrap() + 1],
        );
        assert!(paste_image_from_clipboard(&clipboard, directory.path()).is_err());
        clipboard.script_image(
            "image/png",
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A].to_vec(),
        );
        assert!(paste_image_from_clipboard(&clipboard, directory.path()).is_err());
    }

    #[test]
    fn pasting_into_an_unwritable_cache_fails() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let clipboard = fake::FakeClipboard::new();
        clipboard.script_image("image/png", png_bytes(2, 3));
        assert!(paste_image_from_clipboard(&clipboard, &blocker).is_err());
    }
}
