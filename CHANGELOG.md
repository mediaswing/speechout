# Changelog

All notable changes to the Speech Output Engine are listed here.

## [Unreleased]

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

## [2.8.0] - 2026-10-15

### Added

- PNG and JPEG XL (.jxl) images can be described, as JPEG and HEIC photos
  are. JPEG XL is read by a decoder built into the app, so nothing extra
  needs installing on Windows, macOS or Linux. Transparent parts of an image
  are put on a white background before it is described, and only the first
  frame of an animated image is described. With photo location turned on, the
  location is read from PNG and JPEG XL files too.
- "Read aloud" on the right-click menu in File Explorer now includes PNG and
  JPEG XL files.
- PNG images are always read with the system voices, in the window and from
  the right-click menu, because they are usually screenshots, which can show
  private details. While one is open, the speech service can't be changed;
  the service you chose comes back with the next file of another kind.

### Changed

- The Windows installer now adds "Read aloud" to the right-click menu in File
  Explorer, with a page to leave it out, and removes it when the app is
  uninstalled, so it no longer stays behind. An update keeps the choice; an
  update from an earlier version keeps the menu only if it was added on the
  Settings tab. To add or remove it later, choose Modify for SpeechOut in
  Installed apps, then Change. For installs without questions, CONTEXTMENU=0
  or CONTEXTMENU=1 on the msiexec command line leaves it out or adds it. The
  Settings tab now says whether the menu is there and how to change it,
  instead of adding and removing it. Without the installer, as when running
  a build from source, it can still remove a menu added by an earlier
  version.