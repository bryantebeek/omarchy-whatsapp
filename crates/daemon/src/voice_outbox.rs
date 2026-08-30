use crate::assets;
use anyhow::{Context, Result, bail};
use omarchy_whatsapp_protocol::{VoiceOutboxEntry, VoiceOutboxStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tracing::warn;

const JOB_VERSION: u8 = 1;
const MIN_DURATION_MS: u64 = 250;
const MAX_DURATION_MS: u64 = 15 * 60 * 1_000;
const ORPHAN_TTL_SECONDS: i64 = 24 * 60 * 60;
const JOB_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_JOBS: usize = 8;
const MAX_OUTBOX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ERROR_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StoredStatus {
    Sending,
    Failed,
    Sent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceJob {
    version: u8,
    pub recording_id: String,
    pub chat_jid: String,
    pub duration_ms: u64,
    pub message_id: Option<String>,
    status: StoredStatus,
    error: Option<String>,
    attempts: u32,
    pub created_at: i64,
    updated_at: i64,
}

impl VoiceJob {
    fn entry(&self) -> Option<VoiceOutboxEntry> {
        let status = match self.status {
            StoredStatus::Sending => VoiceOutboxStatus::Sending,
            StoredStatus::Failed => VoiceOutboxStatus::Failed,
            StoredStatus::Sent => return None,
        };
        Some(VoiceOutboxEntry {
            recording_id: self.recording_id.clone(),
            chat_jid: self.chat_jid.clone(),
            duration_ms: self.duration_ms,
            status,
            error: self.error.clone(),
            created_at: self.created_at,
        })
    }
}

#[derive(Debug)]
pub struct PreparedVoiceJob {
    pub job: VoiceJob,
    pub bytes: Vec<u8>,
}

pub fn recording_id_is_valid(recording_id: &str) -> bool {
    (1..=80).contains(&recording_id.len())
        && !recording_id.starts_with('-')
        && !recording_id.ends_with('-')
        && recording_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

pub fn recording_path(outbox_dir: &Path, recording_id: &str) -> Result<PathBuf> {
    if !recording_id_is_valid(recording_id) {
        bail!("voice recording ID is invalid");
    }
    Ok(outbox_dir.join(format!("voice-{recording_id}.ogg")))
}

fn job_path(outbox_dir: &Path, recording_id: &str) -> Result<PathBuf> {
    if !recording_id_is_valid(recording_id) {
        bail!("voice recording ID is invalid");
    }
    Ok(outbox_dir.join(format!("voice-{recording_id}.json")))
}

fn checked_recording_bytes(
    outbox_dir: &Path,
    recording_id: &str,
) -> Result<(PathBuf, Vec<u8>, u64)> {
    let path = recording_path(outbox_dir, recording_id)?;
    let canonical_outbox = outbox_dir
        .canonicalize()
        .context("resolving the voice recording outbox")?;
    let canonical = path
        .canonicalize()
        .context("resolving the voice recording")?;
    if canonical.parent() != Some(canonical_outbox.as_path()) {
        bail!("voice recording is outside the private outbox");
    }
    let metadata = canonical
        .metadata()
        .context("reading voice recording metadata")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > assets::MAX_AUDIO_BYTES {
        bail!("voice recording has an invalid size");
    }
    let bytes = fs::read(&canonical).context("reading voice recording")?;
    let duration_ms = ogg_opus_duration_ms(&bytes)?;
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600))?;
    Ok((canonical, bytes, duration_ms))
}

pub fn prepare(
    outbox_dir: &Path,
    recording_id: &str,
    chat_jid: &str,
    now: i64,
) -> Result<PreparedVoiceJob> {
    if chat_jid.is_empty() {
        bail!("voice message chat is empty");
    }
    cleanup_inner(outbox_dir, now, Some(recording_id))?;
    let (_, bytes, duration_ms) = checked_recording_bytes(outbox_dir, recording_id)?;
    let metadata_path = job_path(outbox_dir, recording_id)?;
    let mut job = if metadata_path.is_file() {
        let stored = fs::read(&metadata_path).context("reading voice outbox metadata")?;
        let job: VoiceJob =
            serde_json::from_slice(&stored).context("parsing voice outbox metadata")?;
        validate_job(&job, recording_id)?;
        job
    } else {
        VoiceJob {
            version: JOB_VERSION,
            recording_id: recording_id.to_owned(),
            chat_jid: chat_jid.to_owned(),
            duration_ms,
            message_id: None,
            status: StoredStatus::Sending,
            error: None,
            attempts: 0,
            created_at: now,
            updated_at: now,
        }
    };
    if job.duration_ms != duration_ms {
        bail!("voice recording changed after it entered the outbox");
    }
    job.status = StoredStatus::Sending;
    job.error = None;
    job.attempts = job.attempts.saturating_add(1);
    job.updated_at = now;
    save_job(outbox_dir, &job)?;
    Ok(PreparedVoiceJob { job, bytes })
}

pub fn assign_delivery(
    outbox_dir: &Path,
    job: &mut VoiceJob,
    chat_jid: &str,
    message_id: &str,
    now: i64,
) -> Result<()> {
    if chat_jid.is_empty() || message_id.is_empty() {
        bail!("voice message delivery identity is incomplete");
    }
    chat_jid.clone_into(&mut job.chat_jid);
    match &job.message_id {
        Some(existing) if existing != message_id => {
            bail!("voice message already has a different delivery ID");
        }
        Some(_) => {}
        None => job.message_id = Some(message_id.to_owned()),
    }
    job.updated_at = now;
    save_job(outbox_dir, job)
}

pub fn mark_failed(outbox_dir: &Path, job: &mut VoiceJob, error: &str, now: i64) -> Result<()> {
    job.status = StoredStatus::Failed;
    job.error = Some(truncated_error(error));
    job.updated_at = now;
    save_job(outbox_dir, job)
}

pub fn finish_sent(outbox_dir: &Path, job: &mut VoiceJob, now: i64) -> Result<()> {
    job.status = StoredStatus::Sent;
    job.error = None;
    job.updated_at = now;
    save_job(outbox_dir, job)?;
    remove_job_files(outbox_dir, &job.recording_id)
}

pub fn discard(outbox_dir: &Path, recording_id: &str) -> Result<()> {
    remove_job_files(outbox_dir, recording_id)
}

pub fn recover_interrupted(outbox_dir: &Path, now: i64) -> Result<Vec<VoiceOutboxEntry>> {
    cleanup(outbox_dir, now)?;
    let mut entries = Vec::new();
    for mut job in load_jobs(outbox_dir)? {
        if job.status == StoredStatus::Sent {
            let _ = remove_job_files(outbox_dir, &job.recording_id);
            continue;
        }
        if job.status == StoredStatus::Sending {
            job.status = StoredStatus::Failed;
            job.error = Some("Send was interrupted; retry is safe".into());
            job.updated_at = now;
            save_job(outbox_dir, &job)?;
        }
        if let Some(entry) = job.entry() {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|entry| entry.created_at);
    Ok(entries)
}

pub fn entries(outbox_dir: &Path) -> Result<Vec<VoiceOutboxEntry>> {
    let mut entries: Vec<_> = load_jobs(outbox_dir)?
        .into_iter()
        .filter_map(|job| job.entry())
        .collect();
    entries.sort_by_key(|entry| entry.created_at);
    Ok(entries)
}

pub fn cleanup(outbox_dir: &Path, now: i64) -> Result<()> {
    cleanup_inner(outbox_dir, now, None)
}

fn cleanup_inner(outbox_dir: &Path, now: i64, preserve: Option<&str>) -> Result<()> {
    let mut audio = HashMap::new();
    let mut jobs = HashMap::new();
    for item in fs::read_dir(outbox_dir).context("reading voice outbox")? {
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((recording_id, kind)) = outbox_name(name) else {
            continue;
        };
        let metadata = item.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .and_then(|value| i64::try_from(value.as_secs()).ok())
            .unwrap_or(now);
        let value = (item.path(), metadata.len(), modified);
        if kind == "ogg" {
            audio.insert(recording_id.to_owned(), value);
        } else {
            jobs.insert(recording_id.to_owned(), value);
        }
    }

    for (recording_id, (path, _, modified)) in &audio {
        if !jobs.contains_key(recording_id) && now.saturating_sub(*modified) > ORPHAN_TTL_SECONDS {
            let _ = fs::remove_file(path);
        }
    }
    for (recording_id, (path, _, _)) in &jobs {
        if !audio.contains_key(recording_id) {
            let _ = fs::remove_file(path);
        }
    }

    let mut retained = Vec::new();
    for job in load_jobs(outbox_dir)? {
        let audio_size = audio.get(&job.recording_id).map_or(0, |value| value.1);
        if preserve != Some(job.recording_id.as_str())
            && now.saturating_sub(job.updated_at) > JOB_TTL_SECONDS
        {
            let _ = remove_job_files(outbox_dir, &job.recording_id);
        } else {
            retained.push((job.created_at, job.recording_id, audio_size));
        }
    }
    retained.sort_by_key(|value| value.0);
    let mut total_bytes: u64 = retained.iter().map(|value| value.2).sum();
    while retained.len() > MAX_JOBS || total_bytes > MAX_OUTBOX_BYTES {
        let Some(index) = retained
            .iter()
            .position(|value| preserve != Some(value.1.as_str()))
        else {
            break;
        };
        let (_, recording_id, size) = retained.remove(index);
        total_bytes = total_bytes.saturating_sub(size);
        let _ = remove_job_files(outbox_dir, &recording_id);
    }
    Ok(())
}

fn load_jobs(outbox_dir: &Path) -> Result<Vec<VoiceJob>> {
    let mut jobs = Vec::new();
    for item in fs::read_dir(outbox_dir).context("reading voice outbox")? {
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((recording_id, "json")) = outbox_name(name) else {
            continue;
        };
        let result = (|| -> Result<VoiceJob> {
            let bytes = fs::read(item.path()).context("reading voice outbox metadata")?;
            let job: VoiceJob =
                serde_json::from_slice(&bytes).context("parsing voice outbox metadata")?;
            validate_job(&job, recording_id)?;
            Ok(job)
        })();
        match result {
            Ok(job) => jobs.push(job),
            Err(error) => {
                warn!(%error, recording_id, "discarding invalid voice outbox metadata");
                let _ = remove_job_files(outbox_dir, recording_id);
            }
        }
    }
    Ok(jobs)
}

fn validate_job(job: &VoiceJob, recording_id: &str) -> Result<()> {
    if job.version != JOB_VERSION
        || job.recording_id != recording_id
        || !recording_id_is_valid(&job.recording_id)
        || job.chat_jid.is_empty()
        || !(MIN_DURATION_MS..=MAX_DURATION_MS).contains(&job.duration_ms)
        || job.message_id.as_ref().is_some_and(String::is_empty)
    {
        bail!("voice outbox metadata is invalid");
    }
    Ok(())
}

fn save_job(outbox_dir: &Path, job: &VoiceJob) -> Result<()> {
    validate_job(job, &job.recording_id)?;
    let bytes = serde_json::to_vec(job).context("serializing voice outbox metadata")?;
    assets::write_private_bytes(&job_path(outbox_dir, &job.recording_id)?, &bytes)
}

fn remove_job_files(outbox_dir: &Path, recording_id: &str) -> Result<()> {
    for path in [
        recording_path(outbox_dir, recording_id)?,
        job_path(outbox_dir, recording_id)?,
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", path.display()));
            }
        }
    }
    Ok(())
}

fn outbox_name(name: &str) -> Option<(&str, &str)> {
    let value = name.strip_prefix("voice-")?;
    for extension in ["ogg", "json"] {
        if let Some(recording_id) = value.strip_suffix(&format!(".{extension}"))
            && recording_id_is_valid(recording_id)
        {
            return Some((recording_id, extension));
        }
    }
    None
}

fn truncated_error(error: &str) -> String {
    error.chars().take(MAX_ERROR_CHARS).collect()
}

fn ogg_opus_duration_ms(bytes: &[u8]) -> Result<u64> {
    let mut offset = 0usize;
    let mut stream_serial: Option<u32> = None;
    let mut pre_skip: Option<u64> = None;
    let mut last_granule: Option<u64> = None;
    let mut sequence: Option<u32> = None;
    while offset < bytes.len() {
        let header = bytes
            .get(offset..offset.saturating_add(27))
            .context("voice recording has a truncated Ogg header")?;
        if &header[..4] != b"OggS" || header[4] != 0 {
            bail!("voice recording is not a supported Ogg stream");
        }
        let segment_count = usize::from(header[26]);
        let segment_table = bytes
            .get(offset + 27..offset + 27 + segment_count)
            .context("voice recording has a truncated Ogg segment table")?;
        let body_len = segment_table
            .iter()
            .try_fold(0usize, |total, value| {
                total.checked_add(usize::from(*value))
            })
            .context("voice recording Ogg page is too large")?;
        let body_start = offset + 27 + segment_count;
        let body = bytes
            .get(body_start..body_start + body_len)
            .context("voice recording has a truncated Ogg page")?;
        let serial = u32::from_le_bytes(header[14..18].try_into().expect("fixed Ogg serial"));
        let page_sequence =
            u32::from_le_bytes(header[18..22].try_into().expect("fixed Ogg sequence"));
        if let Some(expected) = stream_serial {
            if serial != expected
                || sequence.is_some_and(|value| value.checked_add(1) != Some(page_sequence))
            {
                bail!("voice recording has an inconsistent Ogg stream");
            }
        } else {
            stream_serial = Some(serial);
            if body.len() < 19 || !body.starts_with(b"OpusHead") {
                bail!("voice recording does not begin with an Opus header");
            }
            if body[8] != 1 || body[9] == 0 {
                bail!("voice recording uses an unsupported Opus header");
            }
            pre_skip = Some(u64::from(u16::from_le_bytes([body[10], body[11]])));
        }
        sequence = Some(page_sequence);
        let granule = u64::from_le_bytes(header[6..14].try_into().expect("fixed Ogg granule"));
        if granule != u64::MAX {
            last_granule = Some(granule);
        }
        offset = body_start + body_len;
    }
    let samples = last_granule
        .context("voice recording has no Opus duration")?
        .checked_sub(pre_skip.context("voice recording has no Opus pre-skip")?)
        .context("voice recording has an invalid Opus duration")?;
    let duration_ms = samples
        .checked_mul(1_000)
        .context("voice recording duration overflow")?
        .div_ceil(48_000);
    if !(MIN_DURATION_MS..=MAX_DURATION_MS).contains(&duration_ms) {
        bail!("voice recording duration is outside the supported range");
    }
    Ok(duration_ms)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn ogg_page(sequence: u32, granule: u64, body: &[u8]) -> Vec<u8> {
        assert!(body.len() <= 255);
        let mut page = Vec::with_capacity(28 + body.len());
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(if sequence == 0 { 2 } else { 0 });
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&7u32.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(1);
        page.push(u8::try_from(body.len()).unwrap());
        page.extend_from_slice(body);
        page
    }

    fn recording(duration_ms: u64) -> Vec<u8> {
        let pre_skip = 312u16;
        let mut opus_head = b"OpusHead".to_vec();
        opus_head.extend_from_slice(&[1, 1]);
        opus_head.extend_from_slice(&pre_skip.to_le_bytes());
        opus_head.extend_from_slice(&48_000u32.to_le_bytes());
        opus_head.extend_from_slice(&0u16.to_le_bytes());
        opus_head.push(0);
        let mut bytes = ogg_page(0, 0, &opus_head);
        let samples = duration_ms * 48;
        bytes.extend(ogg_page(
            1,
            samples + u64::from(pre_skip),
            b"synthetic opus packet",
        ));
        bytes
    }

    #[test]
    fn parses_structured_ogg_opus_duration_and_rejects_malformed_streams() {
        assert_eq!(ogg_opus_duration_ms(&recording(2_400)).unwrap(), 2_400);
        assert!(ogg_opus_duration_ms(b"OggS OpusHead").is_err());
        let mut wrong_container = recording(1_000);
        wrong_container[0] = b'X';
        assert!(ogg_opus_duration_ms(&wrong_container).is_err());
        let mut wrong_codec = recording(1_000);
        wrong_codec[28..36].copy_from_slice(b"Vorbis!!");
        assert!(ogg_opus_duration_ms(&wrong_codec).is_err());
        let mut unsupported_header = recording(1_000);
        unsupported_header[28 + 8] = 2;
        assert!(ogg_opus_duration_ms(&unsupported_header).is_err());
        let mut broken_sequence = recording(1_000);
        let second_page = 28 + 19;
        broken_sequence[second_page + 18..second_page + 22].copy_from_slice(&4u32.to_le_bytes());
        assert!(ogg_opus_duration_ms(&broken_sequence).is_err());
        assert!(ogg_opus_duration_ms(&recording(MIN_DURATION_MS - 1)).is_err());
        assert!(ogg_opus_duration_ms(&recording(MAX_DURATION_MS + 1)).is_err());
    }

    #[test]
    fn preparation_rejects_untrusted_paths_sizes_and_changed_recordings() {
        let directory = tempfile::tempdir().unwrap();
        let outbox = directory.path();
        assert!(job_path(outbox, "../escape").is_err());
        assert!(prepare(outbox, "empty-chat", "", 1).is_err());

        let empty_id = "empty-recording";
        fs::write(recording_path(outbox, empty_id).unwrap(), []).unwrap();
        assert!(prepare(outbox, empty_id, "chat@s.whatsapp.net", 2).is_err());

        let large_id = "large-recording";
        let large_path = recording_path(outbox, large_id).unwrap();
        fs::File::create(&large_path)
            .unwrap()
            .set_len(assets::MAX_AUDIO_BYTES + 1)
            .unwrap();
        assert!(prepare(outbox, large_id, "chat@s.whatsapp.net", 3).is_err());

        let external = directory.path().with_extension("external-voice.ogg");
        fs::write(&external, recording(1_000)).unwrap();
        let linked_id = "linked-recording";
        symlink(&external, recording_path(outbox, linked_id).unwrap()).unwrap();
        assert!(prepare(outbox, linked_id, "chat@s.whatsapp.net", 4).is_err());
        fs::remove_file(external).unwrap();

        let changed_id = "changed-recording";
        let changed_path = recording_path(outbox, changed_id).unwrap();
        fs::write(&changed_path, recording(1_000)).unwrap();
        prepare(outbox, changed_id, "chat@s.whatsapp.net", 5).unwrap();
        fs::write(&changed_path, recording(2_000)).unwrap();
        assert!(prepare(outbox, changed_id, "chat@s.whatsapp.net", 6).is_err());
    }

    #[test]
    fn delivery_identity_is_stable_and_stored_jobs_are_validated() {
        let directory = tempfile::tempdir().unwrap();
        let outbox = directory.path();
        let recording_id = "delivery-1";
        fs::write(
            recording_path(outbox, recording_id).unwrap(),
            recording(1_000),
        )
        .unwrap();
        let mut prepared = prepare(outbox, recording_id, "chat@s.whatsapp.net", 10).unwrap();
        assert!(assign_delivery(outbox, &mut prepared.job, "", "MSG-1", 11).is_err());
        assign_delivery(
            outbox,
            &mut prepared.job,
            "chat@s.whatsapp.net",
            "MSG-1",
            12,
        )
        .unwrap();
        assign_delivery(
            outbox,
            &mut prepared.job,
            "chat@s.whatsapp.net",
            "MSG-1",
            13,
        )
        .unwrap();
        assert!(
            assign_delivery(
                outbox,
                &mut prepared.job,
                "chat@s.whatsapp.net",
                "MSG-2",
                14,
            )
            .is_err()
        );

        let mut invalid = prepared.job.clone();
        invalid.version = JOB_VERSION + 1;
        assert!(validate_job(&invalid, recording_id).is_err());
        prepared.job.status = StoredStatus::Sent;
        assert!(prepared.job.entry().is_none());
        save_job(outbox, &prepared.job).unwrap();
        assert!(recover_interrupted(outbox, 15).unwrap().is_empty());
        assert!(!recording_path(outbox, recording_id).unwrap().exists());
    }

    #[test]
    fn cleanup_discards_invalid_entries_and_never_removes_unrelated_paths() {
        let directory = tempfile::tempdir().unwrap();
        let outbox = directory.path();
        fs::write(outbox.join("unrelated.txt"), b"keep").unwrap();
        fs::create_dir(outbox.join("voice-directory-1.ogg")).unwrap();

        let missing_audio_id = "missing-audio";
        fs::write(
            recording_path(outbox, missing_audio_id).unwrap(),
            recording(1_000),
        )
        .unwrap();
        prepare(outbox, missing_audio_id, "chat@s.whatsapp.net", 20).unwrap();
        fs::remove_file(recording_path(outbox, missing_audio_id).unwrap()).unwrap();

        let invalid_id = "invalid-job";
        fs::write(
            recording_path(outbox, invalid_id).unwrap(),
            recording(1_000),
        )
        .unwrap();
        fs::write(job_path(outbox, invalid_id).unwrap(), b"not json").unwrap();
        cleanup(outbox, 21).unwrap();
        assert!(outbox.join("unrelated.txt").exists());
        assert!(outbox.join("voice-directory-1.ogg").is_dir());
        assert!(!job_path(outbox, missing_audio_id).unwrap().exists());
        assert!(!job_path(outbox, invalid_id).unwrap().exists());

        let preserved_id = "preserved-large";
        let preserved_path = recording_path(outbox, preserved_id).unwrap();
        fs::write(&preserved_path, recording(1_000)).unwrap();
        prepare(outbox, preserved_id, "chat@s.whatsapp.net", 22).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&preserved_path)
            .unwrap()
            .set_len(MAX_OUTBOX_BYTES + 1)
            .unwrap();
        cleanup_inner(outbox, 23, Some(preserved_id)).unwrap();
        assert!(preserved_path.exists());

        let blocked_id = "blocked-removal";
        fs::create_dir(recording_path(outbox, blocked_id).unwrap()).unwrap();
        assert!(discard(outbox, blocked_id).is_err());
        assert!(outbox_name("voice-invalid!.ogg").is_none());
    }

    #[test]
    fn jobs_survive_failure_recovery_and_retry_with_one_delivery_id() {
        let directory = tempfile::tempdir().unwrap();
        let outbox = directory.path();
        let recording_id = "42-7";
        fs::write(
            recording_path(outbox, recording_id).unwrap(),
            recording(2_400),
        )
        .unwrap();

        let mut prepared = prepare(outbox, recording_id, "chat@s.whatsapp.net", 100).unwrap();
        assert_eq!(prepared.job.duration_ms, 2_400);
        assign_delivery(
            outbox,
            &mut prepared.job,
            "chat@s.whatsapp.net",
            "MSG-1",
            101,
        )
        .unwrap();
        mark_failed(outbox, &mut prepared.job, "offline", 102).unwrap();
        let entries = entries(outbox).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, VoiceOutboxStatus::Failed);

        let retry_time = 100 + JOB_TTL_SECONDS + 1;
        let retry = prepare(outbox, recording_id, "other@s.whatsapp.net", retry_time).unwrap();
        assert_eq!(retry.job.chat_jid, "chat@s.whatsapp.net");
        assert_eq!(retry.job.message_id.as_deref(), Some("MSG-1"));
        let recovered = recover_interrupted(outbox, retry_time + 1).unwrap();
        assert_eq!(recovered[0].status, VoiceOutboxStatus::Failed);
        assert!(
            recovered[0]
                .error
                .as_deref()
                .unwrap()
                .contains("retry is safe")
        );
        let recovered_again = recover_interrupted(outbox, retry_time + 2).unwrap();
        assert_eq!(recovered_again[0].status, VoiceOutboxStatus::Failed);
    }

    #[test]
    fn completion_discard_and_cleanup_remove_only_bounded_outbox_files() {
        let directory = tempfile::tempdir().unwrap();
        let outbox = directory.path();
        let recording_id = "voicejob-1";
        fs::write(
            recording_path(outbox, recording_id).unwrap(),
            recording(1_000),
        )
        .unwrap();
        let mut prepared = prepare(outbox, recording_id, "chat@s.whatsapp.net", 10).unwrap();
        finish_sent(outbox, &mut prepared.job, 11).unwrap();
        assert!(entries(outbox).unwrap().is_empty());
        assert!(!recording_path(outbox, recording_id).unwrap().exists());

        fs::write(
            recording_path(outbox, "discard-1").unwrap(),
            recording(1_000),
        )
        .unwrap();
        discard(outbox, "discard-1").unwrap();
        assert!(!recording_path(outbox, "discard-1").unwrap().exists());
        assert!(discard(outbox, "../outside").is_err());

        let orphan = recording_path(outbox, "orphan-1").unwrap();
        fs::write(&orphan, recording(1_000)).unwrap();
        let modified = i64::try_from(
            fs::metadata(&orphan)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        cleanup(outbox, modified + ORPHAN_TTL_SECONDS + 1).unwrap();
        assert!(!orphan.exists());

        let expired_id = "expired-1";
        fs::write(
            recording_path(outbox, expired_id).unwrap(),
            recording(1_000),
        )
        .unwrap();
        prepare(outbox, expired_id, "chat@s.whatsapp.net", 20).unwrap();
        cleanup(outbox, 20 + JOB_TTL_SECONDS + 1).unwrap();
        assert!(entries(outbox).unwrap().is_empty());
    }

    #[test]
    fn cleanup_caps_retryable_jobs_without_removing_the_new_recording() {
        let directory = tempfile::tempdir().unwrap();
        let outbox = directory.path();
        for index in 0..=MAX_JOBS {
            let recording_id = format!("bounded-{index}");
            let path = recording_path(outbox, &recording_id).unwrap();
            fs::write(&path, recording(1_000)).unwrap();
            prepare(
                outbox,
                &recording_id,
                "chat@s.whatsapp.net",
                100 + i64::try_from(index).unwrap(),
            )
            .unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_len(8 * 1024 * 1024)
                .unwrap();
        }

        cleanup(outbox, 200).unwrap();
        assert_eq!(entries(outbox).unwrap().len(), MAX_JOBS);
        assert!(!recording_path(outbox, "bounded-0").unwrap().exists());
        assert!(recording_path(outbox, "bounded-8").unwrap().exists());
    }
}
