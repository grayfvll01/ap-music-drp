# Changelog

Versions follow [Semantic Versioning](https://semver.org). Each version's section here becomes its release notes.

## 1.1.0 - 2026-09-29

- Spotify support, with the same options as Apple Music. Friends who click the song, artist or album open it on Spotify.
- New **Music apps** menu to choose between Apple Music and Spotify. If both are playing, the song already on your status stays.
- Skipping a few songs in a row no longer makes the status disappear for a while when their album art arrives.

## 1.0.5 - 2026-09-29

- Changing songs now replaces the status straight away instead of briefly clearing it. The album art follows a moment later.

## 1.0.4 - 2026-09-29

- The installer keeps a log in %TEMP% to help troubleshoot updates.

## 1.0.3 - 2026-09-29

- Singles no longer show the song name two or three times (their "album" is just the song's name).
- "Song — Artist": a featured-artist credit moves from the title to the second line, so the artist stays visible.
- A line that repeats another line is never shown.

## 1.0.2 - 2026-09-28

- Uses this project's own "Apple Music" Discord app. Existing settings switch over automatically.

## 1.0.1 - 2026-09-28

- New status text choice: "Listening to <song> — <artist>".

## 1.0.0 - 2026-09-27

First public release.

- Shows the song playing in Apple Music on your Discord profile: title, artist, album art, time bar and clickable links.
- Only Apple Music is ever shown. Personal station names and your country never appear.
- Every setting is in the tray menu.
- Installer with a Start menu entry, optional desktop icon and "Start with Windows".
- Automatic updates from GitHub releases.
