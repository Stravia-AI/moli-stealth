use std::io::Cursor;

use moli_sdk::{
    BrowserConfig, EvaluateOptions, NavigationOptions, ResourceLoading, Session, SessionConfig,
};

pub async fn verify() {
    let fixture = crate::fixture::Fixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let browser = session
        .browser(BrowserConfig {
            block_private_networks: false,
            obey_robots: false,
            resources: ResourceLoading::all(),
            ..Default::default()
        })
        .await
        .unwrap();
    let page = browser
        .fetch(fixture.url("/font"), NavigationOptions::default())
        .await
        .unwrap();
    let world = page
        .create_isolated_world("font-verification")
        .await
        .unwrap();
    let result = page.evaluate("JSON.stringify(Object.fromEntries(['latin','cjk','fallback'].map(id => { const r=document.getElementById(id).getBoundingClientRect(); return [id,{x:r.x,y:r.y,width:r.width,height:r.height}]; })))", EvaluateOptions { context: Some(world), ..Default::default() }).await.unwrap();
    let geometry: serde_json::Value =
        serde_json::from_str(result["value"].as_str().unwrap()).unwrap();
    // DejaVu Sans 2.37 hmtx: W=2025, i=569, M=1767, m=1995，unitsPerEm=2048。
    // 每个字符是独立 inline-block，避免把字偶距行为误当字体发现契约。
    let latin_width = geometry["latin"]["width"].as_f64().unwrap();
    assert!(
        (latin_width - 148.96875).abs() < 0.5,
        "DejaVu Sans metrics mismatch: {geometry}"
    );
    for id in ["cjk", "fallback"] {
        assert!(
            (geometry[id]["width"].as_f64().unwrap() - 144.0).abs() < 0.5,
            "CJK font metrics mismatch: {geometry}"
        );
    }
    let layout = page.layout_metrics().await.unwrap();
    let screenshot = page.screenshot_png().await.unwrap();
    let mut decoder = png::Decoder::new(Cursor::new(&screenshot));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let image = reader.next_frame(&mut pixels).unwrap();
    let channels = match image.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => panic!("unexpected screenshot color type: {other:?}"),
    };
    let scale = image.width as f64 / layout.viewport_width as f64;
    let crop = |id: &str, character: Option<usize>| -> Vec<u8> {
        let rect = &geometry[id];
        let x = rect["x"].as_f64().unwrap() + character.map_or(0.0, |index| index as f64 * 48.0);
        let width = if character.is_some() {
            48.0
        } else {
            rect["width"].as_f64().unwrap()
        };
        let left = (x * scale).round() as usize;
        let top = (rect["y"].as_f64().unwrap() * scale).round() as usize;
        let width = (width * scale).round() as usize;
        let height = (rect["height"].as_f64().unwrap() * scale).round() as usize;
        assert!(left + width <= image.width as usize && top + height <= image.height as usize);
        let mut mask = Vec::with_capacity(width * height);
        for y in top..top + height {
            for x in left..left + width {
                let at = (y * image.width as usize + x) * channels;
                mask.push(u8::from(
                    pixels[at..at + 3].iter().copied().min().unwrap() < 160,
                ));
            }
        }
        mask
    };
    let explicit = crop("cjk", None);
    let fallback = crop("fallback", None);
    assert_eq!(
        explicit, fallback,
        "missing first-choice family must fall back to the same rendered CJK font"
    );
    let glyphs = (0..3)
        .map(|index| crop("cjk", Some(index)))
        .collect::<Vec<_>>();
    for glyph in &glyphs {
        assert!(
            glyph.iter().filter(|&&ink| ink == 1).count() > 100,
            "CJK glyph missing ink"
        );
    }
    for (left, right) in [(0, 1), (0, 2), (1, 2)] {
        let differing = glyphs[left]
            .iter()
            .zip(&glyphs[right])
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            differing > 100,
            "different CJK characters rendered as the same missing-glyph box"
        );
    }
    if let Some(directory) = std::env::var_os("MOLI_SDK_EVIDENCE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("font-render.png"), screenshot).unwrap();
        std::fs::write(
            directory.join("font-geometry.json"),
            serde_json::to_vec_pretty(&geometry).unwrap(),
        )
        .unwrap();
    }
    // This oracle is extracted offline from a pinned upstream font, not from the
    // font selected by the process under test. See font-reference/SOURCE.txt.
    let reference =
        serde_json::to_string(include_str!("../font-reference/noto-sans-cjk-sc-2.004.svg"))
            .unwrap();
    let script = format!(
        r#"document.body.innerHTML = '<div id="actual" style="font-family: &quot;Noto Sans CJK SC&quot;; font-size:192px; font-weight:400; line-height:224px; width:576px; height:224px; white-space:nowrap">中文国</div><div id="reference" style="width:576px;height:224px"></div>';
        document.getElementById('reference').innerHTML = {reference};
        JSON.stringify(Object.fromEntries(['actual','reference'].map(id => {{ const r=document.getElementById(id).getBoundingClientRect(); return [id,{{x:r.x,y:r.y,width:r.width,height:r.height}}]; }})))"#
    );
    let result = page
        .evaluate(
            &script,
            EvaluateOptions {
                context: Some(world),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let reference_geometry: serde_json::Value =
        serde_json::from_str(result["value"].as_str().unwrap()).unwrap();
    let layout = page.layout_metrics().await.unwrap();
    let reference_screenshot = page.screenshot_png().await.unwrap();
    // Save before asserting so a rejected font leaves useful diagnostic evidence.
    if let Some(directory) = std::env::var_os("MOLI_SDK_EVIDENCE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::write(directory.join("font-reference.png"), &reference_screenshot).unwrap();
        std::fs::write(
            directory.join("font-reference-geometry.json"),
            serde_json::to_vec_pretty(&reference_geometry).unwrap(),
        )
        .unwrap();
    }
    verify_outline(
        &reference_screenshot,
        layout.viewport_width as f64,
        &reference_geometry,
    );
    page.close().await.unwrap();
    browser.close().await.unwrap();
    session.close().await.unwrap();
    println!("Latin metrics, pinned Noto Sans CJK outlines and missing-family fallback passed");
}

fn verify_outline(screenshot: &[u8], viewport_width: f64, geometry: &serde_json::Value) {
    let mut decoder = png::Decoder::new(Cursor::new(screenshot));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let image = reader.next_frame(&mut pixels).unwrap();
    let channels = match image.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => panic!("unexpected screenshot color type: {other:?}"),
    };
    let scale = image.width as f64 / viewport_width;
    // At 192 CSS px/em, one CSS pixel permits raster edge rounding, not
    // a changed stroke design. No percentage of unrelated ink may be ignored.
    let tolerance = scale.ceil() as usize;
    let crop = |id: &str| {
        let rect = &geometry[id];
        let left = (rect["x"].as_f64().unwrap() * scale).round() as usize;
        let top = (rect["y"].as_f64().unwrap() * scale).round() as usize;
        let width = (rect["width"].as_f64().unwrap() * scale).round() as usize;
        let height = (rect["height"].as_f64().unwrap() * scale).round() as usize;
        assert!(
            left + width <= image.width as usize && top + height <= image.height as usize,
            "font reference must be fully visible"
        );
        let mut ink = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let at = ((top + y) * image.width as usize + left + x) * channels;
                if pixels[at..at + 3].iter().copied().min().unwrap() < 160 {
                    ink.push((x, y));
                }
            }
        }
        assert!(!ink.is_empty(), "{id} has no rendered outline");
        let min_x = ink.iter().map(|p| p.0).min().unwrap();
        let min_y = ink.iter().map(|p| p.1).min().unwrap();
        for point in &mut ink {
            point.0 -= min_x;
            point.1 -= min_y;
        }
        ink
    };
    let actual = crop("actual");
    let reference = crop("reference");
    let mut comparisons = serde_json::Map::new();
    let mut matches = true;
    // Translate to ink origins to remove font line-box/baseline differences.
    // Never rescale: advance widths, glyph proportions and strokes must agree.
    for (name, source, target) in [
        ("text", &actual, &reference),
        ("reference", &reference, &actual),
    ] {
        let width = target.iter().map(|p| p.0).max().unwrap() + 1;
        let height = target.iter().map(|p| p.1).max().unwrap() + 1;
        let mut mask = vec![false; width * height];
        for &(x, y) in target {
            mask[y * width + x] = true;
        }
        let unmatched = source
            .iter()
            .filter(|&&(x, y)| {
                let min_x = x.saturating_sub(tolerance);
                let min_y = y.saturating_sub(tolerance);
                let max_x = (x + tolerance).min(width - 1);
                let max_y = (y + tolerance).min(height - 1);
                !(min_y..=max_y).any(|yy| (min_x..=max_x).any(|xx| mask[yy * width + xx]))
            })
            .count();
        eprintln!(
            "Noto Sans CJK SC 2.004 {name}: {unmatched}/{} ink pixels exceed {tolerance}px edge tolerance",
            source.len()
        );
        matches &= unmatched == 0;
        comparisons.insert(
            name.into(),
            serde_json::json!({
                "unmatched_pixels": unmatched,
                "ink_pixels": source.len(),
                "target_width": width,
                "target_height": height,
            }),
        );
    }
    if let Some(directory) = std::env::var_os("MOLI_SDK_EVIDENCE_DIR") {
        std::fs::write(
            std::path::PathBuf::from(directory).join("font-outline-comparison.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "matches": matches,
                "tolerance_pixels": tolerance,
                "directions": comparisons,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    assert!(
        matches,
        "Noto Sans CJK SC 2.004 outline mismatch; wrong CJK font or rendering regression"
    );
}
