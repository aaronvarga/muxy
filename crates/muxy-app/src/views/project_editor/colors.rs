use super::{Editor, Target};
use crate::{model::AppModel, views::overlays::Overlay};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, Focusable, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use muxy_ui::{
    components::{SymbolGlyph, Tooltip},
    controls,
    l10n::{tr_key, translate},
};

const COLUMNS: [(&str, [u32; 12]); 13] = [
    (
        tr_key!("Red"),
        [
            0xff_66_66, 0xf3_54_57, 0xff_33_33, 0xd0_4d_3a, 0xff_00_00, 0xb6_35_3f, 0xcc_00_00,
            0xb2_00_00, 0x99_00_00, 0x7f_00_00, 0x66_00_00, 0x33_00_00,
        ],
    ),
    (
        tr_key!("Orange"),
        [
            0xff_b2_66, 0xff_79_39, 0xff_99_33, 0xe6_68_31, 0xf5_86_21, 0xff_a6_02, 0xff_80_00,
            0xe5_73_00, 0xcc_66_00, 0x99_4c_00, 0x66_33_00, 0x33_19_00,
        ],
    ),
    (
        tr_key!("Yellow"),
        [
            0xff_ff_66, 0xff_ff_4c, 0xff_ff_33, 0xff_ff_19, 0xff_ff_00, 0xe5_e5_00, 0xcc_cc_00,
            0xb2_b2_00, 0x99_99_00, 0x7f_7f_00, 0x66_66_00, 0x33_33_00,
        ],
    ),
    (
        tr_key!("Lime"),
        [
            0xb2_ff_66, 0xa5_ff_4c, 0x99_ff_33, 0x8c_ff_19, 0x80_ff_00, 0x73_e5_00, 0x66_cc_00,
            0x59_b2_00, 0x4c_99_00, 0x3f_7f_00, 0x33_66_00, 0x19_33_00,
        ],
    ),
    (
        tr_key!("Green"),
        [
            0x66_ff_66, 0x4c_ff_4c, 0x33_ff_33, 0x19_ff_19, 0x00_ff_00, 0x00_e5_00, 0x00_cc_00,
            0x00_b2_00, 0x00_99_00, 0x00_7f_00, 0x00_66_00, 0x00_33_00,
        ],
    ),
    (
        tr_key!("Mint"),
        [
            0x66_ff_b2, 0x4c_ff_a5, 0x33_ff_99, 0x19_ff_8c, 0x00_ff_80, 0x00_e5_73, 0x00_cc_66,
            0x00_b2_59, 0x00_99_4c, 0x00_7f_3f, 0x00_66_33, 0x00_33_19,
        ],
    ),
    (
        tr_key!("Cyan"),
        [
            0x66_ff_ff, 0x4c_ff_ff, 0x33_ff_ff, 0x19_ff_ff, 0x00_ff_ff, 0x00_e5_e5, 0x00_cc_cc,
            0x00_b2_b2, 0x00_99_99, 0x00_7f_7f, 0x00_66_66, 0x00_33_33,
        ],
    ),
    (
        tr_key!("Sky"),
        [
            0x66_b2_ff, 0x4c_a5_ff, 0x33_99_ff, 0x19_8c_ff, 0x00_80_ff, 0x00_73_e5, 0x00_66_cc,
            0x00_59_b2, 0x00_4c_99, 0x00_33_66, 0x00_26_5c, 0x00_19_33,
        ],
    ),
    (
        tr_key!("Blue"),
        [
            0x66_66_ff, 0x4c_4c_ff, 0x33_33_ff, 0x19_19_ff, 0x00_00_ff, 0x00_00_e5, 0x00_00_cc,
            0x00_00_b2, 0x00_00_99, 0x00_00_7f, 0x00_00_66, 0x00_00_33,
        ],
    ),
    (
        tr_key!("Violet"),
        [
            0xb2_66_ff, 0xa5_4c_ff, 0x99_33_ff, 0x8c_19_ff, 0x80_00_ff, 0x73_00_e5, 0x66_00_cc,
            0x43_2d_73, 0x4c_00_99, 0x3f_00_7f, 0x33_00_66, 0x19_00_33,
        ],
    ),
    (
        tr_key!("Magenta"),
        [
            0xff_66_ff, 0xff_4c_ff, 0xff_33_ff, 0xff_19_ff, 0xff_00_ff, 0xe5_00_e5, 0xcc_00_cc,
            0x78_2e_7e, 0x99_00_99, 0x7f_00_7f, 0x66_00_66, 0x33_00_33,
        ],
    ),
    (
        tr_key!("Pink"),
        [
            0xff_66_b2, 0xff_33_99, 0xd4_38_6e, 0xff_00_80, 0xaa_2f_7c, 0xcc_00_66, 0x9b_21_40,
            0x99_00_4c, 0x7b_10_3e, 0x66_00_33, 0x5c_05_37, 0x33_00_19,
        ],
    ),
    (
        tr_key!("Neutral"),
        [
            0xd1_d1_d1, 0xba_ba_ba, 0xa3_a3_a3, 0x8c_8c_8c, 0x80_80_80, 0x74_74_74, 0x5d_5d_5d,
            0x46_46_46, 0x2f_2f_2f, 0x18_18_18, 0x0c_0c_0c, 0x00_00_00,
        ],
    ),
];

pub(super) fn presets() -> Vec<(String, String)> {
    COLUMNS
        .iter()
        .flat_map(|(name, colors)| {
            colors
                .iter()
                .map(move |rgb| (translate(name).to_string(), format!("#{rgb:06x}")))
        })
        .collect()
}

pub(super) fn grid(editor: &Editor, model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let selected = editor.input.read(cx).text().trim().to_owned();
    let columns: Vec<_> = editor
        .color_presets
        .chunks(12)
        .map(|column| {
            let buttons: Vec<_> = column
                .iter()
                .filter_map(|(label, hex)| {
                    let color = muxy_ui::theme::parse_hex(hex)?;
                    let active = selected.eq_ignore_ascii_case(hex);
                    let hex = hex.clone();
                    let id = format!("project-color-{hex}");
                    let tooltip = format!("{label} ({hex})");
                    let tooltip_colors = (theme.raised(), theme.fg, theme.border, theme.bg);
                    Some(
                        controls::button(
                            controls::Style { theme, metrics: &m },
                            &id,
                            "",
                            true,
                            cx.listener(move |model, _, window, cx| {
                                if let Some(Overlay::ProjectEditor(editor)) = &model.overlay
                                    && matches!(editor.target, Target::ProjectColor(_))
                                {
                                    let input = editor.input.clone();
                                    input.update(cx, |input, cx| {
                                        input.set_text(hex.clone(), cx);
                                        input.select_all_text(cx);
                                    });
                                    input.focus_handle(cx).focus(window);
                                }
                            }),
                        )
                        .debug_selector(move || id.clone())
                        .size(m.scaled(24.0))
                        .px(px(0.0))
                        .bg(color)
                        .border_2()
                        .border_color(if active { theme.fg } else { theme.border })
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(m.radius_sm())
                                .bg(color)
                                .when(active, |swatch| {
                                    swatch.child(SymbolGlyph::new(
                                        "checkmark",
                                        m.font_caption(),
                                        muxy_ui::theme::contrasting_foreground(color).into(),
                                    ))
                                }),
                        )
                        .tooltip(move |_, cx| {
                            cx.new(|_| {
                                Tooltip::new(
                                    tooltip.clone(),
                                    tooltip_colors.0,
                                    tooltip_colors.1,
                                    tooltip_colors.2,
                                    tooltip_colors.3,
                                )
                            })
                            .into()
                        })
                        .into_any_element(),
                    )
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .gap(m.spacing2())
                .children(buttons)
                .into_any_element()
        })
        .collect();
    div()
        .id("project-color-grid")
        .flex()
        .gap(m.spacing2())
        .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                model.dismiss_overlay(cx);
                cx.stop_propagation();
            }
        }))
        .children(columns)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn palette_has_even_gradients_without_the_two_lightest_shades() {
        let colors = super::presets();
        let unique: std::collections::HashSet<_> = colors.iter().map(|(_, hex)| hex).collect();
        assert_eq!(colors.len(), 156);
        assert_eq!(unique.len(), colors.len());
        for column in colors.chunks(12) {
            assert_eq!(column.len(), 12);
            let lightness: Vec<_> = column
                .iter()
                .map(|(_, hex)| {
                    assert!(hex.parse::<muxy_app_core::Color>().is_ok());
                    gpui::Hsla::from(muxy_ui::theme::parse_hex(hex).expect("valid preset")).l
                })
                .collect();
            assert!(lightness.windows(2).all(|pair| pair[0] >= pair[1]));
            assert!(lightness[0] <= 0.82);
        }
        for (_, hex) in colors.iter().take(144) {
            let lightness =
                gpui::Hsla::from(muxy_ui::theme::parse_hex(hex).expect("valid preset")).l;
            assert!(lightness <= 0.7);
        }
        for hex in [
            "#00265c", "#432d73", "#782e7e", "#aa2f7c", "#d4386e", "#f35457", "#ff7939", "#ffa602",
            "#5c0537", "#7b103e", "#9b2140", "#b6353f", "#d04d3a", "#e66831", "#f58621", "#ff0000",
            "#00ff00", "#0000ff", "#ffff00", "#00ffff", "#ff00ff",
        ] {
            assert!(
                colors.iter().any(|(_, color)| color == hex),
                "missing {hex}"
            );
        }
    }
}
