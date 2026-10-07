//! krkr-rs bottom render pipeline.
//!
//! Stateless `RenderPipeline` object + `RenderScene` tree + `RenderState`
//! value, plus the `FontSystem`/`Font` glyph rasterizer. All traversal,
//! composition, batching and drawing happens here; the middle layer only
//! keeps the tree current. See `docs/migration/04-renderer-vulkan.md`.
