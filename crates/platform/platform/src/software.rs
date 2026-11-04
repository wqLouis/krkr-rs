//! The software presenter: a CPU frame copied into the window.
//!
//! The window is the platform's own: this module adds a way to put pixels
//! on it without a GPU, so a frame the CPU rasterizer composited can be
//! looked at. The copy itself is the display server's: the presenter binds
//! to the window's display connection and window handle, and the pixels are
//! handed over in the format the server expects.

use std::num::NonZeroU32;
use std::sync::Arc;

use softbuffer::{Context, Surface};
use winit::window::Window;

use crate::PlatformError;

/// A software presenter that copies a CPU frame into a window.
///
/// The presenter owns the window's display binding, so it lives as long as
/// the window it presents into.
pub struct SoftwareSurface {
    /// The surface the frames are copied into.
    surface: Surface<Arc<Window>, Arc<Window>>,
    /// The size the surface was last sized to.
    size: (u32, u32),
}

impl SoftwareSurface {
    /// Creates a presenter for `window`.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Window`] when the window's display server offers no
    /// software surface.
    pub(crate) fn new(window: Arc<Window>) -> Result<Self, PlatformError> {
        // The context owns the display connection; the surface keeps the
        // connection alive through its own handle, so the context is dropped
        // with the binding it made.
        let context = Context::new(Arc::clone(&window)).map_err(presenter_error)?;
        let surface = Surface::new(&context, window).map_err(presenter_error)?;
        Ok(Self {
            surface,
            size: (0, 0),
        })
    }

    /// Presents an RGBA8 frame to the window.
    ///
    /// The frame's colour is copied into the window's pixels; its alpha is
    /// dropped, because the window's own surface is opaque.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Window`] when the frame does not match `size`, or
    /// when the display server refuses the copy.
    pub fn present(&mut self, pixels: &[u8], size: (u32, u32)) -> Result<(), PlatformError> {
        let width = NonZeroU32::new(size.0)
            .ok_or_else(|| PlatformError::Window("the presented frame has no width".to_owned()))?;
        let height = NonZeroU32::new(size.1)
            .ok_or_else(|| PlatformError::Window("the presented frame has no height".to_owned()))?;
        let expected = (width.get() as usize)
            .checked_mul(height.get() as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                PlatformError::Window("the presented frame does not fit the host".to_owned())
            })?;
        if pixels.len() != expected {
            return Err(PlatformError::Window(format!(
                "the presented frame is {} bytes, expected {expected}",
                pixels.len()
            )));
        }
        if self.size != (size.0, size.1) {
            self.surface
                .resize(width, height)
                .map_err(presenter_error)?;
            self.size = (size.0, size.1);
        }
        let mut buffer = self.surface.buffer_mut().map_err(presenter_error)?;
        for (target, source) in buffer.iter_mut().zip(pixels.as_chunks::<4>().0) {
            // The window's pixels are read as `0x00RRGGBB`: the red channel
            // in the high byte, the blue channel in the low one, and no
            // alpha.
            *target =
                (u32::from(source[0]) << 16) | (u32::from(source[1]) << 8) | u32::from(source[2]);
        }
        buffer.present().map_err(presenter_error)?;
        Ok(())
    }
}

/// Turns a presenter failure into a typed platform error.
fn presenter_error(error: softbuffer::SoftBufferError) -> PlatformError {
    PlatformError::Window(format!("software surface: {error}"))
}
