# Speech Output Engine

The Speech Output Engine reads documents aloud and describes photos. It is
written in Rust, and accessibility is its guiding principle: every feature
works from the keyboard, and the whole interface is exposed to screen readers
(Narrator, NVDA and JAWS on Windows, VoiceOver on macOS, Orca on Linux).

The program file is called `speechout`.

## What it does

- **Reads PDF, TXT, Markdown, DOCX, ODT, CSV, ODS, PPTX, PPT and EPUB files aloud** with the
  voices built into your computer, or with cloud voices from **ElevenLabs**, **OpenAI**, **Deepgram
  Aura** or **Speechify** once you have saved an API key for that service.
- **Saves the spoken text as an audio file**, in MP3 or WAV format.
- **Describes photos and other images** (JPEG, PNG, JPEG XL and HEIC/HEIF)
  using a local AI model running in [Ollama](https://ollama.com). The image
  never leaves your computer. If
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
  information (in JPEG, PNG, JPEG XL and HEIC files). The place name is
  looked up with OpenStreetMap.
- **Applies wordlists** that fix pronunciation (for example "Leicester" →
  "Lester") or keep reading classroom-safe. Example wordlists are included, and
  you can import your own.

## Installing on Windows

Each [release](https://github.com/mediaswing/speechout/releases) has a
Windows Installer (.msi) for each kind of PC: for example
`speechout-2.5.0-windows-x86_64.msi` for most PCs, and
`speechout-2.5.0-windows-aarch64.msi` for PCs with an Arm processor, such as
Snapdragon laptops (Settings › System › About shows "ARM-based processor").
It installs SpeechOut, creates a Start menu shortcut and supports normal uninstallation.
It installs for your account only, in `%LOCALAPPDATA%\Programs\SpeechOut`, so
it doesn't need administrator rights. The installer is the standard Windows
one and works from the keyboard and with screen readers: press Enter for
**Next**, **Next** again and **Install**, then **Finish**. The second page
asks whether to add **Read aloud** to the right-click menu in File Explorer
(described under [Settings](#settings)); it is ticked to start with, and
Space unticks it. To update, install the new version's `.msi`; it replaces the
old one and keeps your right-click menu choice. To add or remove the menu
later, go to Settings › Apps › Installed apps (or Apps & features on
Windows 10), find Speech Output Engine, choose **Modify** and then **Change**. To
uninstall, choose **Uninstall** there instead; that removes the menu too.

To install without any questions, for example on many computers, run
`msiexec /i <the .msi file> /qn`. Add `CONTEXTMENU=0` to leave out the
right-click menu, or `CONTEXTMENU=1` to add it. Without either, a new
installation adds it and an update keeps the choice made before.

The app doesn't keep anything in its own folder. Settings and
wordlists are in `%APPDATA%\SpeechOut`, downloaded models and (unless you
chose another folder) the debug log in `%LOCALAPPDATA%\SpeechOut`, and saved API keys in the registry under
`HKEY_CURRENT_USER\Software\SpeechOut`. Updating or uninstalling leaves all of
these alone. To remove them as well, delete those two folders and that
registry key.

The Windows installer isn't signed yet, so Microsoft Defender SmartScreen may
warn that it doesn't recognise the app when you first open it. If you downloaded it from this project's releases page,
choose **More info** and then **Run anyway**. The release notes normally list
each download's SHA-256 hash and VirusTotal scan, which you can use to check
your copy.

## Installing on macOS

Each release has an installer package (.pkg) for Macs with Apple Silicon
(M1 or later) running macOS 11 or later, for example
`speechout-2.5.0-macos-aarch64.pkg`. It puts **Speech Output Engine** in your
Applications folder, so it asks for an administrator password. To update,
install the new version's `.pkg`; it replaces the old app. To uninstall, drag
the app from Applications to the Bin.

The package and the app aren't notarised by Apple yet, so macOS blocks the
package the first time you open it. If you downloaded it from this project's
releases page, go to System Settings › Privacy & Security and choose
**Open Anyway**, then open the package again. The app itself may need the same
the first time it starts.

## The window

The window has five tabs across the top: **General**, **Settings**,
**Wordlists**, **Audio Player** and **Describe Video**. Every button, box and dropdown sits on its own line and fills the
width of the window, except the two buttons at the bottom of the Describe
Video tab, which share a line. At the bottom, a progress bar shows how far through
reading aloud or saving audio you are (on the Describe Video tab it is under
the preview instead), and a status line below it reports what
is happening and any errors. Screen readers announce the status line
automatically.

### General

| Control | What it does |
| --- | --- |
| Text to read | "Choose File" reads a file you choose. "Paste Text" shows a box to type or paste text into instead. |
| Current file | Shows the file you chose. Shown with "Choose File". |
| Choose a file… | Opens a PDF, TXT, Markdown (.md), DOCX, ODT, CSV, ODS, PPTX, PPT, EPUB, JPEG, PNG, JPEG XL (.jxl) or HEIC file. Photos and other images are described by the local AI model. Transparent parts of an image are shown to the model on a white background, and only the first frame of an animated PNG or JPEG XL file is described. CSV and ODS files are read as a table, row by row, and each sheet of an ODS file is read in turn. PowerPoint presentations are read slide by slide, starting each with its number; speaker notes are left out. In PPTX files, SmartArt is read box by box, and charts are read as their title followed by each series with its values, such as "North. Q1: 10. Q2: 20." EPUB books start with their title and authors, such as "The City of the Blind, by Murray Leinster.", then are read chapter by chapter in reading order. A picture's description (its alt text) is read after the paragraph it's in, such as "Image: A man with a lantern."; pictures with no description, or marked as decoration, are passed over. Footnotes, the numbers that refer to them and printed page numbers are left out. Books with copy protection (DRM) can't be read. Shown with "Choose File". |
| Word, OpenDocument, Markdown and EPUB files (DOCX, ODT, MD and EPUB) | **Content** reads just the text, without Markdown's symbols. **Format** reads the text and says how it's formatted: headings ("Heading level 1."), list items ("List item." or "List item, level 2."), tables ("Table.", "Row 1." and "End of table."), and where bold, italic, underlined, crossed-out, coloured and linked text starts and ends, such as "bold, very, end bold" or "colour red, warning, end colour". A heading's own bold or colour isn't said, and neither is a link's underline and colour. Markdown files also say "Quote." before a quotation, "Code." before a block of code, and "Done." or "Not done." for a task list item; raw HTML, front matter and footnotes are left out. EPUB books say "Quote." and "Code." too; their text colours aren't said. Colours are given as the nearest of a few plain names, such as red, dark blue or grey, and black text is not mentioned. These words are in English, like the part announcements, and appear in the text, so wordlists apply to them too. Changing this opens a Word, OpenDocument, Markdown or EPUB file that's already open again. Content is the default. Shown with "Choose File". |
| Text | The text to read aloud, up to 200,000 characters. Shown with "Paste Text". Invisible characters that could hide or reorder words are removed before it is read. |
| Speech service | System voices, plus any cloud service with a saved API key. A line underneath says whether speech is made on your computer or your text is sent to the service. A PNG image is always read with the system voices, and this can't be changed while one is open, because PNG images are usually screenshots, which can show private details. The service you chose comes back when you open another kind of file or paste text. |
| Voice | The voice to use. The app remembers the voice you chose for each service. |
| Speaking speed | Only for ElevenLabs (0.7 to 1.2 times normal speed) and Deepgram Aura (0.7 to 1.5 times). Remembered for each service. |
| Preview voice | Speaks "This is a preview of the selected voice." with the chosen voice and speed. |
| Read aloud / Pause / Stop | Controls speech. |
| Audio file format | MP3 or WAV. |
| Save spoken text as audio… | Renders the whole text to a file. |

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
| Show the transcript | Shows the transcript text under it, once a file has been transcribed. It becomes **Hide the transcript**. The transcript is hidden again when another file or track opens. |
| Transcript text | Only once you press **Show the transcript**. The words, in paragraphs, which start after a long pause or, when speakers are labelled, when someone else speaks. You can review it with a screen reader. |
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

### Describe Video

| Control | What it does |
| --- | --- |
| Current file | Shows the video you chose. |
| Choose a video… | Opens an MP4, M4V, MOV, MKV, WebM, AVI, WMV, MPEG or 3GP video and describes it. |
| Preview | The picture being described. Screen readers read which picture it is and when it comes in the video. |
| Progress bar | How far through describing the video the app is. |
| Edit the description / Finish editing | Edit the description shows the video and the description side by side (see below). Finish editing puts the tab back as it was, keeping your changes. |
| Read aloud / Stop | Reads the description aloud: each picture's description, starting with when it comes, such as "At 1 minute 5 seconds:". While the video is being described, Stop stops describing. |
| Save spoken text as audio… / Copy the description to the clipboard | Side by side on one line. Takes the description elsewhere. |

The app uses ffmpeg to take the keyframes out of the video. These are the
whole pictures a video is built from; the frames between them only record
what changed. Pictures closer together than 5 seconds are skipped, and at
most 40 are described, spread across the whole video, so a long video has
them further apart. Each picture is then described by the local AI model,
just as a photo is. This can take a minute a picture on a laptop. The sound
isn't described or transcribed.

When the video has been described, the app asks whether you'd like to edit
the description. If you choose **No**, the tab stays as it is. If you choose
**Yes** (or press **Edit the description** later), the video moves to the
left with its controls under it, and the description goes on the right in a
box you can type in, which has the keyboard focus. **Play the video**,
**Pause**, **Stop the video**, **Back 10 seconds** and **Forward 10 seconds**
work on the video, sound and all, and F6, F7 and Escape work on it as they do
on an audio file. Read aloud, Save and Copy use the description as you've
edited it. Reading aloud pauses the video, so you can hear the description
and then carry on from the same place.

If ffmpeg isn't installed, the app offers to install it with winget on
Windows, Homebrew on a Mac, or APT or DNF on Linux (you will be asked for your
password), or to open the ffmpeg download page. If Ollama isn't installed or
running, or has no model that understands pictures, the app offers to set it
up, as it does for photos.

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
- **Photo camera**: whether to read out the make and model of the camera
  that took a photo, such as "Apple iPhone 15 Pro", when the photo records it.
  This is read from the photo itself, so nothing is sent anywhere.
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
- **Right-click menu** (Windows): says whether "Read aloud with Speech
  Output Engine" is on the menu you get when you right-click a PDF, text
  file, Word or OpenDocument file, spreadsheet, PowerPoint file or image
  (JPEG, PNG, JPEG XL or HEIC) in File Explorer. The installer adds it unless
  you untick it, and removes it when you uninstall. To add or remove it later,
  choose **Modify** for Speech Output Engine in Installed apps, then **Change**. The
  menu item is in the app's interface language once the app has been started.
  Without the installer, as when running a build from source, there is no
  menu to add, but one added by an earlier version of the app can be removed
  here.
  It reads the file in the background, without opening the window, using the
  voice, speed, wordlists and other choices made in the app (PNG images, as
  in the window, always use the system voices). Choosing it for
  another file stops the first; choosing it again for the same file stops
  reading. With a cloud voice, a message box first says how many characters
  will be sent, and asks whether to go ahead. Errors, and files with no text
  to read, are shown in a message box too. Nothing needs administrator rights.
  On Windows 11 the item is under **Show more options**.

### Wordlists

Tick or untick each installed wordlist to turn it on or off, import new
wordlists, or remove ones you no longer need.

## Keyboard

These keys always work, whatever the shortcut settings:

| Keys | Action |
| --- | --- |
| Tab / Shift+Tab | Move between controls. Tab also moves out of the text box |
| Space or Enter | Press a button, tick a box or open a dropdown |
| Up / Down / Home / End | Change the choice in a focused dropdown without opening it |
| Alt+Down / Alt+Up (Option on a Mac) | Open or close a focused dropdown's list |
| Left / Right / Home / End | Move between tabs while a tab has focus |
| Escape | Close an open list. Escape never takes focus away from the control you are on |
| Ctrl+Plus / Ctrl+Minus / Ctrl+0 | Make everything larger, smaller, or reset the size |

These are the shortcuts to start with. Each one can be changed or turned off
under "Keyboard shortcuts" on the Settings tab, and all of them can be turned
off at once. Press F1 to see the ones in use.

If a screen reader is running when the app starts (NVDA, JAWS, Narrator or
another that tells Windows it is on, VoiceOver on a Mac, or Orca on Linux),
the start-up message says where to change or turn off the shortcuts, until
you choose whether to use them.

| Windows and Linux | Mac | Action |
| --- | --- | --- |
| Ctrl+O | Cmd+O | Choose a file (an audio file on the Audio Player tab, a video on the Describe Video tab) |
| Ctrl+S | Cmd+S | Save the spoken text as audio (save the transcript on the Audio Player tab, the video's description on the Describe Video tab) |
| Ctrl+1 to Ctrl+5 | Cmd+1 to Cmd+5 | Go to the General, Settings, Wordlists, Audio Player or Describe Video tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Ctrl+Tab / Ctrl+Shift+Tab | Next or previous tab |
| F5 | F5 or Cmd+R | Read aloud (play the audio file on the Audio Player tab, read the video's description on the Describe Video tab) |
| F6 | F6 | Pause or resume. While an audio file or video is playing, pauses or resumes it |
| F7 | F7 | Hear how far through reading or saving you are. While an audio file or video is playing, hear the time, such as "1 minute 5 seconds of 3 minutes" |
| Escape | Escape or Cmd+. | Stop reading, cancel saving, stop a download, stop transcribing or stop describing a video. While an audio file is playing or being opened, or a video is playing, stops it |
| F1 | F1 | List the keyboard shortcuts |

On most Mac keyboards, F1 to F12 control the computer unless you hold Fn, so
Cmd+R and Cmd+. work too, and Pause or resume can be given a shortcut such as
Cmd+J.

The shortcuts are chosen so they don't get in the way of screen readers or
typing:

- None of them is a single letter, digit or symbol. Screen readers use those
  keys to move around (NVDA and JAWS browse mode, Narrator scan mode,
  VoiceOver Quick Nav).
- None uses Insert, Caps Lock, Alt or Option. Screen readers use these keys
  for their own commands, Ctrl+Alt is AltGr on many keyboards, and Option
  types characters on a Mac.
- Shortcuts only work when the exact keys are held, so typing a character
  such as ś with AltGr+S never saves.
- When you change a shortcut, the lists only offer keys that pass these
  checks and that nothing else in the app uses.
- Every command also has a button.

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
  that service to be spoken. Descriptions of PNG images, which are usually
  screenshots, are never sent: they are always read with the system voices. When you read aloud or save audio with a cloud
  voice, the status line says how many characters will be sent, so you can
  judge the cost. Reading aloud only sends text a little ahead of what you
  hear, so stopping early sends less.
- Photos are sent only to Ollama on your own computer (`127.0.0.1:11434`),
  after they have been shrunk, converted to JPEG and had their metadata
  removed.
- Videos are only opened by ffmpeg on your own computer. Their pictures are
  sent only to Ollama on your computer, as photos are, and are deleted when
  the video has been described.
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
speechout --package-macos dist   # macOS: installer package (.pkg) of the ad hoc signed .app
speechout --package-deb dist     # Linux: .deb package
```

The macOS package code lives in `src/platform/macos.rs` and the .deb builder in
`src/platform/linux.rs`, next to the rest of each platform's code.

On Windows, the release workflow gathers `speechout.exe` and its DLLs into a
folder, then builds an MSI installer from them with
[WiX Toolset](https://wixtoolset.org) 6.0.2. The installer is described by
[`src/installer/speechout.wxs`](src/installer/speechout.wxs). To build it yourself,
install the [.NET SDK](https://dotnet.microsoft.com/download), put
`speechout.exe` and its DLLs in a folder, and run:

```
dotnet tool install --global wix --version 6.0.2
wix extension add -g WixToolset.UI.wixext/6.0.2
wix build src\installer\speechout.wxs -arch x64 -culture en-US -ext WixToolset.UI.wixext ^
  -d Version=2.5.0 -d StageDir=<folder> ^
  -d "RuntimeDlls=vcruntime140.dll;vcruntime140_1.dll;msvcp140.dll" ^
  -o speechout-2.5.0-windows-x86_64.msi
```

`RuntimeDlls` lists the Visual C++ runtime DLLs in the folder. For an Arm
PC, build `speechout.exe` on Arm and use `-arch arm64`; whisper.cpp must be
compiled with clang-cl there, as the release workflow shows. The
installer's `UpgradeCode` must never change, or new versions won't replace
old ones. The installer also adds the right-click menu, for the kinds of file
listed in its `ContextMenuExtensions`, which must match `EXTENSIONS` in
`src/platform/context_menu.rs`; a test checks this on Windows. Ad hoc
signing lets the app run on Apple Silicon. On macOS, the `.pkg` is built with
`pkgbuild` and `productbuild`, which come with macOS; the app bundle is also
left in `dist/root` for trying it without installing.

## Releasing

1. Add a section for the new version to [`CHANGELOG.md`](CHANGELOG.md) and
   bump `version` in `Cargo.toml`.
2. Push a tag such as `v0.2.0`. The Windows installer takes its version from
   the tag, so the tag must be three numbers with no suffix such as `-beta`
   (the first two at most 255, the third at most 65535). The workflow stops
   with an error if it isn't.

The [release workflow](.github/workflows/release.yml) builds Windows `.msi`
installers for x64 and Arm (with the `.exe` and the DLLs it needs), an Ubuntu `.deb` and an Apple Silicon `.pkg` installer. It tests
installing, upgrading and uninstalling each `.msi` on a Windows runner of its own kind and
installing the `.pkg` on the macOS runner, then
publishes the downloads
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
src/document.rs       PDF, TXT, Markdown, DOCX, ODT, CSV, ODS, PPTX, PPT and EPUB text extraction
src/vision.rs         Photo descriptions, GPS and place names
src/video.rs          Taking keyframes out of a video with ffmpeg
src/transcribe.rs     Speech recognition with Whisper, and downloading its models
src/speakers.rs       Working out who is speaking, with sherpa-onnx
src/wordlist.rs       Wordlist parsing and substitution
src/i18n.rs           The app's text in English, and translations of it
src/spoken.rs         Email addresses, dates and numbers put the way they are said
src/secrets.rs        API key storage
src/settings.rs       Saved settings
src/logging.rs        Debug log
src/platform/         Platform-specific code: windows.rs, macos.rs, linux.rs
src/installer/        The Windows Installer package, built with WiX Toolset
wordlists/            Example wordlists (built into the app)
assets/fonts/         Google Sans fonts (built into the app) and their licence
assets/sounds/        Success and failure sounds (built into the app) and credits
assets/icon/          The app icon (SVG source and rendered PNG, ICO, ICNS) and credits
assets/tests/         Test data used by the unit tests
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

The app icon is "chat bubbles, speech, conversation, writing" by Laura Reen,
from her Xomo: basics set; see [assets/icon/CREDITS.txt](assets/icon/CREDITS.txt).

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

JPEG XL images are read with [jxl-rs](https://github.com/libjxl/jxl-rs), from
the JPEG XL project, which is built into the app on every platform and is
licensed under the BSD 3-Clause License.

The Windows downloads include [libheif](https://github.com/strukturag/libheif)
and [libde265](https://github.com/strukturag/libde265), which read HEIC photos.
Both are licensed under the
[GNU Lesser General Public License, version 3](https://www.gnu.org/licenses/lgpl-3.0.html).
HEVC, the format inside HEIC photos, is covered by patents in some countries.
