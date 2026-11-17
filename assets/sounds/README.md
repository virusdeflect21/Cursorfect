# Sounds

CursorFect ships **without** sound files. Every sound people associate with these effects is
someone else's copyrighted audio, so you add your own. Put the files here (next to the exe:
`sounds\`) or in `%APPDATA%\CursorFect\sounds\`. The app's **Open sounds folder** button opens
that one. MP3, WAV, OGG and FLAC all work.

Missing files never stop the app: the Sandevistan falls back to built-in synthesized sounds, and
The World runs silently, freezing time instantly instead of waiting for a voice line.

## Sandevistan

| File | Used for |
|---|---|
| `sandevistan.mp3` | One full "start to finish" sound. It is cut into **start**, a **movement loop**, **dash** cracks and the **end** power-down. |
| `activation.mp3`, `move.mp3`, `dash.mp3`, `deactivation.mp3` | Optional: replace one part with your own file. |
| `cuts.json` | Optional: exact cut points in seconds, e.g. `{"start":[0.08,3.27],"move":[5.45,9.35],"dash":[[3.27,4.25],[9.37,10.05]],"end":[13.72,15.84]}` |

Without `cuts.json`, the cuts are proportional to the file's length (tuned on a ~16 s Sandevistan
sound re-design).

## The World (`sounds\theworld\`)

The cut points (where the voice line ends and the SFX starts) were measured on particular
recordings. Other recordings of the same lines may need slightly different timings; edit
`src/audio/dubs.rs` and rebuild.

| File | Content |
|---|---|
| `dio_stop.mp3` | DIO: "ZA WARUDO! Toki yo tomare!", then the time-stop SFX (4.5 s) |
| `dio_resume.mp3` | DIO: "Toki wa ugoki dasu", then the resume SFX (4.5 s) |
| `sfx_only.mp3` | Time-stop SFX, ~10 s of clock ticking, then the resume SFX, no voices (20.5 s) |
| `jotaro_jp.mp3` | Jotaro (Japanese) "Star Platinum: The World", SFX, ticks, resume (12 s) |
| `jotaro_en.mp3` | Jotaro (English dub) "Star Platinum: The World!", then the SFX (3.7 s) |
| `jotaro_resume.mp3` | Time-resume SFX without voice (1.8 s) |
| `diego.mp3` | Diego Brando "THE WORLD", ticks, and his resume line (13 s) |
| `over_heaven.mp3` | DIO, The World Over Heaven "ZA WARUDO", then the SFX (5.4 s) |
