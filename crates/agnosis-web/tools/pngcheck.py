#!/usr/bin/env python3
"""Read a PNG and say whether it is uniformly black.

This is the smoke test's oracle (roadmap Extension LATER §5): a wasm
build that compiles but renders a black rectangle must fail it.

Pure standard library: no PIL, no numpy (neither is installed here).

    python3 tools/pngcheck.py shot.png
    python3 tools/pngcheck.py --region 0,72,1280,648 shot.png
    python3 tools/pngcheck.py --region 0,72,1280,648 --min-ink 0.2 shot.png
    python3 tools/pngcheck.py --region 0,72,1280,648 --min-colour 255,220,80 40 shot.png

It prints the size, the mean colour, the standard deviation, the number
of distinct colours and whether the image is uniformly black. It exits
1 when the image is uniformly black (or when it cannot be read), and 0
otherwise.

WHY the `--region`: the whole screenshot of the web page is NEVER
uniformly black, because the page draws its own toolbar above the
canvas — a dead canvas behind a live toolbar would pass the whole-image
check. The region is the canvas' own rectangle (the smoke test skips
the toolbar), and it is the region that has to carry the parade.

WHY the `--min-ink`: "not uniformly black" is too weak on its own. A
canvas that renders ONE stray balloon edge is still "not uniformly
black", so a page that renders almost nothing would pass. The minimum
ink is the share of the region's pixels that have to carry something.

WHY the `--min-colour`: even the ink share is too weak for the parade's
headline. A canvas whose cell is one pixel wide drops the duck art
entirely — the balloons and the grass keep the ink share above the
bar, so the ink check passes while the ducks are gone. The colour check
asks for the ducks' own colour (`agnosis_core::scene::DUCK_COLOR`, the
`[255, 220, 80]` yellow) inside the region: the ducks are the only
thing in the parade that paints it.

The tolerance is `COLOUR_TOLERANCE` (40) on every channel: the blitter
brightens a glyph that covers less than its cell, so a duck pixel is
the duck colour *or* a shade of it, never another colour. 40 is wide
enough for that shading and narrow enough to keep the sky (blue), the
grass (green) and the ropes (brown) out of the count.

WHY the colour is matched in TWO forms: the page has three kinds of
canvas, and they used to show the same scene in two different colour
spaces. The 2D back end puts the scene's bytes into the canvas as
they are, but the WebGL back end used to upload them as a **linear**
texture into an sRGB surface, which encoded them to sRGB a second
time: the same yellow arrived as `[255, 239, 152]`. A duck is
therefore matched either directly or through its sRGB encoding, and
the check holds on every back end. The two forms are 3 units apart in
blue, so the tolerance covers both; the rope's brightest form is 43
away in green and stays out of the count.

The GPU back ends are now colour-correct (the upload texture's format
follows the surface's, so the sRGB encode and the sRGB decode cancel
and the canvas carries the frame's own bytes), and the direct colour
is the only one that appears. The sRGB form is still accepted: the
tolerance is harmless and it keeps the check robust against a
regression in the colour handling.
"""

import struct
import sys
import zlib

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

# colour type -> channels
CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}

# How far a pixel may sit from the colour it is asked for, per channel.
# The blitter brightens a glyph that covers less than its cell (the
# "small-cell floor" in the core's blitter), so a duck pixel is the
# duck colour or a shade of it — never another colour. 40 is wide
# enough for that shading and narrow enough to keep the sky, the grass
# and the ropes out of the count.
COLOUR_TOLERANCE = 40


class PngError(Exception):
    """The file is not a PNG this script can read."""


def read_chunks(data):
    """Yield (type, payload) for every chunk of the file."""
    if not data.startswith(PNG_SIGNATURE):
        raise PngError("not a PNG file (bad signature)")
    offset = len(PNG_SIGNATURE)
    while offset + 8 <= len(data):
        (length,) = struct.unpack(">I", data[offset:offset + 4])
        kind = data[offset + 4:offset + 8]
        start = offset + 8
        end = start + length
        if end + 4 > len(data):
            raise PngError("truncated chunk %r" % kind)
        yield kind, data[start:end]
        offset = end + 4
    if offset != len(data):
        raise PngError("trailing bytes after the last chunk")


def paeth(a, b, c):
    """The PNG's Paeth predictor."""
    p = a + b - c
    pa = abs(p - a)
    pb = abs(p - b)
    pc = abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def unfilter(raw, height, row_bytes, step):
    """Undo the per-row filters and return the raw rows."""
    out = bytearray(height * row_bytes)
    previous = bytearray(row_bytes)
    offset = 0
    for y in range(height):
        if offset + 1 + row_bytes > len(raw):
            raise PngError("truncated image data on row %d" % y)
        filter_type = raw[offset]
        offset += 1
        line = bytearray(raw[offset:offset + row_bytes])
        offset += row_bytes
        if filter_type == 0:
            pass
        elif filter_type == 1:
            for i in range(step, row_bytes):
                line[i] = (line[i] + line[i - step]) & 0xFF
        elif filter_type == 2:
            for i in range(row_bytes):
                line[i] = (line[i] + previous[i]) & 0xFF
        elif filter_type == 3:
            for i in range(row_bytes):
                left = line[i - step] if i >= step else 0
                line[i] = (line[i] + ((left + previous[i]) >> 1)) & 0xFF
        elif filter_type == 4:
            for i in range(row_bytes):
                a = line[i - step] if i >= step else 0
                b = previous[i]
                c = previous[i - step] if i >= step else 0
                line[i] = (line[i] + paeth(a, b, c)) & 0xFF
        else:
            raise PngError("unknown filter type %d on row %d" % (filter_type, y))
        out[y * row_bytes:(y + 1) * row_bytes] = line
        previous = line
    return out


def samples_of_row(row, width, bit_depth, color_type, palette):
    """The (r, g, b, a) of every pixel of one row."""
    if bit_depth == 8:
        if color_type == 6:
            return [(row[i], row[i + 1], row[i + 2], row[i + 3]) for i in range(0, width * 4, 4)]
        if color_type == 2:
            return [(row[i], row[i + 1], row[i + 2], 255) for i in range(0, width * 3, 3)]
        if color_type == 0:
            return [(row[i], row[i], row[i], 255) for i in range(width)]
        if color_type == 4:
            return [(row[i], row[i], row[i], row[i + 1]) for i in range(0, width * 2, 2)]
        # palette
        return [palette_at(palette, row[i]) for i in range(width)]
    if bit_depth == 16:
        out = []
        for i in range(width):
            base = i * CHANNELS[color_type] * 2
            value = lambda c: row[base + c * 2]  # noqa: E731
            if color_type == 6:
                out.append((value(0), value(1), value(2), value(3)))
            elif color_type == 2:
                out.append((value(0), value(1), value(2), 255))
            elif color_type == 0:
                out.append((value(0), value(0), value(0), 255))
            elif color_type == 4:
                out.append((value(0), value(0), value(0), value(1)))
            else:
                out.append(palette_at(palette, row[base]))
        return out
    # 1, 2 or 4 bits per sample
    per_byte = 8 // bit_depth
    mask = (1 << bit_depth) - 1
    out = []
    for i in range(width):
        byte = row[i // per_byte]
        shift = 8 - bit_depth * (i % per_byte + 1)
        value = (byte >> shift) & mask
        if color_type == 3:
            out.append(palette_at(palette, value))
        else:
            scaled = value * 255 // mask
            out.append((scaled, scaled, scaled, 255))
    return out


def palette_at(palette, index):
    """The RGB of a palette entry (black when the index is out of range)."""
    if index * 3 + 2 >= len(palette):
        return (0, 0, 0, 255)
    base = index * 3
    return (palette[base], palette[base + 1], palette[base + 2], 255)


def analyse(path, region=None, colour=None):
    """Read the PNG and report on it.

    `region` is an optional (x, y, w, h) rectangle: the statistics are
    then taken over that rectangle only (clipped to the image), which is
    how the smoke test looks at the canvas alone.

    `colour` is an optional (r, g, b): the number of the region's
    pixels that ARE that colour — in either of the page's colour
    spaces — is counted and reported as `colour_hits`.
    """
    with open(path, "rb") as handle:
        data = handle.read()

    header = None
    palette = b""
    compressed = []
    for kind, payload in read_chunks(data):
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", payload)
        elif kind == b"PLTE":
            palette = payload
        elif kind == b"IDAT":
            compressed.append(payload)
        elif kind == b"IEND":
            break
    if header is None:
        raise PngError("the file carries no IHDR chunk")

    width, height, bit_depth, color_type, compression, filter_method, interlace = header
    if compression != 0 or filter_method != 0:
        raise PngError("unknown compression or filter method")
    if interlace != 0:
        raise PngError("interlaced PNGs are not supported")
    if color_type not in CHANNELS:
        raise PngError("unknown colour type %d" % color_type)
    if bit_depth not in (1, 2, 4, 8, 16):
        raise PngError("unknown bit depth %d" % bit_depth)
    if color_type == 3 and bit_depth == 16:
        raise PngError("a 16-bit palette is not a thing")

    channels = CHANNELS[color_type]
    row_bytes = (width * channels * bit_depth + 7) // 8
    step = max(1, (channels * bit_depth + 7) // 8)
    raw = zlib.decompress(b"".join(compressed))
    pixels = unfilter(raw, height, row_bytes, step)

    box = clip_region(region, width, height)

    total = 0
    sum_r = sum_g = sum_b = 0
    sum_r2 = sum_g2 = sum_b2 = 0
    colours = set()
    black = 0
    hits = 0

    for y in range(box[1], box[1] + box[3]):
        row = pixels[y * row_bytes:(y + 1) * row_bytes]
        line = samples_of_row(row, width, bit_depth, color_type, palette)
        for pixel in line[box[0]:box[0] + box[2]]:
            r, g, b, _a = pixel
            total += 1
            sum_r += r
            sum_g += g
            sum_b += b
            sum_r2 += r * r
            sum_g2 += g * g
            sum_b2 += b * b
            colours.add((r, g, b))
            if r == 0 and g == 0 and b == 0:
                black += 1
            if colour is not None and is_the_colour((r, g, b), colour):
                hits += 1

    if total == 0:
        raise PngError("the region holds no pixels")

    def mean(total_sum):
        return total_sum / total

    def deviation(total_sum, total_square):
        value = mean(total_square) - mean(total_sum) ** 2
        return value ** 0.5 if value > 0 else 0.0

    return {
        "width": width,
        "height": height,
        "region": box,
        "mean": (mean(sum_r), mean(sum_g), mean(sum_b)),
        "stddev": (
            deviation(sum_r, sum_r2),
            deviation(sum_g, sum_g2),
            deviation(sum_b, sum_b2),
        ),
        "colours": len(colours),
        "pixels": total,
        "black": black,
        "uniformly_black": black == total,
        "colour_hits": hits,
    }


def near_colour(pixel, target, tolerance):
    """True when the pixel is within `tolerance` of the target on EVERY channel."""
    return all(abs(value - wanted) <= tolerance for value, wanted in zip(pixel, target))


def srgb_encode(value):
    """The sRGB encoding of one channel, the way a canvas presents a linear byte."""
    x = value / 255.0
    if x <= 0.04045:
        return 12.92 * x
    return 1.055 * (x ** (1.0 / 2.4)) - 0.055


def encoded_colour(colour):
    """The colour as a WebGL or WebGPU canvas presents it (the sRGB encoding)."""
    return tuple(int(round(srgb_encode(channel) * 255.0)) for channel in colour)


def is_the_colour(pixel, colour):
    """True when the pixel is the colour, in either of the page's colour spaces."""
    if near_colour(pixel, colour, COLOUR_TOLERANCE):
        return True
    return near_colour(pixel, encoded_colour(colour), COLOUR_TOLERANCE)


def clip_region(region, width, height):
    """The region, clipped to the image. The whole image when it is None."""
    if region is None:
        return (0, 0, width, height)
    x, y, w, h = region
    x = max(0, min(x, width))
    y = max(0, min(y, height))
    w = min(w, width - x)
    h = min(h, height - y)
    if w <= 0 or h <= 0:
        raise PngError("the region %r lies outside the %dx%d image" % (region, width, height))
    return (x, y, w, h)


def parse_region(text):
    """Parse `x,y,w,h` into a rectangle."""
    parts = text.split(",")
    if len(parts) != 4:
        raise PngError("the region must be x,y,w,h (got %r)" % text)
    try:
        numbers = [int(part) for part in parts]
    except ValueError:
        raise PngError("the region must be four whole numbers (got %r)" % text)
    if numbers[2] <= 0 or numbers[3] <= 0:
        raise PngError("the region must have a positive size (got %r)" % text)
    return tuple(numbers)


def parse_ink(text):
    """Parse the minimum share of ink, in percent."""
    try:
        value = float(text)
    except ValueError:
        raise PngError("the minimum ink must be a number of percent (got %r)" % text)
    if value < 0.0 or value > 100.0:
        raise PngError("the minimum ink must be between 0 and 100 percent (got %r)" % text)
    return value


def parse_colour(text):
    """Parse `R,G,B` or `R,G,B,COUNT` into a colour and a pixel count.

    The count is `None` when the text carried the colour alone: the
    caller may then take the count from the next argument, so that
    `--min-colour 255,220,80 40` works as well as
    `--min-colour 255,220,80,40`.
    """
    parts = text.split(",")
    if len(parts) not in (3, 4):
        raise PngError("the colour must be R,G,B or R,G,B,COUNT (got %r)" % text)
    try:
        numbers = [int(part) for part in parts]
    except ValueError:
        raise PngError("the colour must be whole numbers (got %r)" % text)
    channels = numbers[:3]
    for channel in channels:
        if channel < 0 or channel > 255:
            raise PngError("the colour channels must be 0..255 (got %r)" % text)
    count = None
    if len(numbers) == 4:
        if numbers[3] < 0:
            raise PngError("the pixel count cannot be negative (got %r)" % text)
        count = numbers[3]
    return tuple(channels), count


def parse_count(text):
    """Parse the minimum number of pixels of a colour."""
    try:
        return int(text)
    except ValueError:
        raise PngError("the pixel count must be a whole number (got %r)" % text)


def usage():
    """The one-line usage message."""
    return (
        "usage: pngcheck.py [--region x,y,w,h] [--min-ink percent] "
        "[--min-colour R,G,B[,COUNT]] <file.png>\n"
    )


def main(argv):
    region = None
    min_ink = None
    colour = None
    colour_count = 1
    path = None
    arguments = list(argv[1:])
    while arguments:
        argument = arguments.pop(0)
        if argument in ("--region", "--min-ink", "--min-colour"):
            if not arguments:
                sys.stderr.write("pngcheck: %s needs its value\n" % argument)
                return 2
            value = arguments.pop(0)
            try:
                if argument == "--region":
                    region = parse_region(value)
                elif argument == "--min-ink":
                    min_ink = parse_ink(value)
                else:
                    colour, given_count = parse_colour(value)
                    colour_count = 1 if given_count is None else given_count
                    if given_count is None and arguments and arguments[0].isdigit():
                        colour_count = parse_count(arguments.pop(0))
            except PngError as error:
                sys.stderr.write("pngcheck: %s\n" % error)
                return 2
            continue
        if argument.startswith("--region="):
            try:
                region = parse_region(argument[len("--region="):])
            except PngError as error:
                sys.stderr.write("pngcheck: %s\n" % error)
                return 2
            continue
        if argument.startswith("--min-ink="):
            try:
                min_ink = parse_ink(argument[len("--min-ink="):])
            except PngError as error:
                sys.stderr.write("pngcheck: %s\n" % error)
                return 2
            continue
        if argument.startswith("--min-colour="):
            try:
                colour, given_count = parse_colour(argument[len("--min-colour="):])
                colour_count = 1 if given_count is None else given_count
            except PngError as error:
                sys.stderr.write("pngcheck: %s\n" % error)
                return 2
            continue
        if path is None:
            path = argument
            continue
        sys.stderr.write(usage())
        return 2

    if path is None:
        sys.stderr.write(usage())
        return 2

    try:
        report = analyse(path, region, colour)
    except (PngError, OSError, zlib.error) as error:
        sys.stderr.write("pngcheck: %s: %s\n" % (path, error))
        return 1

    mean = report["mean"]
    stddev = report["stddev"]
    black_share = 100.0 * report["black"] / report["pixels"]
    ink_share = 100.0 - black_share
    print("size: %dx%d" % (report["width"], report["height"]))
    print("region: %d,%d %dx%d" % report["region"])
    print("mean: %.2f %.2f %.2f" % mean)
    print("stddev: %.2f %.2f %.2f" % stddev)
    print("distinct colours: %d" % report["colours"])
    print("black pixels: %d of %d (%.1f%%)" % (report["black"], report["pixels"], black_share))
    print("ink: %.3f%%" % ink_share)
    print("uniformly black: %s" % ("yes" if report["uniformly_black"] else "no"))
    if colour is not None:
        print(
            "colour %d,%d,%d (+/-%d): %d pixels"
            % (colour[0], colour[1], colour[2], COLOUR_TOLERANCE, report["colour_hits"])
        )
    if report["uniformly_black"]:
        sys.stderr.write("pngcheck: the image is uniformly black\n")
        return 1
    if min_ink is not None and ink_share < min_ink:
        sys.stderr.write("pngcheck: only %.3f%% of the region carries ink, %.3f%% is needed\n" % (ink_share, min_ink))
        return 1
    if colour is not None and report["colour_hits"] < colour_count:
        sys.stderr.write(
            "pngcheck: only %d pixels are within %d of %d,%d,%d, %d are needed\n"
            % (report["colour_hits"], COLOUR_TOLERANCE, colour[0], colour[1], colour[2], colour_count)
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
