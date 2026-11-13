# Changelog

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
