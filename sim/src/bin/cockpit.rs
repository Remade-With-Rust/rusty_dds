//! The cockpit — the launcher for the side-by-side demo.
//!
//! One job: choose what each pane runs, launch them, and watch their telemetry
//! side by side. The headless single-run console that used to live here is gone;
//! that path is `sim run` / `sim bench` on the command line, which is where
//! reportable numbers come from anyway.
//!
//! # What this window is, and what it is not
//!
//! Dioxus desktop on Windows is a WebView2 surface. It renders these controls;
//! it does not own a D3D11 or Vulkan swapchain. Each pane is a **separate
//! process** with its own device, its own swapchain and its own streaming pool —
//! necessarily so, because `#[global_allocator]` is a compile-time choice and an
//! arm on `rusty_alloc` cannot share a process with one on the system heap.
//!
//! Four panes contend for cores and one GPU. The *picture* is comparable — every
//! pane replays the identical deterministic trace and shows the identical frame
//! — but the numbers here are indicative. `sim bench` runs one arm at a time,
//! pinned, with nothing else in the process; that is the measurement.

use std::path::PathBuf;
use std::time::Duration;

use dioxus::prelude::*;
use rusty_dds_sim::metrics;
use rusty_dds_sim::panes::{launch, GridOptions, PaneGrid, PaneSpec, PaneState};

#[cfg(feature = "alloc-counters")]
#[global_allocator]
static ALLOC: metrics::CountingAlloc<std::alloc::System> =
    metrics::CountingAlloc(std::alloc::System);

fn main() {
    metrics::set_allocator_name("system");
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            dioxus::desktop::Config::new().with_window(
                dioxus::desktop::WindowBuilder::new()
                    .with_title("rusty_dds simulator")
                    .with_inner_size(dioxus::desktop::LogicalSize::new(1120.0, 820.0)),
            ),
        )
        .launch(App);
}

// -------------------------------------------------------------------- panes

/// One pane's configuration, as the cockpit's checkboxes express it.
///
/// The two technologies are independent switches rather than a preset list,
/// because that is the comparison that answers the question: turning them on one
/// at a time is what separates "rusty_dds helped" from "rusty_alloc helped" from
/// "they only help together".
#[derive(Clone, PartialEq)]
struct PaneSel {
    enabled: bool,
    api: String,
    rusty_dds: bool,
    rusty_alloc: bool,
}

impl PaneSel {
    /// The arm label the harness understands. `dxtex` is the baseline a
    /// conventional PC engine ships; `+ra` swaps the global allocator.
    fn arm(&self) -> String {
        let base = if self.rusty_dds { "rusty" } else { "dxtex" };
        if self.rusty_alloc {
            format!("{base}+ra")
        } else {
            base.to_string()
        }
    }

    fn api_label(&self) -> &'static str {
        if self.api == "vulkan" {
            "Vulkan"
        } else {
            "D3D11"
        }
    }

    fn what(&self) -> &'static str {
        match (self.rusty_dds, self.rusty_alloc) {
            (false, false) => "baseline",
            (true, false) => "rusty_dds",
            (false, true) => "rusty_alloc",
            (true, true) => "both",
        }
    }

    fn label(&self) -> String {
        format!("{} - {}", self.api_label(), self.what())
    }

    fn to_spec(&self) -> PaneSpec {
        PaneSpec {
            label: self.label(),
            api: self.api.clone(),
            arm: self.arm(),
        }
    }
}

fn sel(api: &str, dds: bool, alloc: bool) -> PaneSel {
    PaneSel {
        enabled: true,
        api: api.to_string(),
        rusty_dds: dds,
        rusty_alloc: alloc,
    }
}

/// The 2x2 factorial on one API: nothing, each technology alone, both.
fn preset_isolate(api: &str) -> Vec<PaneSel> {
    vec![
        sel(api, false, false),
        sel(api, true, false),
        sel(api, false, true),
        sel(api, true, true),
    ]
}

/// Baseline vs both, on each API — the "does the API change the answer" view.
fn preset_stack_x_api() -> Vec<PaneSel> {
    vec![
        sel("d3d11", false, false),
        sel("d3d11", true, true),
        sel("vulkan", false, false),
        sel("vulkan", true, true),
    ]
}

fn default_selection() -> Vec<PaneSel> {
    preset_isolate("d3d11")
}

// ------------------------------------------------------------------- config

/// Settings shared by every pane. Supplied on the command line so the window
/// stays a launcher rather than a form:
///
/// `cockpit [--pack DIR] [--scenario NAME] [--workers N] [--frames N] [--peer P]`
#[derive(Clone, PartialEq)]
struct Shared {
    pack: String,
    scenario: String,
    workers: usize,
    frames: Option<u32>,
    peer: String,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            pack: "pack/high192".into(),
            scenario: "traverse".into(),
            workers: 4,
            frames: Some(3600),
            peer: "loader".into(),
        }
    }
}

fn shared_from_args() -> Shared {
    let mut s = Shared::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let val = argv.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
        match argv[i].as_str() {
            "--pack" => {
                if let Some(v) = val {
                    s.pack = v;
                    i += 1;
                }
            }
            "--scenario" => {
                if let Some(v) = val {
                    s.scenario = v;
                    i += 1;
                }
            }
            "--peer" => {
                if let Some(v) = val {
                    s.peer = v;
                    i += 1;
                }
            }
            "--workers" => {
                if let Some(v) = val.and_then(|v| v.parse().ok()) {
                    s.workers = v;
                    i += 1;
                }
            }
            "--frames" => {
                if let Some(v) = val.and_then(|v| v.parse().ok()) {
                    s.frames = Some(v);
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s
}

// ---------------------------------------------------------------------- app

#[component]
fn App() -> Element {
    let shared = use_hook(shared_from_args);
    let mut sels = use_signal(default_selection);
    let mut grid = use_signal(|| None::<PaneGrid>);
    let mut panes = use_signal(Vec::<PaneState>::new);
    let mut notice = use_signal(String::new);

    // Poll the panes' telemetry. 20 Hz is far below their publish interval, so
    // the UI never spins on a lock a pane is holding.
    use_future(move || async move {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Read and release before touching any signal the branch below
            // writes, or the guard would still be alive when `grid` is set.
            let finished = {
                let g = grid.read();
                match g.as_ref() {
                    Some(g) => {
                        let snap = g.snapshot();
                        let done = !snap.is_empty() && snap.iter().all(|p| p.exited);
                        panes.set(snap);
                        done
                    }
                    None => false,
                }
            };

            if finished {
                // Drop the handle so the controls come back, and put the pane
                // selection back to the default factorial. The results table is
                // deliberately left standing — it is the thing you just ran, and
                // it stays until the next launch replaces it.
                grid.set(None);
                sels.set(default_selection());
                notice.set("panes finished - selection reset".into());
            }
        }
    });

    let running = grid.read().is_some();
    let launch_shared = shared.clone();

    let launch_grid = move |_| {
        let specs: Vec<PaneSpec> = sels
            .read()
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.to_spec())
            .collect();
        if specs.is_empty() {
            notice.set("enable at least one pane".into());
            return;
        }
        let opts = GridOptions {
            pack: PathBuf::from(&launch_shared.pack),
            scenario: launch_shared.scenario.clone(),
            frames: launch_shared.frames,
            workers: launch_shared.workers,
            peer: launch_shared.peer.clone(),
            ..Default::default()
        };
        match launch(&specs, &opts) {
            Ok(g) => {
                // The previous run's table is replaced here, not when it ended.
                panes.set(Vec::new());
                grid.set(Some(g));
                notice.set(format!("launched {} panes", specs.len()));
            }
            Err(e) => notice.set(e.0),
        }
    };

    let stop_grid = move |_| {
        grid.set(None);
        sels.set(default_selection());
        notice.set("panes stopped - selection reset".into());
    };

    let frames_badge = match shared.frames {
        Some(f) => format!("{f} frames"),
        None => "scenario default".to_string(),
    };

    rsx! {
        style { dangerous_inner_html: CSS }
        div { class: "app",
            header {
                div {
                    h1 { "rusty_dds simulator" }
                    p { class: "sub",
                        "rusty_dds and rusty_alloc against Microsoft DirectXTex on the system "
                        "heap, across off-the-shelf Direct3D 11 and Vulkan. Each pane runs in "
                        "its own process."
                    }
                }
                div { class: "badges",
                    span { class: if running { "badge live" } else { "badge" },
                        if running { "panes running" } else { "idle" }
                    }
                    span { class: "badge", "{shared.pack}" }
                    span { class: "badge", "{shared.scenario}" }
                    span { class: "badge", "{frames_badge}" }
                    span { class: "badge", "{shared.workers} workers" }
                }
            }

            div { class: "grid",
                section { class: "panel",
                    h2 { "Panes" }
                    div { class: "presets",
                        button {
                            disabled: running,
                            onclick: move |_| sels.set(preset_isolate("d3d11")),
                            "Isolate D3D11"
                        }
                        button {
                            disabled: running,
                            onclick: move |_| sels.set(preset_isolate("vulkan")),
                            "Isolate Vulkan"
                        }
                        button {
                            disabled: running,
                            onclick: move |_| sels.set(preset_stack_x_api()),
                            "Stack x API"
                        }
                    }

                    for i in 0..sels.read().len() {
                        div { class: if sels.read()[i].enabled { "panesel" } else { "panesel off" },
                            div { class: "paneselhead",
                                input {
                                    r#type: "checkbox",
                                    checked: sels.read()[i].enabled,
                                    disabled: running,
                                    oninput: move |e| sels.write()[i].enabled = e.checked(),
                                }
                                span { class: "panesellabel", "{sels.read()[i].label()}" }
                                code { "{sels.read()[i].arm()}" }
                            }
                            select {
                                value: "{sels.read()[i].api}",
                                disabled: running,
                                oninput: move |e| sels.write()[i].api = e.value(),
                                option { value: "d3d11", "DirectX 11" }
                                option { value: "vulkan", "Vulkan" }
                            }
                            div { class: "toggles",
                                label { class: "row",
                                    input {
                                        r#type: "checkbox",
                                        checked: sels.read()[i].rusty_dds,
                                        disabled: running,
                                        oninput: move |e| sels.write()[i].rusty_dds = e.checked(),
                                    }
                                    span { "rusty_dds" }
                                }
                                label { class: "row",
                                    input {
                                        r#type: "checkbox",
                                        checked: sels.read()[i].rusty_alloc,
                                        disabled: running,
                                        oninput: move |e| sels.write()[i].rusty_alloc = e.checked(),
                                    }
                                    span { "rusty_alloc" }
                                }
                            }
                        }
                    }

                    div { class: "buttons",
                        button {
                            class: "primary",
                            disabled: running,
                            onclick: launch_grid,
                            "Launch panes"
                        }
                        button { disabled: !running, onclick: stop_grid, "Stop" }
                    }
                    if !notice.read().is_empty() {
                        p { class: "notice", "{notice}" }
                    }
                    p { class: "hint",
                        "Off means the conventional PC stack: Microsoft DirectXTex on the system "
                        "heap. Turning the two on one at a time is what separates a rusty_dds "
                        "result from a rusty_alloc one."
                    }
                }

                section { class: "panel",
                    h2 { "Live panes" }
                    if panes.read().is_empty() {
                        p { class: "empty",
                            "Nothing running. Pick a preset, or set each pane's switches, then "
                            "Launch."
                        }
                    } else {
                        table { class: "panes",
                            thead {
                                tr {
                                    th { "pane" }
                                    th { "API" }
                                    th { "DDS stack" }
                                    th { "allocator" }
                                    th { "frame" }
                                    th { "cpu p50" }
                                    th { "cpu p99" }
                                    th { "gpu p50" }
                                    th { "hitches" }
                                    th { "state" }
                                }
                            }
                            tbody {
                                for p in panes.read().iter() {
                                    tr {
                                        td { "{p.label}" }
                                        td { "{p.api}" }
                                        td { "{p.stack()}" }
                                        td { "{p.allocator}" }
                                        td { "{p.get(\"frame\")} / {p.get(\"frames\")}" }
                                        td { "{p.get(\"p50\")}" }
                                        td { "{p.get(\"p99\")}" }
                                        td { "{p.get(\"gpu_p50\")}" }
                                        td { "{p.get(\"hitches\")}" }
                                        td { class: if p.exited { "done" } else { "live" },
                                            if p.exited { "finished" } else { "running" }
                                        }
                                    }
                                }
                            }
                        }
                        for p in panes.read().iter() {
                            if let Some(e) = &p.error {
                                p { class: "paneerr", "{p.label}: {e}" }
                            }
                        }
                    }
                    p { class: "hint",
                        "Every figure is run-wide: each percentile is taken over all of that "
                        "pane's frames since warmup, not the newest one. Panes contend for cores "
                        "and one GPU, so these figures are indicative. "
                        "The comparable part is the picture — every pane replays the identical "
                        "trace and should show the identical frame. Run `sim bench` for anything "
                        "reportable."
                    }
                    if !panes.read().is_empty() && !running {
                        p { class: "hint",
                            "This run has finished. The table stays until the next launch."
                        }
                    }
                }
            }

            footer {
                "Shared settings come from the command line: "
                em { "cockpit --pack DIR --scenario NAME --workers N --frames N" }
                ". Closing this window stops every pane."
            }
        }
    }
}

const CSS: &str = r#"
* { box-sizing: border-box; }
body { margin: 0; background: #0d1117; color: #c9d1d9; overflow-x: hidden;
  font: 13px/1.5 "Segoe UI", system-ui, sans-serif; }
.app { padding: 18px 22px 40px; max-width: 1500px; margin: 0 auto; }
header { display: flex; justify-content: space-between; align-items: flex-start;
  gap: 20px; border-bottom: 1px solid #21262d; padding-bottom: 14px; margin-bottom: 18px; }
h1 { margin: 0; font-size: 19px; letter-spacing: -0.2px; }
h2 { margin: 0 0 10px; font-size: 12px; text-transform: uppercase;
  letter-spacing: 0.9px; color: #7d8590; font-weight: 600; }
.sub { margin: 4px 0 0; color: #7d8590; max-width: 64ch; }
.badges { display: flex; gap: 6px; flex-wrap: wrap; justify-content: flex-end; }
.badge { background: #161b22; border: 1px solid #30363d; border-radius: 999px;
  padding: 3px 10px; font-size: 11px; color: #8b949e; white-space: nowrap;
  font-family: ui-monospace, Consolas, monospace; }
.badge.live { background: #0f2e1d; border-color: #238636; color: #6ee7b7; }
.grid { display: grid; grid-template-columns: 330px minmax(0, 1fr); gap: 18px;
  align-items: start; }
@media (max-width: 900px) { .grid { grid-template-columns: minmax(0, 1fr); } }
.panel { background: #0f141a; border: 1px solid #21262d; border-radius: 10px;
  padding: 16px; min-width: 0; }
.presets { display: flex; gap: 6px; margin-bottom: 10px; flex-wrap: wrap; }
.presets button { flex: 1 1 auto; font-size: 11px; padding: 5px 8px; }
.panesel { border: 1px solid #21262d; border-radius: 8px; padding: 9px 10px;
  margin-bottom: 7px; background: #0d1117; }
.panesel.off { opacity: 0.45; }
.paneselhead { display: flex; align-items: center; gap: 7px; margin-bottom: 7px; }
.paneselhead input { width: auto; margin: 0; }
.panesellabel { flex: 1; font-size: 12px; color: #e6edf3; }
.paneselhead code { font-family: ui-monospace, Consolas, monospace; font-size: 10px;
  color: #6e7681; background: #161b22; border-radius: 4px; padding: 2px 5px; }
select { width: 100%; background: #0d1117; color: #c9d1d9; border: 1px solid #30363d;
  border-radius: 6px; padding: 6px 8px; font-size: 12px;
  font-family: ui-monospace, Consolas, monospace; }
select:disabled, input:disabled { opacity: 0.5; }
.toggles { display: flex; gap: 14px; margin-top: 7px; }
label.row { display: flex; align-items: center; gap: 7px; margin: 0; font-size: 12px; }
label.row input { width: auto; margin: 0; }
.buttons { display: flex; gap: 8px; margin-top: 12px; }
button { flex: 1; background: #21262d; color: #c9d1d9; border: 1px solid #30363d;
  border-radius: 6px; padding: 7px 10px; font-size: 12px; cursor: pointer; }
button:hover:not(:disabled) { background: #30363d; }
button:disabled { opacity: 0.4; cursor: default; }
button.primary { background: #238636; border-color: #2ea043; color: #fff; }
button.primary:hover:not(:disabled) { background: #2ea043; }
.hint { color: #6e7681; font-size: 11px; margin: 10px 0 0; max-width: 74ch; }
.notice { color: #d29922; font-size: 11px; margin-top: 10px; }
.empty { color: #6e7681; font-size: 12px; margin: 0; }
.paneerr { color: #f85149; font-size: 11px; margin: 6px 0 0;
  font-family: ui-monospace, Consolas, monospace; }
table.panes { width: 100%; border-collapse: collapse; font-size: 11.5px;
  font-family: ui-monospace, Consolas, monospace; }
table.panes th { text-align: left; color: #6e7681; font-weight: 600; font-size: 10px;
  text-transform: uppercase; letter-spacing: 0.5px; padding: 6px 8px;
  border-bottom: 1px solid #21262d; white-space: nowrap; }
table.panes td { padding: 6px 8px; border-bottom: 1px solid #161b22; color: #c9d1d9;
  white-space: nowrap; }
table.panes td.live { color: #6ee7b7; }
table.panes td.done { color: #6e7681; }
footer { margin-top: 22px; padding-top: 14px; border-top: 1px solid #21262d;
  color: #6e7681; font-size: 11px; max-width: 96ch; }
footer em { color: #8b949e; font-style: normal;
  font-family: ui-monospace, Consolas, monospace; }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The checkbox pair must map onto exactly the four arms the harness knows,
    /// because a wrong mapping would silently compare the wrong things.
    #[test]
    fn toggles_map_onto_the_four_arms() {
        assert_eq!(sel("d3d11", false, false).arm(), "dxtex");
        assert_eq!(sel("d3d11", true, false).arm(), "rusty");
        assert_eq!(sel("d3d11", false, true).arm(), "dxtex+ra");
        assert_eq!(sel("d3d11", true, true).arm(), "rusty+ra");
    }

    #[test]
    fn every_arm_the_ui_can_produce_is_one_the_harness_accepts() {
        for api in ["d3d11", "vulkan"] {
            for dds in [false, true] {
                for alloc in [false, true] {
                    let s = sel(api, dds, alloc);
                    let parsed = rusty_dds_sim::provider::parse_arm(&s.arm())
                        .unwrap_or_else(|e| panic!("{}: {e}", s.arm()));
                    assert_eq!(parsed.wants_rusty_alloc, alloc);
                }
            }
        }
    }

    #[test]
    fn presets_cover_the_factorial() {
        let arms: Vec<String> = preset_isolate("d3d11").iter().map(|s| s.arm()).collect();
        assert_eq!(arms, ["dxtex", "rusty", "dxtex+ra", "rusty+ra"]);
        let p = preset_stack_x_api();
        assert_eq!(p.len(), 4);
        assert_eq!(p.iter().filter(|s| s.api == "vulkan").count(), 2);
    }

    /// The defaults must be launchable without a single flag.
    #[test]
    fn shared_defaults_are_runnable() {
        let s = Shared::default();
        assert!(!s.pack.is_empty());
        assert!(rusty_dds_sim::scenario::scenario_by_name(&s.scenario).is_some());
        assert!(s.workers > 0);
    }
}
