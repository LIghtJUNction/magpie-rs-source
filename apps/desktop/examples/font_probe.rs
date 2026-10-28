//! Inspect real platform font selection and multilingual shaping without a window.
//! cargo run --release --locked --manifest-path apps/desktop/Cargo.toml --example font_probe
#![allow(dead_code)]

#[path = "../src/localization.rs"]
mod localization;
#[path = "../src/theme.rs"]
mod theme;

use std::sync::{Arc, Mutex};

use gpui::{FontRun, FontWeight, MissingGlyph, MissingGlyphSink, px};
use serde_json::json;

#[derive(Default)]
struct Missing(Arc<Mutex<Vec<String>>>);

impl MissingGlyphSink for Missing {
    fn report(&self, glyphs: Vec<MissingGlyph>) {
        if let Ok(mut missing) = self.0.lock() {
            missing.extend(glyphs.into_iter().map(|glyph| glyph.grapheme().to_string()));
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let platform = gpui_platform::current_platform(true);
    let backend = platform.text_system();
    let families = backend.all_font_names();
    let missing = Missing::default();
    let missing_values = missing.0.clone();
    backend.set_missing_glyph_sink(Some(Arc::new(missing)));
    let mut records = Vec::new();
    let mut incomplete = false;
    for locale in localization::Locale::ALL {
        let selected = theme::font_for_families(&families, locale);
        for weight in [400.0, 500.0, 600.0] {
            let mut font = selected.clone();
            font.weight = FontWeight(weight);
            // Unlike TextSystem::resolve_font, this direct backend operation
            // does not silently substitute GPUI's generic family stack.
            let id = backend.font_id(&font)?;
            let metrics = backend.font_metrics(id);
            for (language, sample) in [
                ("latin", "Magpie Codex 0123456789 / model-name"),
                ("zh-CN", "查询模型，保存连接。中文输入与预览。"),
                ("zh-TW", "查詢模型，儲存連線。繁體中文預覽。"),
                ("ja", "モデルを選択、接続を保存。日本語の入力。"),
                ("de", "Verbindung prüfen · ÄÖÜ äöü ß"),
            ] {
                missing_values.lock().expect("missing-glyph lock").clear();
                let layout = backend.layout_line(
                    sample,
                    px(13.0),
                    &[FontRun {
                        len: sample.len(),
                        font_id: id,
                    }],
                );
                let missing = missing_values.lock().expect("missing-glyph lock").clone();
                let glyphs = layout
                    .runs
                    .iter()
                    .flat_map(|run| {
                        run.glyphs.iter().map(move |glyph| {
                            json!({
                                "font_id": run.font_id.0,
                                "glyph_id": glyph.id.0,
                                "utf8_index": glyph.index,
                                "x": f32::from(glyph.position.x),
                                "y": f32::from(glyph.position.y),
                            })
                        })
                    })
                    .collect::<Vec<_>>();
                let zero_glyphs = layout
                    .runs
                    .iter()
                    .flat_map(|run| &run.glyphs)
                    .filter(|glyph| glyph.id.0 == 0)
                    .count();
                let covers = zero_glyphs == 0 && missing.is_empty() && !glyphs.is_empty();
                incomplete |= !covers;
                records.push(json!({
                    "locale": locale.id(),
                    "requested_family": font.family.as_ref(),
                    "listed_exact_family": families.iter().any(|family| family == font.family.as_ref()),
                    "fallback_families": font.fallbacks.as_ref().map(|list| list.fallback_list()),
                    "requested_weight": weight,
                    "primary_font_id": id.0,
                    "primary_units_per_em": metrics.units_per_em,
                    "primary_ascent": metrics.ascent,
                    "primary_descent": metrics.descent,
                    "language": language,
                    "sample": sample,
                    "font_size": 13,
                    "line_width": f32::from(layout.width),
                    "line_ascent": f32::from(layout.ascent),
                    "line_descent": f32::from(layout.descent),
                    "missing_graphemes": missing,
                    "zero_glyphs": zero_glyphs,
                    "shaping_coverage_passed": covers,
                    "glyphs": glyphs,
                }));
            }
        }
    }
    backend.set_missing_glyph_sink(None);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "gpui_revision": "37faef81f74fc1bb08be9939c4cffc8ede6ced2f",
            "scope": "native platform font selection, metrics and shaping; no window or raster acceptance",
            "all_font_names": families,
            "shaping_coverage_passed": !incomplete,
            "records": records,
        }))?
    );
    if incomplete {
        std::process::exit(2);
    }
    Ok(())
}
