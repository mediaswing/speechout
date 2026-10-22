# Changelog

## [3.4.0] - 2026-10-22

### Added

- EPUB books can now be read aloud. A book starts with its title and authors, such as "The City of the Blind, by Murray Leinster.", then each chapter is read in turn, in the order the book is meant to be read, and the Content and Format settings for Word, OpenDocument and Markdown files apply to EPUB books too, so Format says headings, lists, tables, quotations, bold, italic and links. Pictures are read from their descriptions (alt text), such as "Image: A man with a lantern.", after the paragraph they're in; pictures with no description, or marked as decoration, are passed over. Footnotes, the numbers that refer to them and printed page numbers are left out. Books with copy protection (DRM) can't be read, and say so.

## [3.3.0] - 2026-10-21

### Added

- Speech saved as an MP3 file now has ID3 tags, so music players and File Explorer show more than the file name: the title is the saved file's name, the artist is the voice that read it, and the album is the document it was read from, so recordings of the same document are grouped together. Names in any language, such as "Café" or "日本語", are kept as they are.

### Fixed

- Saved MP3 files now say exactly how long they are, so players show the right length, can play them one after another without gaps, and no longer start each file with a blank frame.
- Text files saved on Windows are no longer read with a pause at the end of every line. Lines wrapped part-way through a sentence are joined up again, and only blank lines mark a new paragraph.
- Text files saved by older Windows programs no longer lose their curly quotes, dashes, euro signs and ellipses ("don’t" was read as "dont").

## [3.2.0] - 2026-10-21

First release from the new repository. Speech Output Engine reads documents aloud and describes photos. Written in Rust, it puts accessibility first: every feature is keyboard-accessible. 
