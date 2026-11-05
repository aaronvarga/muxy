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
};

const COLUMNS: [[u32; 10]; 20] = [
    [
        0x00fa_e8e7,
        0x00f3_d1d0,
        0x00e7_a6a1,
        0x00da_7c76,
        0x00ce_554d,
        0x00c0_302a,
        0x00a6_2b25,
        0x008d_2521,
        0x0075_201c,
        0x005d_1b17,
    ],
    [
        0x00fe_eae9,
        0x00fc_d6d3,
        0x00f9_aea9,
        0x00f7_8881,
        0x00f2_645b,
        0x00ee_4139,
        0x00cd_3932,
        0x00ae_322c,
        0x008f_2a25,
        0x0071_231e,
    ],
    [
        0x00f4_ebf6,
        0x00ea_d9ed,
        0x00d5_b5dc,
        0x00c0_91ca,
        0x00ab_6fba,
        0x0096_4fa9,
        0x0082_4592,
        0x006f_3c7c,
        0x005c_3266,
        0x004a_2953,
    ],
    [
        0x00f3_e9f5,
        0x00e6_d5eb,
        0x00cf_abd7,
        0x00b7_84c4,
        0x00a1_5eb1,
        0x0089_3c9f,
        0x0077_358a,
        0x0065_2e75,
        0x0054_2761,
        0x0044_204e,
    ],
    [
        0x00e6_f0f6,
        0x00cc_e2ee,
        0x009b_c5dd,
        0x006e_aacd,
        0x0041_8fbd,
        0x0013_75ad,
        0x0011_6696,
        0x0011_577f,
        0x000f_4969,
        0x000e_3b54,
    ],
    [
        0x00e7_f4fa,
        0x00ce_e7f6,
        0x00a0_d1ed,
        0x0072_bae3,
        0x0047_a4da,
        0x0019_8ed2,
        0x0018_7bb6,
        0x0016_6999,
        0x0013_577e,
        0x0012_4665,
    ],
    [
        0x00e3_f7f4,
        0x00c6_f0e8,
        0x0090_e0d2,
        0x005a_d2bd,
        0x001a_c2a8,
        0x0000_b493,
        0x0000_9b7f,
        0x0000_846d,
        0x0000_6d5a,
        0x0000_5748,
    ],
    [
        0x00e3_f5f1,
        0x00c6_e9e3,
        0x0090_d4c8,
        0x005b_bfae,
        0x0022_aa93,
        0x0000_967b,
        0x0000_826b,
        0x0000_6f5b,
        0x0000_5b4c,
        0x0000_493d,
    ],
    [
        0x00e4_f6ed,
        0x00cb_eddb,
        0x0098_dbb9,
        0x0065_c798,
        0x0030_b679,
        0x0000_a55b,
        0x0000_8e4f,
        0x0000_7944,
        0x0000_6439,
        0x0000_502e,
    ],
    [
        0x00e5_f9ef,
        0x00cb_f4e0,
        0x0098_e8c1,
        0x0067_dca3,
        0x002f_d087,
        0x0000_c56c,
        0x0000_ab5e,
        0x0000_9051,
        0x0000_7843,
        0x0000_5f37,
    ],
    [
        0x00fe_f8e5,
        0x00fc_f1cb,
        0x00f9_e39b,
        0x00f8_d66f,
        0x00f5_c949,
        0x00f3_bc32,
        0x00d1_a22b,
        0x00b1_8a25,
        0x0091_711f,
        0x0073_5b19,
    ],
    [
        0x00ff_f3e4,
        0x00fe_e8cc,
        0x00fc_d19a,
        0x00fb_bc6e,
        0x00f9_a646,
        0x00f8_902b,
        0x00d6_7d26,
        0x00b5_6b21,
        0x0096_591c,
        0x0076_4717,
    ],
    [
        0x00fe_f0e6,
        0x00fb_e1ce,
        0x00f7_c4a0,
        0x00f3_a874,
        0x00ef_8c4d,
        0x00ea_722c,
        0x00ca_6327,
        0x00ab_5522,
        0x008d_471d,
        0x0070_3918,
    ],
    [
        0x00fc_ebe3,
        0x00f7_d8c7,
        0x00ef_b291,
        0x00e8_8d60,
        0x00df_6a35,
        0x00d6_4918,
        0x00b9_4015,
        0x009c_3712,
        0x0081_2f0f,
        0x0067_260c,
    ],
    [
        0x00fd_fefe,
        0x00fa_fcfc,
        0x00f6_f8f8,
        0x00f2_f5f6,
        0x00ed_f1f2,
        0x00e9_eeef,
        0x00c9_cdce,
        0x00aa_adae,
        0x008c_8f8f,
        0x0070_7272,
    ],
    [
        0x00f7_f8f8,
        0x00f0_f1f2,
        0x00e1_e3e6,
        0x00d1_d6d8,
        0x00c2_c9cc,
        0x00b4_bbbf,
        0x009b_a2a5,
        0x0085_898c,
        0x006d_7274,
        0x0057_5a5c,
    ],
    [
        0x00f2_f5f5,
        0x00e6_eaea,
        0x00ce_d6d6,
        0x00b6_c2c3,
        0x009f_aeaf,
        0x0088_9b9c,
        0x0076_8687,
        0x0065_7273,
        0x0053_5f5f,
        0x0043_4c4c,
    ],
    [
        0x00f0_f2f2,
        0x00e1_e5e5,
        0x00c5_cbcb,
        0x00a8_b1b2,
        0x008d_999a,
        0x0072_8182,
        0x0064_7071,
        0x0055_5f60,
        0x0046_5050,
        0x0039_4040,
    ],
    [
        0x00e8_eaec,
        0x00d0_d6da,
        0x00a4_adb6,
        0x0078_8793,
        0x0051_6272,
        0x002c_4052,
        0x0027_3848,
        0x0023_303d,
        0x001d_2a34,
        0x0019_222b,
    ],
    [
        0x00e7_e9eb,
        0x00cf_d3d7,
        0x00a1_a9b0,
        0x0074_808a,
        0x004b_5a67,
        0x0025_3645,
        0x0022_303d,
        0x001d_2a35,
        0x0019_242c,
        0x0016_1d25,
    ],
];

pub(super) fn presets() -> Vec<(String, String)> {
    COLUMNS
        .iter()
        .flatten()
        .map(|rgb| (String::new(), format!("#{rgb:06x}")))
        .collect()
}

pub(super) fn grid(editor: &Editor, model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let m = model.metrics;
    let theme = &model.theme;
    let selected = editor.input.read(cx).text().trim().to_owned();
    let columns: Vec<_> = editor
        .color_presets
        .chunks(10)
        .map(|column| {
            let buttons: Vec<_> = column
                .iter()
                .filter_map(|(_, hex)| {
                    let color = muxy_ui::theme::parse_hex(hex)?;
                    let active = selected.eq_ignore_ascii_case(hex);
                    let hex = hex.clone();
                    let id = format!("project-color-{hex}");
                    let tooltip = hex.clone();
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
                        .size(m.scaled(22.0))
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

pub(super) fn to_hsv(color: gpui::Rgba, hue: f32) -> [f32; 3] {
    let max = color.r.max(color.g).max(color.b);
    let min = color.r.min(color.g).min(color.b);
    let delta = max - min;
    let h = if delta == 0.0 {
        hue
    } else if color.r >= color.g && color.r >= color.b {
        ((color.g - color.b) / delta / 6.0).rem_euclid(1.0)
    } else if color.g >= color.b {
        ((color.b - color.r) / delta + 2.0) / 6.0
    } else {
        ((color.r - color.g) / delta + 4.0) / 6.0
    };
    [h, if max == 0.0 { 0.0 } else { delta / max }, max]
}

fn rgb([h, s, v]: [f32; 3]) -> gpui::Rgba {
    let lightness = v * (1.0 - s / 2.0);
    gpui::Hsla {
        h,
        s: if lightness == 0.0 || lightness >= 1.0 {
            0.0
        } else {
            (v - lightness) / lightness.min(1.0 - lightness)
        },
        l: lightness,
        a: 1.0,
    }
    .into()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "RGB channels are bounded to 0..1 and rounded to bytes"
)]
pub(super) fn hex(hsv: [f32; 3]) -> String {
    let color = rgb(hsv);
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8
    )
}

impl AppModel {
    pub(super) fn move_project_color(
        &mut self,
        hue: bool,
        bounds: gpui::Bounds<gpui::Pixels>,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::ProjectEditor(editor)) = &mut self.overlay else {
            return;
        };
        let x = (f32::from(position.x - bounds.left()) / f32::from(bounds.size.width).max(1.0))
            .clamp(0.0, 1.0);
        let y = (f32::from(position.y - bounds.top()) / f32::from(bounds.size.height).max(1.0))
            .clamp(0.0, 1.0);
        if hue {
            editor.hsv[0] = 1.0 - y;
        } else {
            editor.hsv[1] = x;
            editor.hsv[2] = 1.0 - y;
        }
        let hex = hex(editor.hsv);
        editor.input.update(cx, |input, cx| input.set_text(hex, cx));
        cx.notify();
    }
}

pub(super) fn picker(editor: &Editor, model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let style = controls::Style {
        theme: &model.theme,
        metrics: &model.metrics,
    };
    div()
        .flex()
        .flex_col()
        .gap(model.metrics.spacing3())
        .child(
            div().flex().gap(model.metrics.spacing2()).children(
                [
                    ("project-color-presets", muxy_ui::tr!("Presets"), false),
                    ("project-color-full", muxy_ui::tr!("Full"), true),
                ]
                .into_iter()
                .map(|(id, label, full)| {
                    controls::button(
                        style,
                        id,
                        &label,
                        true,
                        cx.listener(move |model, _, _, cx| {
                            if let Some(Overlay::ProjectEditor(editor)) = &mut model.overlay {
                                editor.full_color = full;
                                editor.color_drag = None;
                            }
                            cx.notify();
                        }),
                    )
                    .debug_selector(move || id.into())
                    .when(editor.full_color == full, |button| {
                        button.bg(model.theme.accent_soft)
                    })
                }),
            ),
        )
        .child(if editor.full_color {
            full_picker(editor, model, cx)
        } else {
            grid(editor, model, cx)
        })
        .into_any_element()
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep the two gradient controls and their interaction together"
)]
fn full_picker(editor: &Editor, model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    use gpui::{MouseButton, canvas, linear_gradient};
    use std::{cell::Cell, rc::Rc};
    let m = model.metrics;
    let mut surfaces = Vec::new();
    for hue in [false, true] {
        let bounds = Rc::new(Cell::new(None));
        let recorder = bounds.clone();
        let id = if hue {
            "project-color-hue"
        } else {
            "project-color-saturation"
        };
        let mut surface = div()
            .id(id)
            .debug_selector(move || id.into())
            .relative()
            .h(m.scaled(260.0))
            .cursor_crosshair()
            .when(hue, |surface| surface.w(m.scaled(28.0)))
            .when(!hue, Styled::flex_1)
            .child(
                canvas(move |area, _, _| recorder.set(Some(area)), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            );
        if hue {
            surface = surface
                .flex()
                .flex_col()
                .children((0_u8..6).map(|index| {
                    div().flex_1().w_full().bg(linear_gradient(
                        180.0,
                        gpui::linear_color_stop(rgb([1.0 - f32::from(index) / 6.0, 1.0, 1.0]), 0.0),
                        gpui::linear_color_stop(
                            rgb([1.0 - f32::from(index + 1) / 6.0, 1.0, 1.0]),
                            1.0,
                        ),
                    ))
                }))
                .child(
                    div()
                        .absolute()
                        .left(px(-3.0))
                        .right(px(-3.0))
                        .top(m.scaled(260.0) * (1.0 - editor.hsv[0]) - px(2.0))
                        .h(px(4.0))
                        .border_1()
                        .border_color(gpui::black())
                        .bg(gpui::white()),
                );
        } else {
            surface = surface
                .bg(linear_gradient(
                    90.0,
                    gpui::linear_color_stop(gpui::white(), 0.0),
                    gpui::linear_color_stop(rgb([editor.hsv[0], 1.0, 1.0]), 1.0),
                ))
                .child(div().absolute().size_full().bg(linear_gradient(
                    180.0,
                    gpui::linear_color_stop(gpui::transparent_black(), 0.0),
                    gpui::linear_color_stop(gpui::black(), 1.0),
                )))
                .child(
                    div()
                        .absolute()
                        .left(gpui::relative(editor.hsv[1]))
                        .top(gpui::relative(1.0 - editor.hsv[2]))
                        .child(
                            div()
                                .relative()
                                .left(px(-5.0))
                                .top(px(-5.0))
                                .size(px(10.0))
                                .rounded_full()
                                .border_2()
                                .border_color(gpui::white())
                                .shadow_sm(),
                        ),
                );
        }
        surfaces.push(
            surface
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |model, event: &gpui::MouseDownEvent, _, cx| {
                        if let Some(bounds) = bounds.get() {
                            if let Some(Overlay::ProjectEditor(editor)) = &mut model.overlay {
                                editor.color_drag = Some((hue, bounds));
                            }
                            model.move_project_color(hue, bounds, event.position, cx);
                        }
                        cx.stop_propagation();
                    }),
                )
                .into_any_element(),
        );
    }
    div()
        .flex()
        .gap(m.spacing5())
        .children(surfaces)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn hsv_round_trips_and_retains_hue_for_neutrals() {
        for color in super::COLUMNS.iter().flatten().copied().chain([
            0,
            0x00ff_ffff,
            0x00ff_0000,
            0x0000_ff00,
            0x0000_00ff,
        ]) {
            let original = gpui::rgb(color);
            let result = super::rgb(super::to_hsv(original, 0.5));
            assert!((original.r - result.r).abs() < 0.00001);
            assert!((original.g - result.g).abs() < 0.00001);
            assert!((original.b - result.b).abs() < 0.00001);
        }
        assert!((super::to_hsv(gpui::black().into(), 0.7)[0] - 0.7).abs() < f32::EPSILON);
        assert!((super::to_hsv(gpui::white().into(), 0.7)[0] - 0.7).abs() < f32::EPSILON);
    }
}
