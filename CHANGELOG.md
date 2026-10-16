# Changelog

All notable changes to the Speech Output Engine are listed here.

## [Unreleased]

### Added

- The app has its own icon: two speech bubbles and a pencil, by Laura Reen
  (Xomo: basics set). It is shown on the window and taskbar, the Windows
  executable, Start menu shortcut and Installed apps list, the Linux
  application menu, and the macOS app bundle. It replaces the generic
  speaker icon on Linux.

## [3.1.0] - 2026-10-15

### Added

- A Describe Video tab. ffmpeg takes the keyframes out of a video, and each
  is described by the local AI model as a photo is, starting with when it
  comes in the video. The picture being described is shown under the file
  picker, with the progress bar under it. The description can be read aloud,
  saved as audio or copied. If ffmpeg isn't installed, the app offers to
  install it with winget, Homebrew, APT or DNF, or to open its download page.

## [3.0.0] - 2026-10-15

### Changed

- The Apple Silicon download is now an installer package (.pkg) that puts the
  app in the Applications folder, instead of a zip file of the app.