# Driving Glossplay from an AI agent (MCP)

Glossplay can be controlled by an AI agent through the
[Model Context Protocol](https://modelcontextprotocol.io). Started with `--mcp`, it opens its
usual window and takes commands on stdin: open a file or a YouTube link, choose subtitles,
seek, click a word, open the sidebar, resize and place the window, take a screenshot. The
agent prepares the scene; a screen recorder of your choice films it.

```
glossplay --mcp [--quiet] [--size WxH] [FILE or URL]
```

- `--mcp` makes stdin and stdout the MCP channel. Only protocol messages go to stdout;
  diagnostics go to stderr.
- `--quiet` keeps both players (the film and the voice-over) silent and ignores the keyboard,
  for automated checks. Without it the sound plays as usual, so a screen recorder can capture
  it from the system.
- `--size WxH` sets the starting window size; `FILE or URL` is opened once the window is up.

The release build has no console window; stdin and stdout passed as pipes by the MCP client
still work, the same as in a console build.

## Connecting

### Claude Code

```
claude mcp add glossplay -- "C:\Program Files\Glossplay\glossplay.exe" --mcp
```

Add `--scope project` to share the entry through `.mcp.json`, or `-e GLOSSPLAY_CONFIG_DIR=…`
to run the player on a separate settings folder. `claude mcp list` should show
`glossplay … ✔ Connected`.

### Any other MCP client

The usual `mcpServers` entry:

```json
{
  "mcpServers": {
    "glossplay": {
      "command": "C:\\Program Files\\Glossplay\\glossplay.exe",
      "args": ["--mcp"]
    }
  }
}
```

## Protocol

Transport: stdio, one JSON-RPC 2.0 message per line. Both generations of MCP are served:

- **2026-07-28** (stateless): every request carries
  `_meta["io.modelcontextprotocol/protocolVersion"]` and
  `_meta["io.modelcontextprotocol/clientCapabilities"]`; `server/discover` answers with the
  supported versions; results carry `resultType: "complete"` and the server's identity in
  `_meta`. An unknown version gets error `-32022` with the supported list.
- **2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05** (handshake): `initialize` answers with
  the version the client asked for, or with 2025-11-25 if it asked for another one;
  `notifications/initialized` and `ping` are accepted.

Methods: `tools/list`, `tools/call`, `notifications/cancelled` (drops a pending call such as a
long `wait_for`). Tool failures come back as a normal result with `isError: true` and a
readable message ("no file is open: call open first", "word index 7 is out of range: the line
has 5 words"); an unknown tool or malformed arguments object is a JSON-RPC `-32602` error.

Every call answers once its action has taken effect: `open` when the file plays and its
tracks are known, `seek` when the new frame is on screen, `click_word` when the translation
has arrived. A call that cannot finish in its time limit answers with an error; so does every
pending call when the window is closed. Closing stdin closes the window and ends the process,
along with the speech recognition and voice-over servers it started.

## Coordinates

Sizes and positions are physical pixels. "Window" coordinates start at the top-left corner of
the drawing area (no title bar or borders); "screen" coordinates are what a screen recorder
uses. `status` and `set_window` report the drawing area's screen position (`x`, `y`), its size
and the outer frame.

## Tools

All times are in seconds. Results are JSON text.

### Playback

| Tool | Arguments | What it does |
|---|---|---|
| `open` | `path_or_url`, `timeout_seconds?` (90) | Opens a video file, a link (YouTube and other sites yt-dlp knows) or a subtitle file (added to the open video). Answers with the subtitle tracks. |
| `play` | — | Plays (closing an open word card, as the play button does). |
| `pause` | — | Pauses. |
| `seek` | `seconds`, `exact?` (true) | Jumps there; `exact: false` lands on a keyframe, faster. |
| `set_speed` | `speed` (0.25–4) | Playback speed. |
| `set_volume` | `percent` (0–150), `muted?` | Player volume. |
| `status` | — | Position, duration, pause, speed, volume, video size and decoder, title, languages, window geometry, open card and dialogs, voice-over and recognition state, whether a stream is buffering (`buffering`), last message. |
| `video_info` | — | What the Video info window shows, by section: file (title, source — a link by its host only — format, duration, size), video (codec, resolution, frame rate, bitrate, pixel format, colour and HDR, hardware decoding), sound, subtitles, playback (dropped and late frames, output frame rate) and, for a stream, network (cache state, seconds and bytes buffered ahead, download speed). |

### Subtitles and languages

| Tool | Arguments | What it does |
|---|---|---|
| `list_tracks` | — | Subtitle tracks: index, language, origin, state (`not_loaded`, `loading`, `ready`, `failed`, `image`), number of lines, which are primary/secondary; audio tracks; translation provider. |
| `select_subtitles` | `primary?`: index, language or `"none"`; `secondary?`: index, language, `"translate"` or `"off"` | Chooses the subtitles to learn from and the line under them; answers when they are read. |
| `set_languages` | `learn?`, `native?` | Language studied and language translations go into, applied at once (machine translations are made again). |
| `current_lines` | — | Lines at the playhead with timings, translation, the line under them, and every word with its `index` and rectangle. |
| `transcript` | `from?`, `to?`, `limit?` (100) | Lines of the primary subtitles with translations where they exist (they are made around the playhead and the sidebar view). |
| `transcribe` | `start?` | Starts (`true`) or stops (`false`) speech recognition of a local file; without `start` reports its state. |
| `voice_over` | `enabled?`, `original_volume?`, `voice_volume?` | Spoken translation on/off and its volumes in percent of loudness as heard (50 sounds half as loud, −10 dB). Fails with a hint when the engine is not installed. |

### Waiting

`wait_for {condition, value?, timeout_seconds?}` (30 s by default, up to 3600):

| `condition` | Holds when |
|---|---|
| `tracks_ready` | the file is open and the subtitles are read |
| `translation_ready` | the line at the playhead (or the next one) has its translation |
| `transcript_ready` | recognition has finished, or covers `value` seconds |
| `line_shown` | a subtitle line is on screen; `value` = text it must contain |
| `position` | the playhead is at or past `value` seconds |
| `paused`, `playing` | playback state |

### Interface

| Tool | Arguments | What it does |
|---|---|---|
| `click_word` | `index`, `line?` (0) | Clicks the word with that index (from `current_lines`, counting from 0): playback pauses, the word card opens. Answers with the word, translation, and its rectangle (`center`, `screen_center`). |
| `close_word_card` | — | Closes the card; playback resumes if it was playing before the click. |
| `save_word` | — | Saves the word on the card to the word list. |
| `toggle_sidebar` | `open?`, `tab?`: `transcript` / `words` | Sidebar with the transcript or saved words. |
| `search_transcript` | `text` | Types into the sidebar search and returns the matching lines; `""` clears it. |
| `show_controls` | `visible`: `true`, `false` or `"auto"` | `false` hides the control bar, the title strip and pop-up messages, leaving only the video and subtitles; `true` keeps them shown; `"auto"` is the usual behaviour. |
| `open_settings` | `section?`: a tab — `languages`, `subtitles`, `translation`, `speech_recognition`, `voice_over`, `words`, `sources`, `player` (`top` is the first one) — or `help`, `video_info`, `speech_recognition_setup`, `voice_over_setup` | Settings window at that tab, the How to use window, or a setup dialog. `status` reports the open tab as `dialogs.settings_tab`. |
| `close_dialogs` | — | Closes Settings, the How to use window, the Video info window and the setup dialogs without saving. |
| `set_ui_language` | `lang`: `auto`, `en`, `ru`, `uk`, `zh` | Interface language (`zh` is Simplified Chinese). |
| `set_window` | `width?`, `height?`, `x?`, `y?`, `always_on_top?` | Drawing-area size and screen position in pixels (a 1080×1920 window may be taller than the screen), and whether the window stays on top. Answers with the geometry it got. |
| `press_key` | `key`, `text?` | A player shortcut, judged like the keyboard: `space`, `left`, `right`, `up`, `down`, `a`, `s`, `d`, `p`, `f`, `t`, `m`, `i` (the Video info window), `1`, `2`, `[`, `]`, `-` and `=` (subtitle size, 10% steps), `escape`, `ctrl+f`, and `ctrl+v` with `text` standing for the clipboard (a link, magnet link or file path opens; anything else shows a message). Over a dialog only `escape` works. `ctrl+o` is refused: it opens the system file dialog, use `open`. On macOS `ctrl` means ⌘. |
| `screenshot` | `path`, `overwrite?` | Saves the window as drawn (with subtitles, cards, dialogs) to a PNG. |
| `save_frame` | `subtitles?` (false), `path?`, `overwrite?` | Saves the frame on screen to a PNG as the picture's right-click menu does: without subtitles at the video's own size (mpv's screenshot), or with them as the window shows the picture, the control bar and anything over the picture left out. Without `path` it goes to the frames folder of Settings → Player (`Pictures/Glossplay` by default) under a name made of the title and the moment. Answers with the file and its size once it is written. |
| `pointer` | `action`: `move`, `click`, `double_click`, `drag`, `type`, `paste`, `hide`, `where`; `x?`, `y?`, `seconds?`, `text?` | Recording mode only (`GLOSSPLAY_RECORD_DIR`): moves a pointer of its own over the window in window pixels and clicks, drags (from where it is), types into the focused field or pastes `text` as Ctrl+V would, as a mouse and keyboard would; `where` lists the named places on screen (buttons, sliders, the URL field, transcript rows, torrent files) with their centres; the recording shows it as an arrow with a ring at each click. Answers when the move is over. |

## Example: setting up a shot

The agent prepares the scene; the recorder captures the area `status` reports.

1. `set_window {width: 1920, height: 1080, x: 0, y: 0, always_on_top: true}` — a fixed frame
   for the recorder.
2. `open {path_or_url: "D:\\Lectures\\lecture.mp4"}`, then `wait_for {condition: "tracks_ready"}`.
3. `select_subtitles {primary: "en", secondary: "translate"}` and
   `show_controls {visible: false}` — only the video and the two lines on screen.
4. `seek {seconds: 669}` (11:09), `wait_for {condition: "translation_ready"}`.
5. Start the screen recorder; `play`, `wait_for {condition: "position", value: 677}`.
6. `current_lines` to see the words, then `click_word {index: 3}` — the card opens and
   playback pauses; the answer gives the word's `screen_center` for a cursor overlay.
7. After a pause for the viewer, `close_word_card`, `toggle_sidebar {open: true}`,
   `search_transcript {text: "entropy"}`.
8. `screenshot {path: "D:\\Shots\\sidebar.png"}` for a still; stop the recorder.

## Safety and limits

- MCP is off unless the player is started with `--mcp`. It opens no network port: the only
  channel is the stdin/stdout of the process that started it.
- `open` accepts what the player's own Open does — any local file or link — and plays it.
- Files are written only where the agent asks: `screenshot` writes the PNG at the given path,
  in a folder that must exist, and replaces an existing file only with `overwrite: true`.
  `save_frame` does the same with a path; without one it writes a new file in the frames
  folder of the settings and never replaces one there.
  `save_word` writes to the player's word list and keeps a still of the video in the
  player's data folder, as the Save button does.
- `set_languages`, `set_ui_language` and `voice_over` change settings as the Settings window
  does and save them, except under `--quiet`.
- With `--quiet` the keyboard and drag-and-drop in the window are ignored, so typing in
  another window cannot disturb a check.
- A minimized window draws nothing and so answers nothing: calls then fail with a time-out
  message. Keep the window visible while an agent drives it.
- The subtitle size follows the window height, so in a tall 1080×1920 window the lines are
  very large.
