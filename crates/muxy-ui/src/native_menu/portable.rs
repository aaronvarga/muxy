use super::Item;
use crate::dialog::{self, DialogOptions};

pub fn show(
    items: Vec<Item>,
    _anchor: gpui::Point<gpui::Pixels>,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> async_channel::Receiver<Option<usize>> {
    let available: Vec<_> = items
        .into_iter()
        .enumerate()
        .filter(|(_, item)| item.enabled)
        .collect();
    let labels: Vec<_> = available
        .iter()
        .map(|(_, item)| {
            if item.checked {
                format!("✓ {}", item.label)
            } else {
                item.label.clone()
            }
        })
        .collect();
    let cancel = crate::tr!("Cancel").to_string();
    let mut buttons = labels.clone();
    buttons.push(cancel.clone());
    let (send, receive) = async_channel::bounded(1);
    let (response, pending) = async_channel::bounded(1);
    let prompt = dialog::present(
        window,
        cx,
        DialogOptions {
            title: crate::tr!("Composer").to_string(),
            message: String::new(),
            buttons,
            default_button: Some(cancel.clone()),
            cancel_button: Some(cancel),
            input: None,
            style: String::new(),
        },
        move |answer| {
            let _ = response.try_send(answer);
        },
    );
    match prompt {
        Ok(prompt) => {
            cx.spawn(async move |_| {
                let answer = pending.recv().await.ok().flatten();
                let selected =
                    answer.and_then(|answer| labels.iter().position(|label| *label == answer));
                let _ = send.try_send(selected.map(|index| available[index].0));
                drop(prompt);
            })
            .detach();
        }
        Err(_) => {
            let _ = send.try_send(None);
        }
    }
    receive
}
