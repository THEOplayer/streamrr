use crate::ui::human_bytes;
use chrono::{DateTime, Utc};
use console::{Style, Term, measure_text_width};
use indicatif::{MultiProgress, ProgressBar};
use std::collections::VecDeque;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use streamrr::record::{
    DownloadProgress, MediaType, PlaylistInfo, PlaylistKind, PlaylistUpdate, RecordProgress,
    ResourceKind,
};
use url::Url;

const TICK: Duration = Duration::from_millis(100);
const SPEED_WINDOW: Duration = Duration::from_secs(3);
const BAR_WIDTH: usize = 20;
const MAX_NAME_WIDTH: usize = 16;
const MAX_LABEL_WIDTH: usize = 24;

/// Truncate `s` in the middle with `…` so it fits `max_width`.
/// Always keeps the start and the end of the string.
pub fn truncate_middle(s: &str, max_width: usize) -> String {
    if measure_text_width(s) <= max_width {
        return s.to_string();
    }
    if max_width < 3 {
        return s.chars().take(max_width).collect();
    }
    let head_len = max_width / 2;
    let tail_len = max_width - head_len - 1;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(tail_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// A single in-flight download for the detail line.
pub struct DownloadEntry {
    pub name: String,
    pub got: u64,
    /// Total size, or `None` when unknown.
    pub size: Option<u64>,
}

/// Fit the in-flight downloads into `width`, middle-truncating file
/// names and finally falling back to `+N more` entries.
pub fn fit_downloads(entries: &[DownloadEntry], width: usize) -> String {
    fn render(entries: &[DownloadEntry], name_max: usize) -> String {
        entries
            .iter()
            .map(|entry| {
                let name = truncate_middle(&entry.name, name_max);
                match entry.size {
                    Some(size) => {
                        format!("{name} {}/{}", human_bytes(entry.got), human_bytes(size))
                    }
                    None => format!("{name} {}", human_bytes(entry.got)),
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
    // 1. Everything fits.
    let full = render(entries, usize::MAX);
    if measure_text_width(&full) <= width {
        return full;
    }
    // 2. Middle-truncate the names, down to a minimum.
    let longest = entries
        .iter()
        .map(|entry| measure_text_width(&entry.name))
        .max()
        .unwrap_or(0);
    for name_max in (12..=longest).rev() {
        let line = render(entries, name_max);
        if measure_text_width(&line) <= width {
            return line;
        }
    }
    // 3. As many entries as fit, then `+N more`.
    let mut line = String::new();
    let mut shown = 0;
    for entry in entries {
        let name = truncate_middle(&entry.name, 12);
        let text = match entry.size {
            Some(size) => format!("{name} {}/{}", human_bytes(entry.got), human_bytes(size)),
            None => format!("{name} {}", human_bytes(entry.got)),
        };
        let remaining = entries.len() - shown - 1;
        let candidate = if line.is_empty() {
            text
        } else {
            format!("{line} · {text}")
        };
        let with_more = if remaining > 0 {
            format!("{candidate} · +{remaining} more")
        } else {
            candidate.clone()
        };
        if measure_text_width(&with_more) > width {
            break;
        }
        line = candidate;
        shown += 1;
    }
    if shown < entries.len() {
        format!("{line} · +{} more", entries.len() - shown)
    } else {
        line
    }
}

/// Render a fixed-width progress bar with eighth-cell precision.
fn render_bar(fraction: f64, width: usize) -> String {
    const EIGHTHS: &[char] = &['▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let scaled = (fraction.clamp(0.0, 1.0) * (width * 8) as f64) as usize;
    let full = scaled / 8;
    let eighth = scaled % 8;
    let mut bar = String::with_capacity(width);
    bar.push_str(&"█".repeat(full));
    if eighth > 0 {
        bar.push(EIGHTHS[eighth - 1]);
    }
    bar.push_str(&"░".repeat(width - measure_text_width(&bar)));
    bar
}

/// A per-playlist sliding window of downloaded bytes for speed.
struct SpeedWindow {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedWindow {
    fn new() -> Self {
        Self {
            samples: VecDeque::new(),
        }
    }

    fn sample(&mut self, now: Instant, total: u64) {
        self.samples.push_back((now, total));
        while self
            .samples
            .front()
            .is_some_and(|(time, _)| now.duration_since(*time) > SPEED_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    fn bytes_per_second(&self) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }
        let (start_time, start) = self.samples.front().unwrap();
        let (end_time, end) = self.samples.back().unwrap();
        let elapsed = end_time.duration_since(*start_time).as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        (end - start) as f64 / elapsed
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PlStatus {
    Working,
    Waiting(DateTime<Utc>),
    Done,
    Stopped,
}

struct ActiveDownload {
    name: String,
    size: AtomicU64, // u64::MAX = unknown
    got: AtomicU64,
    state: AtomicU8, // 0 active, 1 finished, 2 aborted
}

struct PlaylistState {
    name: String,
    label: String,
    total: usize,
    done: usize,
    gaps: usize,
    bytes: u64,
    status: PlStatus,
    downloads: Vec<Arc<ActiveDownload>>,
    speed: SpeedWindow,
}

impl PlaylistState {
    fn fraction(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let inflight: f64 = self
            .downloads
            .iter()
            .filter(|dl| dl.state.load(Ordering::Relaxed) == 0)
            .map(|dl| {
                let size = dl.size.load(Ordering::Relaxed);
                if size == u64::MAX || size == 0 {
                    0.0
                } else {
                    dl.got.load(Ordering::Relaxed) as f64 / size as f64
                }
            })
            .sum();
        (self.done as f64 + inflight) / self.total as f64
    }
}

struct Shared {
    url: String,
    total_segments: usize,
    total_bytes: u64,
    speed: SpeedWindow,
    playlists: Vec<PlaylistState>,
    url_widths: (usize, usize), // max name/label widths
}

/// The terminal progress UI: a stable panel of one row + one detail
/// line per playlist under a 2-line header, redrawn in place.
pub struct TerminalProgress {
    verb: &'static str,
    summary_verb: &'static str,
    dest: String,
    started_at: Instant,
    multi: MultiProgress,
    header: ProgressBar,
    summary: ProgressBar,
    rows: Mutex<Vec<(ProgressBar, ProgressBar)>>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    ticker: Mutex<Option<JoinHandle<()>>>,
}

impl TerminalProgress {
    pub fn new(verb: &'static str, summary_verb: &'static str, dest: String) -> Self {
        let multi = MultiProgress::new();
        let header = multi.add(ProgressBar::new(1));
        header.set_style(indicatif::ProgressStyle::with_template("{msg}").unwrap());
        let summary = multi.add(ProgressBar::new(1));
        summary.set_style(indicatif::ProgressStyle::with_template("{msg}").unwrap());
        let shared = Arc::new(Mutex::new(Shared {
            url: String::new(),
            total_segments: 0,
            total_bytes: 0,
            speed: SpeedWindow::new(),
            playlists: Vec::new(),
            url_widths: (0, 0),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = {
            let shared = shared.clone();
            let stop = stop.clone();
            let header = header.clone();
            let summary = summary.clone();
            let verb = verb.to_string();
            let dest = dest.clone();
            let started_at = Instant::now();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let mut shared = shared.lock().unwrap();
                    redraw(
                        &verb,
                        &dest,
                        started_at,
                        &mut shared,
                        &header,
                        &summary,
                        None,
                    );
                    drop(shared);
                    std::thread::sleep(TICK);
                }
            })
        };
        Self {
            verb,
            summary_verb,
            dest,
            started_at: Instant::now(),
            multi,
            header,
            summary,
            rows: Mutex::new(Vec::new()),
            shared,
            stop,
            ticker: Mutex::new(Some(ticker)),
        }
    }

    /// Stop the ticker, render a final frame and leave the panel in
    /// scrollback with final statuses.
    pub fn finish(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.lock().unwrap().take() {
            let _ = ticker.join();
        }
        let mut shared = self.shared.lock().unwrap();
        // Mark unfinished playlists as stopped.
        for playlist in &mut shared.playlists {
            if !matches!(playlist.status, PlStatus::Done) {
                playlist.status = PlStatus::Stopped;
            }
        }
        let rows = self.rows.lock().unwrap();
        redraw(
            self.verb,
            &self.dest,
            self.started_at,
            &mut shared,
            &self.header,
            &self.summary,
            Some(&rows),
        );
        for (_, detail) in rows.iter() {
            detail.finish_and_clear();
        }
        for (row, _) in rows.iter() {
            row.finish();
        }
        self.header.finish();
        self.summary.finish();
        println!(
            "{} {} segments ({}) in {:02}:{:02}",
            self.summary_verb,
            shared.total_segments,
            human_bytes(shared.total_bytes),
            self.started_at.elapsed().as_secs() / 60,
            self.started_at.elapsed().as_secs() % 60,
        );
    }
}

/// Recompute and draw all line texts.
fn redraw(
    verb: &str,
    dest: &str,
    started_at: Instant,
    shared: &mut Shared,
    header: &ProgressBar,
    summary: &ProgressBar,
    rows: Option<&Vec<(ProgressBar, ProgressBar)>>,
) {
    let (_, width) = Term::stderr().size();
    let width = width as usize;
    let now = Instant::now();

    // Header
    let title = format!(
        "{} {} → {}",
        Style::new().green().apply_to("●"),
        Style::new().bold().apply_to(format!(
            "{verb} {}",
            truncate_middle(
                &shared.url,
                width.saturating_sub(measure_text_width(verb) + dest.len() + 5)
            )
        )),
        dest,
    );
    header.set_message(title);

    // Summary
    shared.speed.sample(now, shared.total_bytes);
    let speed = shared.speed.bytes_per_second();
    let elapsed = started_at.elapsed().as_secs();
    summary.set_message(format!(
        "  {:02}:{:02} · {} segments · {} · {}/s",
        elapsed / 60,
        elapsed % 60,
        shared.total_segments,
        human_bytes(shared.total_bytes),
        human_bytes(speed as u64),
    ));

    if let Some(rows) = rows {
        for ((row, detail), playlist) in rows.iter().zip(shared.playlists.iter_mut()) {
            row.set_message(row_text(playlist, shared.url_widths, width, now));
            detail.set_message(detail_text(playlist, width));
            // Purge finished/aborted downloads.
            playlist
                .downloads
                .retain(|dl| dl.state.load(Ordering::Relaxed) == 0);
        }
    }
}

fn row_text(
    playlist: &mut PlaylistState,
    (name_w, label_w): (usize, usize),
    width: usize,
    now: Instant,
) -> String {
    playlist.speed.sample(now, playlist.bytes);
    let name = truncate_middle(&playlist.name, name_w.min(MAX_NAME_WIDTH));
    let label = truncate_middle(&playlist.label, label_w.min(MAX_LABEL_WIDTH));
    let bar = render_bar(playlist.fraction(), BAR_WIDTH);
    let counts = if playlist.gaps > 0 {
        format!(
            "{}/{} {}",
            playlist.done,
            playlist.total,
            Style::new()
                .yellow()
                .apply_to(format!("({} gaps)", playlist.gaps))
        )
    } else {
        format!("{}/{}", playlist.done, playlist.total)
    };
    let counts_plain = if playlist.gaps > 0 {
        format!(
            "{}/{} ({} gaps)",
            playlist.done, playlist.total, playlist.gaps
        )
    } else {
        format!("{}/{}", playlist.done, playlist.total)
    };
    let status = match playlist.status {
        PlStatus::Done => Style::new().green().apply_to("✓ done").to_string(),
        PlStatus::Stopped => Style::new().red().apply_to("■ stopped").to_string(),
        PlStatus::Waiting(next) => {
            let secs = (next - Utc::now()).num_seconds();
            if secs > 0 {
                Style::new()
                    .dim()
                    .apply_to(format!("↻ refresh in {secs}s"))
                    .to_string()
            } else {
                Style::new().dim().apply_to("↻ refreshing").to_string()
            }
        }
        PlStatus::Working => {
            let active = playlist
                .downloads
                .iter()
                .filter(|dl| dl.state.load(Ordering::Relaxed) == 0)
                .count();
            if active > 0 {
                format!(
                    "{} {} · {}/s",
                    Style::new().cyan().apply_to("⇣"),
                    active,
                    human_bytes(playlist.speed.bytes_per_second() as u64),
                )
            } else {
                Style::new().cyan().apply_to("⇣").to_string()
            }
        }
    };
    let name_col = format!("{name:>width$}", width = name_w.min(MAX_NAME_WIDTH));
    let label_col = format!("{label:<width$}", width = label_w.min(MAX_LABEL_WIDTH));
    let line = format!(
        "  {name_col}  {label_col}  [{bar}]  {counts}  {}  {status}",
        human_bytes(playlist.bytes),
    );
    // No line may wrap: fit to the terminal width.
    if measure_text_width(&line) > width {
        let plain = format!(
            "  {name_col}  {label_col}  [{bar}]  {counts_plain}  {}  {status}",
            human_bytes(playlist.bytes),
        );
        truncate_middle(&plain, width)
    } else {
        line
    }
}

fn detail_text(playlist: &PlaylistState, width: usize) -> String {
    let entries: Vec<DownloadEntry> = playlist
        .downloads
        .iter()
        .filter(|dl| dl.state.load(Ordering::Relaxed) == 0)
        .map(|dl| DownloadEntry {
            name: dl.name.clone(),
            got: dl.got.load(Ordering::Relaxed),
            size: match dl.size.load(Ordering::Relaxed) {
                u64::MAX => None,
                size => Some(size),
            },
        })
        .collect();
    if entries.is_empty() {
        return String::new();
    }
    fit_downloads(&entries, width.saturating_sub(4))
}

fn label_of(kind: &PlaylistKind) -> String {
    match kind {
        PlaylistKind::Variant {
            bandwidth,
            resolution,
        } => {
            let mut label = String::new();
            if let Some((w, h)) = resolution {
                write!(label, "{w}x{h} ").unwrap();
            }
            write!(label, "{}", human_bandwidth(*bandwidth)).unwrap();
            label
        }
        PlaylistKind::Rendition {
            media_type,
            name,
            language,
        } => {
            let mut label = String::new();
            match media_type {
                MediaType::Audio => label.push_str("audio "),
                MediaType::Video => label.push_str("video "),
                MediaType::Subtitles => label.push_str("subs "),
                MediaType::ClosedCaptions => label.push_str("cc "),
                MediaType::Other => {}
            }
            match language {
                Some(language) => {
                    write!(label, "{name} ({language})").unwrap();
                }
                None => label.push_str(name),
            }
            label
        }
        PlaylistKind::Media => String::new(),
    }
}

fn human_bandwidth(bandwidth: u64) -> String {
    if bandwidth >= 1_000_000 {
        format!("{}M", bandwidth / 1_000_000)
    } else if bandwidth >= 1_000 {
        format!("{}k", bandwidth / 1_000)
    } else {
        format!("{bandwidth}")
    }
}

impl RecordProgress for TerminalProgress {
    fn started(&self, url: &Url) {
        self.shared.lock().unwrap().url = url.to_string();
    }

    fn skipped(&self, url: &Url) {
        let _ = self.multi.println(format!("Skipping {url}: not available"));
    }

    fn playlists(&self, playlists: &[PlaylistInfo]) {
        let mut shared = self.shared.lock().unwrap();
        let mut rows = self.rows.lock().unwrap();
        for info in playlists {
            let name = if info.path.is_empty() {
                "media".to_string()
            } else {
                info.path.clone()
            };
            let label = label_of(&info.kind);
            shared.url_widths.0 = shared.url_widths.0.max(measure_text_width(&name));
            shared.url_widths.1 = shared.url_widths.1.max(measure_text_width(&label));
            shared.playlists.push(PlaylistState {
                name,
                label,
                total: 0,
                done: 0,
                gaps: 0,
                bytes: 0,
                status: PlStatus::Working,
                downloads: Vec::new(),
                speed: SpeedWindow::new(),
            });
            let row = self.multi.add(ProgressBar::new(1));
            row.set_style(indicatif::ProgressStyle::with_template("{msg}").unwrap());
            let detail = self.multi.add(ProgressBar::new(1));
            detail.set_style(indicatif::ProgressStyle::with_template("{msg}").unwrap());
            rows.push((row, detail));
        }
    }

    fn playlist_loaded(&self, playlist: usize, update: &PlaylistUpdate) {
        let mut shared = self.shared.lock().unwrap();
        let Some(playlist) = shared.playlists.get_mut(playlist) else {
            return;
        };
        playlist.total += update.new_segments;
        playlist.gaps += update.new_gaps;
    }

    fn playlist_waiting(&self, playlist: usize, next_refresh: DateTime<Utc>) {
        if let Some(playlist) = self.shared.lock().unwrap().playlists.get_mut(playlist) {
            playlist.status = PlStatus::Waiting(next_refresh);
        }
    }

    fn playlist_ended(&self, playlist: usize) {
        if let Some(playlist) = self.shared.lock().unwrap().playlists.get_mut(playlist) {
            playlist.status = PlStatus::Done;
        }
    }

    fn download_started(
        &self,
        playlist: usize,
        path: &str,
        kind: ResourceKind,
    ) -> Box<dyn DownloadProgress> {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let record = Arc::new(ActiveDownload {
            name,
            size: AtomicU64::new(u64::MAX),
            got: AtomicU64::new(0),
            state: AtomicU8::new(0),
        });
        {
            let mut shared = self.shared.lock().unwrap();
            if let Some(playlist) = shared.playlists.get_mut(playlist) {
                playlist.downloads.push(record.clone());
            }
        }
        let _ = kind;
        Box::new(UiDownload {
            shared: self.shared.clone(),
            playlist,
            kind,
            record,
        })
    }
}

struct UiDownload {
    shared: Arc<Mutex<Shared>>,
    playlist: usize,
    kind: ResourceKind,
    record: Arc<ActiveDownload>,
}

impl DownloadProgress for UiDownload {
    fn set_size(&self, size: u64) {
        self.record.size.store(size, Ordering::Relaxed);
    }

    fn inc(&self, bytes: u64) {
        self.record.got.fetch_add(bytes, Ordering::Relaxed);
        let mut shared = self.shared.lock().unwrap();
        shared.total_bytes += bytes;
        if let Some(playlist) = shared.playlists.get_mut(self.playlist) {
            playlist.bytes += bytes;
        }
    }

    fn finish(&self) {
        self.record.state.store(1, Ordering::Relaxed);
        if matches!(self.kind, ResourceKind::Segment) {
            let mut shared = self.shared.lock().unwrap();
            shared.total_segments += 1;
            if let Some(playlist) = shared.playlists.get_mut(self.playlist) {
                playlist.done += 1;
            }
        }
    }
}

impl Drop for UiDownload {
    fn drop(&mut self) {
        if self.record.state.load(Ordering::Relaxed) == 0 {
            // Aborted without finishing.
            self.record.state.store(2, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_middle_short() {
        assert_eq!(truncate_middle("abc", 5), "abc");
        assert_eq!(truncate_middle("abc", 3), "abc");
    }

    #[test]
    fn truncate_middle_truncates() {
        let result = truncate_middle("0123456789abcdef", 9);
        assert_eq!(measure_text_width(&result), 9);
        assert!(result.starts_with("0123"));
        assert!(result.ends_with("cdef"));
        assert!(result.contains('…'));
    }

    #[test]
    fn truncate_middle_tiny() {
        assert_eq!(truncate_middle("abcdef", 2), "ab");
        assert_eq!(truncate_middle("abcdef", 1), "a");
    }

    fn entry(name: &str, got: u64, size: u64) -> DownloadEntry {
        DownloadEntry {
            name: name.to_string(),
            got,
            size: Some(size),
        }
    }

    #[test]
    fn fit_downloads_all_fit() {
        let entries = vec![entry("seg-1.ts", 1024, 2048), entry("seg-2.ts", 100, 2048)];
        let line = fit_downloads(&entries, 200);
        assert!(line.contains("seg-1.ts"));
        assert!(line.contains("seg-2.ts"));
    }

    #[test]
    fn fit_downloads_unknown_size() {
        let entries = vec![DownloadEntry {
            name: "seg-1.ts".to_string(),
            got: 512,
            size: None,
        }];
        let line = fit_downloads(&entries, 200);
        assert!(line.contains("seg-1.ts 512 B"));
        assert!(!line.contains('/'));
    }

    #[test]
    fn fit_downloads_truncates_names() {
        let entries = vec![
            entry("seg_1080p_very_long_name_0001.m4s", 100, 310 * 1024),
            entry("seg_1080p_very_long_name_0002.m4s", 200, 305 * 1024),
        ];
        let line = fit_downloads(&entries, 60);
        assert!(measure_text_width(&line) <= 60);
        assert!(line.contains('…'));
        // Start and end of the names are kept.
        assert!(line.contains("seg_"));
        assert!(line.contains(".m4s"));
    }

    #[test]
    fn fit_downloads_more() {
        let entries: Vec<DownloadEntry> = (0..10)
            .map(|i| entry(&format!("seg-very-long-name-{i:04}.m4s"), 100, 1024))
            .collect();
        let line = fit_downloads(&entries, 50);
        assert!(measure_text_width(&line) <= 50);
        assert!(line.contains("more"));
    }
}
