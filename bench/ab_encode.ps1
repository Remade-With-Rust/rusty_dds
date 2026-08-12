# ABBA-interleaved encode A/B: two bench_encode_corpus binaries, same core mask.
# Usage: powershell -File bench/ab_encode.ps1 -A target/bench_baseline.exe -B target/release/examples/bench_encode_corpus.exe -Pairs 10 -Filter __bc7
param(
    [string]$A = "target/bench_baseline.exe",
    [string]$B = "target/release/examples/bench_encode_corpus.exe",
    [int]$Pairs = 10,
    [string]$Filter = "",
    [int]$Iters = 3,
    # 4-core mask (cores 2-5), avoid core 0 (interrupts). Same mask both arms.
    [long]$Mask = 60
)

$ErrorActionPreference = "Stop"

function Run-Arm([string]$exe, [string]$tag, [int]$round) {
    $json = "target/ab_$tag.json"
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = (Resolve-Path $exe).Path
    $psi.Arguments = "--json $json"
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["RUSTY_DDS_ITERS"] = "$Iters"
    if ($Filter -ne "") { $psi.EnvironmentVariables["RUSTY_DDS_FILTER"] = $Filter }
    $p = [System.Diagnostics.Process]::Start($psi)
    $null = $p.Handle
    try { $p.ProcessorAffinity = [IntPtr]$Mask } catch {}
    try { $p.PriorityClass = "High" } catch {}
    $out = $p.StandardOutput.ReadToEnd()
    $err = $p.StandardError.ReadToEnd()
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "arm $tag exit $($p.ExitCode): $err" }
    $cpu = $p.TotalProcessorTime.TotalMilliseconds
    $j = Get-Content $json | ConvertFrom-Json
    return @{ total_ns = [double]$j.total_best_ns; cpu_ms = $cpu }
}

$resA = @(); $resB = @()
for ($i = 0; $i -lt $Pairs; $i++) {
    if ($i % 2 -eq 0) {
        $ra = Run-Arm $A "A" $i; $rb = Run-Arm $B "B" $i
    } else {
        $rb = Run-Arm $B "B" $i; $ra = Run-Arm $A "A" $i
    }
    $resA += $ra; $resB += $rb
    $ratio = $ra.total_ns / $rb.total_ns
    "pair $($i+1): A=$([math]::Round($ra.total_ns/1e6,1))ms B=$([math]::Round($rb.total_ns/1e6,1))ms  A/B=$([math]::Round($ratio,4))  cpuA=$([math]::Round($ra.cpu_ms))ms cpuB=$([math]::Round($rb.cpu_ms))ms"
}

$ratios = for ($i = 0; $i -lt $Pairs; $i++) { $resA[$i].total_ns / $resB[$i].total_ns }
$sorted = $ratios | Sort-Object
$median = $sorted[[int]($Pairs / 2)]
$wins = ($ratios | Where-Object { $_ -gt 1.0 }).Count
$z = ($wins - $Pairs / 2.0) / (0.5 * [math]::Sqrt($Pairs))
$minA = ($resA | ForEach-Object { $_.total_ns } | Measure-Object -Minimum).Minimum
$minB = ($resB | ForEach-Object { $_.total_ns } | Measure-Object -Minimum).Minimum
""
"method: ABBA-interleaved, mask=$Mask, High priority, iters=$Iters/case, filter='$Filter', pairs=$Pairs"
"median A/B ratio = $([math]::Round($median,4))  (>1 means B faster)"
"B wins $wins/$Pairs, z=$([math]::Round($z,2))"
"min-of-N: A=$([math]::Round($minA/1e6,1))ms  B=$([math]::Round($minB/1e6,1))ms  A/B=$([math]::Round($minA/$minB,4))"
