# Dialog Presenter (WinUI tray)

The tray's overlay is a Galgame-style bottom dialog box. It is a *presenter*: it
renders what `GET /api/events` already said and never calls into runtime
internals. Everything below lives in `app/VoiceCoreTray/Dialog/`.

## What the runtime actually gives us

Two places where the brief is aspirational and the API is ground truth:

1. **There is no streamed text.** One `speech` event carries the whole utterance.
   The typewriter is therefore client-side pacing over a known string. The seam
   for a future chunked source is that `TextPresenter` paces by a *rate*
   (chars/second) over an append-only buffer, not by a precomputed schedule, so
   `Push()` may extend the buffer mid-flight without recomputing anything.
2. **No speed / volume / branch routes exist.** The control seam is a C# event
   (`IDialogPresenter.ControlRequested`) plus `RequestControl(...)`. The tray
   surfaces raised intents as a status note. No HTTP route is invented.
3. **Audio outlives nothing.** `src/spool.rs`: entries expire at `spool_ttl`
   (default 3600 s), are evicted oldest-first over `spool_max_bytes`
   (default 2048 MiB), and `Spool::open` deletes every `*.wav` on runtime start.
   So backlog text outlives backlog audio: a replay is ATTEMPTED and its failure is
   reported (`语音已释放`), rather than a probe promising something that may expire
   between the promise and the click.

## Module split

| Unit | Role | Knows about |
|---|---|---|
| `IDialogPresenter` | the only surface the tray talks to | `DialogUtterance`, nothing UI |
| `DialogPresenter` | controller: Hold state machine, dwell policy, backlog cursor, gesture routing | window + text + audio + history |
| `DialogWindow` | the box itself: layout, growth, acrylic, Win32 geometry | XAML + Win32 only |
| `TextPresenter` | rate-paced reveal over an append-only buffer, cancellable, instant-complete | a `Action<string>` sink |
| `AudioPresenter` | WAV playback off the UI thread, WAV-header duration, replay by `audioId` | `RuntimeClient` |
| `DialogHistory` | bounded backlog (50 entries), text + speaker per entry | nothing |
| `DialogTheme` | every colour, metric, font and duration | nothing |
| `HotkeyManager` | `RegisterHotKey` + `WM_HOTKEY` on the **tray** window | bindings from `config.json`, note sink |
| `AppConfig` | the whole app's preferences in one `config.json`: reveal preset, annotation side, hotkeys | `System.Text.Json` only |
| `WheelGestureWatcher` | `WH_MOUSE_LL`, wheel over the box → backlog step | screen point + delta |

`DialogTheme` is C# rather than a XAML `ResourceDictionary` so that the values the
geometry code needs (box width it sizes the window's client area to, corner radius
matched to DWM's, the acrylic recipe, typing rate) and the values XAML needs cannot
drift apart. `DialogWindow.xaml` carries structure only; `ApplyTheme()` pushes every
visual value once.

### Interface surface

```csharp
void Show(DialogUtterance u);      // one utterance: text, optional wav, speaker, dwell hint
void Hide();                       // fade out
void SetEnabled(bool enabled);     // tray toggle; false hides and drops utterances
bool Pinned { get; set; }          // 常驻: never auto-hide (tray menu mirrors it)
void ResetPosition();              // forget the dragged anchor
void ToggleVisibility();           // hotkey: hide, or recall the last utterance
void ToggleHold();                 // hotkey Ctrl+Alt+H: 常驻 <-> 倒计时, with a readout
void OpenHistory();                // 历史 control / tray menu: step in, or back out
bool ScrollGesture(int x, int y, int delta);   // wheel over the box; true = consumed
event EventHandler<DialogControlEventArgs>? ControlRequested;            // seam
void RequestControl(DialogControl c, double value, string? detail = null);
```

`SetCharacter` is gone: the speaker is a field on `DialogUtterance`, so it cannot
drift from the line on screen.

## Hold lifecycle

```
Hidden --Show--> Typing --text done--> Speaking --audio done--> settle
                    |                      |
                    +----- no audio -------+--> settle
settle: 常驻 (Pinned)                -> Holding   (blinking indicator, no timer)
        otherwise                    -> Counting(dwell)      <- the DEFAULT
dwell = --subtitle-dwell, else the event's displaySeconds, else 6 s
Counting --hover--> animation paused where it is (frozen bar colour)
Counting --bar reaches 0--> Fading --animation done--> Hidden
Typing --click--> instant-complete, stays Typing until the reveal drains
Holding|Counting|Speaking --click / ToggleVisibility / Hide / 关闭--> Fading
Counting|Holding --Ctrl+Alt+H--> the other one, and the band says which
```

A caption that outlives its usefulness is litter, so the default is now a
countdown and 常驻 is the opt-in. There is ONE mode flag (`Pinned`), reachable from
the tray menu and from `Ctrl+Alt+H`; the old second per-utterance hold override is
gone, because two ways to say "do not auto-hide" meant the menu could disagree with
the dialog.

The countdown itself is one `DoubleAnimation` over the whole dwell, run on the
plate's bottom edge (full width, clipped by the plate's corner radius, no track).
A timer-pushed fraction moved it in ~1 % steps and read as stutter; a storyboard is
interpolated per frame, and hover freeze is `Pause()`/`Resume()` on it, so the
remaining time lives in the animation instead of a deadline this code keeps
patching. The 100 ms tick that remains does nothing but poll the cursor while a
countdown runs (`GetCursorPos` against the box rect - no `TrackMouseEvent` state
machine, and no cost when nothing is counting).

Typing rate: with audio, the reveal is stretched to 85 % of the WAV duration
(parsed from the RIFF header, since `SpeechEvent` carries no `durationMs`), so
the line lands just before the voice stops; without audio, 45 ms/char clamped to
[0.35 s, 4.5 s] so a 400-character line never takes absurdly long. Every reveal
re-measures the box and the window follows the new height, so the plate opens as the
line arrives (see the growth note below).

## Reveal presets

`dialog.reveal` in `config.json` picks how the text arrives. All three see the SAME
layout: it is built from the full line before anything is shown, so the wrap points and
annotation slices are final in every preset - what differs is what is animated.

| Value | Behaviour | Layout movement |
|---|---|---|
| `typewriter` (default) | per character, paced against the audio as above; the box follows the growing text | box grows |
| `sweep` | box opens at final size, then each segment fades up at a time proportional to its x offset - a soft band of light crossing the line at one speed regardless of segment count (`SweepTravel` 420 ms, `SweepFeather` 200 ms) | none |
| `fade` | box opens at final size, then segments fade up in turn a clause-sized pause apart (`FadeStagger` 100 ms, `FadeInSegment` 220 ms) | none |

`sweep` and `fade` are one `Storyboard` of `DoubleAnimation`s on each segment's
`Opacity`, with `CubicEase`/`EaseOut`; the presenter's "text finished" moment is the
storyboard's `Completed`, which is what starts the dwell countdown. Granularity is the
segment, not the pixel: a true per-pixel feather needs a Composition mask over a
`CompositionVisualSurface`, which is a large amount of machinery for a difference that is
invisible at this size.

## The spoken line as an annotation

The utterance the human reads and the line that was actually spoken are ONE utterance,
not two subtitles. They are revealed in lockstep (one clock, one fraction) and the
spoken line is set small and dim, hugging the text it belongs to - above or below,
from `config.json`.

Where it hugs is the hard part, and it is decided by DATA, not by inference:

| Input | Layout | Claim being made |
|---|---|---|
| `rubyPairs` present | one cell per pair, `ruby` centred on its `base` | exactly the caller's mapping |
| absent, clause counts match | one cell per clause, centred | clause i corresponds to clause i |
| absent, counts differ | one run under (or over) the whole block | "this is what was said", nothing positional |

Whatever the source, an annotation is trimmed of leading and trailing punctuation, and a
fragment that was ONLY punctuation annotates nothing (its cell renders bare). The base
line already shows where a clause ends, so a lone 「、」 under a 「，」 carries no
information and at 11 dip mostly reads as dirt on the plate.

Chinese and Japanese do not line up positionally: SOV against SVO, and a translation
freely merges, splits or reorders clauses. Splitting the spoken line by character ratio
or forcing clause 3 under clause 3 therefore prints a correspondence that does not
exist, which is worse than printing none - the reader believes the typography. The
producer of both strings is the only party that knows the mapping, so the protocol
carries it (`rubyPairs`, see `docs/api.md`) and this layout renders it verbatim.

Four typographic rules keep the base text readable and the overhang civilised. Ruby is
allowed to hang over its base - that is normal - but 4 hanzi under 7 kana is enough to
reach the portrait, so the hang is bounded:

* **A cell is as wide as its BASE, never its annotation.** An over-long annotation hangs
  sideways instead of widening the cell, because a widened cell pushes the neighbouring
  segments apart and the line stops reading as one sentence.
* **The annotation is centred on its base and may overlap its neighbours, but never
  leaves the line.** The offset is clamped to the room actually available on each side,
  so the first segment's annotation sits flush with the line start instead of running
  onto the avatar, and the last one cannot pass the line end.
* **Past 1.5x its base's width the annotation is scaled down** (`RubyOverflowRatio`),
  floored at `SecondaryMinSize` - an unreadable annotation is worse than one that
  overhangs, so the shrink stops there and the clamp above takes over.
* **A segment with no annotation reserves no vertical space for one**, so a monolingual
  utterance is exactly as tall as its text.

All four are measured with hidden probe TextBlocks styled exactly like the real runs.
Those probes are `InvalidateMeasure`d before every measurement: a TextBlock whose
measure is not dirty returns its previous `DesiredSize`, and one stale width is enough
to lay out an utterance against another utterance's metrics.

## Backlog, in place

历史对话 is a cursor over `DialogHistory`, not a window: wheel-up over the box walks
back through past utterances IN the box, wheel-down walks forward, and passing the
newest entry returns to the live line. A modal would take focus, cover the thing it
describes, and need its own copy of the layout.

| While browsing | Behaviour |
|---|---|
| band readout | `1 / N` oldest-first with the timestamp - a transcript position, not a distance from now |
| 历史 control | hidden: in this state it is the one control whose meaning would change |
| 关闭 control | unchanged - always hides the dialog |
| body click | replays that line's audio, or says 语音已释放 when the spool has dropped it |
| countdown | suspended (browsing implies Hold), so reading cannot be interrupted |
| new utterance | wins immediately and returns to the live line |

The speaker travels WITH the line: `DialogUtterance` and `HistoryEntry` both carry
`Character` / `AvatarPath`, resolved once per voice pack from `GET /api/voices`. A
single "current character" on the dialog would show whoever spoke last while the box
displays someone else's line.

Dead ends report themselves in the band (`没有更早的对话`, `已到最早的对话`) and clear
after 1.6 s: a control that does nothing visible is indistinguishable from a broken
one. The same readout is where `Ctrl+Alt+H` reports 常驻 / 倒计时.

## Persistence

| File (in `RuntimeClient.DataDir`) | Content |
|---|---|
| `subtitle-pos.json` | box bottom-center anchor, screen physical pixels (unchanged format) |
| `config.json` | every preference of the app: `dialog.*`, `hotkeys.*`, and the `voicePacks` registry the RUNTIME reads. Written with the defaults and inline comments when absent, read back with comments and trailing commas allowed, opened by the tray's single 设置 entry. Supersedes `dialog.json` / `hotkeys.json` / `voicepacks.json`, whose values are migrated once and the files removed. |

One file, not one per feature: a setting nobody can find is a setting nobody uses.
`runtime.json` stays separate because the runtime owns it, and `subtitle-pos.json` because
it is state this window writes, not a preference anyone edits.

The tray is the only WRITER. The runtime reads `voicePacks` out of the same file
(`src/packs.rs`) and reloads it whenever the mtime changes, so a pack edit needs no
restart of anything - while `dialog` and `hotkeys` are read once at tray startup.
`AppConfig` therefore carries the packs section as a raw `JsonElement` and writes it back
verbatim: a typed mirror of the runtime's `VoicePack` here would be a second definition to
keep in step, and would silently drop any field it did not know.

Two hotkeys, deliberately: the backlog has a control on the dialog and a wheel
gesture, so a third binding would be a key to remember for something already one
gesture away.

A registration failure (hotkey owned by another app) is reported through the
tray's existing status-note path and in the tray tooltip, never swallowed.

## Window model (ADR-0009, supersedes v1 ADR-0008)

The window **is** the box. Every relayout measures the box unconstrained, then
`AppWindow.MoveAndResize` sizes the client area to it and places it in one call, so
the plate, its rounded corners and its shadow can all be system-drawn:

| Concern | How |
|---|---|
| frosted plate | `DesktopAcrylicController` with `IsInputActive` pinned true |
| rounded corners | `DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND` |
| shadow | DWM's own window shadow |
| click-through | nothing to click through: the window is only the box |
| drag | `WM_NCHITTEST` → `HTCAPTION` over the top band (OS move loop) |
| never focused | `WS_EX_NOACTIVATE`, and the acrylic keeps sampling anyway |

Two things are load-bearing and were both measured, not assumed:

* **The presenter keeps its border** (`SetBorderAndTitleBar(true, false)`). A
  frameless WinUI 3 window gets neither DWM rounding nor a DWM shadow — that, not
  WinUI 3 itself, is why the v1 canvas model could never produce either.
* **The acrylic is driven through a controller**, not `Window.SystemBackdrop`. The
  stock property ties sampling to input activation, and this window never activates,
  so it renders a flat fallback tint forever.

Carried over from v1 unchanged: bottom-center anchoring with work-area clamping,
DPI-change re-fit, and the two deliberate deviations below. Gone with the canvas:
`SetWindowRgn` clipping, blur-behind per-pixel alpha, the hand-drawn Composition shadow,
and `HTTRANSPARENT`.

**The resize seam.** A growing plate is a stream of `MoveAndResize` calls, and each one
exposes a strip of client area that XAML has not arranged into yet. Three things close
that gap, and all three are needed:

| Fix | What it stops |
|---|---|
| `WM_ERASEBKGND` fills the client rect with `AcrylicFallback` and returns 1 | the OS erasing the exposed strip with the window class brush - a WHITE edge along the growing side for one frame per resize |
| `DialogBox` is `Stretch` in BOTH directions | a `Top` aligned plate leaving that strip unpainted and unbordered once XAML does arrange |
| `RootGrid.UpdateLayout()` right after every `ApplyBox`, growth frames included | the arrange landing a frame late, which is what makes the strip visible at all |

The plate is acrylic, so there is nothing else painting the client area: whatever the OS
put in that strip is what the user sees until the next arrange.

* **Caption region is only the top strip.** Dragging must stay the OS move loop,
  but "click anywhere in the dialog to dismiss" cannot coexist with "the whole box
  is caption": a caption region hands its pointer input to the move loop, so XAML
  would never see the click. The top band is the caption (drag, double-click to
  reset position); the body is ordinary client area and raises `PointerPressed` /
  `RightTapped`.
* **The band's controls need two things, not one.** A `Passthrough` region over the
  icon cluster AND `HTCLIENT` from our own `WM_NCHITTEST` over the same rect - our
  handler runs first, and a caption answer there sends the press to the move loop, so
  the icons look like decoration. Each icon is also wrapped in a padded transparent
  `Border`: a `Path` is hit-testable only where its geometry is FILLED, and these
  glyphs are thin strokes, so the naked icon means hitting a 1 px diagonal.
* **Icons are one size because their cell is.** Each path sits in a fixed-size
  `Canvas` (children do not contribute to layout) and is scaled by `IconSize/16` from
  Fluent's own 16x16 viewBox. `Stretch="Uniform"` instead scales each glyph's tight
  bounds to the cell, which makes a glyph that fills its box small and one that does
  not large, with uneven gaps to match.
* **The box opens with the text, in both dimensions.** There is no ghost line
  pre-sizing it. The box hugs its content between `BoxMinWidth` and `BoxMaxWidth`, so
  every reveal re-measures and BOTH targets move: a short line is a narrow plate, and
  once the text reaches the max width the plate grows upward instead. A 16 ms timer
  closes 22 % of each remaining gap per frame - an exponential follow rather than a
  tween, because the target moves while the animation runs and a fixed-duration
  animation would have to restart, and visibly re-ease, on every typewriter tick.
  Width expands symmetrically and height upward, because placement is anchored on the
  box's bottom-center. A new utterance, a recalled line, a backtracked line and a DPI
  change all snap instead (`SyncBoxBounds(snap: true)`): there is no motion to show.

`RunSelfTestAsync` (`--subtitle-selftest <path>`) renders 9 cases and asserts
`fits_client` (the client area the OS gave us is the size XAML laid out) and
`onscreen` per case, plus `rubies` (cells that actually carry an annotation, so a
punctuation-only one is visibly dropped).

## Cost, measured

Everything on this surface runs on the tray's UI thread, and that thread also owns the
`WH_MOUSE_LL` hook. So a stall here is not a slow dialog, it is a stuttering cursor
across the whole desktop — which is a symptom that says nothing about its cause. Hence
`DialogMetrics`: one JSON line per utterance in `logs/dialog.jsonl`, alongside the
runtime's own `metrics.jsonl`.

| Field | Meaning |
|---|---|
| `reveals` | typewriter ticks that changed the text |
| `growFrames` | growth-timer frames that ran |
| `measures` / `measureMs` | `Measure` calls and their total cost |
| `resizes` / `resizeMs` | `MoveAndResize` calls and their total cost |
| `regionUpdates` | caption/passthrough re-punches |
| `replayFetches` / `replayCacheHits` / `replaysSkipped` | backlog audio: HTTP, cache, single-flight drops |
| `maxTickGapMs` | worst overrun of a 24 ms frame — the UI-thread stall, measured |

What those numbers bought (43-character spoken line, 125 % scale):

| | resizes | resizeMs | regionUpdates | maxTickGapMs |
|---|---|---|---|---|
| measure + resize on every reveal | 115 | 623 | 17 | 16.3 |
| growth owns the measuring; regions on settle | 44 | 361 | 3 | 10.6 |
| plus 24 ms frames, 4 px quantum, 3 px snap | 39 | 199 | 2 | 14.9 |

One `MoveAndResize` of this window costs 4-8 ms (SetWindowPos plus a DWM recomposition
of an acrylic-backed window), which is the whole reason those knobs exist: a reveal
fires per character, and doing a measure, a forced layout pass, a resize and two
non-client region calls on each one saturates the thread the mouse hook lives on.

Replay had the same shape of bug from the other side: each click meant an HTTP GET of a
multi-megabyte WAV plus a new `SoundPlayer`, and `SoundPlayer.Stop` (winmm) ran inline
on the UI thread. Rapid clicks in the backlog therefore hitched the cursor. Now replay
is single-flight (extra clicks report `重播中` and are dropped), the last three clips
are cached, and both stopping and playing happen on a worker.

## Deferred from the brief, and why

* **Streamed/chunked text input** — no such event exists; the rate-paced buffer
  is the seam, and building a chunk protocol against nothing would be fiction.
* **Speed / volume / branch UI** — no backend and no producer; only the seam.
* **Expressions and a name registry** — the utterance carries a name and an avatar
  path from its voice pack; there is no expression asset pipeline and no
  `character.json`, and `SetCharacter`'s expression argument stays unused.
* **Volume control inside `AudioPresenter`** — `SoundPlayer` has no volume knob;
  a mixer would mean a new dependency for a feature nothing can request yet.
* **Backlog search / export / per-entry copy** — not asked for; the backlog is the
  last 50 lines, walked in place, with replay.
