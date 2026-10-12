//! Typed error values for each bottom-core domain.
//!
//! Errors are values, not panics, and carry owned, backend-agnostic data
//! (`String` messages and identifiers) rather than platform error types. The
//! bottom layer constructs them, the middle layer matches on the variants it
//! cares about and surfaces the rest as script-level failures.

use thiserror::Error;

/// Errors from the bottom file-system surface.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum FsError {
    /// An underlying I/O operation failed.
    #[error("file-system I/O error: {message}")]
    Io {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The handle or backend was used in a state that does not permit the
    /// requested operation.
    ///
    /// Unlike [`FsError::Io`], this reports a bad call (for example reading
    /// from a handle opened write-only, or using a file handle that belongs
    /// to a different backend) rather than a failure of the underlying I/O
    /// device.
    #[error("invalid file-system state: {message}")]
    InvalidState {
        /// Human-readable description of the state error.
        message: String,
    },
    /// The requested path or entry does not exist.
    #[error("file-system path not found: {path}")]
    NotFound {
        /// Path or identifier that was not found.
        path: String,
    },
    /// The path is not valid for the file-system backend.
    #[error("invalid file-system path {path}: {reason}")]
    InvalidPath {
        /// Path or identifier that was rejected.
        path: String,
        /// Why it is invalid.
        reason: String,
    },
    /// The operation is not supported by this file-system backend.
    #[error("file-system operation not supported: {operation}")]
    Unsupported {
        /// Name of the unsupported operation.
        operation: String,
    },
    /// Access to the path was denied.
    #[error("permission denied: {path}")]
    PermissionDenied {
        /// Path or identifier that was denied.
        path: String,
    },
    /// The path already exists where it must not.
    #[error("path already exists: {path}")]
    AlreadyExists {
        /// Path that already exists.
        path: String,
    },
    /// A directory was required but the path is not one.
    #[error("not a directory: {path}")]
    NotADirectory {
        /// Path that is not a directory.
        path: String,
    },
    /// A file was required but the path is a directory.
    #[error("is a directory: {path}")]
    IsADirectory {
        /// Path that is a directory.
        path: String,
    },
}

/// Errors from the bottom media decode engine.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MediaError {
    /// An underlying I/O or container read failed.
    #[error("media I/O error: {message}")]
    Io {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The requested media data does not exist.
    #[error("media data not found: {message}")]
    NotFound {
        /// Human-readable description of what was missing.
        message: String,
    },
    /// Decoding failed.
    #[error("failed to decode media ({format}): {message}")]
    Decode {
        /// Detected format/codec/container name, or an empty string when it
        /// could not be determined. Call-site hints belong in `message`.
        format: String,
        /// Human-readable description of the failure.
        message: String,
    },
    /// Encoding failed.
    #[error("failed to encode media ({format}): {message}")]
    Encode {
        /// Target format name (for example `png`, `bmp`, `jpg`).
        format: String,
        /// Human-readable description of the failure.
        message: String,
    },
    /// The format or codec is not supported by this backend.
    #[error("unsupported media format or codec: {message}")]
    Unsupported {
        /// Human-readable description of what is unsupported.
        message: String,
    },
    /// The data is malformed or fails validation.
    #[error("invalid media data: {message}")]
    InvalidData {
        /// Human-readable description of the problem.
        message: String,
    },
    /// The decoder is in the wrong state for the requested operation.
    #[error("invalid media decoder state: {message}")]
    InvalidState {
        /// Human-readable description of the state error.
        message: String,
    },
    /// The stream ended.
    #[error("end of media stream")]
    EndOfStream,
}

/// Errors from the bottom render pipeline.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RenderError {
    /// The render backend cannot provide the requested capability.
    #[error("render backend is not supported: {message}")]
    Unsupported {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The render backend is present, but no usable device could be obtained
    /// or a runtime device operation failed.
    ///
    /// Unlike [`RenderError::Unsupported`], which reports a capability this
    /// build or backend does not provide, this reports a runtime failure while
    /// loading the backend, enumerating physical devices, creating the device
    /// or performing a device operation. `message` carries the underlying
    /// Vulkan result name and description when one is available.
    #[error("render device unavailable: {message}")]
    DeviceUnavailable {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The logical device was lost.
    #[error("render device was lost: {message}")]
    DeviceLost {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The presentation surface was lost.
    #[error("render surface was lost")]
    SurfaceLost,
    /// The presentation surface is out of date.
    #[error("render surface is out of date")]
    SurfaceOutOfDate,
    /// A GPU allocation failed.
    #[error("out of GPU memory: {message}")]
    OutOfMemory {
        /// Human-readable description of the failure.
        message: String,
    },
    /// An argument passed to a render action was invalid.
    #[error("invalid render argument: {message}")]
    InvalidArgument {
        /// Human-readable description of the problem.
        message: String,
    },
    /// Shader compilation or pipeline creation failed.
    #[error("shader or pipeline creation failed: {message}")]
    Shader {
        /// Human-readable description of the failure.
        message: String,
    },
    /// A texture upload failed.
    #[error("texture upload failed: {message}")]
    Upload {
        /// Human-readable description of the failure.
        message: String,
    },
    /// An offscreen target or framebuffer operation failed.
    #[error("framebuffer error: {message}")]
    Framebuffer {
        /// Human-readable description of the failure.
        message: String,
    },
}

/// Errors from the bottom font/glyph rasterizer.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum GlyphError {
    /// Font parsing or loading failed.
    #[error("failed to load font: {message}")]
    Decode {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The font format is not supported by this backend.
    #[error("font format not supported: {message}")]
    Unsupported {
        /// Human-readable description of what is unsupported.
        message: String,
    },
    /// The font data is malformed or inconsistent.
    #[error("invalid font data: {message}")]
    InvalidFont {
        /// Human-readable description of the problem.
        message: String,
    },
    /// The font has no glyph for the requested code point.
    #[error("font has no glyph for U+{codepoint:04X}")]
    MissingGlyph {
        /// Unicode code point that could not be mapped.
        codepoint: u32,
    },
    /// Rasterizing a glyph failed.
    #[error("glyph rasterization failed: {message}")]
    Rasterize {
        /// Human-readable description of the failure.
        message: String,
    },
}

/// Errors from the bottom audio device output.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AudioError {
    /// The audio backend cannot provide the requested capability.
    #[error("audio backend is not supported: {message}")]
    Unsupported {
        /// Human-readable description of the failure.
        message: String,
    },
    /// No output device is available.
    #[error("no audio device available: {message}")]
    DeviceUnavailable {
        /// Human-readable description of the failure.
        message: String,
    },
    /// Opening the output stream failed.
    #[error("failed to open audio device: {message}")]
    Open {
        /// Human-readable description of the failure.
        message: String,
    },
    /// A running output stream failed.
    #[error("audio stream error: {message}")]
    Stream {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The requested audio format is invalid or unsupported.
    #[error("invalid audio specification: {message}")]
    InvalidSpec {
        /// Human-readable description of the problem.
        message: String,
    },
    /// The output stream is already running.
    #[error("audio stream is already running")]
    AlreadyRunning,
    /// The output stream is not running.
    #[error("audio stream is not running")]
    NotRunning,
}
