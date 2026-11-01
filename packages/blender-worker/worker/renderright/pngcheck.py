"""Small PNG decoder used to verify checkpoint integrity without extra packages."""
import struct
import zlib


def decode_png(path):
    with open(path, "rb") as f:
        data = f.read()
    if len(data) <= 64:
        raise ValueError("PNG file is too small to contain a valid image")
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("invalid PNG signature")
    pos, width, height, depth, color, interlace, compressed = 8, 0, 0, 0, 0, 0, bytearray()
    while pos < len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        typ = data[pos + 4:pos + 8]
        chunk = data[pos + 8:pos + 8 + length]
        crc = struct.unpack(">I", data[pos + 8 + length:pos + 12 + length])[0]
        if zlib.crc32(typ + chunk) & 0xffffffff != crc:
            raise ValueError("PNG chunk CRC mismatch")
        if typ == b"IHDR":
            width, height, depth, color, comp, filt, interlace = struct.unpack(">IIBBBBB", chunk)
            if depth != 8 or color not in (0, 2, 4, 6) or interlace != 0 or comp or filt:
                raise ValueError("unsupported PNG encoding")
        elif typ == b"IDAT":
            compressed.extend(chunk)
        elif typ == b"IEND":
            break
        pos += length + 12
    bpp = {0: 1, 2: 3, 4: 2, 6: 4}.get(color)
    if not width or not height or bpp is None:
        raise ValueError("missing PNG header")
    raw = zlib.decompress(bytes(compressed))
    stride = width * bpp
    if len(raw) != height * (stride + 1):
        raise ValueError("PNG image data length mismatch")
    # Reconstruct scanlines; bytes are retained for downsampled anomaly checks.
    pixels = bytearray(height * stride)
    src = 0
    for y in range(height):
        ft = raw[src]
        src += 1
        row = bytearray(raw[src:src + stride])
        src += stride
        prev = pixels[(y - 1) * stride:y * stride] if y else bytearray(stride)
        for i in range(stride):
            a = row[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if ft == 1: row[i] = (row[i] + a) & 255
            elif ft == 2: row[i] = (row[i] + b) & 255
            elif ft == 3: row[i] = (row[i] + ((a + b) >> 1)) & 255
            elif ft == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                row[i] = (row[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
            elif ft != 0: raise ValueError("invalid PNG filter")
        pixels[y * stride:(y + 1) * stride] = row
    return width, height, bpp, pixels


def frame_quality(decoded):
    """Reject empty/flat renders while retaining valid transparent silhouettes."""
    width, height, bpp, pixels = decoded
    if bpp not in (1, 2, 3, 4) or len(pixels) != width * height * bpp:
        raise ValueError("invalid decoded frame channels")
    color_channels = 1 if bpp in (1, 2) else 3
    names = ("luma",) if color_channels == 1 else ("red", "green", "blue")
    ranges = {}
    for i, name in enumerate(names):
        channel = pixels[i::bpp]
        ranges[name] = {"min": min(channel), "max": max(channel)}
    alpha = None
    visible_alpha_silhouette = False
    if bpp in (2, 4):
        channel = pixels[bpp - 1::bpp]
        alpha = {"min": min(channel), "max": max(channel)}
        if alpha["max"] == 0:
            raise ValueError("rendered frame is fully transparent")
        visible_alpha_silhouette = alpha["min"] != alpha["max"]
    if not visible_alpha_silhouette and all(r["max"] - r["min"] < 4 for r in ranges.values()):
        raise ValueError("rendered frame is near-constant")
    return {"status": "ok", "color_ranges": ranges, "alpha_range": alpha,
            "visible_alpha_silhouette": visible_alpha_silhouette}


def luminance_tiles(decoded, cols=16, rows=9):
    width, height, bpp, data = decoded
    out = []
    for ty in range(rows):
        for tx in range(cols):
            x0, x1 = tx * width // cols, max(tx * width // cols + 1, (tx + 1) * width // cols)
            y0, y1 = ty * height // rows, max(ty * height // rows + 1, (ty + 1) * height // rows)
            total = count = 0
            # Sparse, evenly spaced samples approximate area-resize tiles while
            # keeping checkpoint verification cheap at maximum allowed resolution.
            for sy in range(4):
                y = min(height - 1, y0 + (sy * max(1, y1 - y0)) // 4)
                for sx in range(4):
                    x = min(width - 1, x0 + (sx * max(1, x1 - x0)) // 4)
                    off = (y * width + x) * bpp
                    if bpp in (1, 2): lum = data[off]
                    else: lum = (54 * data[off] + 183 * data[off + 1] + 19 * data[off + 2]) >> 8
                    total += lum; count += 1
            out.append(total / max(1, count))
    return out


def anomaly(prev, cur, nxt, threshold=25):
    a, b, c = luminance_tiles(prev), luminance_tiles(cur), luminance_tiles(nxt)
    dp = max(abs(x - y) for x, y in zip(a, b))
    dn = max(abs(x - y) for x, y in zip(b, c))
    dpn = max(abs(x - y) for x, y in zip(a, c))
    return {"delta_prev": round(dp, 3), "delta_next": round(dn, 3), "delta_neighbours": round(dpn, 3), "anomaly": min(dp, dn) > threshold and dpn < 0.5 * min(dp, dn)}
