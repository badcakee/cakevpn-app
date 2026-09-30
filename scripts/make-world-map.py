#!/usr/bin/env python3
"""Makes src/worldmap.ts: the world's countries for the home screen's map.

The shapes are Natural Earth's 1:50m countries (public domain), as packed by
the world-atlas package. They are drawn in the Natural Earth projection;
src/places.ts uses the same formula to put the locations on the map.

    python3 scripts/make-world-map.py
"""
import json, math, os, urllib.request

SHAPES = "https://cdn.jsdelivr.net/npm/world-atlas@2.0.2/countries-50m.json"
CODES = "https://cdn.jsdelivr.net/npm/i18n-iso-countries@7.14.0/codes.json"  # [alpha-2, alpha-3, numeric, ...]
WIDTH = 1000
NORTH, SOUTH = 84.0, -57.0  # the poles are left out: nothing is there to connect to
# Paths are written in tenths of a map unit, as whole numbers. The map zooms
# in up to 8 times, where a tenth is about half a pixel.
FINE = 10
# Border detail below this many degrees is dropped: under half a pixel at full zoom.
TOLERANCE = 0.02
# Islands smaller than this many tenths in both directions are left out.
SMALLEST = 3
# Natural Earth draws these countries together with their lands overseas
# (French Guiana, the Caribbean Netherlands). Coloring "France" should not
# color a piece of South America, so only what lies in this box keeps the code.
EUROPE_ONLY = {"FR", "NL"}
EUROPE = (-25.0, 34.0, 45.0, 72.0)  # west, south, east, north


def raw(lat, lon):
    """Natural Earth projection of a point given in degrees."""
    l, p = math.radians(lon), math.radians(lat)
    p2 = p * p
    p4 = p2 * p2
    x = l * (0.8707 - 0.131979 * p2 + p4 * (-0.013791 + p4 * (0.003971 * p2 - 0.001529 * p4)))
    y = p * (1.007226 + p2 * (0.015085 + p4 * (-0.044475 + 0.028874 * p2 - 0.005916 * p4)))
    return x, y


SCALE = WIDTH / (2 * raw(0, 180)[0])
TOP = raw(NORTH, 0)[1]
HEIGHT = round((TOP - raw(SOUTH, 0)[1]) * SCALE)


def project(lat, lon):
    x, y = raw(lat, lon)
    return WIDTH / 2 + x * SCALE, (TOP - y) * SCALE


def simplified(points, tolerance):
    """Douglas-Peucker: keeps the ends and every point that matters more than the tolerance."""
    if len(points) < 3:
        return points
    keep = [False] * len(points)
    keep[0] = keep[-1] = True
    todo = [(0, len(points) - 1)]
    while todo:
        first, last = todo.pop()
        (ax, ay), (bx, by) = points[first], points[last]
        dx, dy = bx - ax, by - ay
        length = math.hypot(dx, dy)
        worst, at = 0.0, -1
        for i in range(first + 1, last):
            px, py = points[i]
            far = abs(dx * (ay - py) - dy * (ax - px)) / length if length else math.hypot(px - ax, py - ay)
            if far > worst:
                worst, at = far, i
        if worst > tolerance:
            keep[at] = True
            todo += [(first, at), (at, last)]
    return [p for p, k in zip(points, keep) if k]


def cut(points, edge, keep_west):
    """Cuts a ring at a meridian and keeps the part west (or east) of it."""
    inside = (lambda lon: lon <= edge) if keep_west else (lambda lon: lon >= edge)
    kept = []
    for i, (lon, lat) in enumerate(points):
        plon, plat = points[i - 1]
        if inside(lon) != inside(plon):
            kept.append((edge, plat + (lat - plat) * (edge - plon) / (lon - plon)))
        if inside(lon):
            kept.append((lon, lat))
    return kept


def within_the_map(points):
    """
    A ring that crosses the 180° meridian (eastern Russia, Fiji) jumps from one
    edge of the map to the other, which would draw a line across the whole
    world. It is drawn as two pieces instead, one at each edge.
    """
    unwrapped, shift = [], 0
    for i, (lon, lat) in enumerate(points):
        if i:
            step = lon - points[i - 1][0]
            shift += -360 if step > 180 else 360 if step < -180 else 0
        unwrapped.append((lon + shift, lat))
    if all(-180 <= lon <= 180 for lon, _ in unwrapped):
        return [unwrapped]
    pieces = []
    for move in (-360, 0, 360):
        piece = cut(cut([(lon + move, lat) for lon, lat in unwrapped], 180, True), -180, False)
        if len(piece) >= 3:
            pieces.append(piece)
    return pieces


def path_of(points):
    """A ring as an SVG path in whole tenths: one absolute point, then steps from point to point."""
    drawn = []
    for lon, lat in points:
        x, y = project(max(min(lat, NORTH), SOUTH), lon)
        point = (round(x * FINE), round(y * FINE))
        if not drawn or drawn[-1] != point:
            drawn.append(point)
    if len(drawn) > 1 and drawn[0] == drawn[-1]:
        drawn.pop()
    xs, ys = [x for x, _ in drawn], [y for _, y in drawn]
    if len(drawn) < 3 or (max(xs) - min(xs) < SMALLEST and max(ys) - min(ys) < SMALLEST):
        return "", 0
    steps = []
    for (ax, ay), (bx, by) in zip(drawn, drawn[1:]):
        dx, dy = bx - ax, by - ay
        steps.append(f"{dx}{'' if dy < 0 else ' '}{dy}")
    text = f"M{drawn[0][0]} {drawn[0][1]}l" + " ".join(steps).replace(" -", "-") + "z"
    return text, len(drawn)


def main():
    topology = json.load(urllib.request.urlopen(SHAPES))
    alpha2 = {row[2]: row[0] for row in json.load(urllib.request.urlopen(CODES))}
    (sx, sy), (tx, ty) = topology["transform"]["scale"], topology["transform"]["translate"]
    arcs = []
    for arc in topology["arcs"]:
        x = y = 0
        points = []
        for dx, dy in arc:
            x, y = x + dx, y + dy
            points.append((x * sx + tx, y * sy + ty))  # lon, lat
        # A border is simplified once, so the two countries it separates still meet exactly.
        arcs.append(simplified(points, TOLERANCE))

    def ring(indexes):
        points = []
        for i in indexes:
            part = arcs[i] if i >= 0 else arcs[~i][::-1]
            points.extend(part[1:] if points else part)
        return points

    countries, count = [], 0
    for geometry in topology["objects"]["countries"]["geometries"]:
        if geometry.get("id") == "010":
            continue  # Antarctica
        shapes = geometry["arcs"] if geometry["type"] == "MultiPolygon" else [geometry["arcs"]]
        code = alpha2.get(geometry.get("id"), "")
        parts, overseas = [], []
        for shape in shapes:
            outline = ring(shape[0])
            lon = sum(p[0] for p in outline) / len(outline)
            lat = sum(p[1] for p in outline) / len(outline)
            away = code in EUROPE_ONLY and not (EUROPE[0] <= lon <= EUROPE[2] and EUROPE[1] <= lat <= EUROPE[3])
            for r in shape:
                for piece in within_the_map(ring(r)):
                    if max(lat for _, lat in piece) < SOUTH:
                        continue
                    text, n = path_of(piece)
                    (overseas if away else parts).append(text)
                    count += n
        name = geometry["properties"]["name"]
        if any(parts):
            countries.append((code, name, "".join(parts)))
        if any(overseas):
            countries.append(("", name + " (overseas)", "".join(overseas)))
    countries.sort(key=lambda c: (c[0] == "", c[0], c[1]))

    out = os.path.join(os.path.dirname(__file__), "..", "src", "worldmap.ts")
    with open(out, "w") as f:
        f.write("// Made by scripts/make-world-map.py from Natural Earth's 1:50m countries (public domain).\n")
        f.write("// Don't edit by hand.\n\n")
        f.write(f"export const WORLD_WIDTH = {WIDTH};\n")
        f.write(f"export const WORLD_HEIGHT = {HEIGHT};\n")
        f.write("/** The Natural Earth projection's scale and top edge used for the shapes. */\n")
        f.write(f"export const WORLD_SCALE = {SCALE!r};\n")
        f.write(f"export const WORLD_TOP = {TOP!r};\n")
        f.write("/** The shapes are in units this many times finer than the map's. */\n")
        f.write(f"export const WORLD_FINE = {FINE};\n")
        f.write("/** Every country: its two-letter code (\"\" when it has none) and its shape as an SVG path. */\n")
        f.write("export const WORLD_COUNTRIES: readonly (readonly [string, string])[] = [\n")
        for code, _, path in countries:
            f.write(f'  ["{code}", "{path}"],\n')
        f.write("];\n")
    coded = sum(1 for c in countries if c[0])
    print(f"{len(countries)} countries ({coded} with a code), {count} points, {WIDTH}x{HEIGHT}, {os.path.getsize(out)} bytes")


main()
