use crate::ui::{CliProgress, display_name, human_bytes};
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
enum Status {
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
    status: Status,
    downloads: Vec<Arc<ActiveDownload>>,
    speed: SpeedWindow,
    row: ProgressBar,
    detail: ProgressBar,
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
    column_widths: (usize, usize), // max name/label widths
    /// Whether to draw the final frame (stopped/done states, average speed).
    finished: bool,
}

/// The terminal progress UI: a stable panel of one row + one detail
/// line per playlist under a 2-line header, redrawn in place.
pub struct TerminalProgress {
    verb: &'static str,
    dest: String,
    started_at: Instant,
    multi: MultiProgress,
    header: ProgressBar,
    summary: ProgressBar,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    ticker: Mutex<Option<JoinHandle<()>>>,
}

impl TerminalProgress {
    pub fn new(verb: &'static str, dest: String) -> Self {
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
            column_widths: (0, 0),
            finished: false,
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
                    redraw(&verb, &dest, started_at, &mut shared, &header, &summary);
                    drop(shared);
                    std::thread::sleep(TICK);
                }
            })
        };
        Self {
            verb,
            dest,
            started_at: Instant::now(),
            multi,
            header,
            summary,
            shared,
            stop,
            ticker: Mutex::new(Some(ticker)),
        }
    }
}

impl CliProgress for TerminalProgress {
    /// Stop the ticker, render a final frame and leave the panel in
    /// scrollback with final statuses.
    fn finish(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.lock().unwrap().take() {
            let _ = ticker.join();
        }
        let mut shared = self.shared.lock().unwrap();
        // Mark unfinished playlists as stopped.
        for playlist in &mut shared.playlists {
            if !matches!(playlist.status, Status::Done) {
                playlist.status = Status::Stopped;
            }
        }
        shared.finished = true;
        redraw(
            self.verb,
            &self.dest,
            self.started_at,
            &mut shared,
            &self.header,
            &self.summary,
        );
        for playlist in &mut shared.playlists {
            playlist.detail.finish_and_clear();
        }
        for playlist in &shared.playlists {
            playlist.row.finish();
        }
        self.header.finish();
        self.summary.finish();
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
) {
    let (_, width) = Term::stderr().size();
    let width = width as usize;
    let now = Instant::now();

    // Header
    let bullet = if shared.finished {
        if shared
            .playlists
            .iter()
            .all(|playlist| matches!(playlist.status, Status::Done))
        {
            Style::new().green().apply_to("✓").to_string()
        } else {
            Style::new().red().apply_to("■").to_string()
        }
    } else {
        Style::new().green().apply_to("●").to_string()
    };
    let fixed = measure_text_width(verb) + measure_text_width(dest) + 6;
    let title = if fixed <= width && width - fixed >= 8 {
        format!(
            "{} {} → {}",
            bullet,
            Style::new().bold().apply_to(format!(
                "{verb} {}",
                truncate_middle(&shared.url, width - fixed)
            )),
            dest,
        )
    } else {
        // Even without the URL the header does not fit: truncate the end.
        console::truncate_str(
            &format!("{} {verb} {} → {dest}", bullet, shared.url),
            width,
            "…",
        )
        .to_string()
    };
    header.set_message(title);

    // Summary
    shared.speed.sample(now, shared.total_bytes);
    let elapsed = started_at.elapsed().as_secs();
    let speed = if shared.finished {
        // Final frame: average speed over the whole run.
        if elapsed == 0 {
            shared.total_bytes as f64
        } else {
            shared.total_bytes as f64 / elapsed as f64
        }
    } else {
        shared.speed.bytes_per_second()
    };
    let line = format!(
        "  {:02}:{:02} · {} segments · {} · {}/s",
        elapsed / 60,
        elapsed % 60,
        shared.total_segments,
        human_bytes(shared.total_bytes),
        human_bytes(speed as u64),
    );
    summary.set_message(console::truncate_str(&line, width, "…").to_string());

    for playlist in &mut shared.playlists {
        let row_message = row_text(playlist, shared.column_widths, width, now);
        playlist.row.set_message(row_message);
        playlist.detail.set_message(detail_text(playlist, width));
        // Purge finished/aborted downloads.
        playlist
            .downloads
            .retain(|dl| dl.state.load(Ordering::Relaxed) == 0);
    }
}

fn row_text(
    playlist: &mut PlaylistState,
    (name_w, label_w): (usize, usize),
    width: usize,
    now: Instant,
) -> String {
    playlist.speed.sample(now, playlist.bytes);
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
        Status::Done => Style::new().green().apply_to("✓ done").to_string(),
        Status::Stopped => Style::new().red().apply_to("■ stopped").to_string(),
        Status::Waiting(next) => {
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
        Status::Working => {
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
    let size = human_bytes(playlist.bytes);
    let mut name_w = name_w.min(MAX_NAME_WIDTH);
    let mut label_w = label_w.min(MAX_LABEL_WIDTH);
    let mut bar_w = BAR_WIDTH;
    let build = |name_w: usize, label_w: usize, bar_w: usize| {
        let name = truncate_middle(&playlist.name, name_w);
        let label = truncate_middle(&playlist.label, label_w);
        let bar = render_bar(playlist.fraction(), bar_w);
        format!("  {name:<name_w$}  {label:<label_w$}  [{bar}]  {counts}  {size}  {status}")
    };
    let mut line = build(name_w, label_w, bar_w);
    // Shrink the label column first, then the bar, then the name.
    while measure_text_width(&line) > width && label_w > 0 {
        label_w -= 1;
        line = build(name_w, label_w, bar_w);
    }
    while measure_text_width(&line) > width && bar_w > 10 {
        bar_w -= 1;
        line = build(name_w, label_w, bar_w);
    }
    while measure_text_width(&line) > width && name_w > 8 {
        name_w -= 1;
        line = build(name_w, label_w, bar_w);
    }
    // Last resort: truncate the end of the line (ANSI-aware).
    if measure_text_width(&line) > width {
        let plain = format!(
            "  {name:<name_w$}  {label:<label_w$}  [{bar}]  {counts_plain}  {size}  {status}",
            name = truncate_middle(&playlist.name, name_w),
            label = truncate_middle(&playlist.label, label_w),
            bar = render_bar(playlist.fraction(), bar_w),
        );
        console::truncate_str(&plain, width, "…").to_string()
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
    fn scaled(bandwidth: u64, unit: u64, suffix: &str) -> String {
        if bandwidth.is_multiple_of(unit) {
            format!("{}{}", bandwidth / unit, suffix)
        } else {
            format!("{:.1}{}", bandwidth as f64 / unit as f64, suffix)
        }
    }
    if bandwidth >= 1_000_000 {
        scaled(bandwidth, 1_000_000, "M")
    } else if bandwidth >= 1_000 {
        scaled(bandwidth, 1_000, "k")
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
        for info in playlists {
            let name = display_name(&info.path);
            let label = label_of(&info.kind);
            shared.column_widths.0 = shared.column_widths.0.max(measure_text_width(&name));
            shared.column_widths.1 = shared.column_widths.1.max(measure_text_width(&label));
            let row = self.multi.add(ProgressBar::new(1));
            row.set_style(indicatif::ProgressStyle::with_template("{msg}").unwrap());
            let detail = self.multi.add(ProgressBar::new(1));
            detail.set_style(indicatif::ProgressStyle::with_template("{msg}").unwrap());
            shared.playlists.push(PlaylistState {
                name,
                label,
                total: 0,
                done: 0,
                gaps: 0,
                bytes: 0,
                status: Status::Working,
                downloads: Vec::new(),
                speed: SpeedWindow::new(),
                row,
                detail,
            });
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
            playlist.status = Status::Waiting(next_refresh);
        }
    }

    fn playlist_ended(&self, playlist: usize) {
        if let Some(playlist) = self.shared.lock().unwrap().playlists.get_mut(playlist) {
            playlist.status = Status::Done;
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

    fn active(name: &str) -> Arc<ActiveDownload> {
        Arc::new(ActiveDownload {
            name: name.to_string(),
            size: AtomicU64::new(310 * 1024),
            got: AtomicU64::new(120 * 1024),
            state: AtomicU8::new(0),
        })
    }

    #[test]
    fn row_and_detail_fit_any_width() {
        let downloads: Vec<_> = (0..4)
            .map(|i| active(&format!("segment-with-a-quite-long-name-{i:04}.m4s")))
            .collect();
        for width in 20..=200 {
            let mut playlist = PlaylistState {
                name: "variant-with-a-very-long-name-0".to_string(),
                label: "audio a very long rendition name (en)".to_string(),
                total: 612,
                done: 610,
                gaps: 3,
                bytes: 612 * 310 * 1024,
                status: Status::Working,
                downloads: downloads.clone(),
                speed: SpeedWindow::new(),
                row: ProgressBar::hidden(),
                detail: ProgressBar::hidden(),
            };
            let row = row_text(&mut playlist, (32, 40), width, Instant::now());
            assert!(
                measure_text_width(&row) <= width,
                "row wider than {width}: {row}"
            );
            let detail = detail_text(&playlist, width);
            assert!(
                measure_text_width(&detail) <= width,
                "detail wider than {width}: {detail}"
            );
        }
    }
}
