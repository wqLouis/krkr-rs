//! Recording and replay for the `Layer.record` / `getRecordImage` /
//! `redrawRecord` / `saveRecord` / `loadRecord` members.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp`
//! (`createRecord`/`getRecordImage`/`redraw`/`saveRecord`/`loadRecord`). The
//! reference stores an EMF+ metafile; the port records the same operations as
//! plain geometry (paths + appearances and image blits) so they can be
//! replayed with the current transform and serialized without a Windows
//! metafile dependency.

use engine::BitmapState;
use engine::layer_ops::RectI;
use engine::raster;

use super::appearance::{AppearanceData, DrawInfo, Paint};
use super::draw::{ImagePlacement, Mat, blit_parallelogram, render_path};
use super::path::{Figure, PathData};

/// One recorded drawing operation, in local (pre-transform) coordinates.
#[derive(Clone, Debug)]
pub(super) enum RecordOp {
    /// `clear(argb)`.
    Clear([u8; 4]),
    /// A path filled/stroked with an appearance.
    Path { path: PathData, app: AppearanceData },
    /// A bitmap blit: source pixels, source rectangle and the destination
    /// parallelogram (pre-transform).
    Blit {
        src: Vec<u8>,
        sw: u32,
        sh: u32,
        src_rect: RectI,
        p0: (f64, f64),
        p1: (f64, f64),
        p2: (f64, f64),
    },
}

/// Replay `ops` into `bitmap` using `ctm` and `clip`.
pub(super) fn replay_record(bitmap: &mut BitmapState, ops: &[RecordOp], ctm: Mat, clip: RectI) {
    for op in ops {
        match op {
            RecordOp::Clear(color) => {
                raster::fill_rect_replace(bitmap, 0, 0, bitmap.width, bitmap.height, *color);
            }
            RecordOp::Path { path, app } => {
                render_path(bitmap, path, app, ctm, clip);
            }
            RecordOp::Blit {
                src,
                sw,
                sh,
                src_rect,
                p0,
                p1,
                p2,
            } => {
                let source = BitmapState::new(*sw, *sh, src.clone());
                blit_parallelogram(
                    bitmap,
                    ImagePlacement {
                        source: engine::layer_ops::BlitSource {
                            bitmap: &source,
                            rect: *src_rect,
                        },
                        corners: [*p0, *p1, *p2],
                    },
                    ctm,
                    clip,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

const MAGIC: &[u8; 4] = b"KEXR";
const VERSION: u32 = 1;

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        Self { bytes }
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn f64(&mut self, value: f64) {
        self.bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| "record: length overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("record: truncated data".to_string());
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Result<i32, String> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(array))
    }

    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.u64()?))
    }

    fn bytes_vec(&mut self, n: usize) -> Result<Vec<u8>, String> {
        Ok(self.take(n)?.to_vec())
    }
}

fn write_paint(writer: &mut Writer, paint: &Paint) {
    match paint {
        Paint::Solid(color) => {
            writer.u8(0);
            writer.bytes(color);
        }
        Paint::Hatch { style, fore, back } => {
            writer.u8(1);
            writer.i32(*style);
            writer.bytes(fore);
            writer.bytes(back);
        }
        Paint::LinearGradient {
            color1,
            color2,
            p1,
            p2,
        } => {
            writer.u8(2);
            writer.bytes(color1);
            writer.bytes(color2);
            writer.f64(p1.0);
            writer.f64(p1.1);
            writer.f64(p2.0);
            writer.f64(p2.1);
        }
    }
}

fn read_paint(reader: &mut Reader<'_>) -> Result<Paint, String> {
    match reader.u8()? {
        0 => {
            let bytes = reader.bytes_vec(4)?;
            Ok(Paint::Solid([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        1 => {
            let style = reader.i32()?;
            let fore = reader.bytes_vec(4)?;
            let back = reader.bytes_vec(4)?;
            Ok(Paint::Hatch {
                style,
                fore: [fore[0], fore[1], fore[2], fore[3]],
                back: [back[0], back[1], back[2], back[3]],
            })
        }
        2 => {
            let color1 = reader.bytes_vec(4)?;
            let color2 = reader.bytes_vec(4)?;
            Ok(Paint::LinearGradient {
                color1: [color1[0], color1[1], color1[2], color1[3]],
                color2: [color2[0], color2[1], color2[2], color2[3]],
                p1: (reader.f64()?, reader.f64()?),
                p2: (reader.f64()?, reader.f64()?),
            })
        }
        other => Err(format!("record: unknown paint tag {other}")),
    }
}

fn write_appearance(writer: &mut Writer, app: &AppearanceData) {
    writer.u32(app.infos.len() as u32);
    for info in &app.infos {
        writer.u8(u8::from(info.is_pen));
        writer.f64(info.width);
        writer.f64(info.ox);
        writer.f64(info.oy);
        write_paint(writer, &info.paint);
    }
}

fn read_appearance(reader: &mut Reader<'_>) -> Result<AppearanceData, String> {
    let count = reader.u32()? as usize;
    let mut infos = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let is_pen = reader.u8()? != 0;
        let width = reader.f64()?;
        let ox = reader.f64()?;
        let oy = reader.f64()?;
        let paint = read_paint(reader)?;
        infos.push(DrawInfo {
            paint,
            is_pen,
            width,
            ox,
            oy,
        });
    }
    Ok(AppearanceData { infos })
}

fn write_path(writer: &mut Writer, path: &PathData) {
    writer.u32(path.figures.len() as u32);
    for figure in &path.figures {
        writer.u8(u8::from(figure.closed));
        writer.u32(figure.points.len() as u32);
        for &(x, y) in &figure.points {
            writer.f64(x);
            writer.f64(y);
        }
    }
}

fn read_path(reader: &mut Reader<'_>) -> Result<PathData, String> {
    let count = reader.u32()? as usize;
    let mut figures = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let closed = reader.u8()? != 0;
        let point_count = reader.u32()? as usize;
        let mut points = Vec::with_capacity(point_count.min(1 << 20));
        for _ in 0..point_count {
            points.push((reader.f64()?, reader.f64()?));
        }
        figures.push(Figure { points, closed });
    }
    Ok(PathData { figures })
}

/// Serialize `ops` into the plugin's recording format.
pub(super) fn encode_record(ops: &[RecordOp]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(ops.len() as u32);
    for op in ops {
        match op {
            RecordOp::Clear(color) => {
                writer.u8(0);
                writer.bytes(color);
            }
            RecordOp::Path { path, app } => {
                writer.u8(1);
                write_appearance(&mut writer, app);
                write_path(&mut writer, path);
            }
            RecordOp::Blit {
                src,
                sw,
                sh,
                src_rect,
                p0,
                p1,
                p2,
            } => {
                writer.u8(2);
                writer.u32(*sw);
                writer.u32(*sh);
                writer.i32(src_rect.0);
                writer.i32(src_rect.1);
                writer.i32(src_rect.2);
                writer.i32(src_rect.3);
                writer.f64(p0.0);
                writer.f64(p0.1);
                writer.f64(p1.0);
                writer.f64(p1.1);
                writer.f64(p2.0);
                writer.f64(p2.1);
                writer.bytes(src);
            }
        }
    }
    writer.bytes
}

/// Parse a recording produced by [`encode_record`].
pub(super) fn decode_record(bytes: &[u8]) -> Result<Vec<RecordOp>, String> {
    if bytes.len() < 8 || &bytes[..4] != MAGIC {
        return Err("record: not a layerExDraw recording".to_string());
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != VERSION {
        return Err(format!("record: unsupported version {version}"));
    }
    let mut reader = Reader::new(&bytes[8..]);
    let count = reader.u32()? as usize;
    let mut ops = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        match reader.u8()? {
            0 => {
                let color = reader.bytes_vec(4)?;
                ops.push(RecordOp::Clear([color[0], color[1], color[2], color[3]]));
            }
            1 => {
                let app = read_appearance(&mut reader)?;
                let path = read_path(&mut reader)?;
                ops.push(RecordOp::Path { path, app });
            }
            2 => {
                let sw = reader.u32()?;
                let sh = reader.u32()?;
                let src_rect = (reader.i32()?, reader.i32()?, reader.i32()?, reader.i32()?);
                let p0 = (reader.f64()?, reader.f64()?);
                let p1 = (reader.f64()?, reader.f64()?);
                let p2 = (reader.f64()?, reader.f64()?);
                let len = (sw as usize)
                    .checked_mul(sh as usize)
                    .and_then(|n| n.checked_mul(4))
                    .ok_or_else(|| "record: image too large".to_string())?;
                let src = reader.bytes_vec(len)?;
                ops.push(RecordOp::Blit {
                    src,
                    sw,
                    sh,
                    src_rect,
                    p0,
                    p1,
                    p2,
                });
            }
            other => return Err(format!("record: unknown op tag {other}")),
        }
    }
    Ok(ops)
}
