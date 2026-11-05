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
        0xfae8e7, 0xf3d1d0, 0xe7a6a1, 0xda7c76, 0xce554d, 0xc0302a, 0xa62b25, 0x8d2521, 0x75201c,
        0x5d1b17,
    ],
    [
        0xfeeae9, 0xfcd6d3, 0xf9aea9, 0xf78881, 0xf2645b, 0xee4139, 0xcd3932, 0xae322c, 0x8f2a25,
        0x71231e,
    ],
    [
        0xf4ebf6, 0xead9ed, 0xd5b5dc, 0xc091ca, 0xab6fba, 0x964fa9, 0x824592, 0x6f3c7c, 0x5c3266,
        0x4a2953,
    ],
    [
        0xf3e9f5, 0xe6d5eb, 0xcfabd7, 0xb784c4, 0xa15eb1, 0x893c9f, 0x77358a, 0x652e75, 0x542761,
        0x44204e,
    ],
    [
        0xe6f0f6, 0xcce2ee, 0x9bc5dd, 0x6eaacd, 0x418fbd, 0x1375ad, 0x116696, 0x11577f, 0x0f4969,
        0x0e3b54,
    ],
    [
        0xe7f4fa, 0xcee7f6, 0xa0d1ed, 0x72bae3, 0x47a4da, 0x198ed2, 0x187bb6, 0x166999, 0x13577e,
        0x124665,
    ],
    [
        0xe3f7f4, 0xc6f0e8, 0x90e0d2, 0x5ad2bd, 0x1ac2a8, 0x00b493, 0x009b7f, 0x00846d, 0x006d5a,
        0x005748,
    ],
    [
        0xe3f5f1, 0xc6e9e3, 0x90d4c8, 0x5bbfae, 0x22aa93, 0x00967b, 0x00826b, 0x006f5b, 0x005b4c,
        0x00493d,
    ],
    [
        0xe4f6ed, 0xcbeddb, 0x98dbb9, 0x65c798, 0x30b679, 0x00a55b, 0x008e4f, 0x007944, 0x006439,
        0x00502e,
    ],
    [
        0xe5f9ef, 0xcbf4e0, 0x98e8c1, 0x67dca3, 0x2fd087, 0x00c56c, 0x00ab5e, 0x009051, 0x007843,
        0x005f37,
    ],
    [
        0xfef8e5, 0xfcf1cb, 0xf9e39b, 0xf8d66f, 0xf5c949, 0xf3bc32, 0xd1a22b, 0xb18a25, 0x91711f,
        0x735b19,
    ],
    [
        0xfff3e4, 0xfee8cc, 0xfcd19a, 0xfbbc6e, 0xf9a646, 0xf8902b, 0xd67d26, 0xb56b21, 0x96591c,
        0x764717,
    ],
    [
        0xfef0e6, 0xfbe1ce, 0xf7c4a0, 0xf3a874, 0xef8c4d, 0xea722c, 0xca6327, 0xab5522, 0x8d471d,
        0x703918,
    ],
    [
        0xfcebe3, 0xf7d8c7, 0xefb291, 0xe88d60, 0xdf6a35, 0xd64918, 0xb94015, 0x9c3712, 0x812f0f,
        0x67260c,
    ],
    [
        0xfdfefe, 0xfafcfc, 0xf6f8f8, 0xf2f5f6, 0xedf1f2, 0xe9eeef, 0xc9cdce, 0xaaadae, 0x8c8f8f,
        0x707272,
    ],
    [
        0xf7f8f8, 0xf0f1f2, 0xe1e3e6, 0xd1d6d8, 0xc2c9cc, 0xb4bbbf, 0x9ba2a5, 0x85898c, 0x6d7274,
        0x575a5c,
    ],
    [
        0xf2f5f5, 0xe6eaea, 0xced6d6, 0xb6c2c3, 0x9faeaf, 0x889b9c, 0x768687, 0x657273, 0x535f5f,
        0x434c4c,
    ],
    [
        0xf0f2f2, 0xe1e5e5, 0xc5cbcb, 0xa8b1b2, 0x8d999a, 0x728182, 0x647071, 0x555f60, 0x465050,
        0x394040,
    ],
    [
        0xe8eaec, 0xd0d6da, 0xa4adb6, 0x788793, 0x516272, 0x2c4052, 0x273848, 0x23303d, 0x1d2a34,
        0x19222b,
    ],
    [
        0xe7e9eb, 0xcfd3d7, 0xa1a9b0, 0x74808a, 0x4b5a67, 0x253645, 0x22303d, 0x1d2a35, 0x19242c,
        0x161d25,
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
    } else if max == color.r {
        ((color.g - color.b) / delta / 6.0).rem_euclid(1.0)
    } else if max == color.g {
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
        s: if lightness == 0.0 || lightness == 1.0 {
            0.0
        } else {
            (v - lightness) / lightness.min(1.0 - lightness)
        },
        l: lightness,
        a: 1.0,
    }
    .into()
}

impl AppModel {
    fn move_project_color(
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
            editor.hsv[0] = y;
        } else {
            editor.hsv[1] = x;
            editor.hsv[2] = 1.0 - y;
        }
        let color = rgb(editor.hsv);
        let hex = format!(
            "#{:02x}{:02x}{:02x}",
            (color.r * 255.0).round() as u8,
            (color.g * 255.0).round() as u8,
            (color.b * 255.0).round() as u8
        );
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
                        button.bg(model.theme.selected())
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

fn full_picker(editor: &Editor, model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    use gpui::{MouseButton, canvas, linear_gradient};
    use std::{cell::Cell, rc::Rc};
    let m = model.metrics;
    let mut surfaces = Vec::new();
    for hue in [false, true] {
        let bounds = Rc::new(Cell::new(None));
        let recorder = bounds.clone();
        let track = bounds.clone();
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
            .when(!hue, |surface| surface.flex_1())
            .child(
                canvas(move |area, _, _| recorder.set(Some(area)), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            );
        if hue {
            surface = surface
                .flex()
                .flex_col()
                .children((0..6).map(|index| {
                    div().flex_1().w_full().bg(linear_gradient(
                        180.0,
                        rgb([index as f32 / 6.0, 1.0, 1.0]),
                        rgb([(index + 1) as f32 / 6.0, 1.0, 1.0]),
                    ))
                }))
                .child(
                    div()
                        .absolute()
                        .left(px(-3.0))
                        .right(px(-3.0))
                        .top(m.scaled(260.0) * editor.hsv[0] - px(2.0))
                        .h(px(4.0))
                        .border_1()
                        .border_color(gpui::black())
                        .bg(gpui::white()),
                );
        } else {
            surface = surface
                .bg(linear_gradient(
                    90.0,
                    gpui::white(),
                    rgb([editor.hsv[0], 1.0, 1.0]),
                ))
                .child(div().absolute().size_full().bg(linear_gradient(
                    180.0,
                    gpui::transparent_black(),
                    gpui::black(),
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
        surfaces.push(surface.on_mouse_down(MouseButton::Left, cx.listener(move |model, event: &gpui::MouseDownEvent, _, cx| {
            if let Some(Overlay::ProjectEditor(editor)) = &mut model.overlay { editor.color_drag = Some(hue); }
            if let Some(bounds) = track.get() { model.move_project_color(hue, bounds, event.position, cx); }
            cx.stop_propagation();
        })).on_mouse_move(cx.listener(move |model, event: &gpui::MouseMoveEvent, _, cx| {
            if event.pressed_button == Some(MouseButton::Left)
                && matches!(&model.overlay, Some(Overlay::ProjectEditor(editor)) if editor.color_drag == Some(hue))
                && let Some(bounds) = bounds.get() {
                model.move_project_color(hue, bounds, event.position, cx);
            }
        })).on_mouse_up(MouseButton::Left, cx.listener(|model, _, _, _| {
            if let Some(Overlay::ProjectEditor(editor)) = &mut model.overlay { editor.color_drag = None; }
        })).on_mouse_up_out(MouseButton::Left, cx.listener(|model, _, _, _| {
            if let Some(Overlay::ProjectEditor(editor)) = &mut model.overlay { editor.color_drag = None; }
        })).into_any_element());
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
        for color in super::COLUMNS
            .iter()
            .flatten()
            .copied()
            .chain([0, 0xffffff, 0xff0000, 0x00ff00, 0x0000ff])
        {
            let original = gpui::rgb(color);
            let result = super::rgb(super::to_hsv(original, 0.5));
            assert!((original.r - result.r).abs() < 0.00001);
            assert!((original.g - result.g).abs() < 0.00001);
            assert!((original.b - result.b).abs() < 0.00001);
        }
        assert_eq!(super::to_hsv(gpui::black().into(), 0.7)[0], 0.7);
        assert_eq!(super::to_hsv(gpui::white().into(), 0.7)[0], 0.7);
    }
}
