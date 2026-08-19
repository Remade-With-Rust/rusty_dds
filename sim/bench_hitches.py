"""Paired hitch-count comparison for a live GPU pane (P0 of rusty_alloc's
`vulkan_hitch_v1.md`).

    python bench_hitches.py <api> <armA> <armB> <pairs> [frames]
    python bench_hitches.py vulkan dxtex dxtex 20        # NULL ARM
    python bench_hitches.py vulkan dxtex+ra dxtex 20

Drives `sim view` and reads `hitches=` from the final TELEM line. Unlike
`bench_arms.py` (which uses headless `sim run` and reports process CPU), this
needs a real GPU pane, because a hitch is a wall-clock tail event in a
vsync-paced present loop.

Three things this harness will get wrong if you let it:

1. **Do NOT pass --turbo.** It uncaps vsync, and the hitch counter is defined
   against the frame budget: a 1200-frame turbo run reports ~900 hitches. Every
   run here is vsync-paced, which is why each costs ~20 s.
2. **The allocator is a COMPILE-TIME choice.** `+ra` arms need the `sim-ra`
   binary; pointing them at `sim.exe` returns "wants the rusty_alloc allocator
   but this binary linked system".
3. **Run the null arm first.** Same arm against itself. Measured 2026-08-19 on
   Vulkan: mean diff -0.90 but sd 8.97, single-pair range [-19, +14]. Identical
   code swings that far, so ANY one-run-per-arm observation below ~19 hitches is
   worthless. At n=20 the SE is ~2.0, so the harness CAN resolve a real effect;
   low n is the usual problem, not the instrument.

4. **`sim-ra` needs the GPU features to have a `view` command at all.** Build it
   with `--features "rusty-alloc,dxtex,d3d11,vulkan"`, or `view` exits with
   "unknown command".

5. **`--frames` must exceed the scenario warm-up (300)**, so 600 is the floor.
   It is accepted via `sim_config` but missing from `view`'s help; `--scenario`
   takes a name, not a count.

6. **Cross-binary comparisons are ~2x noisier.** The allocator is compile-time,
   so every allocator A/B is `sim` vs `sim-ra`: sd 16.30 against the same-binary
   null arm's 8.97. Budget pairs accordingly.

Hitches do NOT scale with frame count -- 1200 frames gives ~45, and 3600 gives
~48, because they are dominated by warm-up streaming transients. Short runs cost
nothing in resolution.
"""
import subprocess, re, sys, math, statistics

CWD = 'C:/Users/talmo/coding/rusty_dds/sim'


def run(api, arm, frames, slot):
    exe = './target/release/sim-ra.exe' if arm.endswith('+ra') else './target/release/sim.exe'
    # Offset the two slots so neither pane ever lands on the other's pixels.
    x = 40 + 700 * slot
    p = subprocess.run(
        [exe, 'view', '--pack', 'pack/high192', '--api', api, '--arm', arm,
         '--frames', str(frames), '--pin', '--width', '640', '--height', '360',
         '--x', str(x), '--y', '40'],
        capture_output=True, text=True, cwd=CWD)
    hits = re.findall(r'hitches=(\d+)', p.stdout)
    if not hits:
        sys.exit(f'{api}/{arm}: no TELEM -> {p.stdout[-400:]}{p.stderr[-400:]}')
    return int(hits[-1])


def main():
    api, a_arm, b_arm, pairs = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
    frames = int(sys.argv[5]) if len(sys.argv) > 5 else 1200
    null = a_arm == b_arm
    ps = []
    for r in range(pairs):
        # Alternate the leading arm so neither is systematically warmed.
        if r % 2 == 0:
            a = run(api, a_arm, frames, 0); b = run(api, b_arm, frames, 1)
        else:
            b = run(api, b_arm, frames, 1); a = run(api, a_arm, frames, 0)
        ps.append((a, b))
        print(f'  pair {r + 1:2d}/{pairs}: {a_arm}={a:4d}  {b_arm}={b:4d}  d={a - b:+4d}',
              flush=True)

    da = [a for a, _ in ps]
    db = [b for _, b in ps]
    diffs = [a - b for a, b in ps]
    wins = sum(1 for d in diffs if d < 0)
    ties = sum(1 for d in diffs if d == 0)
    dec = len(ps) - ties
    z = (wins - dec / 2) / math.sqrt(dec / 4) if dec else 0.0
    ma, mb = statistics.mean(da), statistics.mean(db)
    sd = statistics.pstdev(diffs)
    tag = '  [NULL ARM]' if null else ''
    print(f'\n{api}: {a_arm} {ma:.1f} hitches vs {b_arm} {mb:.1f}{tag}')
    print(f'  paired diff mean {statistics.mean(diffs):+.2f}, sd {sd:.2f}, '
          f'range [{min(diffs):+d}, {max(diffs):+d}]')
    print(f'  {a_arm} lower in {wins}/{len(ps)} (ties {ties})  z={z:+.2f}  '
          f'{"SIGNIFICANT" if abs(z) > 1.96 else "not significant"}')
    se = sd / math.sqrt(len(ps)) if ps else float('nan')
    print(f'  paired SE of the mean {se:.2f}; a real effect needs ~{2 * se:.1f} hitches to reach 2 sigma')
    if null:
        print(f'  -> NULL ARM: single-pair swings reach [{min(diffs):+d}, {max(diffs):+d}], so ANY '
              f'one-run-per-arm observation smaller than that is worthless.')
        print(f'  -> But at n={len(ps)} the SE is {se:.2f}, so the harness CAN resolve an effect '
              f'of ~{2 * se:.1f}+ hitches. Low n was the problem, not the instrument.')


if __name__ == '__main__':
    main()
