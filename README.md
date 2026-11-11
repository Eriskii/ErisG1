# ErisG1

Custom software for Even Realities G1 glasses, run from a Mac. It talks to both arms over Bluetooth LE
directly, without Even's app, and shows text and full-screen images on them, with Even's firmware or with g1v0
(below). The packet formats live in `src/proto.rs` and `src/v5.rs` with no Bluetooth in them, so an Android app can
share them later.

## Build

```sh
cargo build --release
./target/release/erisg1 --help
```

The first run asks for Bluetooth access for your terminal.

## First connection

The glasses ignore any computer they aren't bound to. To bind this Mac, put them in the charging case with
the lid open and run `erisg1 scan` then `erisg1 connect`. The right arm reports binding success, and after
that they connect while worn. A phone that's connected to them hides them from the Mac, so close the Even
app or turn the phone's Bluetooth off.

## Commands

| | |
|---|---|
| `scan` | List the arms in range |
| `connect [WHAT] [--down WHAT \| --only-up]` | Show text, a 576×136 1-bit `.bmp`, or `@extents` (a test pattern of the display's edges), optionally a different thing with your head down, and print what the glasses report |
| `hud` | Clock top left, battery top right, frame counter bottom right, as an image redrawn about every 1.4 s. `--scale` sets the text size, `--rows` redraws only that many rows from the top, and `--text` sends it as text instead, about 13 times a second, laid out by the glasses |
| `brightness [0-42] [--auto]` | Read or set the brightness. Auto brightness doesn't run while an image is up |
| `head-up [0-60]` | Read or set how far you tilt your head before the glasses count it as up |
| `display [HEIGHT] [DEPTH]` | Read or set where the picture sits: height 0–8, and depth, how far away it appears |
| `probe` | Ask both arms everything they answer without changing anything, then show their debug log |
| `exit` | Close whatever is on screen |
| `reboot` | Restart both arms |
| `firmware-log on\|off` | The glasses' own debug log; with `ERISG1_DEBUG=1`, commands print it as it arrives |
| `inspect FILE` | What's in a firmware update `.zip` or image `.bin`, and whether the glasses' bootloader would take it |
| `images [--arm left\|right]` | Which firmware each arm has installed, and what's waiting in its spare slot. Changes nothing |
| `flash FILE --arm left\|right [--install]` | Upload an update's app core image to one arm's spare slot and check it; with `--install`, install it. See below |

## What the glasses need

Learnt on firmware 1.6.6, with a [reconstruction of its source](https://github.com/JohnRThomas/Even-G1-Firmware)
to explain it:

- **macOS puts the arms in firmware-update mode.** An arm switches to it when the ATT MTU comes out at exactly
  498, which is what macOS negotiates. That mode turns off the link between the arms, the IMU, touch and the
  mic, and stops images redrawing. `erisg1` sends the debug command `#s0` after connecting to put them back.
- **Images:** each arm takes the file in 194-byte packets, then an end marker, then a CRC. An arm redraws when
  its CRC lands, so both CRCs go out together or the eyes drift apart. The CRC waits 100 ms after the end
  marker for the last packets to be stored, and the next image waits 400 ms for the redraw, since its first
  packet resets the screen. About 0.9 s of each frame is Bluetooth moving 2 × 9.8 KB, which is the limit.
- **A killed upload** leaves an arm expecting a later packet and rejecting the next image. `erisg1` resets the
  upload state on every connection.
- **Text** stays in the glasses' memory and redraws within a tick, so it's far faster than images, but the
  glasses lay it out in an area left of center and drop spaces at the start of a line.
- **Display height** moves images and text as well as the glasses' own screens, in 35-pixel steps.
- **Settings are saved** only once the arms' storage has been left alone for a while: five seconds in the
  source, about ten in practice. A restart, or images, sooner than that loses the change, so `display` and
  `brightness` wait 12 s after setting.
- **Auto brightness** is switched off by the firmware while an image is up, and turning it back on then makes
  the left arm reject images. Neither arm's light sensor can be read meanwhile.
- **No raw IMU stream** is known for the G1; it reports head up and head down past an angle you set.
- Gadgetbridge found the glasses drop the connection after about 30 s of silence, so `erisg1` sends a status
  read every 8 s.

## g1v0 firmware

The commands above speak Even's firmware. `erisg1 v0 …` speaks [g1v0](https://github.com/mafik/scouter_kit), an
alternative firmware for the arms (in `firmware/scouter_kit`, a submodule): the whole 640 × 480 panel in 16 greys,
redrawn a region at a time, plus the accelerometer, gyroscope, TouchBar and light sensor. Its protocol is in
`firmware/scouter_kit/g1_firmware/docs/VM.md`; `src/v5.rs` holds it without Bluetooth, `src/v5_link.rs` the link.

| | |
|---|---|
| `v0 scan` | List the arms running g1v0 |
| `v0 hello` | Each arm's firmware, state, battery and sensors |
| `v0 test` | A test pattern, then the panel read back to check every pixel arrived |
| `v0 text TEXT [--scale N]` | Text in an 8×8 font drawn N times over |
| `v0 hud [--scale N]` | Clock, both batteries, head pitch and roll and a frame counter, the same in both eyes. Each frame reaches both arms ahead of time with a moment to show it in each arm's own clock, so the eyes change together; about 20 frames a second on the 15 ms link it asks the Mac for |
| `v0 imu [--hz N] [--gyro]` | Head pitch and roll, the accelerometer and the gyroscope |
| `v0 events` | TouchBar gestures, wear, the case and the battery as they happen |
| `v0 sensors [--boost N]` | Every sensor reading of both arms, the left arm's in a column on the left and the right's on the right, and a level line, the same in both eyes. Brightness follows the right arm's light sensor, N levels up |
| `v0 flight [--boost N]` | A mock flight HUD with your head as the controls: tilt up to climb, bank or turn to turn. The arms draw it themselves with a routine built by `src/v5_asm.rs`, from about 110 bytes of state a frame; about 14 frames a second |
| `v0 cube [--boost N]` | The time with seconds at the top left, the battery (both arms averaged) at the top right, and a wireframe cube spinning in the middle in 3D: each eye gets its own view of it, about 1 m ahead, nearer edges brighter. Runs on the glasses alone, 10 frames a second, until `v0 stop`: a program on each arm keeps the time, reads the battery, follows the light and draws the cube's 48-second loop, and the arms keep in step over their arm-to-arm link. With `--from-mac`, the Mac drives it instead (one view for both eyes, 25–29 frames a second, until Ctrl-C) |
| `v0 pinned [--boost N] [--rotation-only]` | The 3D cube pinned in the room: the arms' gyroscopes and accelerometers follow the head, so it stays put as you turn (a while: the heading drifts). A tap on a TouchBar brings it back ahead. Following where the head moves, not just its turn, adds little |
| `v0 follow [--lag S] [--radius PX] [--boost N]` | A cross that trails your view lazily: turn your head and it holds still in the room up to 30 px off the middle, then glides back over about 0.35 s, through a 1€ filter so small wobbles don't shake it. Text under it moves with it; text above is fixed to the view, to compare |
| `v0 extents` | A frame around the whole 640×480 panel with ticks every 16 pixels, the coordinates every 64 and a cross in the middle, until `v0 stop` |
| `v0 depth` | Boxed rows at seven depths, by how far apart the eyes' copies are (0 to 133 pixels), until `v0 stop` |
| `v0 stop` | Stop any program running on the arms and switch the panel off |
| `v0 panel off\|on\|standby`, `v0 brightness [0-42] [--auto]`, `v0 reboot [--clear]` | |

`--arm left|right` picks one arm; without it, every g1v0 arm in range.

### Apps

`erisg1 v0 apps` lists them, `erisg1 v0 app NAME [ARGS]` runs one on the glasses, and `erisg1 v0 preview NAME`
(or `all`) runs it on two simulated arms instead, saving what each eye shows as `previews/NAME.png` (an animated PNG,
both eyes side by side) and `previews/NAME.sheet.png` (eight moments of it). The simulator is scouter_kit's own VM
core (MIT), compiled in by `build.rs` with `src/apps/sim.c`, on a virtual clock, with a script for the head, the
TouchBars and the sensors and the arm-to-arm link between the arms. Gestures come from either arm's TouchBar.

| App | Runs | What it does |
|---|---|---|
| `notify` | from the Mac | pop-up notifications from anything: `erisg1 v0 say [TITLE:] TEXT` while it runs; tap dismisses, double tap shows the last again |
| `run -- CMD ARGS` | from the Mac | runs a command and watches it: its last lines, the time, then done or failed; tap switches to the summary. `run -- tail -f LOG` follows a log |
| `sysmon` | from the Mac | the Mac's CPU (with a sparkline), memory, network, disk, battery and busiest process, every 2 s |
| `nowplaying` | from the Mac | the track in Music or Spotify with its progress; tap play/pause, double tap next, triple tap previous |
| `weather [--lat L --lon L]` | from the Mac | now, today and the next 12 hours from open-meteo (the place from ipinfo.io if not given) |
| `power` | from the Mac | each arm's charge, voltage, current, temperature, time left, and a graph of the session |
| `stopwatch` | on the glasses | laps: tap start/stop, double tap lap, hold reset |
| `pomodoro [--work 25] [--rest 5]` | on the glasses | focus and break countdowns with a progress bar; tap pause, hold reset |
| `posture [--minutes 3] [--every 20]` | on the glasses | dark until you've looked down too long ("chin up") or it's time for a 20-20-20 eye break |
| `prompter FILE [--wpm 140]` | from the Mac | a teleprompter scrolling smoothly; tap pause, double tap faster, triple tap slower, hold back to the start |
| `cards FILE` | from the Mac | flashcards, "front \| back" a line; tap turns over, double tap knew it, triple tap again |
| `note TEXT...` | on the glasses | notes kept on the glasses (a door code, a wifi password): dark until a tap shows the next |
| `steps` | on the glasses | pedometer: steps, steps a minute, distance; tap hides, hold resets |
| `metronome [--bpm 100] [--beats 4]` | on the glasses | a beat flashing at exact times; tap tempo, double tap beats a bar, hold stops |
| `breathe [--pattern 4-4-4-4]` | on the glasses | a square that grows and shrinks with your breathing; tap pause, hold switches pattern |
| `warp` | from the Mac | fly through stars in 3D, steering with your head |
| `dodge` | from the Mac | turn your head to dodge 3D rocks flying at you |
| `bricks` | from the Mac | knock out a wall of bricks, the paddle moved by tilting your head |
| `sketch` | from the Mac | draw with your head; the drawing is saved as a PNG when you stop |
| `fireworks` | from the Mac | fireworks bursting in 3D at different distances; tap for another |

An app driven from the Mac (`src/apps/kit.rs`, `Glasses::frame`) draws each eye's frame there and has both show it at
the same moment, until Ctrl-C. One that runs on the glasses is a program built with `src/apps/vmkit.rs`, uploaded
with `Glasses::deploy`: the Mac leaves once it starts and `v0 stop` ends it. TouchBar gestures on either arm reach
the other over the arm-to-arm link, and both act on them at the same moment, 200 ms later.

### Shell

The shell is what the glasses run day to day: one program per arm that lives there and shows **tiles**. A tile is a
rectangle of the panel with a kind (its little program), a place in a priority list, the scenes it's in, and on or
off. A tile that's on shows unless a tile above it that shows overlaps it; when it shows it has its whole rectangle,
and every tile below that overlaps it, by even a pixel, hides until it goes. So a notification across the top hides
the clock and battery under it while it's up, and they come back when it goes.

| Kind | Shows |
|---|---|
| `clock` | the time with or without seconds, 12- or 24-hour, kept by the glasses |
| `date` | the date in any strftime format, from a month of dates the Mac sends |
| `battery` | both arms' charge, averaged (a battery and %) or each |
| `notification` | an icon for its kind (message, mail, calendar, call, music, alert, app, bell), a title and wrapped text, or all on one line; a tap dismisses it, a double tap brings it back. New notifications on the Mac pop up in it (unless turned off) |
| `notification_list` | what's in the Mac's Notification Center, newest first: an icon and a line each, as many as fit its height |
| `weather` | an icon, the temperature and the humidity where the Mac is (open-meteo, every 10 minutes), each on or off |
| `steps` | footprints and a step count: a placeholder until the phone sends one |
| `heart_rate` | a heart and beats a minute: a placeholder until the phone or a watch sends them |
| `navigation` | while a route is followed: the next turn (an arrow, the distance over the street) when the tile is at least twice as wide as high, else a square or round map around you, heading up, zoomed to show the turn. No source on the Mac yet (the phone's maps app later); the editor's "Demo route" walks a made-up one |
| `device_battery` | the Mac's charge, and a bolt while it's on power |
| `text` | lines from `erisg1 shell text NAME` |

**Scenes**: tilting the head past a threshold (say 25° down, 20° up) switches to that scene's tiles, with 6° of give
before switching back; a tile can be in several scenes (a notification is in all of them). The right arm watches
the tilt (its accelerometer's `TILT` trigger) and both arms switch together.

| | |
|---|---|
| `shell` | Puts the shell on the glasses (or picks it up if it's already running), keeps their time, passes on notifications, text, the weather, the Mac's battery and its notifications, and serves the layout editor at http://127.0.0.1:4141, until Ctrl-C. The shell goes on running on the glasses after: the clock, the battery, the scenes, taps, notifications timing out |
| `shell say [--icon KIND] [TITLE:] TEXT` | A notification, through a running `erisg1 shell` |
| `shell text NAME [TEXT]` | Text in a text tile; no text turns it off |
| `shell show NAME [SECONDS]`, `shell hide NAME` | A tile on (for a while) or off |
| `shell preview [--default]` | The saved (or default) layout on two simulated arms, as `previews/shell.png` |
| `shell run --no-glasses` | Only the editor |

On the glasses a tap on either TouchBar dismisses the top dismissable tile showing (a notification), a double tap
brings it back, and a hold hides everything until the next hold. The panel is off with nothing showing and in the
case.

The editor drags and resizes tiles over a picture of what the glasses would show in each scene: the layout run on a
simulated arm (`src/shell/sim.rs`) with sample data, the head tilted for the scene, redrawn as you change it. A
dragged tile snaps to the edges and centres of the panel and of the scene's other tiles, to the gaps and margins
they already keep, and (resizing) to their sizes, with guides showing what it lines up with; Shift places it freely.
Strips of the display can be cut off (say the top and bottom 30 px): no tile may reach into them, and the layout's
check refuses one that does. Each tile can have its own depth (else the layout's): the eyes see it that far apart. Clocks, dates and the
tiles that show messages can use Departure Mono (11, 22 or 33 px), a pixel font that reads better small than the
8 × 8 one; in messages it has 164 common emoji at 11 and 22 px (Noto Emoji, black and white), two characters wide.
`tools/fonts.py` makes the data in `assets/fonts/` (see its NOTICE).
Tiles that overlap in either eye count as overlapping, so tiles at different depths never draw over each other.
The list's order is the priority; each kind has its own options. "Save & send to glasses" writes
`~/.config/erisg1/shell.json` and puts the new layout on the glasses.

How it works (`src/shell/program.rs`): the Mac builds each arm's program from the layout, with each tile's
overlaps and each scene's tiles worked out ahead as masks, so when a tile turns on or off or the scene changes the
arm finds what shows in a few instructions, clears the rectangles that changed, redraws the tiles that touch them
and sends the area around them to the panel in one `DRAW`. Icons (`src/shell/icons.rs`) are shapes drawn here at
each tile's size as 4-bit coverage, so their edges are smooth, and `BLIT`ted from the arena; the turn arrow is one
slot the Mac redraws when the turn changes. A map (`src/shell/nav.rs`) is lines the Mac projects and clips to the
tile, and the arm draws with `LINE`, skipping any point outside the tile. The Mac writes a tile's
content, then a generation word, and raises a trigger; it never writes what the program writes, so the two can't
race. Ticks fall on whole seconds the Mac sets in each arm's clock, and the right arm keeps the left's ticks (and
scene) to its own over the arm-to-arm link once a minute, so the eyes stay together for as long as the shell runs
alone. A later `erisg1 shell` reads a signature from the arena and picks up a shell it finds running instead of
starting over. The Mac's notifications come from Notification Center's database
(`~/Library/Group Containers/group.com.apple.usernoted/db2/db`, read-only).

Both arms here run 0.5.60 with `firmware/patches/0001-ask-the-host-to-pair-in-the-case.patch`, built as 0.5.60+1.

To build it, `firmware/scripts/setup.sh` puts NCS 2.5.1 (at the commits g1v0 was tested with), Zephyr SDK 0.16.5-1
and Even's 1.6.6 firmware (parts of which g1v0 embeds, checked against scouter_kit's hashes) in `~/ncs`, then
`firmware/scripts/build.sh 0.5.60` builds `~/ncs/g1work/build/g1v0-v0560/zephyr/app_update.bin`. scouter_kit's
own scripts do the same in Linux containers; its emulator tests don't run on macOS.

### Installing

Each arm's bootloader takes any image signed with MCUboot's public sample key, which is the key Even signs with, and
overwrites the running firmware with it, with no way back. `erisg1 flash` only ever sends the app core image (the net
core runs the Bluetooth radio), uploads into the spare slot and checks it, and installs only with `--install`, with
the arm in the charging case, lid open, charged past half. Left arm first: with Even's firmware the right arm
decides when both eyes draw. After the install the arm advertises as `G1v0_L` (or `G1v0_R`) at a new address.

In the case g1v0 obeys any computer; out of it, only one it has paired with, and it only takes new pairings in the
case. A Mac can't start a pairing itself, only answer a device that asks, and upstream g1v0 asks only out of the
case, where it refuses. Our patch makes it ask in the case too: connect once with the arm in the case (any `v0`
command) and macOS pairs without a prompt, after which the arm works out of the case.

Other references: Even's [EvenDemoApp](https://github.com/even-realities/EvenDemoApp), Gadgetbridge's G1
driver and MentraOS's G1 drivers.
