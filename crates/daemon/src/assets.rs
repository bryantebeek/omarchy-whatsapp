use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tracing::warn;
use whatsapp_rust::prelude::{Client, Jid, wa};

pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_STICKER_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_VIDEO_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_AVATAR_BYTES: usize = 1024 * 1024;
const MAX_AVATAR_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MEDIA_CACHE_BYTES: u64 = 256 * 1024 * 1024;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct PrivateTemporaryFile {
    path: PathBuf,
    committed: bool,
}

impl PrivateTemporaryFile {
    fn create(destination: &Path, kind: &str) -> Result<(Self, File)> {
        Self::create_with_sequence(destination, kind, &TEMPORARY_FILE_SEQUENCE)
    }

    fn create_with_sequence(
        destination: &Path,
        kind: &str,
        sequence_source: &AtomicU64,
    ) -> Result<(Self, File)> {
        let parent = destination
            .parent()
            .context("private destination has no parent directory")?;
        let file_name = destination
            .file_name()
            .context("private destination has no file name")?;
        for _ in 0..32 {
            let sequence = sequence_source.fetch_add(1, Ordering::Relaxed);
            let mut temporary_name = OsString::from(file_name);
            temporary_name.push(format!(".{kind}-{}-{sequence}", std::process::id()));
            let path = parent.join(temporary_name);
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    return Ok((
                        Self {
                            path,
                            committed: false,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not allocate a unique private temporary file")
    }

    fn commit(mut self, file: File, destination: &Path) -> Result<()> {
        file.sync_all()?;
        drop(file);
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&self.path, destination)?;
        self.committed = true;
        sync_parent_directory(destination)?;
        Ok(())
    }

    fn commit_existing(self, destination: &Path) -> Result<()> {
        let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        self.commit(file, destination)
    }
}

impl Drop for PrivateTemporaryFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("private destination has no parent directory")?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn is_temporary_name(name: &str) -> bool {
    name.contains(".part-") || name.contains(".tmp-")
}

fn remove_abandoned_temporary_files(directory: &Path) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && is_temporary_name(&entry.file_name().to_string_lossy()) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    // Only startup/reset calls this helper. No transfer can still own these
    // names, so cleaning all recognized temporary files is safe and prevents
    // crash leftovers from escaping cache accounting forever.
    remove_abandoned_temporary_files(path)?;
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

pub type AvatarFingerprint = (u64, u64, i64, i64);

pub fn avatar_fingerprints(directory: &Path) -> BTreeMap<String, AvatarFingerprint> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return BTreeMap::new();
    };
    entries
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
            let jid = String::from_utf8(bytes).ok()?;
            let metadata = entry.metadata().ok()?;
            Some((
                jid,
                (
                    metadata.ino(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                ),
            ))
        })
        .collect()
}

pub fn available_avatar_jids(directory: &Path) -> Vec<String> {
    avatar_fingerprints(directory).into_keys().collect()
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

pub fn message_sticker_path(directory: &Path, chat_jid: &str, message_id: &str) -> PathBuf {
    directory.join(format!(
        "{}-{}.sticker.webp",
        hex_key(chat_jid),
        hex_key(message_id)
    ))
}

pub fn message_sticker_thumbnail_path(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
) -> PathBuf {
    directory.join(format!(
        "{}-{}.sticker-thumbnail.png",
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

fn audio_extension(mime_type: Option<&str>) -> &'static str {
    match mime_type
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/ogg" => ".ogg",
        "audio/mpeg" => ".mp3",
        "audio/mp4" | "audio/x-m4a" => ".m4a",
        "audio/aac" => ".aac",
        "audio/wav" | "audio/x-wav" => ".wav",
        _ => "",
    }
}

pub fn message_audio_path(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    mime_type: Option<&str>,
) -> PathBuf {
    directory.join(format!(
        "{}-{}.audio{}",
        hex_key(chat_jid),
        hex_key(message_id),
        audio_extension(mime_type)
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
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    if !(3..=MAX_IMAGE_BYTES).contains(&length) {
        return false;
    }
    let mut header = [0u8; 3];
    file.read_exact(&mut header).is_ok() && header == [0xff, 0xd8, 0xff]
}

// The ffmpeg process boundary is verified by release smoke tests. Cache naming,
// validation, migration, and pruning around it remain coverage-instrumented.
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn ensure_message_video_thumbnail(path: &Path, thumbnail_path: &Path) -> Result<bool> {
    if jpeg_file_is_valid(thumbnail_path) {
        return Ok(false);
    }
    if !path.is_file() {
        return Ok(false);
    }

    let (temporary, reserved_file) = PrivateTemporaryFile::create(thumbnail_path, "part")?;
    drop(reserved_file);
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
            "-c:v",
            "mjpeg",
            "-f",
            "image2",
        ])
        .arg(&temporary.path)
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
            bail!("timed out creating a video preview");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if !status.success() || !jpeg_file_is_valid(&temporary.path) {
        bail!("ffmpeg did not create a valid video preview");
    }

    temporary.commit_existing(thumbnail_path)?;
    if let Some(directory) = thumbnail_path.parent() {
        prune_media_cache(directory, thumbnail_path);
    }
    Ok(true)
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

pub fn cache_message_sticker_thumbnail(
    directory: &Path,
    chat_jid: &str,
    message_id: &str,
    thumbnail: Option<&Vec<u8>>,
) -> Result<PathBuf> {
    let thumbnail_path = message_sticker_thumbnail_path(directory, chat_jid, message_id);
    if !thumbnail_path.exists()
        && let Some(thumbnail) = thumbnail.filter(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
    {
        write_private_bytes(&thumbnail_path, thumbnail)?;
    }
    prune_media_cache(directory, &thumbnail_path);
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
    let _ = std::fs::remove_file(message_sticker_path(directory, chat_jid, message_id));
    let _ = std::fs::remove_file(message_sticker_thumbnail_path(
        directory, chat_jid, message_id,
    ));
    let _ = std::fs::remove_file(location_thumbnail_path(directory, chat_jid, message_id));
    let document_prefix = format!("{}-{}.document", hex_key(chat_jid), hex_key(message_id));
    let video_prefix = format!("{}-{}.video", hex_key(chat_jid), hex_key(message_id));
    let audio_prefix = format!("{}-{}.audio", hex_key(chat_jid), hex_key(message_id));
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&document_prefix)
                || name.starts_with(&video_prefix)
                || name.starts_with(&audio_prefix)
            {
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

fn sticker_bytes_are_safe(bytes: &[u8]) -> bool {
    bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
}

fn video_bytes_are_safe(bytes: &[u8]) -> bool {
    bytes.get(4..8) == Some(b"ftyp")
        || bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
        || bytes.starts_with(&[0x00, 0x00, 0x01, 0xba])
}

fn audio_bytes_are_safe(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS")
        || bytes.starts_with(b"ID3")
        || bytes.starts_with(b"ADIF")
        || bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE")
        || bytes.get(4..8) == Some(b"ftyp")
        || matches!(bytes, [0xff, second, ..] if second & 0xe0 == 0xe0)
}

pub fn copy_private_file(source: &Path, destination: &Path) -> Result<()> {
    let (temporary, mut output) = PrivateTemporaryFile::create(destination, "tmp")?;
    let mut input = File::open(source)?;
    std::io::copy(&mut input, &mut output)?;
    temporary.commit(output, destination)
}

pub fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let (temporary, mut file) = PrivateTemporaryFile::create(path, "tmp")?;
    std::io::Write::write_all(&mut file, bytes)?;
    temporary.commit(file, path)
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
            if is_temporary_name(&name) {
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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
    let (temporary, file) = PrivateTemporaryFile::create(&path, "part")?;
    let result = client.download_to_writer(&image, file).await;
    match result {
        Ok(mut file) => {
            let length = file.metadata()?.len();
            if length == 0 || length > MAX_IMAGE_BYTES {
                bail!("downloaded image has invalid size {length}");
            }
            file.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let count = file.read(&mut header)?;
            if !image_bytes_are_safe(&header[..count]) {
                bail!("downloaded media is not a supported raster image");
            }
            temporary.commit(file, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => Err(anyhow!(error)).context("downloading WhatsApp image"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn download_message_sticker(
    client: Arc<Client>,
    sticker: wa::message::StickerMessage,
    path: PathBuf,
) -> Result<bool> {
    let declared = sticker.file_length.unwrap_or(0);
    if declared == 0 || declared > MAX_STICKER_BYTES {
        bail!("sticker declares an invalid size of {declared} bytes");
    }
    if sticker.is_lottie.unwrap_or(false) {
        bail!("Lottie stickers cannot be decoded safely by the shell");
    }
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() == declared) {
        return Ok(false);
    }
    let (temporary, file) = PrivateTemporaryFile::create(&path, "part")?;
    let result = client.download_to_writer(&sticker, file).await;
    match result {
        Ok(mut file) => {
            let length = file.metadata()?.len();
            if length != declared || length > MAX_STICKER_BYTES {
                bail!("downloaded sticker has invalid size {length}");
            }
            file.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let count = file.read(&mut header)?;
            if !sticker_bytes_are_safe(&header[..count]) {
                bail!("downloaded sticker is not WebP media");
            }
            temporary.commit(file, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => Err(anyhow!(error)).context("downloading WhatsApp sticker"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
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
    let (temporary, file) = PrivateTemporaryFile::create(&path, "part")?;
    let result = client.download_to_writer(&video, file).await;
    match result {
        Ok(mut file) => {
            let length = file.metadata()?.len();
            if length != declared || length > MAX_VIDEO_BYTES {
                bail!("downloaded video has invalid size {length}");
            }
            file.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let count = file.read(&mut header)?;
            if !video_bytes_are_safe(&header[..count]) {
                bail!("downloaded media is not a supported video");
            }
            temporary.commit(file, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => Err(anyhow!(error)).context("downloading WhatsApp video"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn download_message_audio(
    client: Arc<Client>,
    audio: wa::message::AudioMessage,
    path: PathBuf,
) -> Result<bool> {
    let declared = audio.file_length.unwrap_or(0);
    if declared == 0 || declared > MAX_AUDIO_BYTES {
        bail!("audio declares an invalid size of {declared} bytes");
    }
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() == declared) {
        return Ok(false);
    }
    let (temporary, file) = PrivateTemporaryFile::create(&path, "part")?;
    let result = client.download_to_writer(&audio, file).await;
    match result {
        Ok(mut file) => {
            let length = file.metadata()?.len();
            if length != declared || length > MAX_AUDIO_BYTES {
                bail!("downloaded audio has invalid size {length}");
            }
            file.seek(SeekFrom::Start(0))?;
            let mut header = [0u8; 16];
            let count = file.read(&mut header)?;
            if !audio_bytes_are_safe(&header[..count]) {
                bail!("downloaded media is not supported audio");
            }
            temporary.commit(file, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => Err(anyhow!(error)).context("downloading WhatsApp audio"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
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
    let (temporary, file) = PrivateTemporaryFile::create(&path, "part")?;
    let result = client.download_to_writer(&document, file).await;
    match result {
        Ok(file) => {
            let length = file.metadata()?.len();
            if length == 0 || length > MAX_DOCUMENT_BYTES {
                bail!("downloaded document has invalid size {length}");
            }
            temporary.commit(file, &path)?;
            if let Some(directory) = path.parent() {
                prune_directory(directory, MAX_MEDIA_CACHE_BYTES, &path);
            }
            Ok(true)
        }
        Err(error) => Err(anyhow!(error)).context("downloading WhatsApp document"),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn cache_names_never_contain_jid_punctuation() {
        let path = avatar_path(Path::new("/tmp/cache"), "123-4@g.us");
        assert_eq!(path.file_name().unwrap(), "3132332d3440672e7573.img");
    }

    #[test]
    fn sticker_cache_uses_webp_media_and_valid_png_previews() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        private_dir(&media).unwrap();
        let sticker_path = message_sticker_path(&media, "123-4@g.us", "sticker");
        assert_eq!(
            sticker_path.file_name().unwrap(),
            "3132332d3440672e7573-737469636b6572.sticker.webp"
        );
        assert!(sticker_bytes_are_safe(b"RIFF\x10\0\0\0WEBPVP8 "));
        assert!(!sticker_bytes_are_safe(b"\x89PNG\r\n\x1a\n"));

        let thumbnail = b"\x89PNG\r\n\x1a\nsynthetic".to_vec();
        let thumbnail_path =
            cache_message_sticker_thumbnail(&media, "123-4@g.us", "sticker", Some(&thumbnail))
                .unwrap();
        assert_eq!(std::fs::read(&thumbnail_path).unwrap(), thumbnail);
        assert_eq!(
            std::fs::metadata(thumbnail_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
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
        let audio = message_audio_path(
            directory,
            "123@g.us",
            "note",
            Some("audio/ogg; codecs=opus"),
        );
        assert_eq!(
            audio.file_name().unwrap(),
            "31323340672e7573-6e6f7465.audio.ogg"
        );
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
        assert!(audio_bytes_are_safe(b"OggS\0\x02voice"));
        assert!(!audio_bytes_are_safe(b"<html>not audio"));
    }

    #[test]
    fn private_file_copy_preserves_source_and_restricts_destination() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.ogg");
        let destination = directory.path().join("destination.ogg");
        std::fs::write(&source, b"voice").unwrap();
        copy_private_file(&source, &destination).unwrap();
        assert_eq!(std::fs::read(source).unwrap(), b"voice");
        assert_eq!(std::fs::read(&destination).unwrap(), b"voice");
        assert_eq!(
            std::fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn private_directory_removes_only_abandoned_transfer_files() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        std::fs::create_dir(&media).unwrap();
        let complete = media.join("complete.img");
        let temporary = media.join("image.img.part-123-4");
        let metadata = media.join("job.json.tmp-123-5");
        std::fs::write(&complete, b"complete").unwrap();
        std::fs::write(&temporary, b"partial").unwrap();
        std::fs::write(&metadata, b"partial").unwrap();

        private_dir(&media).unwrap();

        assert_eq!(std::fs::read(complete).unwrap(), b"complete");
        assert!(!temporary.exists());
        assert!(!metadata.exists());
    }

    #[test]
    fn concurrent_atomic_writes_do_not_share_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let destination = Arc::new(directory.path().join("state.json"));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles = (0..8)
            .map(|index| {
                let destination = Arc::clone(&destination);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let contents = format!("value-{index}");
                    barrier.wait();
                    write_private_bytes(&destination, contents.as_bytes()).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let contents = std::fs::read_to_string(&*destination).unwrap();
        assert!((0..8).any(|index| contents == format!("value-{index}")));
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .all(|entry| !is_temporary_name(&entry.unwrap().file_name().to_string_lossy()))
        );
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
    fn avatar_fingerprints_change_after_an_atomic_replacement() {
        let directory = tempfile::tempdir().unwrap();
        assert!(avatar_fingerprints(&directory.path().join("missing")).is_empty());
        let jid = "123-4@g.us";
        let path = avatar_path(directory.path(), jid);
        write_private_bytes(&path, b"first").unwrap();
        let first = avatar_fingerprints(directory.path());

        write_private_bytes(&path, b"second").unwrap();
        let second = avatar_fingerprints(directory.path());

        assert_eq!(
            first.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![jid]
        );
        assert_eq!(
            second.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![jid]
        );
        assert_ne!(first[jid], second[jid]);
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

    #[test]
    fn extension_and_media_signature_matrix_is_explicit() {
        let directory = Path::new("/tmp/cache");
        for (mime, suffix) in [
            (Some("video/quicktime"), ".mov"),
            (Some("video/webm"), ".webm"),
            (Some("video/3gpp"), ".3gp"),
            (Some("application/octet-stream"), ""),
        ] {
            assert!(
                message_video_path(directory, "chat", "id", mime)
                    .to_string_lossy()
                    .ends_with(suffix)
            );
        }
        for (mime, suffix) in [
            (Some("audio/mpeg"), ".mp3"),
            (Some("audio/mp4"), ".m4a"),
            (Some("audio/x-m4a"), ".m4a"),
            (Some("audio/aac"), ".aac"),
            (Some("audio/wav"), ".wav"),
            (Some("audio/x-wav"), ".wav"),
            (Some("application/octet-stream"), ""),
        ] {
            assert!(
                message_audio_path(directory, "chat", "id", mime)
                    .to_string_lossy()
                    .ends_with(suffix)
            );
        }

        assert!(image_bytes_are_safe(b"RIFF\x04\0\0\0WEBP"));
        assert!(image_bytes_are_safe(b"GIF87a"));
        assert!(image_bytes_are_safe(b"GIF89a"));
        assert!(video_bytes_are_safe(&[0, 0, 1, 0xba]));
        assert!(audio_bytes_are_safe(b"ID3audio"));
        assert!(audio_bytes_are_safe(b"ADIFaudio"));
        assert!(audio_bytes_are_safe(b"RIFF\x04\0\0\0WAVE"));
        assert!(audio_bytes_are_safe(b"\0\0\0\x18ftyp"));
        assert!(audio_bytes_are_safe(&[0xff, 0xe0]));
    }

    #[test]
    fn thumbnail_cache_handles_every_legacy_layout() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path();
        let thumbnail = b"\xff\xd8\xffpreview".to_vec();

        let full = message_image_path(media, "chat", "remove-preview");
        let thumb = message_image_thumbnail_path(media, "chat", "remove-preview");
        std::fs::write(&full, &thumbnail).unwrap();
        std::fs::write(&thumb, b"existing").unwrap();
        cache_message_image_thumbnail(
            media,
            "chat",
            "remove-preview",
            Some(&thumbnail),
            Some(10_000),
        )
        .unwrap();
        assert!(!full.exists());
        assert_eq!(std::fs::read(thumb).unwrap(), b"existing");

        let written = cache_message_video_thumbnail(
            media,
            "chat",
            "write-preview",
            Some("video/webm"),
            Some(&thumbnail),
            None,
        )
        .unwrap();
        assert_eq!(std::fs::read(written).unwrap(), thumbnail);

        let invalid = vec![1, 2, 3];
        let ignored =
            cache_message_image_thumbnail(media, "chat", "invalid-preview", Some(&invalid), None)
                .unwrap();
        assert!(!ignored.exists());

        let png = b"\x89PNG\r\n\x1a\npreview".to_vec();
        let sticker = cache_message_sticker_thumbnail(media, "chat", "once", Some(&png)).unwrap();
        cache_message_sticker_thumbnail(media, "chat", "once", Some(&vec![0; 8])).unwrap();
        assert_eq!(std::fs::read(sticker).unwrap(), png);

        let missing_parent = media.join("missing-parent");
        assert!(
            cache_message_image_thumbnail(
                &missing_parent,
                "chat",
                "image-error",
                Some(&thumbnail),
                None,
            )
            .is_err()
        );
        assert!(
            cache_message_video_thumbnail(
                &missing_parent,
                "chat",
                "video-error",
                Some("video/mp4"),
                Some(&thumbnail),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn cleanup_alias_and_pruning_operations_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path();
        let chat = "chat@s.whatsapp.net";
        let id = "message";
        for path in [
            message_image_path(media, chat, id),
            message_image_thumbnail_path(media, chat, id),
            message_video_path(media, chat, id, Some("video/mp4")),
            message_video_thumbnail_path(media, chat, id),
            message_audio_path(media, chat, id, Some("audio/ogg")),
            message_sticker_path(media, chat, id),
            message_sticker_thumbnail_path(media, chat, id),
            message_document_path(media, chat, id, "file.pdf"),
            location_thumbnail_path(media, chat, id),
        ] {
            std::fs::write(path, b"x").unwrap();
        }
        remove_message_media(media, chat, id);
        assert!(std::fs::read_dir(media).unwrap().next().is_none());
        remove_message_media(&media.join("missing"), chat, id);

        let keep = message_image_path(media, "other", "one");
        let remove = message_image_path(media, chat, "two");
        std::fs::write(&keep, b"keep").unwrap();
        std::fs::write(&remove, b"remove").unwrap();
        remove_chat_media(media, chat);
        assert!(keep.exists());
        assert!(!remove.exists());
        remove_chat_media(&media.join("missing"), chat);

        assert!(copy_chat_media_alias(media, chat, chat).unwrap().is_empty());
        std::fs::write(message_image_path(media, "alias", "one"), b"alias").unwrap();
        std::fs::write(message_image_path(media, "canonical", "one"), b"canonical").unwrap();
        std::fs::write(media.join("unrelated"), b"unrelated").unwrap();
        let replacements = copy_chat_media_alias(media, "alias", "canonical").unwrap();
        assert_eq!(replacements.len(), 1);
        assert_eq!(
            std::fs::read(message_image_path(media, "canonical", "one")).unwrap(),
            b"canonical"
        );

        let invalid_name = std::ffi::OsString::from_vec(vec![0xff]);
        std::fs::write(media.join(invalid_name), b"invalid name").unwrap();
        assert_eq!(
            copy_chat_media_alias(media, "none", "other").unwrap(),
            Vec::new()
        );

        assert!(!copy_avatar_alias(media, chat, chat).unwrap());
        std::fs::write(avatar_missing_path(media, "alias-avatar"), b"none\n").unwrap();
        assert!(copy_avatar_alias(media, "alias-avatar", "canonical-avatar").unwrap());
        assert!(!copy_avatar_alias(media, "missing-avatar", "canonical-avatar").unwrap());
        remove_avatar(media, "alias-avatar");
        remove_avatar(media, "canonical-avatar");

        let old = media.join("old");
        let preserve = media.join("preserve");
        let temporary = media.join("skip.tmp-1-1");
        std::fs::write(&old, [0; 8]).unwrap();
        std::fs::write(&preserve, [0; 8]).unwrap();
        std::fs::write(&temporary, [0; 8]).unwrap();
        prune_directory(media, 8, &preserve);
        assert!(preserve.exists());
        assert!(temporary.exists());
        assert!(!old.exists());
        prune_directory(&media.join("missing"), 0, &preserve);

        let bounded = tempfile::tempdir().unwrap();
        let first = bounded.path().join("first");
        let second = bounded.path().join("second");
        std::fs::write(&first, [0; 8]).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        std::fs::write(&second, [0; 8]).unwrap();
        prune_directory(bounded.path(), 8, Path::new("/not-preserved"));
        assert!(!first.exists());
        assert!(second.exists());
    }

    #[test]
    fn private_temporary_files_clean_up_and_reject_exhaustion() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("destination");
        let (temporary, mut file) = PrivateTemporaryFile::create(&destination, "part").unwrap();
        std::io::Write::write_all(&mut file, b"committed").unwrap();
        drop(file);
        temporary.commit_existing(&destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"committed");

        let (temporary, _file) = PrivateTemporaryFile::create(&destination, "drop").unwrap();
        let temporary_path = temporary.path.clone();
        drop(temporary);
        assert!(!temporary_path.exists());

        let sequence_source = AtomicU64::new(9000);
        let sequence = sequence_source.load(Ordering::Relaxed);
        for offset in 0..32_u64 {
            let path = directory.path().join(format!(
                "destination.full-{}-{}",
                std::process::id(),
                sequence + offset
            ));
            std::fs::write(path, b"occupied").unwrap();
        }
        assert!(
            PrivateTemporaryFile::create_with_sequence(&destination, "full", &sequence_source,)
                .is_err()
        );

        let not_directory = directory.path().join("not-directory");
        std::fs::write(&not_directory, b"file").unwrap();
        assert!(PrivateTemporaryFile::create(&not_directory.join("child"), "bad").is_err());
    }

    #[test]
    fn avatar_and_jpeg_validation_ignore_malformed_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("f.img"), b"odd").unwrap();
        std::fs::write(directory.path().join("zz.img"), b"invalid").unwrap();
        assert!(avatar_fingerprints(directory.path()).is_empty());

        let jpeg = directory.path().join("preview.jpg");
        assert!(!jpeg_file_is_valid(&jpeg));
        std::fs::write(&jpeg, b"no").unwrap();
        assert!(!jpeg_file_is_valid(&jpeg));
        std::fs::write(&jpeg, b"\xff\xd8\xffok").unwrap();
        assert!(jpeg_file_is_valid(&jpeg));
        let oversized = directory.path().join("oversized.jpg");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        assert!(!jpeg_file_is_valid(&oversized));
    }
}
