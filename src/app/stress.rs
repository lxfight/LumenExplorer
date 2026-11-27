//! Opt-in workload test. Reports and fixtures stay under target/stress-runs.
use super::*;
use crate::shell::icons::IconKey;
use crate::shell::op_worker::{OpRequest, OpResult, OpWorker};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources, GetProcessHandleCount};

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[derive(Serialize)]
struct Sample {
    seconds: f64,
    phase: String,
    private_mb: f64,
    working_set_mb: f64,
    handles: u32,
    gdi_objects: u32,
    ui_state_entries: Option<usize>,
}

fn sample(start: Instant, phase: &str, ctx: Option<&egui::Context>) -> Sample {
    let mut memory = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let mut handles = 0;
    unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), memory.cb)
            .unwrap();
        GetProcessHandleCount(GetCurrentProcess(), &mut handles).unwrap();
    }
    Sample {
        seconds: start.elapsed().as_secs_f64(),
        phase: phase.into(),
        private_mb: memory.PrivateUsage as f64 / 1048576.0,
        working_set_mb: memory.WorkingSetSize as f64 / 1048576.0,
        handles,
        gdi_objects: unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) },
        ui_state_entries: ctx.map(|ctx| ctx.data(|d| d.len())),
    }
}

#[derive(Default, Serialize)]
struct Report {
    run_dir: String,
    pid: u32,
    fixture_files: usize,
    listing_ms: Vec<f64>,
    search_ms: Vec<f64>,
    cancelled_searches: usize,
    file_operations: usize,
    navigations: usize,
    icon_requests: usize,
    frames: usize,
    frame_p50_ms: f64,
    frame_p95_ms: f64,
    frame_max_ms: f64,
    maximum_history: usize,
    idle_icons_busy: bool,
    samples: Vec<Sample>,
    elapsed_seconds: f64,
    failures: Vec<String>,
}

fn save_report(dir: &Path, report: &Report) {
    fs::write(dir.join("report.json"), serde_json::to_vec_pretty(report).unwrap()).unwrap();
}

fn render(app: &mut LumenApp, ctx: &egui::Context) -> f64 {
    // Exercise production UI/state and tessellation without changing user settings.
    app.last_save = f64::INFINITY;
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0))),
        time: Some(now()),
        ..Default::default()
    };
    let start = Instant::now();
    let mut output = ctx.run_pass(input, |ctx| {
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::unique("stress-root"),
            egui::UiBuilder::new().max_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0))),
        );
        app.frame(&mut ui);
    });
    let _ = ctx.tessellate(output.shapes, output.pixels_per_point);
    output.textures_delta.clear();
    start.elapsed().as_secs_f64() * 1000.0
}

fn wait_for(app: &mut LumenApp, ctx: &egui::Context, predicate: impl Fn(&LumenApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate(app) {
        app.poll_background(ctx);
        assert!(Instant::now() < deadline, "background work timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn operation(worker: &OpWorker, req: OpRequest, report: &mut Report) -> OpResult {
    worker.submit(req);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(result) = worker.poll().into_iter().next() {
            report.file_operations += 1;
            return result.result.expect("background file operation");
        }
        assert!(Instant::now() < deadline, "file operation timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "creates 30000 isolated fixtures and runs a 5-minute workload; run explicitly"]
fn stress_soak() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().unwrap();
    let _com = Com;
    let start = Instant::now();
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("stress-runs").join(format!("{stamp}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("stress-runs").join("latest.txt"),
        dir.to_string_lossy().as_bytes(),
    )
    .unwrap();
    let mut report =
        Report { run_dir: dir.to_string_lossy().into_owned(), pid: std::process::id(), ..Default::default() };
    report.samples.push(sample(start, "before-fixtures", None));
    save_report(&dir, &report);
    println!("STRESS pid={} report={}", report.pid, dir.display());

    let flat = dir.join("large-folder");
    let tree = dir.join("tree");
    fs::create_dir_all(&flat).unwrap();
    fs::create_dir_all(&tree).unwrap();
    let png_path = dir.join("template.png");
    image::RgbaImage::from_fn(128, 128, |x, y| image::Rgba([x as u8, y as u8, 128, 255])).save(&png_path).unwrap();
    let png = fs::read(png_path).unwrap();
    for index in 0..20_000 {
        let ext = if index % 2 == 0 { "png" } else { "txt" };
        fs::write(
            flat.join(format!("item-{index:05}.{ext}")),
            if ext == "png" { png.as_slice() } else { b"stress file contents" },
        )
        .unwrap();
        report.fixture_files += 1;
    }
    let mut folders = vec![flat.clone()];
    for index in 0..100 {
        let folder = tree.join(format!("folder-{index:03}"));
        fs::create_dir_all(&folder).unwrap();
        for item in 0..100 {
            fs::write(folder.join(format!("nested-{item:03}.txt")), b"nested fixture").unwrap();
            report.fixture_files += 1;
        }
        folders.push(folder);
    }
    println!("STRESS fixtures={} elapsed={:.1}s", report.fixture_files, start.elapsed().as_secs_f64());
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    let settings = Settings { tabs: vec![flat.to_string_lossy().into_owned()], ..Default::default() };
    let mut app = LumenApp::with_context(&ctx, settings);
    app.last_save = f64::INFINITY;
    for _ in 0..8 {
        let phase = Instant::now();
        app.reload_active();
        wait_for(&mut app, &ctx, |app| !app.active().loading);
        assert_eq!(app.active().entries.len(), 20_000);
        report.listing_ms.push(phase.elapsed().as_secs_f64() * 1000.0);
    }
    report.samples.push(sample(start, "after-large-listings", Some(&ctx)));
    println!("STRESS listing_ms={:?}", report.listing_ms);

    for _ in 0..200 {
        app.search.engine.start(&dir, "item", false);
        std::thread::sleep(Duration::from_millis(2));
        app.search.engine.cancel();
        report.cancelled_searches += 1;
    }
    for _ in 0..5 {
        let phase = Instant::now();
        app.search.engine.start(&dir, "item", false);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut count = 0;
        while app.search.engine.is_running() {
            count += app.search.engine.poll().len();
            assert!(Instant::now() < deadline, "search timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(count, 20_000);
        report.search_ms.push(phase.elapsed().as_secs_f64() * 1000.0);
    }
    println!("STRESS cancelled={} search_ms={:?}", report.cancelled_searches, report.search_ms);
    report.samples.push(sample(start, "after-searches", Some(&ctx)));
    save_report(&dir, &report);

    let op_src = dir.join("op-src");
    let op_dst = dir.join("op-dst");
    fs::create_dir_all(&op_src).unwrap();
    fs::create_dir_all(&op_dst).unwrap();
    for cycle in 0..100 {
        let source = op_src.join(format!("original-{cycle}.txt"));
        fs::write(&source, b"operation checksum").unwrap();
        let OpResult::Copied { pairs } = operation(
            &app.op_worker,
            OpRequest::Copy { items: vec![source.clone()], dest_dir: op_dst.clone() },
            &mut report,
        ) else {
            panic!("copy result")
        };
        assert_eq!(pairs.len(), 1);
        let copied = pairs[0].1.clone();
        let OpResult::Renamed { to: renamed, .. } = operation(
            &app.op_worker,
            OpRequest::Rename { from: copied, new_name: format!("renamed-{cycle}.txt"), is_undo: false },
            &mut report,
        ) else {
            panic!("rename result")
        };
        let OpResult::Moved { pairs, .. } = operation(
            &app.op_worker,
            OpRequest::Move { items: vec![renamed], dest_dir: op_src.clone(), clear_clipboard: false },
            &mut report,
        ) else {
            panic!("move result")
        };
        assert_eq!(pairs.len(), 1);
        let moved = pairs[0].1.clone();
        let OpResult::Deleted { items, count, .. } =
            operation(&app.op_worker, OpRequest::Delete { items: vec![moved.clone()], permanent: false }, &mut report)
        else {
            panic!("delete result")
        };
        assert_eq!(count, 1);
        assert_eq!(items.len(), 1);
        let OpResult::Restored { result, .. } = operation(&app.op_worker, OpRequest::Restore { items }, &mut report)
        else {
            panic!("restore result")
        };
        assert_eq!(result.unwrap(), 1);
        assert_eq!(fs::read(moved).unwrap(), b"operation checksum");
    }
    println!("STRESS operations={} elapsed={:.1}s", report.file_operations, start.elapsed().as_secs_f64());
    report.samples.push(sample(start, "after-file-operations", Some(&ctx)));
    save_report(&dir, &report);

    let thumbnails: Vec<PathBuf> = (0..1600).map(|index| flat.join(format!("item-{:05}.png", index * 2))).collect();
    let duration = std::env::var("LUMEN_STRESS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(300u64);
    let soak = Instant::now();
    let mut next_sample = Instant::now();
    let mut frame_times = Vec::new();
    // Ten simultaneously open tabs exercise background listing and retained resources.
    for folder in folders.iter().skip(1).take(9) {
        app.tabs.push(Tab::new(Location::from_path(folder).unwrap(), false, false));
    }
    while soak.elapsed() < Duration::from_secs(duration) {
        let cycle = report.frames;
        if cycle.is_multiple_of(5) {
            let destination = &folders[(report.navigations + 1) % folders.len()];
            app.navigate_to_path(destination);
            report.navigations += 1;
            report.maximum_history = report.maximum_history.max(app.active().history.len());
            assert!(app.active().history.len() <= 256);
        }
        if cycle.is_multiple_of(100) {
            app.active_mut().view = match (cycle / 100) % 4 {
                0 => ViewMode::Details,
                1 => ViewMode::List,
                2 => ViewMode::Tiles,
                _ => ViewMode::Icons,
            };
        }
        app.active_mut().scroll_offset = ((cycle * 71) % 40_000) as f32;
        // Saturate the viewport queue much faster than four Shell workers can consume.
        for offset in 0..64 {
            app.icons.request(IconKey {
                path: Some(thumbnails[(cycle * 64 + offset) % thumbnails.len()].clone()),
                pidl_bytes: None,
                ext: "png".into(),
                is_folder: false,
                size: 128,
                thumbnail: true,
                stamp: 1,
            });
            report.icon_requests += 1;
        }
        frame_times.push(render(&mut app, &ctx));
        report.frames += 1;
        if Instant::now() >= next_sample {
            let point = sample(start, "active-soak", Some(&ctx));
            println!(
                "STRESS soak={:.0}s frames={} nav={} requests={} private={:.1}MB handles={} gdi={} ui_states={}",
                soak.elapsed().as_secs_f64(),
                report.frames,
                report.navigations,
                report.icon_requests,
                point.private_mb,
                point.handles,
                point.gdi_objects,
                point.ui_state_entries.unwrap_or_default()
            );
            report.samples.push(point);
            save_report(&dir, &report);
            next_sample = Instant::now() + Duration::from_secs(10);
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    app.icons.bump_generation();
    app.search.engine.cancel();
    app.navigate_to_path(&folders[1]);
    wait_for(&mut app, &ctx, |app| !app.icons.is_busy() && !app.active().loading);
    // Ten seconds of no navigation or icon requests to observe recovery.
    let idle = Instant::now();
    while idle.elapsed() < Duration::from_secs(10) {
        app.poll_background(&ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
    report.idle_icons_busy = app.icons.is_busy();
    assert!(!report.idle_icons_busy);
    report.samples.push(sample(start, "idle-recovery", Some(&ctx)));
    frame_times.sort_by(f64::total_cmp);
    report.frame_p50_ms = frame_times[frame_times.len() / 2];
    report.frame_p95_ms = frame_times[frame_times.len() * 95 / 100];
    report.frame_max_ms = *frame_times.last().unwrap();
    if report.frame_max_ms > 5000.0 {
        report.failures.push("UI processing exceeded 5 seconds".into());
    }
    report.elapsed_seconds = start.elapsed().as_secs_f64();
    drop(app);
    std::thread::sleep(Duration::from_secs(2));
    report.samples.push(sample(start, "after-app-drop", Some(&ctx)));
    save_report(&dir, &report);
    println!(
        "STRESS COMPLETE seconds={:.1} frames={} p50={:.2}ms p95={:.2}ms max={:.2}ms report={}",
        report.elapsed_seconds,
        report.frames,
        report.frame_p50_ms,
        report.frame_p95_ms,
        report.frame_max_ms,
        dir.display()
    );
    assert!(report.failures.is_empty(), "{:?}", report.failures);
}
