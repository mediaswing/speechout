# Changelog

All notable changes to the Speech Output Engine are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project uses [Semantic Versioning](https://semver.org/).

When a version tag is pushed, the release workflow copies the section for that
version onto the GitHub release page, so write each entry for people who use
the app.

## [Unreleased]

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

## [1.0.0] - 2026-10-02
