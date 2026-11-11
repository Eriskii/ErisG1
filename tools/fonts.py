#!/usr/bin/env python3
"""Makes the shell's font data in assets/fonts/ (src/shell/fonts.rs reads it). Needs Pillow.

  tools/fonts.py DepartureMono-Regular.otf "NotoEmoji[wght].ttf"

departure_mono.bin: Departure Mono's characters at 11 px (its pixel size), in the order of v5's glyphs (0-9, A-Z,
a-z, the rest of printable ASCII, the degree sign), each a 7 x 12 cell from 9 px above the baseline to 3 below: 12
bytes, one a row, the leftmost pixel in the top bit.

emoji_N.bin, N = 1, 2 (at 33 px they'd crowd out other fonts): the emoji in EMOJI from Noto Emoji (black and
white), each drawn to a box two cells
wide and a cell high at that multiple of 11 px: u16 count, then for each: u32 codepoint, u8 w, u8 h, i8 x (from the
box's left), i8 y (from the baseline, up is negative), then h rows of ceil(w / 8) bytes, the leftmost pixel in the
top bit. Ink where the coverage is at least half.
"""
import struct
import sys

from PIL import Image, ImageDraw, ImageFont

ADVANCE, ASCENT, DESCENT = 7, 9, 3
SUPERSAMPLE = 8

# The emoji the glasses know, in order (each is a glyph after the 128th).
EMOJI = (
    "😀😃😄😁😆😅😂🤣😊😇🙂😉😍🥰😘😋😛😜🤪😎🤓🥳😏😒😞😔😟🙁😣😫😩🥺😢😭😤😠😡🤯😳😱😨😰🤔🤭🤫🙄😐😬😴🤤😷🤒🤢🤮🥵🥶😵🤠"
    "💀👻👽🤖💩😺"
    "👍👎👌✌🤞🤟🤘👏🙌🙏🤝💪👋👀👉👈👆👇"
    "❤🧡💛💚💙💜🖤🤍💔💕💖💯"
    "✅❌⚠❗❓💡🔥⭐✨🎉🎂🎁🏆🔔🔒🔑💬📞📧📅📷🎵🎧🎮💻📱⏰⌛📌📍📎📝📚💰📈📉🛒"
    "☀🌤⛅☁🌧⛈❄🌈🌙☔"
    "🚗🚌🚆✈🚀🚲🏠🏢🍕🍔🍜🍣🍺🍷☕🍰🍎🐶🐱🌱🌸⚽🏃"
)


def chars():
    out = [chr(c) for c in range(ord("0"), ord("9") + 1)]
    out += [chr(c) for c in range(ord("A"), ord("Z") + 1)]
    out += [chr(c) for c in range(ord("a"), ord("z") + 1)]
    out += [chr(c) for c in range(ord(" "), ord("~") + 1) if not chr(c).isalnum()]
    return out + ["°"]


def departure(path):
    """Each character's 7 x 12 cell: the font drawn 8 times over, each 8 x 8 block read at its centre."""
    font = ImageFont.truetype(path, 11 * SUPERSAMPLE)
    out = bytearray()
    for ch in chars():
        k = SUPERSAMPLE
        img = Image.new("L", (ADVANCE * k, (ASCENT + DESCENT) * k), 0)
        ImageDraw.Draw(img).text((0, ASCENT * k), ch, font=font, fill=255, anchor="ls")
        for y in range(ASCENT + DESCENT):
            row = 0
            for x in range(ADVANCE):
                if img.getpixel((x * k + k // 2, y * k + k // 2)) >= 128:
                    row |= 0x80 >> x
            out.append(row)
    return bytes(out)


def emoji(path, n):
    """The emoji at n times 11 px, each in a box 2 cells wide and a cell high, cropped to its ink."""
    w, h, ascent = 2 * ADVANCE * n, (ASCENT + DESCENT) * n, ASCENT * n
    font = ImageFont.truetype(path, h)
    try:
        font.set_variation_by_axes([600])
    except OSError:
        pass
    out = bytearray(struct.pack("<H", len(EMOJI)))
    for ch in EMOJI:
        img = Image.new("L", (w, h), 0)
        draw = ImageDraw.Draw(img)
        left, top, right, bottom = draw.textbbox((0, 0), ch, font=font)
        # Centred in the box.
        draw.text(((w - (right - left)) / 2 - left, (h - (bottom - top)) / 2 - top), ch, font=font, fill=255)
        ink = [(x, y) for y in range(h) for x in range(w) if img.getpixel((x, y)) >= 128]
        if not ink:
            raise SystemExit(f"{ch!r} (U+{ord(ch):04X}) has no ink at {h} px")
        x0, x1 = min(x for x, _ in ink), max(x for x, _ in ink) + 1
        y0, y1 = min(y for _, y in ink), max(y for _, y in ink) + 1
        out += struct.pack("<IBBbb", ord(ch), x1 - x0, y1 - y0, x0, y0 - ascent)
        on = set(ink)
        for y in range(y0, y1):
            row = bytearray((x1 - x0 + 7) // 8)
            for x in range(x0, x1):
                if (x, y) in on:
                    row[(x - x0) // 8] |= 0x80 >> ((x - x0) % 8)
            out += row
    return bytes(out)


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    assert len(EMOJI) == len(set(EMOJI)), "an emoji twice"
    assert all(ord(c) > 0x7F for c in EMOJI)
    root = __file__.rsplit("/tools/", 1)[0] + "/assets/fonts/"
    open(root + "departure_mono.bin", "wb").write(departure(sys.argv[1]))
    for n in (1, 2):
        open(root + f"emoji_{n}.bin", "wb").write(emoji(sys.argv[2], n))
    print(len(EMOJI), "emoji")


if __name__ == "__main__":
    main()
