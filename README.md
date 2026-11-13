# Common Speech Output

Common Speech Output reads documents aloud and describes photos. It is written
in Rust, and accessibility is its guiding principle: everything can be done from
the keyboard, and every control can be used with a screen reader (NVDA, JAWS and
Narrator on Windows, VoiceOver on macOS, Orca on Linux).

The program is called `speechout`: that is what to type to start it from a
terminal, and the name of the file that is installed.

## What it does

- **Reads PDF, TXT, DOCX and CSV files aloud**, in a voice built into your computer or
  a cloud voice from ElevenLabs, OpenAI TTS, Deepgram Aura or Speechify.
- **Saves the speech as an MP3 file.**
- **Describes JPEG and HEIF/HEIC photos** using an AI model on your own computer,
  through Ollama or an OpenAI-compatible server such as LM Studio.
- **Says where a geotagged photo was taken**, if you turn this on.
- **Changes how words are spoken** with XML wordlists, to fix pronunciation or
  keep what is read aloud suitable for a classroom.
- **Plays MP3 files and transcribes speech in them**, on your own computer with
  [whisper.cpp](https://github.com/ggml-org/whisper.cpp), naming each voice
  Speaker 1, Speaker 2 and so on.

## Installing

Downloads are on the [releases page](https://github.com/mediaswing/speechout/releases).

| System | Download |
| --- | --- |
| Windows on x64 PCs | `speechout-<version>-windows-x86_64.msi` |
| Windows on Arm PCs | `speechout-<version>-windows-aarch64.msi` |
| macOS on Apple Silicon | `speechout-<version>-macos-aarch64.pkg` |
| Ubuntu 22.04 or later | `speechout-<version>-ubuntu-amd64.deb` |

The downloads aren't signed with a paid certificate, so your computer may warn
you. On Windows, choose **More info**, then **Run anyway**, then follow the
installer's steps; its last page has a box to open the app. On a Mac the app has an
ad hoc signature: if macOS refuses the package, open **System Settings**, then
**Privacy & Security**, and choose **Open Anyway**.

On Ubuntu, install with `sudo apt install ./speechout-<version>-ubuntu-amd64.deb`,
which also installs the eSpeak NG voices. HEIC photos need `libheif-examples`,
which is recommended and normally installed with it.

## Using it

The window has four tabs down the left side, with the selected tab's controls to
the right. Each control has a line to itself, apart from the four buttons that
control reading, which share one, and the status bar at the bottom
says what is happening. Screen readers announce its messages without the focus
moving.

### General

**What to read** chooses between **Read a File** and **Free Text**. With
**Read a File**, type the path of a file and press Enter, or use **Choose a file**.
With **Free Text**, type or paste the text into the box that appears across the
pane. Choosing a file, with Ctrl+O or by dropping one on the window, goes back to
**Read a File** and keeps what you typed. Then choose a
**Speech Service** and a **Voice**, and use **Read Aloud**, **Pause**, **Stop** or
**Save as MP3**. Cloud services appear once you've saved a key for them.

Text over 4,500 characters is saved as several MP3 files, named like
`speech (part 1 of 3).mp3`. To get one file instead, choose **Stitch the parts
together into one file** under **Saving MP3 Files** on the Settings tab.

A CSV file is read as sentences rather than cell by cell. The reading starts with
how many rows there are and what the columns are called, then each row is one
sentence that names its columns, such as `Transaction ID TXN-0002: date 3 January
2026, description Rent, amount minus 950 pounds and balance 5,150 pounds.` Dates
written as 2026-01-03 are read as dates, and amounts with a currency symbol or a
currency column as pounds and pence, dollars or euros. Something every row shares,
such as the currency, is said once at the start, and empty cells are left out.
Files that use semicolons or tabs between values, as some spreadsheets save them,
work too.

When you choose a photo, the local AI system describes it and the description goes
in the text box, ready to read aloud or save. If Ollama is installed but not running,
the app starts it as soon as you choose a photo and says so in the status bar, and
Ollama keeps running after the app closes. If the chosen model
gives no answer, the app tries your other models that understand images.

### Settings

- **API keys** for the cloud services: choose a **Speech Service**, then type its
  key. Services that already have a key say so in the list. A **Sign up for**
  link under the box opens that service's sign-up page in your web browser.
- **Appearance**: Standard, or High Contrast with white and yellow on black.
- **Local AI system**, its **address**, and the **image description model**.
- **Say where a geotagged photo was taken**.
- **Folder for debug logs**.

- **Transcribing Audio**: where whisper.cpp is, if it isn't found by itself.
- **Updates**: whether to check for a new version when the app starts, and a
  button to check now. When there is one, the status bar says so and this section
  has a link to download it. The app never downloads or installs anything itself.

### Wordlist

Import wordlists, turn each on or off with its checkbox, and remove those you no
longer want. Three examples are installed the first time the app runs:

| Wordlist | Starts | What it does |
| --- | --- | --- |
| UK place names | On | Pronounces names such as Leicester and Bicester |
| Technology terms | On | Pronounces terms such as SQL and JSON |
| Classroom safe | Off | Replaces swear words and slurs with mild alternatives |

The text you see isn't changed, only what is spoken and saved. No word list can be
complete, so check anything important before playing it to a class.

A wordlist is an XML file, up to 5 MB and 50,000 entries, saved as UTF-8:

```xml
<wordlist name="My wordlist" description="What it is for">
  <entry match="Leicester" replace="Lester"/>
  <entry match="SQL" replace="sequel" case-sensitive="true"/>
  <entry match="C#" replace="C sharp"/>
  <entry match="Dr." replace="Doctor" whole-word="false"/>
</wordlist>
```

`match` is plain text, not a pattern. Entries match whole words and ignore case
unless you say otherwise. The longest match wins.

### Audio Player

Choose an MP3 (or WAV) file, then use **Play**, **Pause** and **Stop**. While the file
opens, the app listens to its waveform to tell whether it is mostly speech. If it is,
it is transcribed, and the transcript appears in the **Transcript** box, with a new
paragraph each time the speaker changes, such as `Speaker 2 (1:05): Thanks for
having me.` Music and other sounds aren't transcribed, but **Transcribe** tries
anyway. **Save the transcript** saves it as a text file. Only the first two hours of
a recording are transcribed.

Transcribing uses whisper.cpp's `whisper-cli` program and its base English model.
**Nothing is installed or downloaded without asking you first**:

- If whisper.cpp isn't installed and you have Homebrew on a Mac, the app asks before
  running `brew install whisper-cpp`. On Ubuntu and Windows there is no installer to
  offer, so get whisper.cpp from [its releases](https://github.com/ggml-org/whisper.cpp/releases)
  or build it, then choose `whisper-cli` on the Settings tab.
- The model, about 148 MB, is downloaded from Hugging Face the first time it is
  needed, after you agree. It is checked against its SHA-256 and kept beside the
  settings database, in a `whisper` folder.

If transcription can't be done, the status bar tells you why, with a certain amount
of attitude.

Speakers are told apart by the pitch and tone of their voices, worked out in the app.
This works well when voices differ clearly, but two similar voices can be taken for
one, and a change of speaker in the middle of a sentence can be missed.

## Keyboard

Press F1 at any time to see this list in the app. "Ctrl" is Cmd (⌘) on a Mac, where
F1 and F2 may need you to hold Fn as well.

| Keys | Action |
| --- | --- |
| Tab, Shift+Tab | Move between controls |
| Space, Enter | Press the control with the focus |
| Ctrl+1, Ctrl+2, Ctrl+3, Ctrl+4 | Go to General, Settings, Wordlist or Audio Player |
| Ctrl+O | Choose a file; on the Audio Player tab, an audio file |
| Ctrl+R | Read aloud; on the Audio Player tab, play |
| Ctrl+P | Pause or resume |
| Escape | Stop reading, saving, opening, playing or transcribing; close a dropdown list or the keyboard shortcuts |
| Ctrl+S | Save as MP3; on the Settings tab, save the API key; on the Audio Player tab, save the transcript |
| F1 | Show or hide the keyboard shortcuts |
| F2 | Say the status message again |
| Up, Down, Home, End, or a letter | Change the choice in a dropdown |
| Alt+Down (Option+Down on a Mac) | Open a dropdown's list; Alt+Up closes it |
| Ctrl+Plus, Ctrl+Minus, Ctrl+0 | Make everything larger or smaller, or reset it |

## Where your data goes

Everything the app remembers, including API keys, is in one SQLite database:

| System | Location |
| --- | --- |
| Windows | `%APPDATA%\CommonSpeech\settings.sqlite3` |
| macOS | `~/Library/Application Support/CommonSpeech/settings.sqlite3` |
| Linux | `~/.config/CommonSpeech/settings.sqlite3` |

**API keys in it aren't encrypted**, so that the database can be moved and read by
other tools. Anyone who can read the file can use them. See [SECURITY.md](SECURITY.md).

| What you do | What leaves your computer |
| --- | --- |
| Read a document with system voices | Nothing |
| Read a document with a cloud voice | The text (with wordlists applied), to that service |
| Describe a photo | Nothing: the photo only goes to the AI system on this computer |
| Play or transcribe an audio file | Nothing: whisper.cpp runs on this computer |
| Transcribe for the first time, and agree to the download | A request to Hugging Face for the speech recognition model |
| Turn on location lookup and describe a geotagged photo | The photo's coordinates, rounded to about 100 metres, to [OpenStreetMap Nominatim](https://nominatim.org/) |
| Start the app, unless update checks are turned off, or check for updates | A request to GitHub for the latest version number |

The app doesn't send any statistics. Debug logs are in a
`logs` folder beside the database, unless you choose another, and never contain
API keys or the text being read.

## Building

You need [Rust](https://rustup.rs/). On Ubuntu you also need:

```
sudo apt install build-essential pkg-config libasound2-dev libxkbcommon-dev libwayland-dev \
  libgl1-mesa-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev espeak-ng
```

```
cargo build --release
cargo test
```

Some tests need something real, so they only run when asked for:

```
cargo test real_read_aloud -- --ignored                # plays through your speakers
E2E_IMAGE=photo.jpg cargo test real_description -- --ignored --nocapture   # needs Ollama and the internet
E2E_AUDIO=talk.mp3 cargo test real_transcription -- --ignored --nocapture   # needs whisper.cpp and its model
E2E_CSV=table.csv cargo test real_csv -- --ignored --nocapture              # prints how a CSV file is read
```

On Windows, HEIC photos use Windows' own codecs unless you build with
`--features bundled-heif`, which needs libheif from vcpkg, as
[the release workflow](.github/workflows/release.yml) shows.

### Layout

| Path | What it is |
| --- | --- |
| `src/ui/` | The window, with one file per tab, and its accessibility tests |
| `src/platform/` | Code that differs by system, one file per system |
| `src/package/` | Release packaging, one file per system |
| `src/speech/` | System and cloud voices |
| `src/document.rs` | Text from PDF, TXT, DOCX and CSV |
| `src/table.rs` | CSV tables as sentences to read aloud |
| `src/vision.rs` | Image descriptions and GPS |
| `src/transcribe/` | Telling speech from music, whisper.cpp, and telling speakers apart |
| `src/wordlist.rs`, `wordlists/` | Wordlists and the examples |
| `src/store.rs` | The SQLite database |

### Making a release

Set the version in `Cargo.toml`, add a section to `CHANGELOG.md`, and push a tag
such as `v0.1.0`. [The release workflow](.github/workflows/release.yml) builds the
.msi files, the .pkg and the .deb, and publishes them. The macOS package is built
and ad hoc signed by `speechout --package-macos`, with no shell code.

## Licence

[MIT](LICENSE).
