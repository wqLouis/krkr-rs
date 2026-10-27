# Video fixtures

Short clips whose every picture differs, so a decoder that keeps returning
one picture shows up as a frozen clip. The clips are committed, so the tests
read them from this directory and never run a command line tool: the CI
installs the FFmpeg libraries but not the `ffmpeg` command line.

Every clip is the `testsrc` pattern at 64x64 and ten pictures a second for
two seconds, which is twenty pictures. The pattern draws a running counter, so
no two pictures of a clip are the same.

The clips are made with the `ffmpeg` command line. To make them again:

```sh
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc=size=64x64:rate=10:duration=2 \
  -c:v libx264 -pix_fmt yuv420p clip.mp4

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc=size=64x64:rate=10:duration=2 \
  -vf 'fps=10,split[a][b];[a]palettegen=max_colors=64[p];[b][p]paletteuse' \
  clip.gif

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc=size=64x64:rate=10:duration=2 \
  -c:v libvpx-vp9 -pix_fmt yuv420p -b:v 200k clip.webm

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc=size=64x64:rate=10:duration=2 \
  -c:v libx264 -pix_fmt yuv420p -f h264 clip.h264

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc=size=64x64:rate=10:duration=2 \
  -f lavfi -i sine=frequency=440:sample_rate=44100:duration=2 \
  -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest clip_av.mp4
```

What the clips hold, as `ffprobe` reports it:

| file | container | video | audio | pictures | size |
|---|---|---|---|---|---|
| `clip.mp4` | `mov,mp4,m4a,3gp,3g2,mj2` | h264, 64x64, 10 fps | - | 20 | 3470 |
| `clip.gif` | `gif` | gif, 64x64, 10 fps | - | 20 | 9204 |
| `clip.webm` | `matroska,webm` | vp9, 64x64, 10 fps | - | 20 | 4522 |
| `clip.h264` | `h264` | h264, 64x64, 20/25 fps | - | 20 | 2401 |
| `clip_av.mp4` | `mov,mp4,m4a,3gp,3g2,mj2` | h264, 64x64, 10 fps | aac, 44100 Hz, 1 channel | 20 | 22317 |

`clip.h264` is a raw elementary stream: it carries no index and no
timestamps, so it is the clip that shows how a stream without them is read.

The `clip.mp4`, `clip.gif`, `clip.webm` and `clip_av.mp4` clips carry a
timestamp on every picture, 100 milliseconds apart. The raw stream carries
none, and its pictures are stamped by the picture clock instead.
