use super::*;

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::empty();
    fonts.font_data.insert(
        "Nowar".into(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/NowarSansTW-Regular.ttf"))
            .into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.insert(family, vec!["Nowar".into()]);
    }
    ctx.set_fonts(fonts);
    ctx
}

fn assert_visible_equal(actual: &egui::Galley, expected: &egui::Galley, case: &str) {
    assert_eq!(actual.elided, expected.elided, "{case}: elision");
    assert_eq!(actual.rect, expected.rect, "{case}: rectangle");
    assert_eq!(
        actual.rows, expected.rows,
        "{case}: visible glyphs and mesh"
    );
}

#[test]
fn unicode_previews_match_complete_layout_at_width_and_scale_changes() {
    let ctx = context();
    let cases = [
        "長備註內容／Detail ，。！？「中文」 ".repeat(120),
        "AV office affinity ffi fl efficient text ".repeat(120),
        format!("{} office ffi fl {}", "x".repeat(126), " end".repeat(500)),
        "e\u{301} a\u{30a} n\u{303} ".repeat(180),
        "👩\u{200d}💻 ❤️ 🇹🇼 family 👨\u{200d}👩\u{200d}👧 ".repeat(100),
        "العربية עברית office 中文 ".repeat(130),
        format!("{}\u{202e}אבג\u{202c}", "neutral 123 ".repeat(180)),
        "Cyrillic Привет Ελληνικά हिन्दी ไทย ".repeat(120),
        "full\r\n原文\twith\nnewlines ".repeat(180),
        format!("{}後方內容", " ".repeat(2_000)),
        format!("{}中文後文", "12345 ,.-()/[] ".repeat(100)),
        "fifflAV".repeat(600),
    ];
    let mut cache = NotePreviews::default();
    let mut token = 0;
    for scale in [1.0, 1.25, 2.0] {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(2200.0, 1000.0),
            )),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        let mut output = ctx.run_ui(input, |ui| {
            for (index, text) in cases.iter().enumerate() {
                token += 1;
                for width in [0.0, 1.0, 8.0, 80.0, 190.0, 700.0, 1120.0] {
                    let full = single_line(ui.painter(), text, 12.0, Color32::WHITE, width);
                    let actual = cache.layout(
                        ui.painter(),
                        index as u64,
                        token,
                        text,
                        12.0,
                        Color32::WHITE,
                        width,
                    );
                    assert_visible_equal(
                        &actual,
                        &full,
                        &format!("case {index} width {width} scale {scale}"),
                    );
                }
            }
        });
        output.textures_delta.clear();
    }
}

#[test]
fn long_note_work_is_bounded_and_cached_text_reflows_without_retaining_galleys() {
    let ctx = context();
    let mut cache = NotePreviews::default();
    let text = "長備註內容／Detail ".repeat(600);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let full = single_line(ui.painter(), &text, 12.0, Color32::WHITE, 190.0);
        let actual = cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, 190.0);
        assert_visible_equal(&actual, &full, "initial");
        assert_eq!(cache.preparations, 1);
        assert!(cache.entries[&1].text.as_ref().unwrap().len() < 1024);
        assert!(actual.job.text.len() < 1024);
        for _ in 0..10 {
            cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, 190.0);
        }
        assert_eq!(cache.preparations, 1);
        let full = single_line(ui.painter(), &text, 12.0, Color32::WHITE, 1120.0);
        let wide = cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, 1120.0);
        assert_visible_equal(&wide, &full, "wider");
        assert_eq!(cache.preparations, 2);
        let changed = "Changed 原文 ".repeat(700);
        let full = single_line(ui.painter(), &changed, 12.0, Color32::WHITE, 1120.0);
        let actual = cache.layout(ui.painter(), 1, 2, &changed, 12.0, Color32::WHITE, 1120.0);
        assert_visible_equal(&actual, &full, "new model token");
        assert_eq!(cache.preparations, 3);
        assert_eq!(cache.entries.len(), 1);
    });
    output.textures_delta.clear();
    ctx.set_fonts(egui::FontDefinitions::default());
    ctx.set_zoom_factor(1.25);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let full = single_line(ui.painter(), &text, 12.0, Color32::WHITE, 190.0);
        let actual = cache.layout(ui.painter(), 1, 3, &text, 12.0, Color32::WHITE, 190.0);
        assert_visible_equal(&actual, &full, "new atlas and scale");
        assert_eq!(cache.preparations, 4);
    });
    output.textures_delta.clear();
}

#[test]
fn medium_notes_use_only_the_complete_layout_during_continuous_width_and_scale_changes() {
    let ctx = context();
    let ascii = "abc def ".repeat(129);
    let cases = [
        format!("00 {}", "備註用".repeat(57)),
        format!("00 {}", "備註用".repeat(66)),
        format!("00 {}", "abc def ".repeat(65)),
        "full\r\n原文\twith\nnewline ".repeat(25),
        "e\u{301} 👩\u{200d}💻 🇹🇼\r\n".repeat(25),
        "עברית العربية\r\n".repeat(30),
        ascii[..1023].to_owned(),
        ascii[..1024].to_owned(),
    ];
    assert_eq!(cases[0].len(), 516);
    assert_eq!(cases[1].len(), 597);
    assert_eq!(cases[2].len(), 523);
    let widths: Vec<_> = std::iter::once(190.0)
        .chain((1080..=1100).rev().map(|width| width as f32))
        .chain(std::iter::once(190.0))
        .collect();
    let mut cache = NotePreviews::default();
    for scale in [1.0, 1.25, 2.0] {
        for &width in &widths {
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            let mut output = ctx.run_ui(input, |ui| {
                for (index, text) in cases.iter().enumerate() {
                    let full = single_line(ui.painter(), text, 12.0, Color32::WHITE, width);
                    let galleys = ctx.fonts(|fonts| fonts.num_galleys_in_cache());
                    let actual = cache.layout(
                        ui.painter(),
                        index as u64,
                        1,
                        text,
                        12.0,
                        Color32::WHITE,
                        width,
                    );
                    assert_visible_equal(&actual, &full, "medium note resize");
                    assert_eq!(actual.job.text, text.replace(['\r', '\n', '\t'], " "));
                    // The complete job is already cached. Any extra prefix job
                    // would increase this count, even if visible output matched.
                    assert_eq!(ctx.fonts(|fonts| fonts.num_galleys_in_cache()), galleys);
                }
                assert_eq!(cache.preparations, 0);
                assert!(cache.entries.is_empty());
            });
            output.textures_delta.clear();
        }
    }
}

#[test]
fn long_previews_remain_correct_and_bounded_while_width_changes_every_frame() {
    let ctx = context();
    let near_threshold = "abc def ".repeat(513);
    for text in [
        near_threshold[..4095].to_owned(),
        near_threshold[..4096].to_owned(),
        near_threshold[..4097].to_owned(),
        "長備註內容／Detail ".repeat(600),
    ] {
        assert!(text.len() <= 16 * 1024);
        let mut cache = NotePreviews::default();
        for width in std::iter::once(190.0)
            .chain((1080..=1100).rev().map(|width| width as f32))
            .chain(std::iter::once(190.0))
        {
            let before = cache.preparations;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let full = single_line(ui.painter(), &text, 12.0, Color32::WHITE, width);
                let actual = cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, width);
                assert_visible_equal(&actual, &full, "long note resize");
                assert!(actual.job.text.len() < text.len());
                assert_eq!(cache.preparations, before + 1);
                assert_eq!(cache.entries.len(), 1);
                cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, width);
                assert_eq!(cache.preparations, before + 1);
            });
            output.textures_delta.clear();
        }
    }
}

#[test]
fn note_just_above_medium_length_only_prepares_a_preview_when_it_saves_layout_work() {
    let ctx = context();
    let text = "備註用".repeat(114);
    assert_eq!(text.len(), 1026);
    let mut cache = NotePreviews::default();
    for width in std::iter::once(190.0)
        .chain((1080..=1100).rev().map(|width| width as f32))
        .chain(std::iter::once(190.0))
    {
        let before = cache.preparations;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let full = single_line(ui.painter(), &text, 12.0, Color32::WHITE, width);
            let galleys = ctx.fonts(|fonts| fonts.num_galleys_in_cache());
            let actual = cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, width);
            assert_visible_equal(&actual, &full, "note just above medium length");
            if width == 190.0 {
                assert!(actual.job.text.len() < text.len());
                assert!(cache.entries[&1].text.is_some());
            } else {
                assert_eq!(actual.job.text, text);
                assert!(cache.entries[&1].text.is_none());
                // At these real wide-list sizes, preparing prefixes would add
                // work before returning the same complete layout.
                assert_eq!(ctx.fonts(|fonts| fonts.num_galleys_in_cache()), galleys);
            }
            assert_eq!(cache.preparations, before + 1);
            assert_eq!(cache.entries.len(), 1);
            cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, width);
            assert_eq!(cache.preparations, before + 1);
        });
        output.textures_delta.clear();
    }
}

#[test]
fn long_word_fallback_skips_prefix_layouts_during_continuous_resize() {
    let ctx = context();
    let text = format!("{}{}", "a".repeat(3000), " suffix ".repeat(200));
    assert!(text.len() > 4096 && text.len() <= 16 * 1024);
    let mut cache = NotePreviews::default();
    for width in (1080..=1100).rev().map(|width| width as f32) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let full = single_line(ui.painter(), &text, 12.0, Color32::WHITE, width);
            let galleys = ctx.fonts(|fonts| fonts.num_galleys_in_cache());
            let actual = cache.layout(ui.painter(), 1, 1, &text, 12.0, Color32::WHITE, width);
            assert_visible_equal(&actual, &full, "long word fallback");
            assert_eq!(actual.job.text, text);
            assert_eq!(ctx.fonts(|fonts| fonts.num_galleys_in_cache()), galleys);
            assert_eq!(cache.entries.len(), 1);
            assert!(cache.entries[&1].text.is_none());
        });
        output.textures_delta.clear();
    }
}
