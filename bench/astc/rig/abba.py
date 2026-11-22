import sys, statistics as st, math
# Part of the ASTC copies-round rig (bench/astc/rig): WSL, /root/astcws layout.
out, R = sys.argv[1], int(sys.argv[2])
def rows(p):
    d = {}
    for l in open(p):
        f = l.split()
        if len(f) < 6: continue
        # decode: name fp K min med mts hash ; encode: preset fp min med mts hash
        if f[2].isdigit() and len(f) == 7:
            key, mn, md, h = f"{f[0]} {f[1]}", float(f[3]), float(f[4]), f[6]
        else:
            key, mn, md, h = f"{f[0]} {f[1]}", float(f[2]), float(f[3]), f[5]
        d[key] = (mn, md, h)
    return d
A = [rows(f"{out}/r{r}_A.txt") for r in range(R)]
B = [rows(f"{out}/r{r}_B.txt") for r in range(R)]
print(f"method: {R} rounds, whole processes ABBA-alternated, taskset -c 2 nice -5, thread CPU clock; per run median of samples; ratio = A ms / B ms (>1: B faster)")
print(f"{'row':34s} {'A med ms':>10s} {'B med ms':>10s} {'ratio med':>9s} {'min ratio':>9s} {'B wins':>7s} {'z':>6s}  hash")
for k in A[0]:
    a = [x[k][1] for x in A]; b = [x[k][1] for x in B]
    ratios = [x / y for x, y in zip(a, b)]
    wins = sum(1 for x in ratios if x > 1)
    z = (wins - R / 2) / (0.5 * math.sqrt(R))
    amin = min(x[k][0] for x in A); bmin = min(x[k][0] for x in B)
    hs = {x[k][2] for x in A} | {x[k][2] for x in B}
    print(f"{k:34s} {st.median(a):10.3f} {st.median(b):10.3f} {st.median(ratios):9.4f} {amin/bmin:9.4f} {wins:>3d}/{R:<3d} {z:6.2f}  {'equal' if len(hs)==1 else 'DIFFER'}")
