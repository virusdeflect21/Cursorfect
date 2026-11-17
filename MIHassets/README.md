# Made in Heaven sounds

This is the existing MIH asset directory. Cursorfect loads the files here during
development and from `MIHassets` beside the portable executable. The build script
copies these files without renaming or modifying them.

| Provided filename | Event |
| --- | --- |
| `made-in-heaven-summon-start.mp3` | Activation, once per cycle |
| `time-accelerate.mp3` | Entering Time Acceleration (35%) |
| `universe-reset.mp3` | Universe Reset (100%) |

There are no supplied maximum-acceleration or deactivation files. Those events
are silent. Optional MIH-specific filenames containing `maximum`, `max-accel`,
`deactivation`, or `power-down` are recognized if you add your own recordings.

You can also put files in `%APPDATA%\CursorFect\sounds\mih` or `sounds\mih`
beside the exe; these take priority. The UI lists the files actually loaded and
provides sound-test buttons. MP3 and WAV are supported.

Audio remains external and is ignored by Git, consistent with the existing
project's copyright policy. Do not publish copyrighted recordings you do not
have permission to distribute.
