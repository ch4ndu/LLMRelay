use crate::config::atomic_write;
use crate::domain::{TranscriptFrame, TranscriptPage};
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::Utc;
use sha2::{Digest, Sha256};
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

#[derive(Clone, Copy, Default)]
enum PlainTextState {
    #[default]
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    String,
    StringEscape,
}

/// Incremental UTF-8 decoding and terminal-control stripping. It mirrors the
/// dashboard's `PlainRecordedBytes`, so the status summary, the browser reader
/// and the generated history export show the same text for the same bytes.
#[derive(Default)]
struct PlainTerminalText {
    state: PlainTextState,
    utf8_pending: Vec<u8>,
}

impl PlainTerminalText {
    fn push_bytes(&mut self, bytes: &[u8], out: &mut impl FnMut(char)) {
        for byte in bytes {
            self.push_byte(*byte, out);
        }
    }

    fn push_text(&mut self, text: &str, out: &mut impl FnMut(char)) {
        self.finish(out);
        for character in text.chars() {
            self.push_character(character, out);
        }
    }

    fn finish(&mut self, out: &mut impl FnMut(char)) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.push_character('\u{fffd}', out);
        }
    }

    fn reset(&mut self) {
        self.state = PlainTextState::Text;
        self.utf8_pending.clear();
    }

    fn push_byte(&mut self, byte: u8, out: &mut impl FnMut(char)) {
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
                                self.push_character(character, out);
                            }
                        }
                        Err(_) => self.push_character('\u{fffd}', out),
                    }
                }
                return;
            }
            self.utf8_pending.clear();
            self.push_character('\u{fffd}', out);
            self.push_byte(byte, out);
            return;
        }

        match byte {
            0x00..=0x7f | 0x80..=0x9f => {
                if let Some(character) = char::from_u32(byte as u32) {
                    self.push_character(character, out);
                }
            }
            0xc2..=0xf4 => self.utf8_pending.push(byte),
            _ => self.push_character('\u{fffd}', out),
        }
    }

    fn push_character(&mut self, character: char, out: &mut impl FnMut(char)) {
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
                    out('\n');
                    PlainTextState::Text
                }
                '\n' | '\t' => {
                    out(character);
                    PlainTextState::Text
                }
                _ if !character.is_control() => {
                    out(character);
                    PlainTextState::Text
                }
                _ => PlainTextState::Text,
            },
        };
    }
}

/// Returns a small printable tail suitable for an exited-session status row.
/// The transcript remains the source of truth; this summary is intentionally
/// bounded and strips terminal control sequences before persistence/display.
struct BoundedPlainText {
    decoder: PlainTerminalText,
    text: VecDeque<char>,
    retained_chars: usize,
}

impl BoundedPlainText {
    fn new(max_chars: usize) -> Self {
        Self {
            decoder: PlainTerminalText::default(),
            text: VecDeque::new(),
            retained_chars: max_chars
                .max(1)
                .saturating_mul(4)
                .clamp(max_chars.max(1), 32 * 1024),
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        let Self {
            decoder,
            text,
            retained_chars,
        } = self;
        decoder.push_bytes(bytes, &mut |character| {
            retain_tail(text, *retained_chars, character)
        });
    }

    fn finish(mut self) -> String {
        let Self {
            decoder,
            text,
            retained_chars,
        } = &mut self;
        decoder.finish(&mut |character| retain_tail(text, *retained_chars, character));
        self.text.into_iter().collect()
    }
}

fn retain_tail(text: &mut VecDeque<char>, retained_chars: usize, character: char) {
    text.push_back(character);
    while text.len() > retained_chars {
        text.pop_front();
    }
}

fn recorded_boundary(frame: &TranscriptFrame, reason: &str) -> String {
    format!(
        "\n[recorded output {reason} at exact epoch {}, frame {}]\n",
        frame.epoch, frame.sequence
    )
}

/// The authoritative plain text of retained frames, frame for frame the same as
/// the dashboard's `recordedOutputText`: decoder state carries across adjacent
/// frames and resets only at an epoch change, a gap or an undecodable frame.
pub(crate) fn recorded_history_text(frames: &[TranscriptFrame]) -> String {
    let mut output = String::new();
    let mut emit = |character: char| output.push(character);
    let mut plain = PlainTerminalText::default();
    let mut epoch: Option<&str> = None;
    for frame in frames {
        if epoch.is_some_and(|epoch| epoch != frame.epoch) {
            plain.finish(&mut emit);
            plain.reset();
            recorded_boundary(frame, "changed")
                .chars()
                .for_each(&mut emit);
        }
        if frame.gap {
            plain.finish(&mut emit);
            plain.reset();
            recorded_boundary(frame, "gap").chars().for_each(&mut emit);
        }
        epoch = Some(frame.epoch.as_str());
        if frame.encoding != "base64" {
            plain.push_text(&frame.data, &mut emit);
            continue;
        }
        match base64::engine::general_purpose::STANDARD.decode(&frame.data) {
            Ok(bytes) => plain.push_bytes(&bytes, &mut emit),
            Err(_) => {
                plain.finish(&mut emit);
                plain.reset();
                "[recorded output frame could not be decoded]\n"
                    .chars()
                    .for_each(&mut emit);
            }
        }
    }
    plain.finish(&mut emit);
    output
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

pub const MAX_HISTORY_PAGE_BYTES: usize = 512 * 1024;
const HISTORY_EXPORT_DIRECTORY: &str = "history-exports";
const MAX_HISTORY_EXPORT_BYTES: u64 = 16 * 1024 * 1024;

/// One page of the complete retained selection of exactly one session epoch,
/// resumed after the epoch frame `after_sequence`. Unlike the live attachment
/// reader, an evicted cursor never jumps to the recent tail: the page reports
/// the loss and continues from the earliest later frame still retained, so
/// following `has_more` always reaches every retained frame.
pub fn read_history_page(
    root: &Path,
    session_id: &str,
    epoch: &str,
    after_sequence: u64,
    limit_bytes: usize,
) -> Result<TranscriptPage> {
    const MAX_PAGE_FRAMES: usize = 512;
    let page_bytes = limit_bytes.clamp(1024, MAX_HISTORY_PAGE_BYTES);
    let selection = retained_epoch_selection(root, session_id, epoch)?;
    let is_epoch_frame = |frame: &TranscriptFrame| !frame.gap && frame.epoch == epoch;
    let mut frames = Vec::new();
    let start = if after_sequence == 0 {
        if selection.is_empty() {
            frames.push(history_gap(
                epoch,
                0,
                "no output for this epoch is retained".to_owned(),
            ));
        }
        0
    } else if let Some(index) = selection
        .iter()
        .position(|(frame, _)| is_epoch_frame(frame) && frame.sequence == after_sequence)
    {
        index + 1
    } else {
        let resume = selection
            .iter()
            .position(|(frame, _)| is_epoch_frame(frame) && frame.sequence > after_sequence);
        frames.push(history_gap(
            epoch,
            after_sequence,
            match resume {
                Some(index) => format!(
                    "output after frame {after_sequence} before frame {} of this epoch was removed by the 10 MiB retention limit",
                    selection[index].0.sequence
                ),
                None => format!(
                    "output after frame {after_sequence} of this epoch was removed by the 10 MiB retention limit"
                ),
            },
        ));
        resume.unwrap_or(selection.len())
    };
    let mut bytes = 0;
    let mut next_sequence = after_sequence;
    let mut last_epoch_frame = None;
    let mut has_more = false;
    for (frame, frame_bytes) in selection.into_iter().skip(start) {
        if is_epoch_frame(&frame) {
            if last_epoch_frame.is_some()
                && (bytes + frame_bytes > page_bytes || frames.len() >= MAX_PAGE_FRAMES)
            {
                has_more = true;
                break;
            }
            next_sequence = frame.sequence;
            last_epoch_frame = Some(frames.len());
        }
        bytes += frame_bytes;
        frames.push(frame);
    }
    // Gaps after this page's last epoch frame follow the cursor it issues, so
    // the next page delivers them exactly once.
    if let (true, Some(last)) = (has_more, last_epoch_frame) {
        frames.truncate(last + 1);
    }
    Ok(TranscriptPage {
        frames,
        next_epoch: Some(epoch.to_owned()),
        next_sequence,
        has_more,
    })
}

fn history_gap(epoch: &str, sequence: u64, data: String) -> TranscriptFrame {
    TranscriptFrame {
        epoch: epoch.to_owned(),
        sequence,
        captured_at: Utc::now().to_rfc3339(),
        encoding: "utf8".to_owned(),
        data,
        gap: true,
    }
}

/// Every retained frame of one epoch from a single locked scan, with its
/// encoded size: the epoch's frames and any later gap in file order, led by a
/// gap when the epoch's first frames were evicted.
fn retained_epoch_selection(
    root: &Path,
    session_id: &str,
    epoch: &str,
) -> Result<Vec<(TranscriptFrame, usize)>> {
    uuid::Uuid::parse_str(epoch).context("history epoch must be a UUID")?;
    let path = transcript_path(root, session_id)?;
    let coordinator = session_coordinator(&path)?;
    let stored = {
        let mut coordination = coordinator
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript session lock poisoned"))?;
        recover_incomplete_tail(&path, &mut coordination)?
    };
    let Some(start) = stored.iter().position(|(frame, _)| frame.epoch == epoch) else {
        return Ok(Vec::new());
    };
    let mut selection = stored
        .into_iter()
        .skip(start)
        .filter(|(frame, _)| frame.gap || frame.epoch == epoch)
        .collect::<Vec<_>>();
    let leading_gap = selection
        .first()
        .map(|(frame, _)| frame)
        .filter(|frame| !frame.gap && frame.sequence > 1)
        .map(|first| {
            history_gap(
                epoch,
                first.sequence - 1,
                format!(
                    "output before frame {} of this epoch was removed by the 10 MiB retention limit",
                    first.sequence
                ),
            )
        });
    if let Some(gap) = leading_gap {
        selection.insert(0, (gap, 0));
    }
    Ok(selection)
}

#[derive(Clone, Debug)]
pub struct HistoryExport {
    pub path: PathBuf,
    pub export_hash: String,
    pub high_water_sequence: u64,
    pub retention: serde_json::Value,
    pub text: String,
}

/// Generates the one plain-text export for a session epoch from every retained
/// frame. Identical content reuses the existing file; changed content replaces
/// it by atomic rename, so a pager that already opened the prior file keeps its
/// snapshot. The path is always generated here, never supplied by a caller.
pub fn write_history_export(
    transcripts: &Path,
    artifacts: &Path,
    session_id: &str,
    epoch: &str,
) -> Result<HistoryExport> {
    let frames = retained_epoch_selection(transcripts, session_id, epoch)?
        .into_iter()
        .map(|(frame, _)| frame)
        .collect::<Vec<_>>();
    let retained = frames
        .iter()
        .filter(|frame| !frame.gap && frame.epoch == epoch)
        .map(|frame| frame.sequence);
    let first_retained = retained.clone().min();
    let high_water_sequence = retained.max().unwrap_or(0);
    let gap_count = frames.iter().filter(|frame| frame.gap).count();
    let note = match (first_retained, gap_count) {
        (None, _) => "No output for this epoch is retained.",
        (Some(_), 0) => "All retained output for this epoch is included.",
        (Some(_), _) => {
            "Some output for this epoch was not retained; each gap is marked where it occurs."
        }
    };
    let mut text = format!(
        "LLMRelay recorded output (read-only)\nSession: {session_id}\nEpoch: {epoch}\nRetained through frame: {high_water_sequence}\n{note}\n\n"
    );
    text.push_str(&recorded_history_text(&frames));
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let export_hash = hex::encode(Sha256::digest(text.as_bytes()));
    let directory = history_export_directory(artifacts)?;
    let path = directory.join(format!("{session_id}-{epoch}.txt"));
    let unchanged = fs::symlink_metadata(&path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .and_then(|_| fs::read(&path).ok())
        .is_some_and(|existing| existing == text.as_bytes());
    if !unchanged {
        atomic_write(&path, text.as_bytes())?;
    }
    Ok(HistoryExport {
        path,
        export_hash,
        high_water_sequence,
        retention: serde_json::json!({
            "first_retained_sequence": first_retained,
            "high_water_sequence": high_water_sequence,
            "gap_count": gap_count,
            "complete": gap_count == 0 && first_retained.is_some_and(|sequence| sequence <= 1),
        }),
        text,
    })
}

/// Resolves a generated export only when the file is still the exact bytes the
/// service bound to a history view.
pub fn validated_history_export(
    artifacts: &Path,
    session_id: &str,
    epoch: &str,
    expected_hash: &str,
) -> Result<PathBuf> {
    uuid::Uuid::parse_str(session_id).context("history session must be a UUID")?;
    uuid::Uuid::parse_str(epoch).context("history epoch must be a UUID")?;
    let path = history_export_directory(artifacts)?.join(format!("{session_id}-{epoch}.txt"));
    open_bound_history_export(&path, expected_hash)?;
    Ok(path)
}

/// Opens a generated export once, without following links, and checks the
/// bytes read through that handle against the hash bound to the view. The file
/// is returned rewound, so an atomic replacement of the path afterwards cannot
/// change what this reader shows; a replacement before the open is refused.
pub fn open_bound_history_export(path: &Path, expected_hash: &str) -> Result<File> {
    use std::io::{Read, Seek};
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("history export {} is unavailable", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_HISTORY_EXPORT_BYTES {
        bail!("history export is not a bounded generated regular file")
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_HISTORY_EXPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read history export {}", path.display()))?;
    if bytes.len() as u64 > MAX_HISTORY_EXPORT_BYTES
        || hex::encode(Sha256::digest(&bytes)) != expected_hash
    {
        bail!("history output changed after this pane was requested; choose Open history pane again from the dashboard")
    }
    file.rewind()?;
    Ok(file)
}

/// Disposable exports are reclaimed at service start. A pager from an earlier
/// boot keeps reading its open file; authoritative transcripts are untouched.
pub fn reclaim_history_exports(artifacts: &Path) -> Result<usize> {
    let directory = artifacts.join(HISTORY_EXPORT_DIRECTORY);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_file() || file_type.is_symlink() {
            fs::remove_file(entry.path())
                .with_context(|| format!("remove history export {}", entry.path().display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn history_export_directory(artifacts: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let directory = artifacts.join(HISTORY_EXPORT_DIRECTORY);
    fs::create_dir_all(&directory).with_context(|| format!("create {}", directory.display()))?;
    let metadata = fs::symlink_metadata(&directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("history export directory must be an owned directory, not a link")
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    Ok(directory)
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

    fn frame(epoch: &str, sequence: u64, bytes: &[u8]) -> TranscriptFrame {
        TranscriptFrame {
            epoch: epoch.to_owned(),
            sequence,
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            encoding: "base64".to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            gap: false,
        }
    }

    fn gap(epoch: &str, sequence: u64, text: &str) -> TranscriptFrame {
        TranscriptFrame {
            epoch: epoch.to_owned(),
            sequence,
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            encoding: "utf8".to_owned(),
            data: text.to_owned(),
            gap: true,
        }
    }

    /// Named literal vectors shared with the dashboard's `recordedOutputText`
    /// tests; both readers must produce exactly this text for these frames.
    fn recorded_history_vectors() -> Vec<(&'static str, Vec<TranscriptFrame>, &'static str)> {
        vec![
            (
                "split_utf8",
                vec![
                    frame("epoch-a", 1, &[0xe2, 0x82]),
                    frame("epoch-a", 2, &[0xac, b'!']),
                ],
                "\u{20ac}!",
            ),
            (
                "split_csi_osc",
                vec![
                    frame("epoch-a", 1, b"red\x1b[3"),
                    frame("epoch-a", 2, b"1mtext\x1b]0;tit"),
                    frame("epoch-a", 3, b"le\x07done"),
                ],
                "redtextdone",
            ),
            (
                "retention_gap",
                vec![
                    frame("epoch-a", 1, &[0xe2, 0x82]),
                    gap("epoch-a", 2, "lost"),
                    frame("epoch-a", 3, b"after"),
                ],
                "\u{fffd}\n[recorded output gap at exact epoch epoch-a, frame 2]\nlostafter",
            ),
            (
                "epoch_boundary",
                vec![
                    frame("epoch-a", 1, b"\x1b]0;unterminated"),
                    frame("epoch-b", 1, b"next\r\n"),
                ],
                "\n[recorded output changed at exact epoch epoch-b, frame 1]\nnext\n\n",
            ),
        ]
    }

    #[test]
    fn recorded_history_text_matches_the_shared_literal_vectors() {
        for (name, frames, expected) in recorded_history_vectors() {
            assert_eq!(recorded_history_text(&frames), expected, "{name}");
        }
    }

    fn history_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("agenticjira-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn history_pages_stay_on_one_epoch_and_reach_the_final_retained_frame() {
        let root = history_root("history-pages");
        let session = uuid::Uuid::new_v4().to_string();
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        let sink = TranscriptSink::create(&root, &session, &first).unwrap();
        for index in 0..40 {
            sink.append(format!("line {index:02} {}\n", "x".repeat(120)).as_bytes())
                .unwrap();
        }
        sink.append(b"FINAL-MARKER\n").unwrap();
        drop(sink);
        let later = TranscriptSink::create(&root, &session, &second).unwrap();
        later.append(b"other epoch\n").unwrap();
        drop(later);

        let mut after = 0;
        let mut frames = Vec::new();
        let mut pages = 0;
        loop {
            let page = read_history_page(&root, &session, &first, after, 1024).unwrap();
            pages += 1;
            frames.extend(page.frames);
            if !page.has_more {
                break;
            }
            assert!(page.next_sequence > after, "the cursor always advances");
            after = page.next_sequence;
        }
        assert!(pages > 1, "a 1 KiB page cannot hold the whole epoch");
        assert!(frames
            .iter()
            .all(|frame| frame.epoch == first && !frame.gap));
        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            (1..=41).collect::<Vec<_>>()
        );
        let text = recorded_history_text(&frames);
        assert!(text.ends_with("FINAL-MARKER\n"));
        assert!(!text.contains("other epoch"));

        // Retention later evicts the frame an issued cursor names, keeping
        // more of the epoch than one page plus the later epoch.
        let issued = read_history_page(&root, &session, &first, 0, 1024)
            .unwrap()
            .next_sequence;
        assert!(issued < 20);
        let path = root.join(format!("{session}.jsonl"));
        let mut compacted =
            encoded_frame(&gap("retention-fixture", 1, "older output was evicted")).unwrap();
        for line in std::fs::read(&path)
            .unwrap()
            .split_inclusive(|byte| *byte == b'\n')
        {
            let stored: TranscriptFrame = serde_json::from_slice(line).unwrap();
            if stored.epoch != first || stored.sequence >= 20 {
                compacted.extend_from_slice(line);
            }
        }
        std::fs::write(&path, compacted).unwrap();

        let mut after = issued;
        let mut resumed = Vec::new();
        let mut pages = 0;
        loop {
            let page = read_history_page(&root, &session, &first, after, 1024).unwrap();
            pages += 1;
            resumed.extend(page.frames);
            if !page.has_more {
                break;
            }
            assert!(page.next_sequence > after, "the cursor always advances");
            after = page.next_sequence;
        }
        assert!(pages > 1, "the retained remainder spans several pages");
        assert!(resumed[0].gap);
        assert!(resumed[0]
            .data
            .contains(&format!("after frame {issued} before frame 20")));
        assert_eq!(resumed[1].sequence, 20);
        assert!(recorded_history_text(&resumed[1..2]).contains("line 19"));
        assert_eq!(resumed.iter().filter(|frame| frame.gap).count(), 1);
        assert_eq!(
            resumed
                .iter()
                .filter(|frame| !frame.gap)
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            (20..=41).collect::<Vec<_>>()
        );
        let text = recorded_history_text(&resumed);
        assert!(text.ends_with("FINAL-MARKER\n"));
        assert!(!text.contains("other epoch"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_of_an_epoch_with_evicted_frames_starts_with_an_explicit_gap() {
        let root = history_root("history-gap");
        let session = uuid::Uuid::new_v4().to_string();
        let epoch = uuid::Uuid::new_v4().to_string();
        let mut bytes = Vec::new();
        for sequence in 5..=6 {
            bytes.extend(encoded_frame(&frame(&epoch, sequence, b"kept\n")).unwrap());
        }
        std::fs::write(root.join(format!("{session}.jsonl")), bytes).unwrap();
        let page = read_history_page(&root, &session, &epoch, 0, 4096).unwrap();
        assert!(page.frames[0].gap);
        assert_eq!(page.frames[0].epoch, epoch);
        assert!(page.frames[0].data.contains("before frame 5"));
        assert_eq!(page.frames.len(), 3);
        let resumed = read_history_page(&root, &session, &epoch, 5, 4096).unwrap();
        assert!(resumed.frames.iter().all(|frame| !frame.gap));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_export_is_generated_reused_and_refused_after_change() {
        use std::os::unix::fs::MetadataExt;
        let root = history_root("history-export");
        let transcripts = root.join("transcripts");
        let artifacts = root.join("artifacts");
        let session = uuid::Uuid::new_v4().to_string();
        let epoch = uuid::Uuid::new_v4().to_string();
        let sink = TranscriptSink::create(&transcripts, &session, &epoch).unwrap();
        sink.append(b"\x1b[32mgreen\x1b[0m line\r\nFINAL-MARKER\n")
            .unwrap();

        let export = write_history_export(&transcripts, &artifacts, &session, &epoch).unwrap();
        assert_eq!(
            export.path,
            artifacts
                .join("history-exports")
                .join(format!("{session}-{epoch}.txt"))
        );
        let text = std::fs::read_to_string(&export.path).unwrap();
        assert!(text.starts_with("LLMRelay recorded output (read-only)\n"));
        assert!(text.contains(
            "Retained through frame: 1\nAll retained output for this epoch is included."
        ));
        assert!(text.ends_with("green line\n\nFINAL-MARKER\n"));
        assert!(!text.contains('\u{1b}'));
        assert_eq!(export.high_water_sequence, 1);
        assert_eq!(export.retention["complete"], true);
        assert_eq!(
            validated_history_export(&artifacts, &session, &epoch, &export.export_hash).unwrap(),
            export.path
        );

        let inode = std::fs::metadata(&export.path).unwrap().ino();
        let again = write_history_export(&transcripts, &artifacts, &session, &epoch).unwrap();
        assert_eq!(again.export_hash, export.export_hash);
        assert_eq!(std::fs::metadata(&export.path).unwrap().ino(), inode);

        let mut pager_input = open_bound_history_export(&export.path, &export.export_hash).unwrap();
        sink.append(b"later output\n").unwrap();
        let changed = write_history_export(&transcripts, &artifacts, &session, &epoch).unwrap();
        assert_ne!(changed.export_hash, export.export_hash);
        assert_eq!(changed.high_water_sequence, 2);
        assert_eq!(pager_input.metadata().unwrap().ino(), inode);
        let mut shown = String::new();
        std::io::Read::read_to_string(&mut pager_input, &mut shown).unwrap();
        assert_eq!(
            shown, text,
            "the verified handle keeps the bound snapshot from its first byte after replacement"
        );
        for stale in [
            open_bound_history_export(&export.path, &export.export_hash)
                .map(|_| ())
                .unwrap_err(),
            validated_history_export(&artifacts, &session, &epoch, &export.export_hash)
                .map(|_| ())
                .unwrap_err(),
        ] {
            assert!(
                stale
                    .to_string()
                    .contains("changed after this pane was requested"),
                "{stale:#}"
            );
        }

        // The link target holds exactly the bound bytes, so only refusing to
        // follow the link can reject it.
        let elsewhere = root.join("elsewhere.txt");
        std::fs::write(&elsewhere, changed.text.as_bytes()).unwrap();
        std::fs::remove_file(&changed.path).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &changed.path).unwrap();
        assert!(open_bound_history_export(&changed.path, &changed.export_hash).is_err());
        assert!(
            validated_history_export(&artifacts, &session, &epoch, &changed.export_hash).is_err()
        );
        assert_eq!(reclaim_history_exports(&artifacts).unwrap(), 1);
        assert!(std::fs::symlink_metadata(&changed.path).is_err());
        assert!(transcripts.join(format!("{session}.jsonl")).exists());
        drop(sink);
        let _ = std::fs::remove_dir_all(root);
    }
}
