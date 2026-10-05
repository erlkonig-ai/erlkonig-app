#!/usr/bin/env python3
"""Draws the menu-bar template icon (a small crown) with the standard library
only, and writes assets/menubar.png (36x36, black on transparent, for an
18 pt menu bar at 2x). macOS tints a template image for light and dark bars.

Usage: python3 scripts/make-icon.py [out.png]
"""
import struct
import sys
import zlib

SIZE = 36
SS = 4  # supersamples per axis

CROWN = [(5.5, 27.0), (5.5, 12.5), (12.0, 19.5), (18.0, 8.5), (24.0, 19.5), (30.5, 12.5), (30.5, 27.0)]
BAND = (5.5, 28.5, 30.5, 32.0)  # x0, y0, x1, y1
JEWELS = [(5.5, 10.0, 2.4), (18.0, 6.0, 2.6), (30.5, 10.0, 2.4)]


def inside_polygon(x, y, poly):
    hit = False
    j = len(poly) - 1
    for i in range(len(poly)):
        xi, yi = poly[i]
        xj, yj = poly[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            hit = not hit
        j = i
    return hit


def covered(x, y):
    if inside_polygon(x, y, CROWN):
        return True
    x0, y0, x1, y1 = BAND
    if x0 <= x <= x1 and y0 <= y <= y1:
        return True
    return any((x - cx) ** 2 + (y - cy) ** 2 <= r * r for cx, cy, r in JEWELS)


def render():
    rows = []
    for py in range(SIZE):
        row = bytearray([0])  # filter type None
        for px in range(SIZE):
            n = sum(
                covered(px + (sx + 0.5) / SS, py + (sy + 0.5) / SS)
                for sx in range(SS)
                for sy in range(SS)
            )
            row += bytes((0, 0, 0, round(255 * n / (SS * SS))))
        rows.append(bytes(row))
    return b"".join(rows)


def png(width, height, rgba_rows):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(rgba_rows, 9))
        + chunk(b"IEND", b"")
    )


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "assets/menubar.png"
    with open(out, "wb") as f:
        f.write(png(SIZE, SIZE, render()))
    print(out)
