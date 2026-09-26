use crate::config::atomic_write;
use crate::domain::{TranscriptFrame, TranscriptPage};
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const MAX_TRANSCRIPT_BYTES: u64 = 10 * 1024 * 1024;
const RETAINED_TRANSCRIPT_BYTES: u64 = 6 * 1024 * 1024;

type SessionCoordinator = Arc<Mutex<SessionCoordination>>;

#[derive(Default)]
struct SessionCoordination {
    writer_generation: u64,
    active_writer: Weak<Mutex<TranscriptState>>,
}

static SESSION_COORDINATORS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<SessionCoordination>>>>> =
    OnceLock::new();

#[derive(Clone)]
pub struct TranscriptSink {
    inner: Arc<Mutex<TranscriptState>>,
    coordinator: SessionCoordinator,
    writer_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptWatermark {
    pub epoch: String,
    pub sequence: u64,
}

struct TranscriptState {
    epoch: String,
    sequence: u64,
    bytes_written: u64,
    path: PathBuf,
    file: Option<File>,
}

impl TranscriptSink {
    pub fn create(root: &Path, session_id: &str, epoch: &str) -> Result<Self> {
        fs::create_dir_all(root)?;
        let path = transcript_path(root, session_id)?;
        let coordinator = session_coordinator(&path)?;
        let mut coordination = coordinator
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript session lock poisoned"))?;
        coordination.writer_generation = coordination.writer_generation.saturating_add(1);
        let writer_generation = coordination.writer_generation;
        if let Some(active_writer) = coordination.active_writer.upgrade() {
            let mut active_writer = active_writer
                .lock()
                .map_err(|_| anyhow::anyhow!("superseded transcript lock poisoned"))?;
            let _closed_file = active_writer.file.take();
        }
        coordination.active_writer = Weak::new();
        let stored = recover_incomplete_tail(&path, &mut coordination)?;
        let sequence = stored
            .iter()
            .filter(|(frame, _)| frame.epoch == epoch)
            .map(|(frame, _)| frame.sequence)
            .max()
            .unwrap_or(0);
        if path.exists() && path.metadata()?.len() >= MAX_TRANSCRIPT_BYTES {
            compact_transcript(&path)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open transcript {}", path.display()))?;
        let bytes_written = file.metadata()?.len();
        let inner = Arc::new(Mutex::new(TranscriptState {
            epoch: epoch.to_owned(),
            sequence,
            bytes_written,
            path,
            file: Some(file),
        }));
        coordination.active_writer = Arc::downgrade(&inner);
        Ok(Self {
            inner,
            coordinator: coordinator.clone(),
            writer_generation,
        })
    }

    pub fn append(&self, bytes: &[u8]) -> Result<Option<TranscriptFrame>> {
        let coordination = self
            .coordinator
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript session lock poisoned"))?;
        if self.writer_generation != coordination.writer_generation {
            return Ok(None);
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript lock poisoned"))?;
        let frame = TranscriptFrame {
            epoch: state.epoch.clone(),
            sequence: state.sequence.saturating_add(1),
            captured_at: Utc::now().to_rfc3339(),
            encoding: "base64".to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            gap: false,
        };
        let encoded = encoded_frame(&frame)?;
        if encoded.len() as u64 > MAX_TRANSCRIPT_BYTES - RETAINED_TRANSCRIPT_BYTES {
            anyhow::bail!("transcript frame exceeds the bounded retention window")
        }
        if state.bytes_written.saturating_add(encoded.len() as u64) > MAX_TRANSCRIPT_BYTES {
            let _closed_file = state.file.take();
            if let Err(error) = compact_transcript(&state.path) {
                return Err(error);
            }
            let reopened = OpenOptions::new().append(true).open(&state.path)?;
            let bytes_written = reopened.metadata()?.len();
            state.file = Some(reopened);
            state.bytes_written = bytes_written;
        }
        if state.bytes_written.saturating_add(encoded.len() as u64) > MAX_TRANSCRIPT_BYTES {
            anyhow::bail!("transcript frame cannot fit within the bounded retention file")
        }
        let file = state
            .file
            .as_mut()
            .context("current transcript file is unavailable")?;
        file.write_all(&encoded)?;
        file.flush()?;
        state.bytes_written += encoded.len() as u64;
        state.sequence = frame.sequence;
        Ok(Some(frame))
    }

    pub fn path(&self) -> Result<PathBuf> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript lock poisoned"))?
            .path
            .clone())
    }

    pub fn sequence(&self) -> Result<u64> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript lock poisoned"))?
            .sequence)
    }
}

/// Returns the current in-memory writer watermark when this service boot owns
/// the transcript. An attachment can use it to prove that a poll cursor is
/// already current without reparsing the retained file.
pub fn current_watermark(root: &Path, session_id: &str) -> Result<Option<TranscriptWatermark>> {
    let path = transcript_path(root, session_id)?;
    let coordinator = session_coordinator(&path)?;
    let coordination = coordinator
        .lock()
        .map_err(|_| anyhow::anyhow!("transcript session lock poisoned"))?;
    let Some(writer) = coordination.active_writer.upgrade() else {
        return Ok(None);
    };
    let state = writer
        .lock()
        .map_err(|_| anyhow::anyhow!("transcript lock poisoned"))?;
    Ok(Some(TranscriptWatermark {
        epoch: state.epoch.clone(),
        sequence: state.sequence,
    }))
}

fn encoded_frame(frame: &TranscriptFrame) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(frame)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn session_coordinator(path: &Path) -> Result<SessionCoordinator> {
    let coordinators = SESSION_COORDINATORS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut coordinators = coordinators
        .lock()
        .map_err(|_| anyhow::anyhow!("transcript coordinator registry poisoned"))?;
    coordinators.retain(|_, coordinator| coordinator.strong_count() > 0);
    if let Some(coordinator) = coordinators.get(path).and_then(Weak::upgrade) {
        return Ok(coordinator);
    }
    let coordinator = Arc::new(Mutex::new(SessionCoordination::default()));
    coordinators.insert(path.to_path_buf(), Arc::downgrade(&coordinator));
    Ok(coordinator)
}

fn transcript_path(root: &Path, session_id: &str) -> Result<PathBuf> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("transcript session ID must match [A-Za-z0-9_-]{{1,128}}")
    }
    let path = root.join(format!("{session_id}.jsonl"));
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("transcript path is not an owned regular file")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspect transcript {}", path.display()))
        }
    }
    Ok(path)
}

fn scan_complete_frames(path: &Path) -> Result<(Vec<(TranscriptFrame, usize)>, Vec<u8>, bool)> {
    if !path.exists() {
        return Ok((Vec::new(), Vec::new(), false));
    }
    let file = File::open(path).with_context(|| format!("open transcript {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut stored = Vec::new();
    let mut complete = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = reader.read_until(b'\n', &mut line)?;
        if count == 0 {
            return Ok((stored, complete, false));
        }
        if line.last() != Some(&b'\n') {
            return Ok((stored, complete, true));
        }
        let Ok(frame) = serde_json::from_slice::<TranscriptFrame>(&line) else {
            return Ok((stored, complete, true));
        };
        complete.extend_from_slice(&line);
        stored.push((frame, line.len()));
    }
}

/// Returns a small printable tail suitable for an exited-session status row.
/// The transcript remains the source of truth; this summary is intentionally
/// bounded and strips terminal control sequences before persistence/display.
#[derive(Clone, Copy)]
enum PlainTextState {
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    String,
    StringEscape,
}

struct BoundedPlainText {
    state: PlainTextState,
    utf8_pending: Vec<u8>,
    text: VecDeque<char>,
    retained_chars: usize,
}

impl BoundedPlainText {
    fn new(max_chars: usize) -> Self {
        Self {
            state: PlainTextState::Text,
            utf8_pending: Vec::with_capacity(4),
            text: VecDeque::new(),
            retained_chars: max_chars
                .max(1)
                .saturating_mul(4)
                .clamp(max_chars.max(1), 32 * 1024),
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.push_byte(*byte);
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if !self.utf8_pending.is_empty() {
            if (0x80..=0xbf).contains(&byte) {
                self.utf8_pending.push(byte);
                let expected = match self.utf8_pending[0] {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                if self.utf8_pending.len() == expected {
                    let pending = std::mem::take(&mut self.utf8_pending);
                    match std::str::from_utf8(&pending) {
                        Ok(value) => {
                            for character in value.chars() {
                                self.push_character(character);
                            }
                        }
                        Err(_) => self.push_character('\u{fffd}'),
                    }
                }
                return;
            }
            self.utf8_pending.clear();
            self.push_character('\u{fffd}');
            self.push_byte(byte);
            return;
        }

        match byte {
            0x00..=0x7f | 0x80..=0x9f => {
                if let Some(character) = char::from_u32(byte as u32) {
                    self.push_character(character);
                }
            }
            0xc2..=0xf4 => self.utf8_pending.push(byte),
            _ => self.push_character('\u{fffd}'),
        }
    }

    fn push_character(&mut self, character: char) {
        self.state = match self.state {
            PlainTextState::Escape => match character {
                '\u{1b}' => PlainTextState::Escape,
                '[' => PlainTextState::Csi,
                ']' => PlainTextState::Osc,
                'P' | 'X' | '^' | '_' => PlainTextState::String,
                '\u{20}'..='\u{2f}' => PlainTextState::EscapeIntermediate,
                _ => PlainTextState::Text,
            },
            PlainTextState::EscapeIntermediate => match character {
                '\u{1b}' => PlainTextState::Escape,
                '\u{20}'..='\u{2f}' => PlainTextState::EscapeIntermediate,
                _ => PlainTextState::Text,
            },
            PlainTextState::Csi => {
                if ('@'..='~').contains(&character) {
                    PlainTextState::Text
                } else {
                    PlainTextState::Csi
                }
            }
            PlainTextState::Osc => match character {
                '\u{7}' | '\u{9c}' => PlainTextState::Text,
                '\u{1b}' => PlainTextState::OscEscape,
                _ => PlainTextState::Osc,
            },
            PlainTextState::OscEscape => match character {
                '\\' | '\u{9c}' => PlainTextState::Text,
                '\u{1b}' => PlainTextState::OscEscape,
                _ => PlainTextState::Osc,
            },
            PlainTextState::String => match character {
                '\u{9c}' => PlainTextState::Text,
                '\u{1b}' => PlainTextState::StringEscape,
                _ => PlainTextState::String,
            },
            PlainTextState::StringEscape => match character {
                '\\' | '\u{9c}' => PlainTextState::Text,
                '\u{1b}' => PlainTextState::StringEscape,
                _ => PlainTextState::String,
            },
            PlainTextState::Text => match character {
                '\u{1b}' => PlainTextState::Escape,
                '\u{9b}' => PlainTextState::Csi,
                '\u{9d}' => PlainTextState::Osc,
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => PlainTextState::String,
                '\r' => {
                    self.retain('\n');
                    PlainTextState::Text
                }
                '\n' | '\t' => {
                    self.retain(character);
                    PlainTextState::Text
                }
                _ if !character.is_control() => {
                    self.retain(character);
                    PlainTextState::Text
                }
                _ => PlainTextState::Text,
            },
        };
    }

    fn retain(&mut self, character: char) {
        self.text.push_back(character);
        while self.text.len() > self.retained_chars {
            self.text.pop_front();
        }
    }

    fn finish(mut self) -> String {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.push_character('\u{fffd}');
        }
        self.text.into_iter().collect()
    }
}

pub fn recent_output_summary(
    root: &Path,
    session_id: &str,
    epoch: &str,
    max_chars: usize,
) -> Result<Option<String>> {
    let path = transcript_path(root, session_id)?;
    let (frames, _, _) = scan_complete_frames(&path)?;
    let mut plain = BoundedPlainText::new(max_chars);
    let mut saw_bytes = false;
    for (frame, _) in frames
        .into_iter()
        .filter(|(frame, _)| frame.epoch == epoch && !frame.gap)
    {
        match frame.encoding.as_str() {
            "base64" => {
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(frame.data) {
                    saw_bytes |= !decoded.is_empty();
                    plain.push_bytes(&decoded);
                }
            }
            "utf8" => {
                saw_bytes |= !frame.data.is_empty();
                plain.push_bytes(frame.data.as_bytes());
            }
            _ => {}
        }
    }
    if !saw_bytes {
        return Ok(None);
    }
    let printable = plain.finish();
    let normalized = printable
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let summary = normalized
        .chars()
        .rev()
        .take(max_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    Ok((!summary.is_empty()).then_some(summary))
}

fn rewrite_and_sync_active(
    path: &Path,
    bytes: &[u8],
    stored: &[(TranscriptFrame, usize)],
    coordination: &mut SessionCoordination,
) -> Result<()> {
    let active = coordination.active_writer.upgrade();
    let mut active_state = active
        .as_ref()
        .map(|writer| {
            writer
                .lock()
                .map_err(|_| anyhow::anyhow!("active transcript lock poisoned"))
        })
        .transpose()?;
    if let Some(state) = active_state.as_mut() {
        let _closed_file = state.file.take();
    }
    if let Err(error) = atomic_write(path, bytes) {
        return Err(error);
    }
    if let Some(state) = active_state.as_mut() {
        let reopened = OpenOptions::new().create(true).append(true).open(path)?;
        let bytes_written = reopened.metadata()?.len();
        state.file = Some(reopened);
        state.bytes_written = bytes_written;
        let epoch = state.epoch.clone();
        let recovered_sequence = stored
            .iter()
            .filter(|(frame, _)| frame.epoch == epoch)
            .map(|(frame, _)| frame.sequence)
            .max()
            .unwrap_or(0);
        state.sequence = state.sequence.max(recovered_sequence);
    }
    Ok(())
}

fn recover_incomplete_tail(
    path: &Path,
    coordination: &mut SessionCoordination,
) -> Result<Vec<(TranscriptFrame, usize)>> {
    let (mut stored, mut complete, incomplete_tail) = scan_complete_frames(path)?;
    if !incomplete_tail {
        return Ok(stored);
    }
    let gap = TranscriptFrame {
        epoch: format!("transcript-recovery-{}", uuid::Uuid::new_v4()),
        sequence: 1,
        captured_at: Utc::now().to_rfc3339(),
        encoding: "utf8".to_owned(),
        data: "an incomplete or malformed app transcript tail was discarded during crash recovery; complete earlier output was preserved"
            .to_owned(),
        gap: true,
    };
    let encoded = encoded_frame(&gap)?;
    complete.extend_from_slice(&encoded);
    stored.push((gap, encoded.len()));
    rewrite_and_sync_active(path, &complete, &stored, coordination)?;
    Ok(stored)
}

fn compact_transcript(path: &Path) -> Result<()> {
    let compacted = compacted_transcript(path)?;
    atomic_write(path, &compacted)
}

fn compacted_transcript(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("open transcript {}", path.display()))?;
    let mut retained = std::collections::VecDeque::<Vec<u8>>::new();
    let mut retained_bytes = 0_u64;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = reader.read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if line.last() != Some(&b'\n') || serde_json::from_slice::<TranscriptFrame>(&line).is_err()
        {
            continue;
        }
        retained_bytes += line.len() as u64;
        retained.push_back(line.clone());
        while retained_bytes > RETAINED_TRANSCRIPT_BYTES {
            let Some(removed) = retained.pop_front() else {
                break;
            };
            retained_bytes -= removed.len() as u64;
        }
    }
    let gap = TranscriptFrame {
        epoch: format!("retention-{}", uuid::Uuid::new_v4()),
        sequence: 1,
        captured_at: Utc::now().to_rfc3339(),
        encoding: "utf8".to_owned(),
        data: "older app transcript output was evicted by the 10 MiB retention bound".to_owned(),
        gap: true,
    };
    let mut compacted = encoded_frame(&gap)?;
    for frame in retained {
        compacted.extend(frame);
    }
    Ok(compacted)
}

pub fn read_frames(
    root: &Path,
    session_id: &str,
    after_epoch: Option<&str>,
    after_sequence: u64,
    requested_bytes: usize,
) -> Result<TranscriptPage> {
    const MAX_PAGE_BYTES: usize = 512 * 1024;
    const MAX_PAGE_FRAMES: usize = 512;
    let page_bytes = requested_bytes.clamp(1024, MAX_PAGE_BYTES);
    let path = transcript_path(root, session_id)?;
    if let (Some(epoch), Some(watermark)) = (after_epoch, current_watermark(root, session_id)?) {
        if epoch == watermark.epoch && after_sequence == watermark.sequence {
            return Ok(TranscriptPage {
                frames: Vec::new(),
                next_epoch: Some(watermark.epoch),
                next_sequence: watermark.sequence,
                has_more: false,
            });
        }
    }
    let coordinator = session_coordinator(&path)?;
    let mut coordination = coordinator
        .lock()
        .map_err(|_| anyhow::anyhow!("transcript session lock poisoned"))?;
    let mut stored = recover_incomplete_tail(&path, &mut coordination)?;
    if path.metadata()?.len() > MAX_TRANSCRIPT_BYTES {
        let compacted = compacted_transcript(&path)?;
        rewrite_and_sync_active(&path, &compacted, &stored, &mut coordination)?;
        stored = scan_complete_frames(&path)?.0;
    }
    let requested_start = match after_epoch {
        None => Some(0),
        Some(epoch) if after_sequence == 0 => {
            stored.iter().position(|(frame, _)| frame.epoch == epoch)
        }
        Some(epoch) => stored
            .iter()
            .position(|(frame, _)| frame.epoch == epoch && frame.sequence == after_sequence)
            .map(|index| index + 1),
    };
    let stale_cursor = after_epoch.is_some() && requested_start.is_none();
    let recovery_gap = stale_cursor.then(|| TranscriptFrame {
        epoch: "retention-recovery".to_owned(),
        sequence: 0,
        captured_at: Utc::now().to_rfc3339(),
        encoding: "utf8".to_owned(),
        data: "requested app transcript cursor is unavailable; replaying retained recent output"
            .to_owned(),
        gap: true,
    });
    let recovery_bytes = recovery_gap
        .as_ref()
        .map(encoded_frame)
        .transpose()?
        .map_or(0, |frame| frame.len());
    let start = if stale_cursor {
        let mut tail_bytes = recovery_bytes;
        let mut tail_frames = 1_usize;
        let mut tail_start = stored.len();
        for index in (0..stored.len()).rev() {
            let frame_bytes = stored[index].1;
            if tail_start < stored.len()
                && (tail_bytes + frame_bytes > page_bytes || tail_frames >= MAX_PAGE_FRAMES)
            {
                break;
            }
            tail_start = index;
            tail_bytes += frame_bytes;
            tail_frames += 1;
        }
        tail_start
    } else {
        requested_start.unwrap_or(0)
    };
    let mut frames = recovery_gap.into_iter().collect::<Vec<_>>();
    let mut encoded_bytes = recovery_bytes;
    let mut has_more = false;
    if !frames.is_empty() {
        encoded_bytes = encoded_frame(&frames[0])?.len();
    }
    let mut actual_frames = 0_usize;
    for (frame, frame_bytes) in stored.into_iter().skip(start) {
        if actual_frames > 0
            && (encoded_bytes + frame_bytes > page_bytes || frames.len() >= MAX_PAGE_FRAMES)
        {
            has_more = !stale_cursor;
            break;
        }
        encoded_bytes += frame_bytes;
        frames.push(frame);
        actual_frames += 1;
    }
    let (next_epoch, next_sequence) = if stale_cursor && actual_frames == 0 {
        (None, 0)
    } else {
        frames
            .last()
            .map(|frame| (Some(frame.epoch.clone()), frame.sequence))
            .unwrap_or_else(|| (after_epoch.map(str::to_owned), after_sequence))
    };
    Ok(TranscriptPage {
        frames,
        next_epoch,
        next_sequence,
        has_more,
    })
}

/// Reads transcript output for one attachment binding only. Historical
/// transcript clients retain the general cross-epoch replay behavior in
/// `read_frames`; an attachment must never cross into another generation.
pub fn read_attachment_frames(
    root: &Path,
    session_id: &str,
    bound_epoch: &str,
    after_epoch: Option<&str>,
    after_sequence: u64,
    requested_bytes: usize,
) -> Result<TranscriptPage> {
    if let Some(epoch) = after_epoch {
        if epoch != bound_epoch {
            bail!("attachment transcript cursor epoch does not match its binding")
        }
    }

    // A first attachment read starts at the bound epoch rather than at the
    // beginning of the retained file. Passing the epoch through also keeps the
    // active-writer watermark fast path available for an idle attachment.
    let page = read_frames(
        root,
        session_id,
        Some(bound_epoch),
        after_sequence,
        requested_bytes,
    )?;
    let mut frames = page
        .frames
        .into_iter()
        .filter(|frame| frame.gap || frame.epoch == bound_epoch)
        .collect::<Vec<_>>();
    let last_bound_sequence = frames
        .iter()
        .rev()
        .find(|frame| !frame.gap && frame.epoch == bound_epoch)
        .map(|frame| frame.sequence);
    let contains_gap = frames.iter().any(|frame| frame.gap);
    let next_sequence = match last_bound_sequence {
        Some(sequence) => sequence,
        None if contains_gap => current_watermark(root, session_id)?
            .filter(|watermark| watermark.epoch == bound_epoch)
            .map(|watermark| watermark.sequence)
            .unwrap_or(after_sequence),
        None => after_sequence,
    };

    // When retention has discarded every frame for the bound epoch, the
    // explicit gap is the only delivered representation of that loss. Bind it
    // to the original epoch so a client can advance without following another
    // epoch or replaying the same recovery marker forever.
    if last_bound_sequence.is_none() && contains_gap {
        for frame in &mut frames {
            if frame.gap {
                frame.epoch = bound_epoch.to_owned();
                frame.sequence = next_sequence;
            }
        }
    }

    Ok(TranscriptPage {
        frames,
        next_epoch: Some(bound_epoch.to_owned()),
        next_sequence,
        has_more: page.has_more && last_bound_sequence.is_some(),
    })
}

pub fn initialize_gap_transcript(
    root: &Path,
    session_id: &str,
    epoch: &str,
    reason: &str,
) -> Result<()> {
    let path = transcript_path(root, session_id)?;
    let coordinator = session_coordinator(&path)?;
    let _coordination = coordinator
        .lock()
        .map_err(|_| anyhow::anyhow!("transcript session lock poisoned"))?;
    if path.exists() {
        return Ok(());
    }
    let frame = TranscriptFrame {
        epoch: epoch.to_owned(),
        sequence: 1,
        captured_at: Utc::now().to_rfc3339(),
        encoding: "utf8".to_owned(),
        data: reason.to_owned(),
        gap: true,
    };
    let bytes = encoded_frame(&frame)?;
    atomic_write(&path, &bytes)
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    #[test]
    fn transcript_paths_reject_traversal_and_symlink_targets_without_mutation() {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-transcript-paths-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.parent().unwrap().join(format!(
            "agenticjira-transcript-outside-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let sentinel = b"malformed outside transcript sentinel";
        std::fs::write(&outside, sentinel).unwrap();
        assert!(read_frames(&root, "../outside", None, 0, 1024).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), sentinel);

        std::os::unix::fs::symlink(&outside, root.join("linked.jsonl")).unwrap();
        assert!(read_frames(&root, "linked", None, 0, 1024).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), sentinel);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn recent_output_summary_is_bounded_and_strips_terminal_controls() {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-transcript-summary-{}",
            uuid::Uuid::new_v4()
        ));
        let session = uuid::Uuid::new_v4().to_string();
        let epoch = uuid::Uuid::new_v4().to_string();
        let sink = TranscriptSink::create(&root, &session, &epoch).unwrap();
        sink.append(
            b"prompt\r\n\x1b]0;SECRET-OSC\x07\x1bPSECRET-DCS\x07SECRET-DCS-AFTER-BEL\x1b\\\x1b_SECRET-APC\x1b\\\x1b^SECRET-PM\x1b\\\x1b[31mInvalid model identifier gpt-missing\x1b[0m\r\n",
        )
        .unwrap();
        sink.append(
            "\u{9d}SECRET-C1-OSC\u{9c}\u{90}SECRET-C1-DCS\u{9c}\u{9f}SECRET-C1-APC\u{9c}\u{9e}SECRET-C1-PM\u{9c}safe\n"
                .as_bytes(),
        )
        .unwrap();
        let mut long_osc = b"\x1b]0;".to_vec();
        long_osc.extend(std::iter::repeat_n(b'S', 20 * 1024));
        long_osc.extend_from_slice(b"SECRET-LONG-OSC");
        sink.append(&long_osc).unwrap();
        sink.append(b"SECRET-SPLIT\x9csafe-after-long\n\x9dSECRET-RAW-OSC\x9c\x90SECRET-RAW-DCS\x9c\x98SECRET-RAW-SOS\x9c\x9fSECRET-RAW-APC\x9c\x9eSECRET-RAW-PM\x9c\x9b31mraw-csi-safe\n\x1b")
            .unwrap();
        sink.append(b"]SECRET-SPLIT-FRAME\x07trailing-safe\n")
            .unwrap();
        sink.append(&[0xc2]).unwrap();
        let mut split_utf8_c1 = vec![0x9d];
        split_utf8_c1.extend_from_slice(b"SECRET-SPLIT-UTF8-C1");
        split_utf8_c1.push(0xc2);
        sink.append(&split_utf8_c1).unwrap();
        sink.append(b"\x9cvalid-utf8-c1-safe\n").unwrap();
        sink.append(b"double-escape-left\x1b").unwrap();
        sink.append(b"\x1b]SECRET-DOUBLE-ESC-OSC\x07double-escape-safe\n")
            .unwrap();
        sink.append(b"escape-left\x1b(").unwrap();
        sink.append(
            b"Bescape-middle\x1b)0escape-next\x1b#8escape-safe\x7f\x85control-filter-safe\n",
        )
        .unwrap();
        let summary = recent_output_summary(&root, &session, &epoch, 4096)
            .unwrap()
            .unwrap();
        assert!(summary.contains("Invalid model identifier gpt-missing"));
        assert!(summary.contains("safe-after-long"));
        assert!(summary.contains("raw-csi-safe"));
        assert!(summary.contains("valid-utf8-c1-safe"));
        assert!(summary.contains("double-escape-leftdouble-escape-safe"));
        assert!(
            summary.ends_with("escape-leftescape-middleescape-nextescape-safecontrol-filter-safe")
        );
        assert!(!summary.contains("Bescape-middle"));
        assert!(!summary.contains("0escape-next"));
        assert!(!summary.contains("8escape-safe"));
        assert!(!summary.contains('\u{7f}'));
        assert!(!summary.contains('\u{85}'));
        assert!(!summary.contains('\u{1b}'));
        assert!(!summary.contains("SECRET"));
        let bounded = recent_output_summary(&root, &session, &epoch, 32)
            .unwrap()
            .unwrap();
        assert!(bounded.chars().count() <= 32);
        drop(sink);
        let _ = std::fs::remove_dir_all(root);
    }
}
