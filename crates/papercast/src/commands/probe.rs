use papercast_capture::probe;

pub fn run() -> anyhow::Result<()> {
    let report = probe::run()?;

    println!("== Outputs ==");
    for o in &report.outputs {
        let name = o.name.as_deref().unwrap_or("<unnamed>");
        let mode = o
            .mode
            .map(|(w, h, r)| format!("{w}x{h} @ {:.1} Hz", r as f64 / 1000.0))
            .unwrap_or_else(|| "unknown mode".into());
        let transform = o.transform.as_deref().unwrap_or("?");
        println!("  {name}: {mode}, scale {}, transform {transform}", o.scale);
        if let Some(desc) = &o.description {
            println!("    {desc}");
        }
    }

    println!("\n== Capture protocol support ==");
    let supports_capture = report.supports_capture();
    let has_cosmic = report.has_cosmic_screencopy();
    let has_wlr = report.has_wlr_screencopy();
    let ext_mark = if supports_capture { "✔" } else { "✘" };
    let ext_detail = format!(
        "manager={:?} output-source={:?} toplevel-source={:?}",
        report.global_version("ext_image_copy_capture_manager_v1"),
        report.global_version("ext_output_image_capture_source_manager_v1"),
        report.global_version("ext_foreign_toplevel_image_capture_source_manager_v1"),
    );
    println!(
        "  {ext_mark} ext-image-copy-capture-v1 (standard, preferred): {ext_detail}",
    );
    let cosmic = report
        .globals
        .iter()
        .filter(|g| g.interface.starts_with("zcosmic_screencopy"))
        .map(|g| format!("{} v{}", g.interface, g.version))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "  {} COSMIC zcosmic screencopy: {}{}",
        if has_cosmic { "•" } else { "✘" },
        if cosmic.is_empty() { "not detected" } else { &cosmic },
        if has_cosmic { " (detected, no PaperCast backend)" } else { "" },
    );
    println!(
        "  {} wlr-screencopy (legacy wlroots): {:?}{}",
        if has_wlr { "•" } else { "✘" },
        report.global_version("zwlr_screencopy_manager_v1"),
        if has_wlr { " (detected, no PaperCast backend)" } else { "" },
    );

    println!("\n  All capture-related globals seen:");
    for g in report.capture_like_globals() {
        println!("    {} v{}", g.interface, g.version);
    }

    if supports_capture {
        println!("\n  → selected backend: ext-image-copy-capture-v1");
    } else {
        println!(
            "\n  → no supported Wayland capture protocol found; \
             the portal/PipeWire fallback backend would be required"
        );
    }

    println!("\n== SHM formats ({}) ==", report.shm_formats.len());
    let interesting = ["Xrgb8888", "Argb8888", "Xbgr8888", "Abgr8888", "R8"];
    let listed: Vec<_> = report
        .shm_formats
        .iter()
        .filter(|f| interesting.contains(&f.as_str()))
        .cloned()
        .collect();
    println!("  usable for us: {}", listed.join(", "));
    println!("  (run with RUST_LOG=debug for the full list)");
    for f in &report.shm_formats {
        tracing::debug!("shm format: {f}");
    }

    println!("\n== All globals ({}) ==", report.globals.len());
    for g in &report.globals {
        println!("  {} v{}", g.interface, g.version);
    }

    Ok(())
}
