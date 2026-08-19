"""Paired total-CPU comparison across the sim's Stack x Allocator matrix.

    python bench_arms.py <scenario> <pack> <pairs>
    python bench_arms.py arrival pack/high192 20

Reports process CPU seconds, not frame percentiles. That distinction matters:
the cockpit's `cpu p50` is per-frame main-thread time, while decode and upload
staging run on workers, so a stack difference can be invisible at p50 and still
show up here.

Two things that will waste an afternoon if you do not know them:

1. **The allocator is a COMPILE-TIME choice.** A `+ra` arm must be run by the
   `sim-ra` binary and a plain arm by `sim`; pointing every arm at `sim.exe`
   just returns "arm `rusty+ra` wants the rusty_alloc allocator but this binary
   linked system". This script dispatches on the suffix.

2. **The sim never decodes.** Its whole rusty_dds surface is `DdsView::parse`,
   `SubresourceId::mip_layer` and `upload` - it hands compressed BCn straight to
   the GPU, which is what a streaming engine should do. So the `rusty` arm
   measures *parse and upload-plan* cost and cannot show a decode change at all.
   Use `probe_dec` for that.

Ten pairs is not enough. At n=10 this matrix reported a 9.5% stack regression
and a 22%-vs-8% allocator asymmetry; neither survived n=20. Use 20, and read the
z rather than the percentage.
"""
import subprocess, re, math, sys
CWD = 'C:/Users/talmo/coding/rusty_dds/sim'

def run(arm, scen, pack):
    exe = './target/release/sim-ra.exe' if arm.endswith('+ra') else './target/release/sim.exe'
    o = subprocess.run([exe, 'run', '--pack', pack, '--scenario', scen,
                        '--arm', arm, '--pin'],
                       capture_output=True, text=True, cwd=CWD).stdout
    m = re.search(r'cpu ([0-9.]+)s', o)
    if not m:
        sys.exit(f'arm {arm}: unparsed -> {o.strip()}')
    return float(m.group(1))

def pair(a_arm, b_arm, rounds, scen, pack, label):
    ps = []
    for r in range(rounds):
        # Alternate the leading arm so neither is systematically warmed.
        if r % 2 == 0: a = run(a_arm, scen, pack); b = run(b_arm, scen, pack)
        else:          b = run(b_arm, scen, pack); a = run(a_arm, scen, pack)
        ps.append((a, b))
    wins = sum(1 for a, b in ps if a < b); ties = sum(1 for a, b in ps if a == b)
    dec = len(ps) - ties
    z = (wins - dec / 2) / math.sqrt(dec / 4) if dec else 0.0
    mn = sum(a for a, _ in ps) / len(ps); mo = sum(b for _, b in ps) / len(ps)
    sig = '' if abs(z) > 1.96 else '   (n.s.)'
    print(f"  {label}: {mn:.4f}s vs {mo:.4f}s  {wins}/{len(ps)} (ties {ties})  z={z:+.2f}  {100*(mo-mn)/mo:+.1f}%{sig}")

if __name__ == '__main__':
    scen, pack, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
    print(f"[{scen} / {pack}]")
    print("  -- DDS stack effect, allocator held fixed --")
    pair('rusty',    'dxtex',    n, scen, pack, 'rusty_dds vs DirectXTex  @ system   ')
    pair('rusty+ra', 'dxtex+ra', n, scen, pack, 'rusty_dds vs DirectXTex  @ rusty_alloc')
    print("  -- allocator effect, DDS stack held fixed --")
    pair('rusty+ra', 'rusty',    n, scen, pack, 'rusty_alloc vs system    @ rusty_dds ')
    pair('dxtex+ra', 'dxtex',    n, scen, pack, 'rusty_alloc vs system    @ DirectXTex')
