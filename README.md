# ErisG1

Custom software for Even Realities G1 glasses, run from a Mac. It talks to both arms over Bluetooth LE
directly, without Even's app, and shows text and full-screen images on them. The packet format lives in
`src/proto.rs` with no Bluetooth in it, so an Android app can share it later.

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

Other references: Even's [EvenDemoApp](https://github.com/even-realities/EvenDemoApp), Gadgetbridge's G1
driver and MentraOS's G1 drivers.
