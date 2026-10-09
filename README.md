# Speech Output Engine

The Speech Output Engine reads documents aloud and describes photos. It is
written in Rust, and accessibility is its guiding principle: every feature
works from the keyboard, and the whole interface is exposed to screen readers
(Narrator, NVDA and JAWS on Windows, VoiceOver on macOS, Orca on Linux).

The program file is called `speechout`.

## What it does

- **Reads PDF, TXT, DOCX, ODT, CSV, ODS, PPTX and PPT files aloud** with the
  voices built into your computer, or with cloud voices from **ElevenLabs**, **OpenAI**, **Deepgram
  Aura** or **Speechify** once you have saved an API key for that service.
- **Saves the spoken text as an audio file**, in MP3 or WAV format.
- **Describes photos** (JPEG and HEIC/HEIF) using a local AI model running in
  [Ollama](https://ollama.com). The photo never leaves your computer. If
  Ollama isn't installed when you choose a photo, the app offers to install it
  with winget (Windows), Homebrew (macOS) or Snap (Linux), or to start it if it
  is installed but not running.
- **Plays WAV and MP3 files and transcribes speech in them.** The app listens
  to the waveform, and if the file is mostly spoken word, it writes down what
  is said using [Whisper](https://github.com/openai/whisper), an AI model that
  runs on your computer, so the audio never leaves it. It can also label who
  is speaking (Speaker 1, Speaker 2 and so on), again on your computer.
- **Plays a zip file of WAV and MP3 files as a playlist**, one track after
  another. A `listing.txt` file in the zip sets the order.
- **Says where a photo was taken**, if you turn this on and the photo has GPS
  information. The place name is looked up with OpenStreetMap.
- **Applies wordlists** that fix pronunciation (for example "Leicester" →
  "Lester") or keep reading classroom-safe. Example wordlists are included, and
  you can import your own.

## The window

The window has four tabs across the top: **General**, **Settings**,
**Wordlists** and **Audio Player**. Every button, box and dropdown sits on its own line and fills the
width of the window. At the bottom, a progress bar shows how far through
reading aloud or saving audio you are, and a status line below it reports what
is happening and any errors. Screen readers announce the status line
automatically.

### General

| Control | What it does |
| --- | --- |
| Text to read | "Choose File" reads a file you choose. "Paste Text" shows a box to type or paste text into instead. |
| Current file | Shows the file you chose. Shown with "Choose File". |
| Choose a file… | Opens a PDF, TXT, DOCX, ODT, CSV, ODS, PPTX, PPT, JPEG or HEIC file. Photos are described by the local AI model. CSV and ODS files are read as a table, row by row, and each sheet of an ODS file is read in turn. PowerPoint presentations are read slide by slide, starting each with its number; speaker notes are left out. In PPTX files, SmartArt is read box by box, and charts are read as their title followed by each series with its values, such as "North. Q1: 10. Q2: 20." Shown with "Choose File". |
| Text | The text to read aloud, up to 200,000 characters. Shown with "Paste Text". Invisible characters that could hide or reorder words are removed before it is read. |
| Speech service | System voices, plus any cloud service with a saved API key. A line underneath says whether speech is made on your computer or your text is sent to the service. |
| Voice | The voice to use. The app remembers the voice you chose for each service. |
| Speaking speed | Only for ElevenLabs (0.7 to 1.2 times normal speed) and Deepgram Aura (0.7 to 1.5 times). Remembered for each service. |
| Preview voice | Speaks "This is a preview of the selected voice." with the chosen voice and speed. |
| Read aloud / Pause / Stop | Controls speech. |
| Audio file format | MP3 or WAV. |
| Save spoken text as audio… | Renders the whole text to a file. |
| Copy text to the clipboard | Copies the document's text, or the photo's description, so you can read or paste it elsewhere. |

### Audio Player

| Control | What it does |
| --- | --- |
| Current file | Shows the audio file you chose. |
| Choose an audio file… | Opens a WAV or MP3 file, or a zip file of them to play as a playlist. The app says how long it is and whether it sounds like speech, music or silence. A long file takes a moment to open; Escape or Stop cancels it. |
| Track | Only for a playlist. Chooses which track to play. |
| Waveform | A picture of the sound, with the part already played highlighted. Screen readers read its length and what it sounds like. |
| Play / Pause / Stop | Controls playback. Play carries on from where you paused or moved to. |
| Back 10 seconds / Forward 10 seconds | Moves through the file, and says the new position. |
| Previous track / Next track | Only for a playlist. Moves to the track before or after, and plays it if the track you were on was playing. |
| Label who is speaking | Whether the transcript says who is speaking. Choose to have the app work out how many people there are, or choose the number yourself, which is more accurate. |
| Transcribe the speech | Writes down what is said. This happens by itself when a file sounds like speech; press this to try a file that didn't, or to transcribe again with a different model. While transcribing, it becomes **Stop transcribing**. |
| Transcript text | The words, in paragraphs, which start after a long pause or, when speakers are labelled, when someone else speaks. You can review it with a screen reader. |
| Copy the transcript to the clipboard / Save the transcript as a text file… | Takes the transcript elsewhere. |

A zip file of WAV and MP3 files plays as a playlist: when one track ends, the
next one opens and plays. Each track is transcribed when it opens, as a single
file would be. The app only offers to download the models for transcribing
when the first track opens; if you say no, later tracks aren't transcribed
until you press **Transcribe the speech**.

To choose the order, put a text file called `listing.txt` in the zip, with
the name of one audio file on each line:

```
introduction.mp3
chapter 1.mp3
chapter 2.mp3
```

A name can include the folder the file is in inside the zip, such as
`part 2/chapter 3.mp3`. Capitals don't matter, and blank lines are skipped.
The app tells you if a line doesn't name a WAV or MP3 file in the zip. Files
that `listing.txt` leaves out play after the listed ones. Without a
`listing.txt`, the files play in order of name, with numbers counted properly,
so "track 2" comes before "track 10".

To decide whether a file is speech, the app looks at each second of sound.
Speech rises and falls with each syllable and has short gaps between words,
and it mixes vowels with hissy sounds such as "s" and "f"; music and most
other sounds don't. A file is transcribed by itself when most of its seconds
look like speech. Songs and music with talking over it may not be, so press
**Transcribe the speech** if you want to try.

The first time anything is transcribed, the app asks to download the Whisper
model from Hugging Face. It is downloaded once and kept on your computer.
Transcribing a few minutes of speech takes seconds on most computers; longer
files take longer, and the progress bar and F7 say how far it has got. Whisper
works out which language is spoken by itself. Only the first three hours of a
very long file can be transcribed, though all of it plays.

When **Label who is speaking** is on, each paragraph of the transcript starts
with "Speaker 1:", "Speaker 2:" and so on, numbered in the order people first
speak. The app can't know anyone's name. Two more small AI models work this
out on your computer, after the words have been written down: one finds where
the voice changes, and the other compares how each stretch of speech sounds,
so the stretches that sound alike can be put together as one person. The
first time, the app asks to download them (about 46 MB) from Hugging Face and
GitHub. If you say no, the file is transcribed without labels. Labelling works
best with clear recordings of a few people, such as interviews and meetings.
It finds it harder when people talk over each other, when voices sound alike,
or on phone-quality audio. Choosing the number of people helps.

### Settings

- **Language of the app**: English, or one of about 30 other languages,
  including Welsh, Irish, French, German, Spanish, Polish and Ukrainian. The
  app is translated on your computer by a local AI model in Ollama, the same
  way photos are described. Choose a language, then press **Translate the app
  into…**; it takes a few minutes, shows on the progress bar, and Escape stops
  it. Choose a **Translation model** if you'd rather not use the image
  description model. When you choose a language that's already been
  translated, the app switches straight away. The label of this list always
  says "(Language)" in English too, so you can find your way back.

  Translations by a small AI model can contain mistakes. Each one is saved as
  a JSON file in the `languages` folder next to your settings, where you can
  correct it, or share it with others. Text the translation leaves out is
  shown in English, and pressing the button again fills it in. A translation
  file for a language that isn't in the list, such as `eo.json` for
  Esperanto, is offered as well, using the `language` name inside it.
  Only use translation files from people you trust: a file can't run
  anything or reach your files, but it decides what every button and
  message says, so a dishonest one could label a button misleadingly.

  Languages that use other alphabets, such as Arabic, Hindi or Chinese, aren't
  offered yet, because the window can't draw them. Only the app's own text
  is translated: documents are read as they are, and the voice preview and
  part announcements stay in English.
- **Image description model**: choose which Ollama model describes photos.
  It needs a model that understands images, such as `llama3.2-vision`,
  `gemma3` or `llava`. If Ollama has none, a button downloads `gemma3:4b`
  (about 3.3 GB), and choosing a photo offers the same. Escape stops the
  download, and the next one carries on where it left off.
- **Photo location**: whether to read out where a geotagged photo was taken.
- **Speech recognition model (Whisper)**: the model the Audio Player tab
  transcribes with. **Whisper base** (about 150 MB) is quick and good with
  clear speech. **Whisper small** (about 490 MB) is slower, but better with
  accents, background noise and languages other than English. Each is
  downloaded the first time it's needed, and the list says which you already
  have.
- **Long texts**: text over 4,800 characters is read in parts. Choose whether
  each part starts with "This is part 1 of 3", or the parts run on with no
  announcement, with only the usual short pause between sentences.
- **Sounds**: whether to play a short sound when something succeeds (a file
  opens, reading aloud reaches the end, audio is saved, a transcript is done)
  or fails (whenever an error is shown). Sounds never play over speech or an
  audio file that is playing, and starting either cuts off a sound already
  playing. On by default.
- **Updates**: whether to check GitHub for a new version when the app starts,
  and a button to check now. When a new version is out, a dialog offers to
  open its download page.
- **Debug log folder**: where the app writes `speechout.log`. Each launch
  starts a fresh log; the one from the session before is kept as
  `speechout.previous.log`.
- **Cloud voice API keys**: one box per service. Boxes are left empty for
  security; type a new key only to add or replace one.

### Wordlists

Tick or untick each installed wordlist to turn it on or off, import new
wordlists, or remove ones you no longer need.

## Keyboard

| Keys | Action |
| --- | --- |
| Tab / Shift+Tab | Move between controls |
| Space or Enter | Press a button, tick a box or open a dropdown |
| Up / Down / Home / End | Change the choice in a focused dropdown without opening it |
| Ctrl+1, Ctrl+2, Ctrl+3, Ctrl+4 | Go to the General, Settings, Wordlists or Audio Player tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next or previous tab |
| Ctrl+O | Choose a file (an audio file on the Audio Player tab) |
| F5 | Read aloud (play the audio file on the Audio Player tab) |
| F6 | Pause or resume. While an audio file is playing, pauses or resumes it |
| F7 | Hear how far through reading or saving you are. While an audio file is playing, hear the time, such as "1 minute 5 seconds of 3 minutes" |
| Escape | Stop reading, cancel saving, or stop transcribing. While an audio file is playing or being opened, stops it |
| Ctrl+S | Save the spoken text as audio (save the transcript on the Audio Player tab) |
| Ctrl+Plus / Ctrl+Minus / Ctrl+0 | Make everything larger, smaller, or reset the size |

On a Mac, use Command instead of Ctrl, except for Ctrl+Tab.

A thick outline (blue in light mode, yellow in dark mode) shows which control
has focus. The app follows your system's light or dark setting.

## Requirements by platform

| | System voices | HEIC photos | API keys stored in |
| --- | --- | --- | --- |
| Windows 10/11 | Windows speech voices (the same ones Narrator uses) | Built in. If the built-in decoder can't open a photo, Windows' own codecs are tried, which need **HEIF Image Extensions** and **HEVC Video Extensions** from the Microsoft Store | `HKEY_CURRENT_USER\Software\SpeechOut` in the registry. If the registry can't be used, a text file in `%APPDATA%\SpeechOut` |
| macOS 11+ (Apple Silicon) | The voices in System Settings › Accessibility › Spoken Content | Built in | `~/Library/Application Support/SpeechOut/api-keys.txt` (readable only by you) |
| Ubuntu 22.04+ | eSpeak NG (installed with the .deb) | `libheif-examples` package | `~/.config/speechout/api-keys.txt` (readable only by you) |

API keys are never written to the debug log, and they are only sent to the
service they belong to. Anyone who can sign in to your user account can read
them, so don't save keys on a shared account.

## Privacy

- Documents are read on your computer. With a cloud voice, the text is sent to
  that service to be spoken. When you read aloud or save audio with a cloud
  voice, the status line says how many characters will be sent, so you can
  judge the cost. Reading aloud only sends text a little ahead of what you
  hear, so stopping early sends less.
- Photos are sent only to Ollama on your own computer (`127.0.0.1:11434`),
  after they have been shrunk and had their metadata removed.
- Audio files are played and transcribed on your computer, and are never sent
  anywhere. The first time you transcribe, and only if you agree, the Whisper
  model is downloaded from Hugging Face (`huggingface.co`). The first time you
  label who is speaking, and only if you agree, two speaker models are
  downloaded from Hugging Face and GitHub (`github.com`). Nothing about you or
  your files is sent with these requests.
- When you translate the app, its own labels and messages are sent to Ollama
  on your computer. Nothing is sent over the internet, and your documents
  aren't involved.
- With photo location turned on, the photo's GPS coordinates, rounded to about
  11 metres, are sent to OpenStreetMap's Nominatim service. The photo itself is
  not sent.
- Unless you turn it off in Settings, the app asks GitHub for the latest
  release version each time it starts. Nothing about you or your files is
  sent.
- The debug log records what the app did and any errors. It does not record the
  text it reads or your API keys.

## Cloud voices

If a cloud service is busy, limits how fast requests can be made, or can't be
reached, the app waits and tries again by itself, up to four tries in all. It
waits as long as the service asks (up to a minute), or otherwise 2, 4 and then
8 seconds. The status line says when it is waiting, and Escape stops it. The
app doesn't try again when the problem can't fix itself, such as a rejected API
key or an account that has run out of credit. A piece of text that is tried
again is sent again, so the number of characters sent can be a little higher
than the status line said.

Text is sent in pieces well under each service's limit: up to 2,500 characters
for ElevenLabs, 4,000 for OpenAI, and 1,900 for Deepgram and Speechify.

## Wordlists

Wordlists are UTF-8 XML files:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<wordlist name="UK place names" description="Respells UK place names" language="en-GB">
  <entry match="Leicester" replace="Lester"/>
  <entry match="SQL" replace="sequel" case-sensitive="true"/>
  <entry match="e.g." replace="for example" whole-word="false"/>
</wordlist>
```

| Attribute | Meaning |
| --- | --- |
| `name` (on `wordlist`) | Required. Shown on the Wordlists tab. |
| `description` (on `wordlist`) | Optional. May also be written as a `<description>` element. |
| `match` | Required. The text to find. `find` is also accepted. |
| `replace` | What to say instead. Leave it empty to skip the word. |
| `case-sensitive` | `true` to match the exact capitalisation only. Default `false`. `match-case` is also accepted. |
| `whole-word` | `false` to match inside longer words too. Default `true`. |

When several entries could match at the same place, the longest one wins. A
case-insensitive replacement written in lower case takes the capital letter of
the word it replaces, so "Damn" becomes "Darn".

The included examples are in [`wordlists/`](wordlists):

- `uk-place-names.xml`: UK place names that synthetic voices get wrong.
- `technology-terms.xml`: computing abbreviations and symbols.
- `classroom-safe.xml`: swaps swear words for mild alternatives.

They are copied into your wordlist folder the first time the app runs.

## Building

You need the current stable Rust toolchain.

```
cargo build --release
```

Speech recognition is built from [whisper.cpp](https://github.com/ggml-org/whisper.cpp),
so you also need [CMake](https://cmake.org) and a C++ compiler: Visual Studio
on Windows, the Xcode command line tools on macOS, or `g++` on Linux.

Labelling who is speaking uses [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx).
The first build downloads its prebuilt libraries from GitHub (about 100 MB)
into `target/`. They are linked into the app, except on Windows, where
`sherpa-onnx-c-api.dll`, `onnxruntime.dll` and `onnxruntime_providers_shared.dll`
are copied next to `speechout.exe` and must be kept with it.

On Ubuntu, install the build dependencies first:

```
sudo apt install pkg-config libasound2-dev libxkbcommon-dev libwayland-dev \
  libgl1-mesa-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev cmake g++
```

By default, whisper.cpp is tuned for the processor of the computer that builds
it, so the app may not transcribe on older computers. Set `GGML_NATIVE=OFF`
when building copies for other people, as the release workflow does. Built
like this, PCs need a processor with AVX2 to transcribe. Most PCs from the
last ten years have it, though some low-cost Pentium and Celeron ones don't.

Run the tests with `cargo test`. Tests that use your computer's real speech
synthesiser are skipped by default; run them with `cargo test -- --ignored`.

### HEIC photos on Windows

Release builds include libheif and libde265, so HEIC photos open without any
Microsoft Store add-ons. A plain `cargo build` leaves them out and uses
Windows' own codecs instead. To build them in, install libheif with
[vcpkg](https://vcpkg.io) and turn on the `bundled-heif` feature:

```
vcpkg install "libheif[core]:x64-windows-static-md"
set VCPKG_ROOT=<your vcpkg folder>
set VCPKGRS_TRIPLET=x64-windows-static-md
cargo build --release --features bundled-heif
```

### Packaging

The app packages itself, so no packaging scripts are needed:

```
speechout --package-macos dist   # macOS: ad hoc signed .app bundle, zipped
speechout --package-deb dist     # Linux: .deb package
```

The macOS bundle code lives in `src/platform/macos.rs` and the .deb builder in
`src/platform/linux.rs`, next to the rest of each platform's code. Ad hoc
signing lets the app run on Apple Silicon. Because the app is not notarised,
macOS blocks it the first time you open it. To allow it, go to System Settings ›
Privacy & Security and choose **Open Anyway**.

## Releasing

1. Add a section for the new version to [`CHANGELOG.md`](CHANGELOG.md) and
   bump `version` in `Cargo.toml`.
2. Push a tag such as `v0.2.0`.

The [release workflow](.github/workflows/release.yml) builds a Windows `.zip`
(the `.exe` with the Visual C++ runtime DLLs it needs), an Ubuntu `.deb` and an
Apple Silicon `.app` (zipped). It then publishes them
on a GitHub release, using the newest version section of the changelog as the
release notes. Before publishing, each download is submitted to VirusTotal,
and a table of scan links is added to the release notes. This needs a repository secret called
`VT_API_KEY` containing a VirusTotal API key.

## Project layout

```
src/main.rs           Command line and start-up
src/app.rs            The window, tabs and keyboard handling
src/worker.rs         Background jobs (speaking, saving, loading)
src/speech/           System and cloud voices
src/audio.rs          Decoding, playback, WAV and MP3 export, and the speech check
src/playlist.rs       Zip files of audio played as playlists
src/document.rs       PDF, TXT, DOCX, ODT, CSV, ODS, PPTX and PPT text extraction
src/vision.rs         Photo descriptions, GPS and place names
src/transcribe.rs     Speech recognition with Whisper, and downloading its models
src/speakers.rs       Working out who is speaking, with sherpa-onnx
src/wordlist.rs       Wordlist parsing and substitution
src/i18n.rs           The app's text in English, and translations of it
src/spoken.rs         Email addresses, dates and numbers put the way they are said
src/secrets.rs        API key storage
src/settings.rs       Saved settings
src/logging.rs        Debug log
src/platform/         Platform-specific code: windows.rs, macos.rs, linux.rs
wordlists/            Example wordlists (built into the app)
assets/fonts/         Google Sans fonts (built into the app) and their licence
assets/sounds/        Success and failure sounds (built into the app) and credits
```

## Licence

The Speech Output Engine is free software, released under the
[GNU General Public License, version 3 or later](LICENSE).

The interface uses Google Sans: Medium for body text and Bold for the tabs.
The fonts are built into the app and are licensed under the
[SIL Open Font License 1.1](assets/fonts/OFL.txt).

The success and failure sounds come from [Freesound](https://freesound.org)
and are released under Creative Commons 0; see
[assets/sounds/CREDITS.txt](assets/sounds/CREDITS.txt).

Speech recognition uses [whisper.cpp](https://github.com/ggml-org/whisper.cpp),
which is built into the app and is licensed under the MIT License. The Whisper
models it downloads are released by OpenAI under the MIT License.

Labelling who is speaking uses [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx),
licensed under the Apache License 2.0, and
[ONNX Runtime](https://github.com/microsoft/onnxruntime), licensed under the
MIT License. Both are built into the app (on Windows, as DLLs beside it). The
models it downloads are
[pyannote segmentation 3.0](https://huggingface.co/pyannote/segmentation-3.0)
(MIT License) and NVIDIA's
[TitaNet small](https://catalog.ngc.nvidia.com/orgs/nvidia/teams/nemo/models/titanet_small)
([CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)), as converted by
the sherpa-onnx project.

The Windows download includes [libheif](https://github.com/strukturag/libheif)
and [libde265](https://github.com/strukturag/libde265), which read HEIC photos.
Both are licensed under the
[GNU Lesser General Public License, version 3](https://www.gnu.org/licenses/lgpl-3.0.html).
HEVC, the format inside HEIC photos, is covered by patents in some countries.
