# Changelog

All notable changes to the Speech Output Engine are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project uses [Semantic Versioning](https://semver.org/).

When a version tag is pushed, the release workflow copies the section for that
version onto the GitHub release page, so write each entry for people who use
the app.

## [2.0.0] - 2026-10-08

### Added

- OpenDocument files from LibreOffice and other office apps can now be read
  aloud. ODT documents are read like DOCX files, and ODS spreadsheets are read
  as a table, row by row, like CSV files. When a spreadsheet has more than one
  sheet, each sheet is read in turn, starting with its name.
- Transcripts on the Audio player tab can now say who is speaking, starting
  each paragraph with "Speaker 1:", "Speaker 2:" and so on. Choose this under
  "Label who is speaking", either letting the app work out how many people
  there are or choosing the number yourself, which is more accurate. Like
  Whisper, this runs on your computer, so the audio isn't sent anywhere. The
  first time, the app asks to download the two small AI models it needs
  (about 46 MB).
