use anyhow::{Context, Result, anyhow, bail};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;
use whatsapp_rust::prelude::{Client, Jid, wa};

pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_VIDEO_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_AVATAR_BYTES: usize = 1024 * 1024;
const MAX_AVATAR_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MEDIA_CACHE_BYTES: u64 = 256 * 1024 * 1024;

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn hex_key(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub fn avatar_path(directory: &Path, jid: &str) -> PathBuf {
    directory.join(format!("{}.img", hex_key(jid)))
}

pub fn avatar_missing_path(directory: &Path, jid: &str) -> PathBuf {
    directory.join(format!("{}.none", hex_key(jid)))
}

pub fn available_avatar_jids(directory: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut jids = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("img") {
                return None;
            }
            let encoded = path.file_stem()?.to_str()?;
            if encoded.len() % 2 != 0 {
                return None;
            }
            let bytes = (0..encoded.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()?;
            String::from_utf8(bytes).ok()
        })
        .collect::<Vec<_>>();
    jids.sort();
    jids
}

pub fn message_image_path(directory: &Path, chat_jid: &str, message_id: &str) -> PathBuf {
    directory.join(format!("{}-{}.img", hex_key(chat_jid), hex_key(message_id)))
}

pub fn message_image_thumbnail_path(directory: &Path, chat_jid: &str, message_id: &str) -> PathBuf {
    directory.join(format!(
        "{}-{}.thumbnail.jpg",
        hex_key(chat_jid),
        hex_key(message_id)
    ))
}

fn video_extension(mime_type: Option<&str>) -> &'static str {
    match mime_type.unwrap_or_default().to_ascii_lowercase().as_str() {
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "video/webm" => ".webm",
        "video/3gpp" => ".3gp",
        _ => "",
    }
}

pub fn message_video_path(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    mime_type: Option<&str>,
) -> PathBuf {
    directory.join(format!(
        "{}-{}.video{}",
        hex_key(chat_jid),
        hex_key(message_id),
        video_extension(mime_type)
    ))
}

pub fn message_video_thumbnail_path(directory: &Path, chat_jid: &str, message_id: &str) -> PathBuf {
    directory.join(format!(
        "{}-{}.video-thumbnail.jpg",
        hex_key(chat_jid),
        hex_key(message_id)
    ))
}

fn cached_video_thumbnail_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let suffix = [
        ".video.mp4",
        ".video.mov",
        ".video.webm",
        ".video.3gp",
        ".video",
    ]
    .into_iter()
    .find(|suffix| name.ends_with(suffix))?;
    Some(path.with_file_name(format!(
        "{}.video-thumbnail.jpg",
        &name[..name.len() - suffix.len()]
    )))
}

fn jpeg_file_is_valid(path: &Path) -> bool {
    use std::io::Read;

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.len() < 3 || metadata.len() > MAX_IMAGE_BYTES {
        return false;
    }
    let mut header = [0u8; 3];
    file.read_exact(&mut header).is_ok() && header == [0xff, 0xd8, 0xff]
}

pub fn ensure_message_video_thumbnail(path: &Path, thumbnail_path: &Path) -> Result<bool> {
    if jpeg_file_is_valid(thumbnail_path) {
        return Ok(false);
    }
    if !path.is_file() {
        return Ok(false);
    }

    let temporary = thumbnail_path.with_extension(format!("part-{}.jpg", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-ss",
            "0.1",
            "-i",
        ])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=w='min(1280,iw)':h=-2",
            "-q:v",
            "4",
        ])
        .arg(&temporary)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting ffmpeg to create a video preview")?;

    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&temporary);
            bail!("timed out creating a video preview");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if !status.success() || !jpeg_file_is_valid(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        bail!("ffmpeg did not create a valid video preview");
    }

    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    if thumbnail_path.exists() {
        std::fs::remove_file(thumbnail_path)?;
    }
    std::fs::rename(&temporary, thumbnail_path)?;
    if let Some(directory) = thumbnail_path.parent() {
        prune_media_cache(directory, thumbnail_path);
    }
    Ok(true)
}

pub fn backfill_message_video_thumbnails(directory: &Path) -> Result<usize> {
    let mut generated = 0;
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(thumbnail_path) = cached_video_thumbnail_path(&path) else {
            continue;
        };
        match ensure_message_video_thumbnail(&path, &thumbnail_path) {
            Ok(true) => generated += 1,
            Ok(false) => {}
            Err(error) => {
                warn!(%error, video = %path.display(), "could not generate video preview");
            }
        }
    }
    Ok(generated)
}

fn cache_media_thumbnail(
    directory: &Path,
    path: &Path,
    thumbnail_path: &Path,
    thumbnail: Option<&Vec<u8>>,
    declared_full_length: Option<u64>,
) -> Result<()> {
    let thumbnail = thumbnail.filter(|bytes| bytes.starts_with(&[0xff, 0xd8, 0xff]));

    // Early image builds wrote previews to the final media path. The declared
    // plaintext length also makes this safe for future media migrations where
    // a recovered history record omits its embedded preview.
    let existing_is_preview = std::fs::metadata(path).is_ok_and(|metadata| {
        thumbnail.is_some_and(|bytes| metadata.len() == bytes.len() as u64)
            || declared_full_length.is_some_and(|length| length > 0 && metadata.len() != length)
    });
    if existing_is_preview {
        if thumbnail_path.exists() {
            std::fs::remove_file(path)?;
        } else {
            std::fs::rename(path, thumbnail_path)?;
        }
    } else if !thumbnail_path.exists()
        && let Some(thumbnail) = thumbnail
    {
        write_private_bytes(thumbnail_path, thumbnail)?;
    }
    prune_media_cache(directory, thumbnail_path);
    Ok(())
}

pub fn cache_message_image_thumbnail(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    thumbnail: Option<&Vec<u8>>,
    declared_full_length: Option<u64>,
) -> Result<PathBuf> {
    let path = message_image_path(directory, chat_jid, message_id);
    let thumbnail_path = message_image_thumbnail_path(directory, chat_jid, message_id);
    cache_media_thumbnail(
        directory,
        &path,
        &thumbnail_path,
        thumbnail,
        declared_full_length,
    )?;
    Ok(thumbnail_path)
}

pub fn cache_message_video_thumbnail(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    mime_type: Option<&str>,
    thumbnail: Option<&Vec<u8>>,
    declared_full_length: Option<u64>,
) -> Result<PathBuf> {
    let path = message_video_path(directory, chat_jid, message_id, mime_type);
    let thumbnail_path = message_video_thumbnail_path(directory, chat_jid, message_id);
    cache_media_thumbnail(
        directory,
        &path,
        &thumbnail_path,
        thumbnail,
        declared_full_length,
    )?;
    Ok(thumbnail_path)
}

fn safe_document_extension(file_name: &str) -> Option<String> {
    let extension = Path::new(file_name).extension()?.to_str()?;
    (!extension.is_empty()
        && extension.len() <= 12
        && extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric()))
    .then(|| extension.to_ascii_lowercase())
}

pub fn message_document_path(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    file_name: &str,
) -> PathBuf {
    let extension = safe_document_extension(file_name)
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    directory.join(format!(
        "{}-{}.document{}",
        hex_key(chat_jid),
        hex_key(message_id),
        extension
    ))
}

pub fn location_thumbnail_path(directory: &Path, chat_jid: &str, message_id: &str) -> PathBuf {
    directory.join(format!(
        "{}-{}.location.jpg",
        hex_key(chat_jid),
        hex_key(message_id)
    ))
}

pub fn remove_message_media(directory: &Path, chat_jid: &str, message_id: &str) {
    let _ = std::fs::remove_file(message_image_path(directory, chat_jid, message_id));
    let _ = std::fs::remove_file(message_image_thumbnail_path(
        directory, chat_jid, message_id,
    ));
    let _ = std::fs::remove_file(message_video_thumbnail_path(
        directory, chat_jid, message_id,
    ));
    let _ = std::fs::remove_file(location_thumbnail_path(directory, chat_jid, message_id));
    let document_prefix = format!("{}-{}.document", hex_key(chat_jid), hex_key(message_id));
    let video_prefix = format!("{}-{}.video", hex_key(chat_jid), hex_key(message_id));
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&document_prefix) || name.starts_with(&video_prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

pub fn remove_chat_media(directory: &Path, chat_jid: &str) {
    let prefix = format!("{}-", hex_key(chat_jid));
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn copy_chat_media_alias(
    directory: &Path,
    alias_jid: &str,
    canonical_jid: &str,
) -> Result<Vec<(String, String)>> {
    if alias_jid == canonical_jid {
        return Ok(Vec::new());
    }
    let alias_prefix = format!("{}-", hex_key(alias_jid));
    let canonical_prefix = format!("{}-", hex_key(canonical_jid));
    let mut replacements = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(suffix) = file_name.strip_prefix(&alias_prefix) else {
            continue;
        };
        let source = entry.path();
        let destination = directory.join(format!("{canonical_prefix}{suffix}"));
        if !destination.exists() {
            std::fs::copy(&source, &destination)?;
        }
        replacements.push((
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
        ));
    }
    Ok(replacements)
}

pub fn copy_avatar_alias(directory: &Path, alias_jid: &str, canonical_jid: &str) -> Result<bool> {
    if alias_jid == canonical_jid {
        return Ok(false);
    }
    for (source, destination) in [
        (
            avatar_path(directory, alias_jid),
            avatar_path(directory, canonical_jid),
        ),
        (
            avatar_missing_path(directory, alias_jid),
            avatar_missing_path(directory, canonical_jid),
        ),
    ] {
        if source.exists() && !destination.exists() {
            std::fs::copy(source, destination)?;
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn remove_avatar(directory: &Path, jid: &str) {
    let _ = std::fs::remove_file(avatar_path(directory, jid));
    let _ = std::fs::remove_file(avatar_missing_path(directory, jid));
}

fn image_bytes_are_safe(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
}

fn video_bytes_are_safe(bytes: &[u8]) -> bool {
    bytes.get(4..8) == Some(b"ftyp")
        || bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
        || bytes.starts_with(&[0x00, 0x00, 0x01, 0xba])
}

pub fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn prune_directory(directory: &Path, max_bytes: u64, preserve: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains(".part-") || name.contains(".tmp-") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then(|| (entry.path(), metadata.len(), metadata.modified().ok()))
        })
        .collect::<Vec<_>>();
    let mut total = files.iter().map(|entry| entry.1).sum::<u64>();
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|entry| entry.2);
    for (path, length, _) in files {
        if total <= max_bytes {
            break;
        }
        if path != preserve && std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(length);
        }
    }
}

pub fn prune_media_cache(directory: &Path, preserve: &Path) {
    prune_directory(directory, MAX_MEDIA_CACHE_BYTES, preserve);
}

async fn download_url(url: String) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut response = agent.get(&url).call().context("fetching profile picture")?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_AVATAR_BYTES as u64 + 1)
            .read_to_vec()
            .context("reading profile picture")?;
        if bytes.len() > MAX_AVATAR_BYTES {
            bail!("profile picture exceeds 1 MiB limit");
        }
        if !image_bytes_are_safe(&bytes) {
            bail!("profile picture is not a supported raster image");
        }
        Ok(bytes)
    })
    .await
    .context("profile-picture worker panicked")?
}

pub async fn fetch_avatar(client: Arc<Client>, directory: PathBuf, jid: Jid) -> Result<bool> {
    let raw_jid = jid.to_non_ad_string();
    let path = avatar_path(&directory, &raw_jid);
    let missing = avatar_missing_path(&directory, &raw_jid);
    // The generic picture IQ accepts both contact and group JIDs and supports
    // a short timeout. The dedicated group batch IQ can wait for the global IQ
    // timeout when even one stale group is included, delaying every avatar.
    let url = client
        .contacts()
        .get_profile_picture_with_timeout(&jid, true, Some(Duration::from_secs(6)))
        .await?
        .map(|picture| picture.url);

    let Some(url) = url.filter(|url| !url.is_empty()) else {
        let existed = path.exists();
        let _ = std::fs::remove_file(path);
        write_private_bytes(&missing, b"none\n")?;
        return Ok(existed);
    };
    let bytes = download_url(url).await?;
    write_private_bytes(&path, &bytes)?;
    prune_directory(&directory, MAX_AVATAR_CACHE_BYTES, &path);
    let _ = std::fs::remove_file(missing);
    Ok(true)
}

pub async fn download_message_image(
    client: Arc<Client>,
    image: wa::message::ImageMessage,
    path: PathBuf,
) -> Result<bool> {
    let declared = image.file_length.unwrap_or(0);
    if declared == 0 || declared > MAX_IMAGE_BYTES {
        bail!("image declares an invalid size of {declared} bytes");
    }
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() == declared) {
        return Ok(false);
    }
    let temporary = path.with_extension(format!("part-{}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = client.download_to_writer(&image, file).await;
    match result {
        Ok(mut file) => {
            let length = file.metadata()?.len();
            if length == 0 || length > MAX_IMAGE_BYTES {
                let _ = std::fs::remove_file(&temporary);
                bail!("downloaded image has invalid size {length}");
            }
            file.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let count = file.read(&mut header)?;
            if !image_bytes_are_safe(&header[..count]) {
                let _ = std::fs::remove_file(&temporary);
                bail!("downloaded media is not a supported raster image");
            }
            drop(file);
            std::fs::rename(&temporary, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(anyhow!(error)).context("downloading WhatsApp image")
        }
    }
}

pub async fn download_message_video(
    client: Arc<Client>,
    video: wa::message::VideoMessage,
    path: PathBuf,
) -> Result<bool> {
    let declared = video.file_length.unwrap_or(0);
    if declared == 0 || declared > MAX_VIDEO_BYTES {
        bail!("video declares an invalid size of {declared} bytes");
    }
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() == declared) {
        return Ok(false);
    }
    let temporary = path.with_extension(format!("part-{}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = client.download_to_writer(&video, file).await;
    match result {
        Ok(mut file) => {
            let length = file.metadata()?.len();
            if length != declared || length > MAX_VIDEO_BYTES {
                let _ = std::fs::remove_file(&temporary);
                bail!("downloaded video has invalid size {length}");
            }
            file.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let count = file.read(&mut header)?;
            if !video_bytes_are_safe(&header[..count]) {
                let _ = std::fs::remove_file(&temporary);
                bail!("downloaded media is not a supported video");
            }
            drop(file);
            std::fs::rename(&temporary, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(anyhow!(error)).context("downloading WhatsApp video")
        }
    }
}

pub async fn download_message_document(
    client: Arc<Client>,
    document: wa::message::DocumentMessage,
    path: PathBuf,
) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let declared = document.file_length.unwrap_or(0);
    if declared == 0 || declared > MAX_DOCUMENT_BYTES {
        bail!("document declares an invalid size of {declared} bytes");
    }
    let temporary = path.with_extension(format!("part-{}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = client.download_to_writer(&document, file).await;
    match result {
        Ok(file) => {
            let length = file.metadata()?.len();
            if length == 0 || length > MAX_DOCUMENT_BYTES {
                drop(file);
                let _ = std::fs::remove_file(&temporary);
                bail!("downloaded document has invalid size {length}");
            }
            drop(file);
            std::fs::rename(&temporary, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(anyhow!(error)).context("downloading WhatsApp document")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names_never_contain_jid_punctuation() {
        let path = avatar_path(Path::new("/tmp/cache"), "123-4@g.us");
        assert_eq!(path.file_name().unwrap(), "3132332d3440672e7573.img");
    }

    #[test]
    fn document_cache_names_only_retain_safe_extensions() {
        let directory = Path::new("/tmp/cache");
        let pdf = message_document_path(directory, "123@g.us", "message", "quote.PDF");
        assert_eq!(
            pdf.file_name().unwrap(),
            "31323340672e7573-6d657373616765.document.pdf"
        );
        let unsafe_name =
            message_document_path(directory, "123@g.us", "message", "x.long-extension!");
        assert_eq!(
            unsafe_name.file_name().unwrap(),
            "31323340672e7573-6d657373616765.document"
        );
        let video = message_video_path(directory, "123@g.us", "clip", Some("video/mp4"));
        assert_eq!(
            video.file_name().unwrap(),
            "31323340672e7573-636c6970.video.mp4"
        );
        assert_eq!(
            cached_video_thumbnail_path(&video)
                .unwrap()
                .file_name()
                .unwrap(),
            "31323340672e7573-636c6970.video-thumbnail.jpg"
        );
        assert!(cached_video_thumbnail_path(Path::new("/tmp/cache/not-video.mp4")).is_none());
    }

    #[test]
    fn raster_sniffing_rejects_svg() {
        assert!(image_bytes_are_safe(b"\xff\xd8\xfftest"));
        assert!(image_bytes_are_safe(b"\x89PNG\r\n\x1a\n"));
        assert!(!image_bytes_are_safe(
            b"<svg xmlns='http://www.w3.org/2000/svg'>"
        ));
        assert!(video_bytes_are_safe(b"\0\0\0\x18ftypisom"));
        assert!(video_bytes_are_safe(b"\x1a\x45\xdf\xa3webm"));
        assert!(!video_bytes_are_safe(b"<html>not a video"));
    }

    #[test]
    fn legacy_thumbnail_is_moved_out_of_the_full_image_path() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        private_dir(&media).unwrap();
        let thumbnail = b"\xff\xd8\xffsmall-preview".to_vec();
        let full_path = message_image_path(&media, "1@s.whatsapp.net", "photo");
        write_private_bytes(&full_path, &thumbnail).unwrap();

        let thumbnail_path =
            cache_message_image_thumbnail(&media, "1@s.whatsapp.net", "photo", None, Some(10_000))
                .unwrap();

        assert!(!full_path.exists());
        assert_eq!(std::fs::read(thumbnail_path).unwrap(), thumbnail);
    }

    #[test]
    fn available_avatars_decode_cache_names() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(avatar_path(directory.path(), "123-4@g.us"), b"image").unwrap();
        std::fs::write(
            avatar_missing_path(directory.path(), "missing@s.whatsapp.net"),
            b"none\n",
        )
        .unwrap();
        assert_eq!(available_avatar_jids(directory.path()), vec!["123-4@g.us"]);
    }

    #[test]
    fn chat_media_aliases_are_copied_to_the_canonical_cache_key() {
        let directory = tempfile::tempdir().unwrap();
        let alias = "100000012345678@lid";
        let canonical = "31612345678@s.whatsapp.net";
        let source = message_image_path(directory.path(), alias, "message-id");
        std::fs::write(&source, b"image").unwrap();

        let replacements = copy_chat_media_alias(directory.path(), alias, canonical).unwrap();
        let destination = message_image_path(directory.path(), canonical, "message-id");
        assert_eq!(std::fs::read(&destination).unwrap(), b"image");
        assert_eq!(
            replacements,
            vec![(
                source.to_string_lossy().into_owned(),
                destination.to_string_lossy().into_owned()
            )]
        );
    }
}
