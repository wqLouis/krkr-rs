# `app::video` — the L1-backed video source

Source: `crates/app/src/video.rs`

`compat` never links `media`. The `VideoOverlay` native and the
`LayerExMovie` members read decoded streams through the
`compat::VideoSource` seam, and this module is the composition root that
fills it: it reads each stream's bytes through the L2 `Storage`, opens them
with the L1 decoder and serves frames and the decoded audio track.

```rust
pub unsafe fn install_video_source(
    host: &CompatHost,
    media: &Media,
    storage: &Storage,
) -> Result<(), CompatError>;
```

One source is installed on **both** seams — `compat::set_video_source` for
the overlay and `compat::set_movie_source` for the movie — so a file opened
through either surface uses the same decoder.

---

## 1. The decoder cache

```rust
struct Inner {
    media: Media,                       // the stateless L1 backend
    storage: *const Storage,            // the L2 storage the bytes come from
    stats: Rc<StartupStats>,            // the start-up counters
    streams: HashMap<String, OpenStream>,  // path → spec + decoder
}
```

The source is cloned into the two seams and the clone shares one
`Rc<RefCell<Inner>>`, so a file is decoded once.

`open(path)` reads the bytes through the storage, opens them with
`media::open_video` and caches the stream under its storage path. A read
failure, a decode failure and an open failure are all typed `MediaError`s —
never a silent absence.

`frame(path, at_ms)` decodes the frame at the given timestamp. The decoded
frame is shared with the decoder (`Arc<RgbaFrame>`), so the caller does not
copy its pixels. A decode failure is reported as absence after a warning:
the video seam callers log it and report "no frame".

`audio(path)` returns the decoded audio track, or `Ok(None)` when the
container has no audio stream.

`spec(path)` returns the stream description, opening the stream if needed; a
failure is reported as absence after a warning.

---

## 2. The storage lifetime

The compat state holds the installed source, so the source is `'static`,
while the bytes come from the L2 `Storage` the game host owns. The source
therefore holds the **storage address**, and the invariant is an ordering
one:

> the `Storage` must stay alive for as long as the compat host remains
> installed.

`GameHost` declares the VM host before the storage, so the source drops
first and the invariant holds. The module is the one place in the crate
where this is written down, and `install_video_source` is `unsafe` so the
caller states the obligation at the call site.

---

## 3. Headless and windowed hosts

Decoding and layer compositing need no display, so the source is installed
for headless and windowed hosts alike.

The one video mode with no display-independent meaning is `vomOverlay`,
whose frames the reference presents through an OS overlay window. Leaving
it alone is the faithful behaviour rather than fabricating a surface.

---

## 4. Failure modes

| situation | behaviour |
|---|---|
| the storage read fails | typed `MediaError::Io`, surfaced to the caller |
| the container cannot be opened | typed `MediaError`, surfaced to the caller |
| a frame cannot be decoded at a timestamp | a warning, and the frame is reported as absent |
| the spec cannot be read | a warning, and the spec is reported as absent |
| the host has no bound context | `CompatError::Engine` |

---

## 5. Tests

The video source is covered from the outside, through the production game
host: the seam installation is exercised by the game-host tests, and the
decoder behaviour itself is pinned by the L1 media crate's own tests. The
module has no unit test of its own, because everything it does is a
composition of an L1 decoder and an L2 storage read.

---

## 6. Related documents

| topic | document |
|---|---|
| the crate overview and the wiring | [README.md](README.md) |
| the game host and the seam installation order | [game.md](game.md) |
| the L2 video overlay | [`../engine/video.md`](../engine/video.md) |
| the L3 video natives that read the seam | [`../compat/video.md`](../compat/video.md) |
| the L1 media backends and the video decoders | [`../arch/l1.md`](../../arch/l1.md) §4 |
| the media engine | [`../../migration/05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) |
