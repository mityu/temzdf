# A one-page PDF with many vector paths (strokes and fills), no text or images.
import random, sys
random.seed(1)
W, H = 595, 842
ops = ["0.3 w"]
for _ in range(5000):
    p = [random.uniform(0, W) if i % 2 == 0 else random.uniform(0, H) for i in range(8)]
    ops.append("%.2f %.2f %.2f RG %.1f %.1f m %.1f %.1f %.1f %.1f %.1f %.1f c S" % (random.random(), random.random(), random.random(), *p))
for _ in range(3000):
    x, y, r = random.uniform(0, W), random.uniform(0, H), random.uniform(1, 12)
    k = 0.5523 * r
    ops.append("%.2f %.2f %.2f rg %.1f %.1f m %.1f %.1f %.1f %.1f %.1f %.1f c %.1f %.1f %.1f %.1f %.1f %.1f c %.1f %.1f %.1f %.1f %.1f %.1f c %.1f %.1f %.1f %.1f %.1f %.1f c f" % (
        random.random(), random.random(), random.random(), x + r, y,
        x + r, y + k, x + k, y + r, x, y + r,
        x - k, y + r, x - r, y + k, x - r, y,
        x - r, y - k, x - k, y - r, x, y - r,
        x + k, y - r, x + r, y - k, x + r, y))
content = "\n".join(ops).encode()
objs = [
    b"<< /Type /Catalog /Pages 2 0 R >>",
    b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %d %d] /Contents 4 0 R >>" % (W, H),
    b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
]
out = bytearray(b"%PDF-1.4\n")
offsets = []
for i, o in enumerate(objs):
    offsets.append(len(out))
    out += b"%d 0 obj\n" % (i + 1) + o + b"\nendobj\n"
xref = len(out)
out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
for off in offsets:
    out += b"%010d 00000 n \n" % off
out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
open(sys.argv[1], "wb").write(out)
