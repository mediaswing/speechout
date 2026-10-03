# Changelog

All notable changes to the Speech Output Engine are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project uses [Semantic Versioning](https://semver.org/).

When a version tag is pushed, the release workflow copies the section for that
version onto the GitHub release page, so write each entry for people who use
the app.

## [Unreleased]

### Added

- A new **Audio player** tab plays WAV and MP3 files. Play, Pause, Stop, and
  Back and Forward 10 seconds work from the keyboard, F7 says how far through
  you are, and a waveform shows the sound.
- When an audio file is mostly speech, the app writes down what is said, using
  Whisper, an AI model that runs on your computer, so the audio isn't sent
  anywhere. The app tells speech from music by itself, and you can transcribe
  any file with the "Transcribe the speech" button. The transcript can be
  copied or saved as a text file. The first time, the app asks to download the
  Whisper model (about 150 MB). A larger, more accurate model can be chosen on
  the Settings tab.

## [1.8.0] - 2026-10-03

### Added

- The app can now be used in about 30 languages besides English, including
  Welsh, Irish, French, German, Spanish, Polish and Ukrainian. Choose one under
  Language on the Settings tab, then press "Translate the app". A local AI
  model in Ollama translates it on your computer in a few minutes, so nothing
  is sent over the internet. Translations are saved as files you can correct,
  and anything not translated is shown in English.

### Changed

- The debug log now starts fresh each time the app opens, so it only covers
  the current session. The log from the session before is kept as
  `speechout.previous.log`; attach that one if a problem happened before you
  restarted the app.

## [1.6.0] - 2026-10-02

### Changed

- On Windows, HEIC photos (the format iPhones use) now open without
  installing anything from the Microsoft Store. The app has its own HEIC
  decoder built in, and only uses Windows' codecs if that can't open a photo.
- The Windows download is now a zip file. Unzip it and keep the files
  together: `speechout.exe` sits next to the Microsoft Visual C++ runtime files
  it needs, so it runs even if they aren't installed on the PC.
- Errors, such as a file that can't be opened or a cloud voice that fails,
  now appear in a dialog as well as in the status line, so they can't be
  missed.
- The text in the progress bar is now centred.

### Fixed

- On smaller or zoomed screens, the window could open taller than the space
  above the taskbar, hiding the progress bar and status line. It now opens at
  a size that fits.
- On Windows, if a HEIC photo can't be opened because HEVC Video Extensions is
  missing, the app now says so instead of showing a Windows error code.

## [1.5.0] - 2026-10-02

### Added

- CSV files can be opened and are read as a table. The app says how many
  columns and rows there are, then reads each row with its column names, such
  as "Row 1. Name: Ann. Town: Leeds." Empty cells are skipped. Files separated
  by semicolons or tabs work too.
- If you choose a photo and Ollama isn't installed, the app offers to install
  it for you with winget on Windows, Homebrew on macOS or Snap on Linux. If
  there is no package manager, it offers to open the Ollama download page. If
  Ollama is installed but not running, the app offers to start it. The photo
  is then described without choosing it again.
- If Ollama has no AI model that can describe photos, the app offers to
  download one (gemma3:4b, about 3.3 GB) when you choose a photo, then
  describes the photo. There is also a button for this on the Settings tab.
  The download shows on the progress bar, is announced every quarter, and
  Escape stops it. The next download carries on from where it stopped.
- Text longer than 4,800 characters is read in parts. A new "Long texts"
  setting chooses whether each part starts with "This is part 1 of 3", or the
  parts run on with no announcement, with only the usual short pause between
  sentences. The parts run on unless you change it.
- When you save a photo's description as audio, it ends by saying that the
  description was made on your computer and whether it was voiced by a local
  voice or a cloud voice.

### Changed

- Email addresses, web addresses, IP addresses, dates and long numbers are now
  read the way a person would say them. For example, "rachel@example.com" is
  read as "rachel at example dot com", "2026-10-01" as "1 October 2026", and
  10536166 as "ten million, five hundred and thirty-six thousand...". Four-digit
  numbers such as years, and codes and phone numbers, are left as they are. The
  text you see and copy is not changed.

### Fixed

- Choosing a photo now checks for Ollama again, so once you install or start
  it, the photo is described without restarting the app or visiting the
  Settings tab. If Ollama still can't be found, the app says what is missing.

## [1.3.0] - 2026-10-02

### Added

- A "Speaking speed" setting for ElevenLabs and Deepgram Aura voices. The app
  remembers a speed for each service.
- A "Preview voice" button speaks a short sentence with the chosen voice and
  speed.
- Under the speech service, a line says whether speech is made on your
  computer or your text is sent to the service.
- When you read aloud or save audio with a cloud voice, the status line says
  how many characters will be sent to the service.

### Changed

- If a cloud service is busy, limits how fast requests can be made, or can't
  be reached, the app now waits and tries again by itself instead of stopping.
  The status line says when it is waiting, and Escape stops it.
- A cloud account that has run out of credit is now reported as that, instead
  of as too many requests.

## [1.2.0] - 2026-10-02

### Added

- A progress bar at the bottom of the window shows how far through reading
  aloud or saving audio you are. Screen readers can read its percentage.
- Press F7 to hear how far through reading or saving you are.
- A "Copy text to the clipboard" button copies the document's text or the
  photo's description, so you can read or paste it elsewhere.
- The app checks for new versions when it starts and offers to open the
  download page. You can turn this off, or check now, on the Settings tab.

### Changed

- The text preview box has been replaced by the progress bar.
- Screen readers now repeat a status message even when it is the same as the
  previous one.
