# Changelog

## [5.1.2] - 2026-11-13

- **A new version found when the app starts is shown in a dialog**, which
  offers to open the download page in your web browser. Checking the Settings
  tab by hand still answers in the status bar, beside the download link.
- **Saving long text in parts asks before replacing files.** If any of the
  numbered part files already exist, the app asks once before replacing them.
  When the text is saved in parts, an existing file with the name you chose is
  left as it was, and the status bar now says so.
- **A save that fails or is stopped leaves no files behind**, not even the
  parts that were finished.
- **Saving very long text uses much less memory.** The speech is turned into
  MP3 as it is made, rather than kept until the end.
- **Part files keep the extension as you typed it**, so `Talk.MP3` is saved
  as `Talk (part 1 of 2).MP3`.
- **The Windows installer remembers the folder you chose** and offers it again
  when you install a newer version, instead of moving the app back to Program
  Files. This starts with 5.1.2, so an upgrade from 5.1.1 still suggests Program
  Files once.

## [5.1.1] - 2026-11-13

- **The Windows installer shows its steps.** Opening the .msi now shows the
  standard installer dialogs, which work with screen readers: a welcome, the
  licence, where to install, and a last page that says installation finished,
  with a box to open the app. Before, it showed only a progress bar and closed.
  Installing with `msiexec /qn` is still silent.

## [5.1.0] - 2026-11-13

- **Long text is saved in parts.** Text over 4,500 characters is saved as
  numbered MP3 files, such as `speech (part 1 of 3).mp3`. A new setting under
  **Saving MP3 Files** on the Settings tab stitches the parts into one file.

## [5.0.0] - 2026-11-12

Common Speech Output (`speechout`) is an accessible text-to-speech reader for
Windows, macOS and Ubuntu. Accessibility is its guiding principle: everything can
be done from the keyboard, and every control works with a screen reader (NVDA,
JAWS and Narrator on Windows, VoiceOver on macOS, Orca on Linux).

- **Reads PDF, TXT, DOCX and CSV files aloud**, or text you type or paste, in a
  voice built into your computer or a cloud voice from ElevenLabs, OpenAI TTS,
  Deepgram Aura or Speechify. CSV tables are read as sentences, with dates and
  amounts of money said the way people say them.
- **Saves the speech as an MP3 file.**
- **Describes JPEG and HEIF/HEIC photos** with an AI model on your own computer,
  through Ollama or an OpenAI-compatible server such as LM Studio, and can say
  where a geotagged photo was taken.
- **Changes how words are spoken** with XML wordlists, to fix pronunciation or
  keep what is read aloud suitable for a classroom.
- **Plays MP3 and WAV files and transcribes the speech in them** on your own
  computer with whisper.cpp, naming each voice Speaker 1, Speaker 2 and so on.
- **Checks for new versions** when it starts, which can be turned off, and links
  to the download.
- **High Contrast appearance**, larger or smaller text, and a status bar that
  screen readers announce without moving the focus.

Your documents, photos and recordings stay on your computer unless you choose a
cloud voice. See the README for what leaves your computer and when.
