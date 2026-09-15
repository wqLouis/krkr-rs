<p align="center">
  <img src="icon.png" alt="krkr-rs" width="180">
</p>

<h1 align="center">krkr-rs</h1>

<p align="center">
  <b>基于 <a href="https://github.com/2468785842/krkr2">krkr2</a> 原版源码移植、
  用 Rust + Bevy 重写的 KiriKiri2（TVP）视觉小说引擎。</b>
</p>

<p align="center">
  <a href="README.md">English</a> · <b>简体中文</b>
</p>

---

krkr-rs 可以直接运行**未经修改的真实 KiriKiri 游戏** —— 游戏自带的 `.xp3`
封包、`.tjs` 脚本和 `.ks` 剧本原封不动。指向游戏目录即可开玩：

```bash
./target/debug/krkr-rs run "/path/to/game"
```

菜单 → 标题 → 剧本 → 对话 → 存档 → 影片，跑的都是真实游戏。游戏文件不会被修改、
转换或重新打包。

## 突破点

**整个引擎是被重写的，而不是套壳。**

这是一个**移植项目**，而不是凭空造轮子。原版 KiriKiri2/TVP 的 C++ 源码 ——
[krkr2](https://github.com/2468785842/krkr2) —— 是本项目所有原生语义、文件格式与
边界行为的参考：这些行为都是从源码中读出来并如实复现的，而不是靠猜测或自我发挥。
krkr-rs 带来的，是在这套行为之上换了一个应用外壳：原版的 Android 平台层建立在
**Cocos2d-x** 之上，而 krkr-rs 用 **Rust + Bevy** 替换了整套技术栈，并通过
**Vulkan** 渲染（经由 Bevy/wgpu，因此也顺带支持 Metal、DirectX 12 与 GLES；
同一套代码还能编译成 Android 应用）。

除脚本语言本身之外，几乎所有部分都是 Rust 的原生重写 —— 每一处都对照参考实现逐项
移植：

- **封包与存储** —— 真实读取 `.xp3`，支持链式索引，实现 `Storages` /
  `Scripts` 接口，以及游戏依赖的大小写不敏感语义。
- **图形** —— 支持 KiriKiri 自有的 **TLG5/TLG6** 格式，以及 PNG/JPEG/WebP/BMP/GIF
  等多种格式；完整的 `Layer` 合成模型、引擎全部混合模式、仿射图层、转场与截屏。
- **文字** —— `.tft` 预渲染字体与真正的字体光栅化，并保持排版所依赖的基线与测量
  行为。
- **音频** —— Ogg Vorbis 与 Opus、`.sli` 循环、`WaveSoundBuffer` /
  `SoundChannel`，通过平台音频栈混音与推流。
- **影片** —— MP4/H.264/AAC：桌面端走 FFmpeg，Android 端走 **MediaCodec**。
- **KAG 剧本语言** —— 解析器与标签、存档读档、游戏内的存读档与设置界面。
- **输入、窗口，以及游戏会调用到的 Win32 风格接口** —— 其中包括大量通过与原版
  逐项比对找出的原生成员，并由工具自动进行接口一致性校验。

有一个刻意的例外：**TJS2 虚拟机仍然是原版 C++ 实现** —— 内置在仓库中，仅做极少
量修补，通过一层很薄的 Rust FFI 调用。在这里，语言与脚本层面的兼容性比"用 Rust
重写虚拟机"更重要，也正是它让未经修改的游戏能够运行。

而且它并非只支持桌面：仓库中已经包含 **Android 前端** —— 一个原生 **Material 3**
启动器，通过系统文件选择器挑选游戏目录，然后交给同一套引擎运行；使用 NDK 针对
`arm64-v8a` 构建。

## 特别鸣谢

没有 **[krkr2](https://github.com/2468785842/krkr2)**（作者
[@2468785842](https://github.com/2468785842)）就不会有 krkr-rs —— 它的
KiriKiri2/TVP 源码是本次移植的参考实现。

能够直接阅读原版实现，是"忠实重写"得以成立的前提：原生语义、文件格式与各种边界
情况都是从它移植而来，而不是靠猜测；它的行为也正是本项目用来衡量自己的标准。
在此向作者，以及所有让 KiriKiri 得以延续的人致以诚挚谢意。

## 构建与运行

```bash
cargo build

# 带窗口运行
./target/debug/krkr-rs run "/path/to/game"

# 无头冒烟测试（不需要窗口或 GPU，只输出一次场景信息）
./target/debug/krkr-rs run "/path/to/game" --headless
```

影片播放依赖系统的 FFmpeg 库；在无法提供 FFmpeg 的系统上可以关闭它
（`cargo build --no-default-features`），其余功能两种配置下都能构建。

Android 构建请见 [`android/README.md`](android/README.md)。

## 文档

- [`docs/`](docs) —— 设计与调研笔记（可移植性、字体、渲染、接口一致性）。
- [`docs/android.md`](docs/android.md) —— Android 移植：架构、决策、里程碑与待定
  问题。
- [`docs/native_parity.md`](docs/native_parity.md) —— 如何用工具自动校验引擎的
  原生接口与原版一致。
- [`TODO.md`](TODO.md) —— 已完成、接下来要做的，以及完整的调研记录。

## 许可证

MIT 或 Apache-2.0，任选其一。
