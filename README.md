# Glossplay

**English** · [Русский](README.ru.md) · [Українська](README.uk.md) · [中文](README.zh.md)

A video player for people who learn languages by watching. It shows the original line and its
translation together, writes subtitles for videos that have none, explains any word you click,
and keeps the words you want to remember.

![Dual subtitles with a word looked up](docs/screenshots/en/word.png)

## What it does

- **Dual subtitles.** The original on top, the translation below. If the file has a track in
  your language, Glossplay uses it; if not, it translates the original as you watch, a little
  ahead of playback so the translation is already there when the line is spoken.
- **Subtitles from speech.** No subtitles at all? Whisper transcribes the audio locally. It is
  set up with one button: Glossplay picks the build that suits your graphics card, downloads it
  and a model. On a recent NVIDIA card an hour of video takes a couple of minutes, and the part
  you are watching is always done first. Without an NVIDIA card it runs on the processor with a
  smaller model and is slower: about as fast as the video plays.
- **Click a word.** Playback pauses and you get the translation, plus an explanation of the
  word *in this sentence* if you use an LLM for translation. Japanese and Chinese are split
  into words properly.
- **Your own word list.** Save a word with its sentence and the moment in the video. Send it
  to Anki with one click — the card gets a screenshot and an audio clip of the line.
- **Practice tools.** Auto-pause after every line, repeat the line, jump line by line, and a
  searchable transcript next to the video.
- **Voice-over.** The translated lines can be spoken over the video while the original sound is
  turned down; the volume of the original and of the voice are set separately. The voice engine
  (OmniVoice, run through audio.cpp) is downloaded on request. It is off by default, needs a
  graphics card, and is for non-commercial use only (the model weights are CC BY-NC).
- **Plays nearly everything.** Playback is done by [mpv](https://mpv.io), so anything FFmpeg
  can open works: MKV, MP4, WebM, AVI, MOV, TS; H.264, HEVC, AV1, VP9 and the rest, with
  hardware decoding. Paste a link to an online video and it plays: Glossplay downloads yt-dlp by itself and keeps it up to date. On the first launch it fetches yt-dlp (about 23 MB) into its own data folder.
- **Interface** in English, Russian and Ukrainian. You choose in Settings; by default it follows
  the language of the system.
- **Your own sources.** Search through your own search server and watch through your own
  TorrServer. The player contains no sources.

![Transcript made by speech recognition, with translations](docs/screenshots/en/transcript.png)

## Status

This is version 0.1, the first usable release.

| | |
|---|---|
| Windows 10/11 x64 | Built and tested by hand, on Windows 11 with an NVIDIA GPU. |
| Linux, macOS | The code builds and the test suite passes there in CI. **Nobody has run the interface on them yet** — reports are welcome. Speech recognition and voice-over have to be set up by hand there: the one-button install is for Windows only. |

Known limits of this version:

- Live transcription may not keep up with playback without a graphics card; on the processor,
  let it finish first.
- Transcription works for local files and for streams from your own TorrServer; for video
  played from a link on a web page it does not.
- Voice-over was tried only on an NVIDIA card. Its Vulkan build is meant for any graphics card
  but has not been tried on AMD or Intel.
- The quality of the voice depends on the model; one-word lines ("Yes.", "No.") can sound uneven.
- On Windows only NVIDIA cards decode without copying frames; other GPUs use hardware decoding
  with a copy (`d3d11va-copy`), which is fine up to 4K but uses more CPU.
- Image-based subtitles (Blu-ray PGS, DVD) are shown by mpv as pictures: no word lookup.
- The styling of ASS subtitles is not reproduced; they are shown as plain text.

## Install

### Windows

1. Download `glossplay-windows-x64.zip` from [Releases](../../releases) and unpack it.
2. Install FFmpeg — it is used to read subtitle tracks and audio:
   `winget install Gyan.FFmpeg`
3. Run `glossplay.exe`, drop a video on the window.

Speech recognition and voice-over need the Microsoft Visual C++ Redistributable 2015–2022 (x64).
Most computers already have it. If speech recognition does not start, install it:
`winget install Microsoft.VCRedist.2015+.x64`

Optionally, turn on "Show Glossplay in Explorer's Open with and context menu" in Settings: it adds entries for the current user only and does not make Glossplay your default player. On Windows 11 the entry is under "Show more options".

### Linux and macOS

Build from source (below) and install mpv's library and FFmpeg:

```sh
# Debian / Ubuntu
sudo apt install libmpv2 ffmpeg
# macOS
brew install mpv ffmpeg
```

## Setting up the extras

Everything here is in **Settings** (the gear button). Nothing is required for plain dual
subtitles: translation works out of the box through Google Translate.

**Translation.** Pick a service:

| Service | Key | Notes |
|---|---|---|
| Google Translate | not needed | The default. Fast, literal. |
| DeepL | DeepL API key | Better wording for European languages. |
| OpenAI-compatible | depends | OpenAI, or a local model through Ollama / LM Studio (`http://localhost:11434/v1`). Explains words in context. |
| Claude | Anthropic API key | Translates with the surrounding dialogue in mind. Explains words in context. |

Privacy: with machine translation the subtitle text is sent to the service you chose. A local
model (Ollama / LM Studio) sends nothing to the internet, and speech recognition is always local.

The key is stored in `config.toml` in your user profile. The environment variable
`GLOSSPLAY_DEEPL_API_KEY`, `GLOSSPLAY_OPENAI_API_KEY` or `GLOSSPLAY_ANTHROPIC_API_KEY` (for the matching provider only) overrides it.

**Speech recognition.** Choose **Subtitles → Transcribe speech…**, or **Settings → Speech
recognition → Install**. Glossplay checks your graphics card and downloads
[whisper.cpp](https://github.com/ggml-org/whisper.cpp), a Whisper model and the Silero voice
detector (it stops Whisper from inventing text over silence and music):

| Your computer | What is installed | Download |
|---|---|---|
| NVIDIA card, driver 525 or newer | NVIDIA build, model `large-v3-turbo` | about 2.3 GB |
| The same, with little video memory (under about 3.8 GB) | NVIDIA build, model `large-v3-turbo-q5_0` | about 1.3 GB |
| No NVIDIA card (or an older driver) | processor build, model `small` | about 500 MB |

The dialog shows the exact size before anything is downloaded, and lets you pick the build and
the model (`large-v3-turbo`, `large-v3-turbo-q5_0`, `small` or `base`) yourself. An interrupted
download continues where it stopped. Without an NVIDIA card recognition is slower — about as fast
as the video plays — so give it a head start before you watch. On the first transcription the
NVIDIA build can take up to a minute to prepare for the card (seen on the RTX 50 series). Use
**Subtitles → Transcribe speech**, or tick "Transcribe automatically". Transcripts are cached,
so a video is only transcribed once.

*Advanced: use my own whisper.cpp.* Under Settings → Speech recognition → Advanced you can point
Glossplay at your own `whisper-server` and model file. Get a whisper.cpp build that contains
`whisper-server`: the newest release often has no files, so open the
[list of all releases](https://github.com/ggml-org/whisper.cpp/releases) and take the latest one
that has files (Assets); its tag looks like `b5454`. For an NVIDIA card on Windows take
`whisper-bin-win-cuda-12.4.0-x64.zip`; for other video cards or the CPU, `whisper-bin-x64.zip`.
Models are at [Hugging Face](https://huggingface.co/ggerganov/whisper.cpp/tree/main); put
[`ggml-silero-v5.1.2.bin`](https://huggingface.co/ggml-org/whisper-vad/tree/main) in the same
folder as the model.

**Voice-over.** Turn it on with **Subtitles → Voice-over (dub the translation)**, or set it up in
Settings → Voice-over. It speaks the translation, so a translation has to be shown first. The
first time, Glossplay offers to download the voice engine: audio.cpp (release v0.9.5) from GitHub
and the OmniVoice model (`omnivoice-q8_0`) from Hugging Face. The voice is made on your
computer, so nothing is sent anywhere.

| Build | Download |
|---|---|
| Vulkan — any graphics card (the default) | about 1.44 GB |
| NVIDIA (CUDA) — a little faster, much bigger | about 2.21 GB |

A graphics card is needed: on the processor alone the voice is 6–10 times slower than the video
plays and cannot keep up. The voice speaks in the "Translate into" language. In the Subtitles menu
two sliders set the volume of the original sound (25% by default) and of the voice (100%);
the first start on a graphics card can take up to half a minute.

Terms of use, which you accept before the download:

- Non-commercial use only. The OmniVoice model is licensed under CC BY-NC 4.0: neither the model
  nor what it says may be used for commercial purposes.
- Do not use the voice engine to imitate a real person's voice without their consent.
- Built with Higgs Materials licensed from Boson AI USA, Inc. (the audio tokenizer inside the
  model, under the Boson Higgs Audio 2 Community License).

*Advanced: my own speech server.* If you already run a server with an OpenAI-compatible
`/v1/audio/speech`, put its address in Settings → Voice-over → Advanced. It is stored in the
`[tts]` section of `config.toml`: `base_url` (for example `http://host:port/v1`; empty means the
installed engine), `model`, `voice` (empty means the server's default), `original_volume` and
`voice_volume` in percent, and `enabled` (off by default).

**Anki.** Install the [AnkiConnect](https://ankiweb.net/shared/info/2055492159) add-on and keep
Anki running. The deck and the "Glossplay" note type are created on first use.

**Your own sources.** Glossplay can search through a search server that speaks the Torznab
protocol ([Jackett](https://github.com/Jackett/Jackett), [Prowlarr](https://prowlarr.com)) and
play the result through [TorrServer](https://github.com/YouROK/TorrServer) without waiting for the
whole download, with everything the player does: dual subtitles, translation, click a word,
speech recognition.

Glossplay contains no sources and recommends none. Which sources you add to your search server
and what you watch through them is your decision and your responsibility; respect copyright and
the laws of your country.

1. Install and set up a search server and TorrServer. If you already use them (for example with
   a TV app), there is nothing to install.
2. In Settings, in the **Sources** section, enter the Torznab address (for Jackett it looks like
   `http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab/api`), the API key and the
   TorrServer address (like `http://127.0.0.1:8090`). Press **Check**.
3. The search field now appears on the start screen, and **Ctrl+F** works in the player.

Type a query and you get a list (title, size, seeds, source); double-click a row to watch. If the
torrent holds several video files, you choose the file.

Limits: subtitles embedded in a streamed file can only be read once the whole file has been
downloaded (`.srt` files lying next to the video in the torrent are picked up at once); a
TorrServer protected by a password is not supported; the search stays hidden until both addresses
are filled in. The settings live in the `[sources]` section of `config.toml` (`torznab_url`,
`torznab_key`, `torrserver_url`); the environment variable `GLOSSPLAY_TORZNAB_KEY` overrides the
key.

## Keys

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `A` / `D` | Previous / next line |
| `S` | Repeat the current line |
| `P` | Auto-pause after every line |
| `←` / `→` | Back / forward 5 seconds |
| `↑` / `↓` | Volume |
| `[` / `]` | Slower / faster |
| `1` / `2` | Hide or show the original / the translation |
| `T` | Transcript and saved words |
| `F`, double click | Full screen |
| `M` | Mute |
| `Ctrl+O` | Open a file |
| `Ctrl+F` | Search your sources (shown when sources are set up) |
| `Esc` | Close the word card, leave full screen |

Subtitle files next to the video (`Movie.srt`, `Movie.en.srt`, `Movie.ru.ass`) are picked up
automatically. You can also drop a subtitle file on the window.

## Roadmap

No dates are promised; this is the direction.

- Export a video to a file with translated tracks, single or dual subtitles burned in, and a voice-over track.
- A catalogue of films and series, with posters and descriptions, on top of your own sources.
- A built-in torrent engine, so a separate TorrServer is not needed.
- Faster speech recognition on AMD and Intel graphics cards.
- Playback without copying frames on AMD and Intel graphics cards on Windows.
- Tested builds and packages for Linux and macOS.
- Dictionaries in the Yomitan format, and dictionary forms of words.
- Voice-over in the voice of the original speaker — only with the consent of whoever owns that voice.
- Chinese and other interface languages.
- An installer for Windows, and an entry in the short context menu of Windows 11.

## Build from source

You need [Rust](https://rustup.rs) 1.95 or newer and a C compiler.

```sh
git clone https://github.com/Magerko/glossplay
cd glossplay
cargo build --release
```

The player loads mpv's library at run time, so nothing has to be linked:

- **Windows:** run `scripts/fetch-mpv.ps1` — it downloads `libmpv-2.dll` next to the built
  program. Or put the DLL from an [mpv dev build](https://github.com/shinchiro/mpv-winbuild-cmake/releases)
  there yourself.
- **Linux / macOS:** the system `libmpv` is found automatically.

To build a release for distribution on Windows, use `scripts/build-release.ps1`: it strips the paths of your machine (home folder, user name) out of the executable. `scripts/package.ps1` then packs the zip with `glossplay.exe`, `libmpv-2.dll` and the licence files.

Run the tests with `cargo test --workspace`. The player can also test itself on a real file:

```sh
glossplay --selftest video.mkv --subs en,ru --at 30
```

It plays for a few seconds, prints what it saw (frame size, decoder, dropped frames, the
subtitle lines on screen) and exits with a non-zero code if something is wrong. `glossplay --help`
lists the other options, among them `--install-asr`, `--install-tts` and `--asr-status` for
installing and checking the components from the command line.

## How it is built

Rust throughout, in five crates:

| Crate | What it holds |
|---|---|
| `gp-core` | Subtitle parsers (SRT, WebVTT, ASS), word segmentation, practice logic, config. No I/O. |
| `gp-store` | SQLite: translation cache, transcripts, saved words. |
| `gp-ai` | Translation services, the Whisper client, the speech synthesis client, AnkiConnect, the Torznab and TorrServer clients. |
| `gp-media` | A small run-time binding to libmpv; FFmpeg, whisper-server and the voice engine as child processes. |
| `glossplay` | The application: an [egui](https://github.com/emilk/egui) interface that shares one OpenGL context with mpv. |

mpv renders each frame into a texture on the GPU and egui draws the subtitles on top, so every
word is a real interface element you can hover and click, and no frame is copied through the
CPU. Network requests, ffmpeg, speech recognition and the voice-over run on background threads, so playback never stalls while they work.

## Credits and licence

Glossplay is free software under the [GNU GPL v3](LICENSE) or later.

It stands on [mpv](https://mpv.io) and [FFmpeg](https://ffmpeg.org),
[whisper.cpp](https://github.com/ggml-org/whisper.cpp), [egui](https://github.com/emilk/egui),
[Lindera](https://github.com/lindera/lindera) and [jieba-rs](https://github.com/messense/jieba-rs).
The voice-over uses [audio.cpp](https://github.com/0xShug0/audio.cpp) (Apache-2.0) and the
[OmniVoice](https://github.com/k2-fsa/OmniVoice) model; the model is not part of the release:
you download it yourself, on its own terms (see above).
The idea of a player built around language learning owes a lot to
[LLPlayer](https://github.com/umlx5h/LLPlayer).

The Windows release bundles `libmpv-2.dll` from
[shinchiro's mpv builds](https://github.com/shinchiro/mpv-winbuild-cmake) (GPL; the source is
available there). The archive also contains `THIRD-PARTY.txt` and `THIRD-PARTY-RUST.txt` with the
versions, sources and licences of all third-party components.

Screenshots show the trailer of *Sintel* — © Blender Foundation, [durian.blender.org](https://durian.blender.org), CC BY 3.0.
