#!/usr/bin/env python3
"""Makes src/worldmap.ts: the outline of the world's land for the home screen's map.

The outline is Natural Earth's 1:110m land (public domain), as packed by the
world-atlas package. It is drawn in the Natural Earth projection; src/places.ts
uses the same formula to put the locations on it.

    python3 scripts/make-world-map.py
"""
import json, math, os, urllib.request

SOURCE = "https://cdn.jsdelivr.net/npm/world-atlas@2.0.2/land-110m.json"
WIDTH = 1000
NORTH, SOUTH = 84.0, -57.0  # the poles are left out: nothing is there to connect to


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


def main():
    topology = json.load(urllib.request.urlopen(SOURCE))
    (sx, sy), (tx, ty) = topology["transform"]["scale"], topology["transform"]["translate"]
    arcs = []
    for arc in topology["arcs"]:
        x = y = 0
        points = []
        for dx, dy in arc:
            x, y = x + dx, y + dy
            points.append((x * sx + tx, y * sy + ty))  # lon, lat
        arcs.append(points)

    def ring(indexes):
        points = []
        for i in indexes:
            part = arcs[i] if i >= 0 else arcs[~i][::-1]
            points.extend(part[1:] if points else part)
        return points

    polygons = []
    for geometry in topology["objects"]["land"]["geometries"]:
        shapes = geometry["arcs"] if geometry["type"] == "MultiPolygon" else [geometry["arcs"]]
        for shape in shapes:
            for r in shape:
                polygons.extend(within_the_map(ring(r)))

    parts, count = [], 0
    for points in polygons:
        if max(lat for _, lat in points) < -60:
            continue  # Antarctica
        drawn = []
        for lon, lat in points:
            x, y = project(max(min(lat, NORTH), SOUTH), lon)
            point = (round(x), round(y))
            if not drawn or drawn[-1] != point:
                drawn.append(point)
        if len(drawn) < 3:
            continue
        count += len(drawn)
        parts.append("M" + "L".join(f"{x},{y}" for x, y in drawn) + "Z")

    out = os.path.join(os.path.dirname(__file__), "..", "src", "worldmap.ts")
    with open(out, "w") as f:
        f.write("// Made by scripts/make-world-map.py from Natural Earth's 1:110m land (public domain).\n")
        f.write("// Don't edit by hand.\n\n")
        f.write(f"export const WORLD_WIDTH = {WIDTH};\n")
        f.write(f"export const WORLD_HEIGHT = {HEIGHT};\n")
        f.write("/** The Natural Earth projection's scale and top edge used for the outline. */\n")
        f.write(f"export const WORLD_SCALE = {SCALE!r};\n")
        f.write(f"export const WORLD_TOP = {TOP!r};\n")
        f.write(f'export const WORLD_PATH =\n  "{"".join(parts)}";\n')
    print(f"{len(parts)} shapes, {count} points, {WIDTH}x{HEIGHT}, {os.path.getsize(out)} bytes")


main()
