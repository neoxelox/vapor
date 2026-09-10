"""Checks every export under assets/ at the file level: dimensions, alpha
geometry, container representations, and the references the README block,
the web manifest, and the head snippet make. Requires Pillow and NumPy. It
does not build an app or render anything on a platform; run it after
build-icons.cjs or after replacing a hand export.
"""
from pathlib import Path
from PIL import Image
import json, re, sys
import numpy as np
from collections import deque
import xml.etree.ElementTree as ET

root = Path(__file__).resolve().parents[2]
MASTERS = 'source/masters'
report = {'status': 'passed'}

def opened(p):
    im = Image.open(root / p); im.load(); return im

# macOS legacy fallback: the 1024 slot is the light tile master.
master = opened('macos/Vapor.iconset/icon_512x512@2x.png')
assert master.size == (1024, 1024) and master.mode == 'RGBA'
a = np.asarray(master.getchannel('A'))
assert master.getchannel('A').getbbox() == (100, 100, 924, 924)
assert np.array_equal(a, a[::-1, :]) and np.array_equal(a, a[:, ::-1]) and np.array_equal(a, a.T)
assert not np.any(a[:100]) and not np.any(a[924:]) and not np.any(a[:, :100]) and not np.any(a[:, 924:])
mask = np.asarray(opened(f'{MASTERS}/macos-legacy-mask-1024.png').getchannel('A'))
assert np.array_equal(a, mask), 'Enclosure alpha must equal the mask; no interior holes or exterior glow'
report['curve_validation'] = json.loads((root / f'{MASTERS}/curve-validation.json').read_text())
report['macos_fallback'] = {'visible_alpha_bounds': [100, 100, 924, 924], 'exterior_alpha_pixels': 0,
                            'mask_symmetry': 'horizontal, vertical, and diagonal', 'enclosure_alpha_matches_mask': True}
for n in [16, 32, 128, 256, 512]:
    for scale in [1, 2]:
        im = opened(f'macos/Vapor.iconset/icon_{n}x{n}{"@2x" if scale == 2 else ""}.png')
        assert im.size == (n * scale, n * scale)

# The sculpture master and the copy inside the Icon Composer document.
fg = opened(f'{MASTERS}/vapor-foreground-1024.png'); assert fg.size == (1024, 1024) and fg.mode == 'RGBA'
assert (root / f'{MASTERS}/vapor-foreground-1024.png').read_bytes() == (root / 'macos/Vapor.icon/Assets/vapor-sculpted.png').read_bytes()
doc = json.loads((root / 'macos/Vapor.icon/icon.json').read_text())
for group in doc['groups']:
    for layer in group['layers']:
        assert (root / 'macos/Vapor.icon/Assets' / layer['image-name']).is_file(), layer['image-name']
fa = np.asarray(fg.getchannel('A')); assert fa.min() == 0 and fa.max() == 255
assert not fa[:200].any() and not fa[800:].any() and not fa[:, :50].any() and not fa[:, 980:].any()
# Transparent region between wisps remains connected to the exterior; highlights have no holes.
solid = fa >= 128; outside = ~solid; seen = np.zeros_like(outside); q = deque([(0, 0)]); seen[0, 0] = True
while q:
    y, x = q.popleft()
    for dy, dx in [(0, 1), (0, -1), (1, 0), (-1, 0)]:
        yy, xx = y + dy, x + dx
        if 0 <= yy < 1024 and 0 <= xx < 1024 and outside[yy, xx] and not seen[yy, xx]:
            seen[yy, xx] = True; q.append((yy, xx))
assert np.array_equal(outside, seen), 'Transparent holes detected inside the foreground'
for p in ['vapor-mono-1024.png', 'vapor-flat-orange-1024.png']:
    im = opened(f'{MASTERS}/{p}'); assert im.size == (1024, 1024) and np.array_equal(np.asarray(im.getchannel('A')), fa)
for p in ['background-light-1024.png', 'background-dark-1024.png']:
    im = opened(f'{MASTERS}/{p}'); assert im.size == (1024, 1024)
    assert im.mode == 'RGB' or im.getchannel('A').getextrema() == (255, 255)
report['native_layers'] = {'foreground_alpha_bounds': fg.getchannel('A').getbbox(), 'transparent_interior_holes': 0,
                           'foreground_variants_registered': True, 'backgrounds_full_bleed_opaque': True}

# Windows
win_sizes = [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 128, 256]
for p in ['windows/Vapor.ico', 'windows/Vapor-unplated.ico']:
    im = opened(p); assert im.ico.sizes() == {(n, n) for n in win_sizes}
    for n in win_sizes:
        f = im.ico.getimage((n, n)); f.load(); assert f.mode == 'RGBA' and f.getpixel((0, 0))[3] <= 1
report['windows_ico_sizes'] = win_sizes
for n in [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256]:
    for suffix in ['', '_altform-unplated', '_altform-lightunplated']:
        assert opened(f'windows/msix/Assets/AppList.targetsize-{n}{suffix}.png').size == (n, n)
spec = {'AppList': (44, 44), 'SmallTile': (71, 71), 'MedTile': (150, 150), 'WideTile': (310, 150),
        'LargeTile': (310, 310), 'StoreLogo': (50, 50), 'SplashScreen': (620, 300)}
for name, (w, h) in spec.items():
    for scale in [100, 125, 150, 200, 250, 300, 400]:
        expected = (int(w * scale / 100 + 0.5), int(h * scale / 100 + 0.5))
        assert opened(f'windows/msix/Assets/{name}.scale-{scale}.png').size == expected
report['msix_png_count'] = len(list((root / 'windows/msix/Assets').glob('*.png')))
ET.parse(root / 'windows/msix/VisualElements.xml')

# Linux
for n in [16, 22, 24, 32, 48, 64, 96, 128, 192, 256, 512, 1024]:
    assert opened(f'linux/hicolor/{n}x{n}/apps/vapor.png').size == (n, n)
for p in root.rglob('*.svg'):
    if 'node_modules' not in p.parts: ET.parse(p)

# Marketing sources the banner builder composes from
word = json.loads((root / 'source/marketing/wordmark-paths.json').read_text())
assert word['glyphs'] and word['width'] > 0 and word['height'] > 0
for theme in ['light', 'dark']:
    plate = opened(f'source/marketing/atmosphere-{theme}.png'); assert plate.size[0] * 1 >= 1800 and plate.size[0] / plate.size[1] > 2.5

# Web and GitHub
for n in [16, 32, 48]:
    f = opened(f'web/public/favicon-{n}x{n}.png'); assert f.mode == 'RGBA' and f.getpixel((0, 0))[3] == 0
assert '<rect' not in (root / 'web/public/favicon.svg').read_text()
for theme in ['light', 'dark']:
    assert opened(f'github/vapor-banner-{theme}.png').size == (1800, 600)
    p = f'github/vapor-social-{theme}-1280x640.jpg'; assert opened(p).size == (1280, 640) and (root / p).stat().st_size < 1000000
    for kind, w, h in [('og', 1200, 630), ('card', 1200, 600)]:
        assert opened(f'web/public/social/vapor-{kind}-{theme}-{w}x{h}.jpg').size == (w, h)
readme = (root.parent / 'README.md').read_text()
for ref in re.findall(r'(?:src|srcset)="(assets/[^"]+)"', readme):
    assert (root.parent / ref).is_file(), ref
for icon in json.loads((root / 'web/public/site.webmanifest').read_text())['icons']:
    assert (root / 'web/public' / icon['src'].lstrip('/')).is_file()
for ref in re.findall(r'(?:href|content)="(?:https://vapor\.arn\.sh)?(/[^" ]+)"', (root / 'web/head.html').read_text()):
    if ref != '/': assert (root / 'web/public' / ref.lstrip('/')).is_file(), ref
report['web_references_valid'] = True
report['png_count'] = len([p for p in root.rglob('*.png') if 'node_modules' not in p.parts])
report['jpeg_count'] = len(list(root.rglob('*.jpg')))
json.dump(report, sys.stdout, indent=2); print()
