//! Debug overlay: frame rate with its rolling 1% low, process CPU, storage I/O,
//! resident memory and live object counts drawn as a render-only node.
//!
//! The overlay is **not** part of the engine [`Scene`](engine::scene::Scene) and
//! owns no `Bitmap` a script can reach. It builds a small RGBA panel in memory,
//! uploads it as a texture and contributes one [`RenderNode`] to the projected
//! render tree. Nothing above the render pipeline can observe it, so the overlay
//! cannot change what a script sees.
//!
//! The engine refreshes it once per frame through its [`EngineOverlay`]
//! implementation, which re-uploads the panel only a few times a second: it
//! increments a frame counter, and once the refresh interval has elapsed it
//! averages the frames over that interval into the displayed rate and samples
//! the host counters.

use api::error::RenderError;
use api::geometry::{PixelRegion, RectF};
use engine::{EngineOverlay, ObjectCounts};
use platform::{ProcessIo, process_cpu_ns, process_io};
use render::{
    RenderContent, RenderNode, RenderPipeline, RenderState, Texture, TextureDesc, create_texture,
    upload_texture,
};

use std::collections::VecDeque;

/// Panel width in pixels.
///
/// It is wide enough for the longest line [`format_lines`] can produce: the
/// five object counts plus the `OBJ W… L… B… F… T…` labels fit at the glyph
/// scale, with room for the frame rate and its 1% low.
const PANEL_WIDTH: u32 = 400;

/// Panel height in pixels: five text lines plus padding.
const PANEL_HEIGHT: u32 = 104;

/// Integer scale applied to every font pixel.
const CELL_SCALE: i32 = 2;

/// Font glyph width in pixels.
const GLYPH_WIDTH: i32 = 5;

/// Font glyph height in pixels.
const GLYPH_HEIGHT: i32 = 7;

/// Vertical gap between lines, in font pixels (scaled by [`CELL_SCALE`]).
const LINE_GAP: i32 = 2;

/// Left edge of the text inside the panel.
const TEXT_LEFT: i32 = 4;

/// Top edge of the first line inside the panel.
const TEXT_TOP: i32 = 4;

/// Translucent black panel background.
const PANEL_COLOR: [u8; 4] = [0, 0, 0, 176];

/// Opaque white text.
const TEXT_COLOR: [u8; 4] = [255, 255, 255, 255];

/// Milliseconds between panel refreshes (four times a second).
const REFRESH_INTERVAL_MS: f64 = 250.0;

/// Frames kept for the rolling 1% low.
const FRAME_HISTORY: usize = 10_000;

/// Sibling sort key that keeps the overlay above every projected layer.
const OVERLAY_Z: i32 = i32::MAX;

/// Errors produced while building or refreshing the debug overlay.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub(crate) enum OverlayError {
    /// The panel text contains a character the built-in font does not cover.
    #[error("the debug overlay font has no glyph for {0:?}")]
    MissingGlyph(char),
    /// An L1 render action (texture creation or upload) failed.
    #[error(transparent)]
    Render(#[from] RenderError),
}

/// The debug overlay's texture and refresh bookkeeping.
pub(crate) struct Overlay {
    /// Uploaded RGBA panel.
    texture: Texture,
    /// Frames stepped since the last panel refresh.
    frames: u32,
    /// The frame clock at the previous tick, or `None` before the first one.
    last_tick_ms: Option<f64>,
    /// Recent frame times in milliseconds, oldest first.
    frame_times: VecDeque<f64>,
    /// Host elapsed time of the last refresh, or `None` before the first one.
    last_refresh_ms: Option<f64>,
    /// Counters observed at the previous refresh, for rate calculation.
    last_sample: Option<HostSample>,
}

impl Overlay {
    /// Creates the overlay's panel texture.
    ///
    /// The pixels are uploaded by the first [`Overlay::tick`], so construction
    /// only allocates the backend resource.
    ///
    /// # Errors
    ///
    /// Returns [`OverlayError::Render`] when the texture cannot be created.
    pub(crate) fn new(pipeline: &RenderPipeline) -> Result<Self, OverlayError> {
        let texture = create_texture(pipeline, TextureDesc::new(PANEL_WIDTH, PANEL_HEIGHT))?;
        Ok(Self {
            texture,
            frames: 0,
            last_tick_ms: None,
            frame_times: VecDeque::with_capacity(FRAME_HISTORY),
            last_refresh_ms: None,
            last_sample: None,
        })
    }

    /// Advances the overlay by one frame and re-uploads the panel when the
    /// refresh interval has elapsed.
    ///
    /// `now_ms` is the engine's frame clock; `objects` are the live engine
    /// counts; `host` carries the current process counters. A counter
    /// that is unavailable is shown as `?`, never as a fabricated zero.
    ///
    /// # Errors
    ///
    /// Returns [`OverlayError::MissingGlyph`] for a character the font does not
    /// cover, and [`OverlayError::Render`] when the upload fails.
    fn tick(
        &mut self,
        pipeline: &RenderPipeline,
        state: &mut RenderState,
        now_ms: f64,
        objects: ObjectCounts,
        host: HostMetrics,
    ) -> Result<(), OverlayError> {
        self.frames = self.frames.saturating_add(1);
        if let Some(last) = self.last_tick_ms {
            let delta = now_ms - last;
            if delta > 0.0 {
                self.frame_times.push_back(delta);
                if self.frame_times.len() > FRAME_HISTORY {
                    self.frame_times.pop_front();
                }
            }
        }
        self.last_tick_ms = Some(now_ms);
        let due = match self.last_refresh_ms {
            Some(last) => now_ms - last >= REFRESH_INTERVAL_MS,
            None => true,
        };
        if !due {
            return Ok(());
        }

        let elapsed_ms = match self.last_refresh_ms {
            Some(last) => now_ms - last,
            None => now_ms,
        };
        let elapsed_s = elapsed_ms / 1000.0;
        let fps = if elapsed_ms > 0.0 {
            f64::from(self.frames) * 1000.0 / elapsed_ms
        } else {
            0.0
        };

        let previous = self.last_sample;
        let cpu_percent = cpu_percent(
            previous.and_then(|sample| sample.cpu_ns),
            host.cpu_ns,
            elapsed_s,
        );
        let (io_read_per_s, io_write_per_s) = match (previous.and_then(|sample| sample.io), host.io)
        {
            (Some(previous), Some(current))
                if current.read_bytes >= previous.read_bytes
                    && current.write_bytes >= previous.write_bytes
                    && elapsed_s > 0.0 =>
            {
                (
                    Some((current.read_bytes - previous.read_bytes) as f64 / elapsed_s),
                    Some((current.write_bytes - previous.write_bytes) as f64 / elapsed_s),
                )
            }
            _ => (None, None),
        };

        let stats = OverlayStats {
            fps,
            one_percent_low_fps: one_percent_low_fps(&self.frame_times),
            cpu_percent,
            io_read_per_s,
            io_write_per_s,
            rss: host.rss,
            objects,
        };
        let mut panel = Panel::filled(PANEL_COLOR);
        let mut y = TEXT_TOP;
        for line in format_lines(&stats) {
            draw_text_color(&mut panel, TEXT_LEFT, y, &line, TEXT_COLOR)?;
            y += (GLYPH_HEIGHT + LINE_GAP) * CELL_SCALE;
        }
        upload_texture(
            pipeline,
            state,
            &self.texture,
            PixelRegion::new(0, 0, PANEL_WIDTH, PANEL_HEIGHT),
            panel.rgba(),
        )?;

        self.frames = 0;
        self.last_refresh_ms = Some(now_ms);
        self.last_sample = Some(HostSample {
            cpu_ns: host.cpu_ns,
            io: host.io,
        });
        Ok(())
    }

    /// Builds the overlay's render node.
    ///
    /// The node is positioned at the top-left of the framebuffer at the
    /// panel's own pixel size. The engine sets the node's parent and adds it
    /// to the render tree.
    pub(crate) fn node(&self) -> RenderNode {
        RenderNode {
            content: RenderContent::Texture(self.texture.clone()),
            dst: RectF::new(0.0, 0.0, PANEL_WIDTH as f32, PANEL_HEIGHT as f32),
            z: OVERLAY_Z,
            ..RenderNode::default()
        }
    }
}

impl EngineOverlay for Overlay {
    /// Reads the host counters and advances the panel by one frame.
    fn refresh(
        &mut self,
        pipeline: &RenderPipeline,
        state: &mut RenderState,
        now_ms: u64,
        objects: ObjectCounts,
    ) -> Result<(), String> {
        let host = HostMetrics {
            rss: platform::memory_usage(),
            cpu_ns: process_cpu_ns(),
            io: process_io(),
        };
        Overlay::tick(self, pipeline, state, now_ms as f64, objects, host)
            .map_err(|error| error.to_string())
    }

    fn node(&self) -> RenderNode {
        Overlay::node(self)
    }
}

/// One RGBA panel being composed.
struct Panel {
    /// Tightly packed row-major RGBA8 pixels.
    rgba: Vec<u8>,
}

impl Panel {
    /// Creates a `PANEL_WIDTH`x`PANEL_HEIGHT` panel filled with `color`.
    fn filled(color: [u8; 4]) -> Self {
        let mut rgba = vec![0u8; PANEL_WIDTH as usize * PANEL_HEIGHT as usize * 4];
        for texel in rgba.chunks_exact_mut(4) {
            texel.copy_from_slice(&color);
        }
        Self { rgba }
    }

    /// Writes `color` at `(x, y)` when the point is inside the panel.
    fn put(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x >= PANEL_WIDTH as i32 || y >= PANEL_HEIGHT as i32 {
            return;
        }
        let offset = (y as usize * PANEL_WIDTH as usize + x as usize) * 4;
        self.rgba[offset..offset + 4].copy_from_slice(&color);
    }

    /// The panel's tightly packed pixels.
    fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// The values one overlay refresh displays.
#[derive(Clone, Copy, Debug)]
struct OverlayStats {
    /// Frames per second over the refresh interval.
    fps: f64,
    /// Average FPS of the slowest 1% of recent frame times.
    one_percent_low_fps: Option<f64>,
    /// Process CPU usage in percent of one core; may exceed 100.
    cpu_percent: Option<f64>,
    /// Storage read rate in bytes per second.
    io_read_per_s: Option<f64>,
    /// Storage write rate in bytes per second.
    io_write_per_s: Option<f64>,
    /// Resident set size in bytes.
    rss: Option<u64>,
    /// Live scene and texture counts.
    objects: ObjectCounts,
}

/// Counters observed at the previous refresh.
#[derive(Clone, Copy, Debug)]
struct HostSample {
    /// Cumulative process CPU runtime in nanoseconds.
    cpu_ns: Option<u64>,
    /// Cumulative storage read/write bytes.
    io: Option<ProcessIo>,
}

/// Host counters read for one overlay refresh.
#[derive(Clone, Copy, Debug)]
struct HostMetrics {
    /// Resident set size in bytes.
    rss: Option<u64>,
    /// Cumulative process CPU runtime in nanoseconds.
    cpu_ns: Option<u64>,
    /// Cumulative storage read/write bytes.
    io: Option<ProcessIo>,
}

/// Returns process CPU usage as a percentage of one fully used core.
///
/// Both counters are cumulative runtimes in nanoseconds, so their difference
/// divided by the `elapsed_s` seconds between the samples is the number of
/// cores that were busy over the interval: one saturated core is `100`, two
/// are `200`, half a core is `50`. A missing counter, a counter that went
/// backwards (a reset), or a non-positive interval is `None`, which the panel
/// shows as `?`.
fn cpu_percent(previous_ns: Option<u64>, current_ns: Option<u64>, elapsed_s: f64) -> Option<f64> {
    let delta_ns = current_ns?.checked_sub(previous_ns?)?;
    if elapsed_s <= 0.0 || !elapsed_s.is_finite() {
        return None;
    }
    Some(delta_ns as f64 / elapsed_s / 1_000_000_000.0 * 100.0)
}

/// Returns the average FPS of the slowest 1% of `frame_times`.
fn one_percent_low_fps(frame_times: &VecDeque<f64>) -> Option<f64> {
    if frame_times.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = frame_times.iter().copied().collect();
    sorted.sort_by(|left, right| right.total_cmp(left));
    let count = (sorted.len() / 100).max(1);
    let slowest = sorted.get(..count)?;
    let average_ms = slowest.iter().sum::<f64>() / count as f64;
    (average_ms > 0.0).then_some(1000.0 / average_ms)
}

/// Formats the overlay's lines for one refresh.
fn format_lines(stats: &OverlayStats) -> Vec<String> {
    let one_percent = stats
        .one_percent_low_fps
        .map_or_else(|| "?".to_owned(), |fps| format!("{fps:.1}"));
    let mut lines = vec![format!("FPS {:.1} 1% {one_percent}", stats.fps)];
    lines.push(format!("CPU {}", format_percent(stats.cpu_percent)));
    lines.push(format!(
        "IO R {} W {}",
        format_rate(stats.io_read_per_s),
        format_rate(stats.io_write_per_s)
    ));
    if let Some(bytes) = stats.rss {
        lines.push(format!("MEM {}M", bytes / (1024 * 1024)));
    }
    lines.push(format!(
        "OBJ W{} L{} B{} F{} T{}",
        stats.objects.windows,
        stats.objects.layers,
        stats.objects.bitmaps,
        stats.objects.fonts,
        stats.objects.textures
    ));
    lines
}

/// Formats an optional percentage.
fn format_percent(value: Option<f64>) -> String {
    value.map_or_else(|| "?".to_owned(), |value| format!("{value:.0}%"))
}

/// Formats a byte-per-second rate with a binary magnitude suffix.
fn format_rate(value: Option<f64>) -> String {
    let Some(value) = value else {
        return "?".to_owned();
    };
    if value >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1}G", value / (1024.0 * 1024.0 * 1024.0))
    } else if value >= 1024.0 * 1024.0 {
        format!("{:.1}M", value / (1024.0 * 1024.0))
    } else if value >= 1024.0 {
        format!("{:.0}K", value / 1024.0)
    } else {
        format!("{value:.0}")
    }
}

/// Composites `text` at `(x, y)` into `panel` with an explicit ink colour.
fn draw_text_color(
    panel: &mut Panel,
    x: i32,
    y: i32,
    text: &str,
    color: [u8; 4],
) -> Result<(), OverlayError> {
    let mut pen_x = x;
    for ch in text.chars() {
        let glyph = glyph_for(ch).ok_or(OverlayError::MissingGlyph(ch))?;
        draw_glyph(panel, pen_x, y, glyph, color);
        pen_x += (GLYPH_WIDTH + 1) * CELL_SCALE;
    }
    Ok(())
}

/// Composites one glyph at `(x, y)`, scaled by [`CELL_SCALE`].
fn draw_glyph(panel: &mut Panel, x: i32, y: i32, glyph: &Glyph, color: [u8; 4]) {
    for (row, bits) in glyph.rows.iter().enumerate() {
        for (column, &cell) in bits.as_bytes().iter().enumerate() {
            if cell != b'#' {
                continue;
            }
            for dy in 0..CELL_SCALE {
                for dx in 0..CELL_SCALE {
                    panel.put(
                        x + column as i32 * CELL_SCALE + dx,
                        y + row as i32 * CELL_SCALE + dy,
                        color,
                    );
                }
            }
        }
    }
}

/// Returns the glyph for `ch`, or `None` when the font does not cover it.
fn glyph_for(ch: char) -> Option<&'static Glyph> {
    FONT.iter().find(|glyph| glyph.ch == ch)
}

/// One 5x7 glyph drawn with `#` for ink and any other byte for background.
struct Glyph {
    /// The character this glyph represents.
    ch: char,
    /// Seven rows of five columns, top to bottom.
    rows: [&'static str; 7],
}

/// The built-in font.
///
/// It covers exactly the characters [`format_lines`] can emit: digits, the
/// decimal point, `%`, `?`, the space, and the letters in the line labels and
/// byte-rate suffixes.
const FONT: &[Glyph] = &[
    Glyph {
        ch: ' ',
        rows: [
            "     ", "     ", "     ", "     ", "     ", "     ", "     ",
        ],
    },
    Glyph {
        ch: '0',
        rows: [
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '1',
        rows: [
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
    },
    Glyph {
        ch: '2',
        rows: [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
    },
    Glyph {
        ch: '3',
        rows: [
            "#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '4',
        rows: [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
    },
    Glyph {
        ch: '5',
        rows: [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '6',
        rows: [
            "..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '7',
        rows: [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
    },
    Glyph {
        ch: '8',
        rows: [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '9',
        rows: [
            ".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##..",
        ],
    },
    Glyph {
        ch: '.',
        rows: [
            "     ", "     ", "     ", "     ", "     ", ".##..", ".##..",
        ],
    },
    Glyph {
        ch: 'F',
        rows: [
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
    },
    Glyph {
        ch: 'P',
        rows: [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
    },
    Glyph {
        ch: 'S',
        rows: [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
    },
    Glyph {
        ch: 'R',
        rows: [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
    },
    Glyph {
        ch: 'M',
        rows: [
            "#...#", "##.##", "#.#.#", "#...#", "#...#", "#...#", "#...#",
        ],
    },
    Glyph {
        ch: 'B',
        rows: [
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
    },
    Glyph {
        ch: 'C',
        rows: [
            ".####", "#....", "#....", "#....", "#....", "#....", ".####",
        ],
    },
    Glyph {
        ch: 'E',
        rows: [
            "#####", "#....", "####.", "#....", "#....", "#....", "#####",
        ],
    },
    Glyph {
        ch: 'G',
        rows: [
            ".####", "#....", "#....", "#..##", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: 'I',
        rows: [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
        ],
    },
    Glyph {
        ch: 'J',
        rows: [
            "....#", "....#", "....#", "....#", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: 'K',
        rows: [
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
    },
    Glyph {
        ch: 'L',
        rows: [
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
    },
    Glyph {
        ch: 'O',
        rows: [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: 'T',
        rows: [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
    },
    Glyph {
        ch: 'U',
        rows: [
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: 'W',
        rows: [
            "#...#", "#...#", "#...#", "#...#", "#.#.#", "##.##", "#...#",
        ],
    },
    Glyph {
        ch: '%',
        rows: [
            "##..#", "##.#.", "..#..", ".#...", "#..##", "#..##", ".....",
        ],
    },
    Glyph {
        ch: '?',
        rows: [
            ".###.", "#...#", "....#", "...#.", "..#..", ".....", "..#..",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> OverlayStats {
        OverlayStats {
            fps: 60.0,
            one_percent_low_fps: Some(45.2),
            cpu_percent: Some(12.4),
            io_read_per_s: Some(1.5 * 1024.0 * 1024.0),
            io_write_per_s: Some(0.0),
            rss: Some(256 * 1024 * 1024),
            objects: ObjectCounts {
                windows: 1,
                layers: 23,
                bitmaps: 51,
                fonts: 3,
                textures: 51,
            },
        }
    }

    #[test]
    fn formats_all_overlay_lines() {
        assert_eq!(
            format_lines(&stats()),
            vec![
                "FPS 60.0 1% 45.2",
                "CPU 12%",
                "IO R 1.5M W 0",
                "MEM 256M",
                "OBJ W1 L23 B51 F3 T51",
            ]
        );
    }

    /// One saturated core for one second is 100%: the delta is nanoseconds, so
    /// it converts through 1e9 ns per second.
    #[test]
    fn cpu_percentage_is_a_fraction_of_one_core() {
        assert_eq!(cpu_percent(Some(0), Some(1_000_000_000), 1.0), Some(100.0));
        assert_eq!(cpu_percent(Some(0), Some(2_000_000_000), 1.0), Some(200.0));
        assert_eq!(cpu_percent(Some(0), Some(500_000_000), 1.0), Some(50.0));
        assert_eq!(
            cpu_percent(Some(1_000_000_000), Some(1_500_000_000), 1.0),
            Some(50.0)
        );
        assert_eq!(cpu_percent(Some(0), Some(1_000_000_000), 2.0), Some(50.0));
    }

    #[test]
    fn cpu_percentage_is_unknown_without_a_valid_interval() {
        assert_eq!(cpu_percent(None, Some(1_000_000_000), 1.0), None);
        assert_eq!(cpu_percent(Some(0), None, 1.0), None);
        assert_eq!(cpu_percent(Some(1_000), Some(999), 1.0), None);
        assert_eq!(cpu_percent(Some(0), Some(1_000), 0.0), None);
        assert_eq!(cpu_percent(Some(0), Some(1_000), -1.0), None);
        assert_eq!(cpu_percent(Some(0), Some(1_000), f64::NAN), None);
    }

    #[test]
    fn one_percent_low_averages_the_slowest_hundredth() {
        let mut frame_times = VecDeque::new();
        for _ in 0..99 {
            frame_times.push_back(10.0);
        }
        frame_times.push_back(100.0);
        // 100 frames, 1% is the single slowest frame.
        let low = one_percent_low_fps(&frame_times).expect("low value");
        assert!((low - 10.0).abs() < 1e-9, "got {low}");
        assert_eq!(one_percent_low_fps(&VecDeque::new()), None);

        let mut wide = VecDeque::new();
        for _ in 0..198 {
            wide.push_back(10.0);
        }
        wide.push_back(20.0);
        wide.push_back(100.0);
        // 200 frames, 1% is the two slowest frames: (100 + 20) / 2 ms.
        let low = one_percent_low_fps(&wide).expect("low value");
        assert!((low - 1000.0 / 60.0).abs() < 1e-9, "got {low}");
    }

    #[test]
    fn unavailable_metrics_are_question_marks() {
        let mut stats = stats();
        stats.cpu_percent = None;
        stats.one_percent_low_fps = None;
        stats.io_read_per_s = None;
        stats.io_write_per_s = None;
        stats.rss = None;
        let lines = format_lines(&stats);
        assert_eq!(lines[0], "FPS 60.0 1% ?");
        assert_eq!(lines[1], "CPU ?");
        assert_eq!(lines[2], "IO R ? W ?");
        assert!(!lines.iter().any(|line| line.starts_with("MEM")));
    }

    #[test]
    fn formats_byte_rates_with_binary_suffixes() {
        assert_eq!(format_rate(None), "?");
        assert_eq!(format_rate(Some(0.0)), "0");
        assert_eq!(format_rate(Some(1023.0)), "1023");
        assert_eq!(format_rate(Some(1024.0)), "1K");
        assert_eq!(format_rate(Some(1536.0 * 1024.0)), "1.5M");
        assert_eq!(format_rate(Some(2.0 * 1024.0 * 1024.0 * 1024.0)), "2.0G");
    }

    /// Every character the formatter can emit has a glyph, so the panel never
    /// hits the missing-glyph error in normal operation.
    #[test]
    fn the_font_covers_the_formatter_alphabet() {
        let mut stats = stats();
        stats.fps = 12.0;
        for line in format_lines(&stats) {
            for ch in line.chars() {
                assert!(glyph_for(ch).is_some(), "no glyph for {ch:?} in {line:?}");
            }
        }
    }

    #[test]
    fn unknown_glyph_is_an_error() {
        let mut panel = Panel::filled([0, 0, 0, 0]);
        assert!(matches!(
            draw_text_color(&mut panel, 0, 0, "@", TEXT_COLOR),
            Err(OverlayError::MissingGlyph('@'))
        ));
    }

    #[test]
    fn drawing_ink_marks_pixels_inside_the_panel() {
        let mut panel = Panel::filled([0, 0, 0, 0]);
        draw_text_color(&mut panel, 4, 4, "1", TEXT_COLOR).expect("draw");
        let inked = panel
            .rgba()
            .chunks_exact(4)
            .filter(|pixel| pixel.iter().any(|&byte| byte != 0))
            .count();
        assert!(inked > 0, "the glyph drew no ink");
        // The top-left panel corner is outside the drawn glyph.
        assert_eq!(&panel.rgba()[0..4], &[0, 0, 0, 0]);
    }
}
