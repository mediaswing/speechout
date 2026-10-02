# Speech Output Engine

The Speech Output Engine reads documents aloud and describes photos. It is
written in Rust, and accessibility is its guiding principle: every feature
works from the keyboard, and the whole interface is exposed to screen readers
(Narrator, NVDA and JAWS on Windows, VoiceOver on macOS, Orca on Linux).

The program file is called `speechout`.

## What it does

- **Reads PDF, TXT and DOCX files aloud** with the voices built into your
  computer, or with cloud voices from **ElevenLabs**, **OpenAI**, **Deepgram
  Aura** or **Speechify** once you have saved an API key for that service.
- **Saves the spoken text as an audio file**, in MP3 or WAV format.
- **Describes photos** (JPEG and HEIC/HEIF) using a local AI model running in
  [Ollama](https://ollama.com). The photo never leaves your computer.
- **Says where a photo was taken**, if you turn this on and the photo has GPS
  information. The place name is looked up with OpenStreetMap.
- **Applies wordlists** that fix pronunciation (for example "Leicester" →
  "Lester") or keep reading classroom-safe. Example wordlists are included, and
  you can import your own.

## The window

The window has three tabs across the top: **General**, **Settings** and
**Wordlists**. Every button, box and dropdown sits on its own line and fills the
width of the window. At the bottom, a progress bar shows how far through
reading aloud or saving audio you are, and a status line below it reports what
is happening and any errors. Screen readers announce the status line
automatically.

### General

| Control | What it does |
| --- | --- |
| Current file | Shows the file you chose. |
| Choose a file… | Opens a PDF, TXT, DOCX, JPEG or HEIC file. Photos are described by the local AI model. |
| Speech service | System voices, plus any cloud service with a saved API key. |
| Voice | The voice to use. |
| Read aloud / Pause / Stop | Controls speech. |
| Audio file format | MP3 or WAV. |
| Save spoken text as audio… | Renders the whole text to a file. |
| Copy text to the clipboard | Copies the document's text, or the photo's description, so you can read or paste it elsewhere. |

### Settings

- **Image description model**: choose which Ollama model describes photos.
  It needs a model that understands images, such as `llama3.2-vision`,
  `gemma3` or `llava`. Install one with `ollama pull llama3.2-vision`.
- **Photo location**: whether to read out where a geotagged photo was taken.
- **Updates**: whether to check GitHub for a new version when the app starts,
  and a button to check now. When a new version is out, a dialog offers to
  open its download page.
- **Debug log folder**: where the app writes `speechout.log`.
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
| Ctrl+1, Ctrl+2, Ctrl+3 | Go to the General, Settings or Wordlists tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next or previous tab |
| Ctrl+O | Choose a file |
| F5 | Read aloud |
| F6 | Pause or resume |
| F7 | Hear how far through reading or saving you are |
| Escape | Stop reading, or cancel saving |
| Ctrl+S | Save the spoken text as audio |
| Ctrl+Plus / Ctrl+Minus / Ctrl+0 | Make everything larger, smaller, or reset the size |

On a Mac, use Command instead of Ctrl, except for Ctrl+Tab.

A thick outline (blue in light mode, yellow in dark mode) shows which control
has focus. The app follows your system's light or dark setting.

## Requirements by platform

| | System voices | HEIC photos | API keys stored in |
| --- | --- | --- | --- |
| Windows 10/11 | Windows speech voices (the same ones Narrator uses) | Install **HEIF Image Extensions** from the Microsoft Store | `HKEY_CURRENT_USER\Software\SpeechOut` in the registry. If the registry can't be used, a text file in `%APPDATA%\SpeechOut` |
| macOS 11+ (Apple Silicon) | The voices in System Settings › Accessibility › Spoken Content | Built in | `~/Library/Application Support/SpeechOut/api-keys.txt` (readable only by you) |
| Ubuntu 22.04+ | eSpeak NG (installed with the .deb) | `libheif-examples` package | `~/.config/speechout/api-keys.txt` (readable only by you) |

API keys are never written to the debug log, and they are only sent to the
service they belong to. Anyone who can sign in to your user account can read
them, so don't save keys on a shared account.

## Privacy

- Documents are read on your computer. With a cloud voice, the text is sent to
  that service to be spoken.
- Photos are sent only to Ollama on your own computer (`127.0.0.1:11434`),
  after they have been shrunk and had their metadata removed.
- With photo location turned on, the photo's GPS coordinates, rounded to about
  11 metres, are sent to OpenStreetMap's Nominatim service. The photo itself is
  not sent.
- Unless you turn it off in Settings, the app asks GitHub for the latest
  release version each time it starts. Nothing about you or your files is
  sent.
- The debug log records what the app did and any errors. It does not record the
  text it reads or your API keys.

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

On Ubuntu, install the build dependencies first:

```
sudo apt install pkg-config libasound2-dev libxkbcommon-dev libwayland-dev \
  libgl1-mesa-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev
```

Run the tests with `cargo test`. Tests that use your computer's real speech
synthesiser are skipped by default; run them with `cargo test -- --ignored`.

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

The [release workflow](.github/workflows/release.yml) builds a Windows `.exe`,
an Ubuntu `.deb` and an Apple Silicon `.app` (zipped). It then publishes them
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
src/audio.rs          Decoding, playback, WAV and MP3 export
src/document.rs       PDF, TXT and DOCX text extraction
src/vision.rs         Photo descriptions, GPS and place names
src/wordlist.rs       Wordlist parsing and substitution
src/secrets.rs        API key storage
src/settings.rs       Saved settings
src/logging.rs        Debug log
src/platform/         Platform-specific code: windows.rs, macos.rs, linux.rs
wordlists/            Example wordlists (built into the app)
assets/fonts/         Google Sans fonts (built into the app) and their licence
```

## Licence

The Speech Output Engine is free software, released under the
[GNU General Public License, version 3 or later](LICENSE).

The interface uses Google Sans: Medium for body text and Bold for the tabs.
The fonts are built into the app and are licensed under the
[SIL Open Font License 1.1](assets/fonts/OFL.txt).
