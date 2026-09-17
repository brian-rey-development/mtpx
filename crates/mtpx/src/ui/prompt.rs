//! Pickers shown on stderr when a choice is needed and a terminal is there to make it.

use crate::ui::format;
use console::Term;
use dialoguer::Select;
use mtpx_core::{DeviceSummary, StorageSummary};
use std::io;

const DEVICE_PROMPT: &str = "Several devices are attached, pick one";
const STORAGE_PROMPT: &str = "The device has several storages, pick one";

/// What a picker came back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The index the user confirmed.
    Picked(usize),
    /// Escape, `q`, an empty list, or a terminal that refused the prompt.
    Dismissed,
    /// Ctrl-C while the picker was open.
    Interrupted,
}

pub fn pick_device(devices: &[DeviceSummary]) -> Choice {
    let items: Vec<String> = devices.iter().map(format::device).collect();
    select(DEVICE_PROMPT, &items)
}

pub fn pick_storage(storages: &[StorageSummary]) -> Choice {
    let items: Vec<String> = storages.iter().map(format::storage).collect();
    select(STORAGE_PROMPT, &items)
}

fn select(prompt: &str, items: &[String]) -> Choice {
    if items.is_empty() {
        return Choice::Dismissed;
    }
    let term = Term::stderr();
    let answer = Select::new()
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact_on_opt(&term);
    let choice = choice_from(answer);
    // dialoguer hides the cursor before reading keys and only restores it on Enter or Escape,
    // so a Ctrl-C that escapes through `?` leaves the terminal without one.
    if choice == Choice::Interrupted {
        let _ = term.show_cursor();
    }
    choice
}

fn choice_from(answer: dialoguer::Result<Option<usize>>) -> Choice {
    match answer {
        Ok(Some(index)) => Choice::Picked(index),
        Err(dialoguer::Error::IO(error)) if error.kind() == io::ErrorKind::Interrupted => {
            Choice::Interrupted
        }
        Ok(None) | Err(_) => Choice::Dismissed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ctrl_c_in_the_picker_is_an_interrupt_and_other_failures_a_dismissal() {
        assert_eq!(choice_from(Ok(Some(2))), Choice::Picked(2));
        assert_eq!(choice_from(Ok(None)), Choice::Dismissed);
        let interrupted = io::Error::from(io::ErrorKind::Interrupted);
        assert_eq!(
            choice_from(Err(dialoguer::Error::IO(interrupted))),
            Choice::Interrupted
        );
        let refused = io::Error::from(io::ErrorKind::NotConnected);
        assert_eq!(
            choice_from(Err(dialoguer::Error::IO(refused))),
            Choice::Dismissed
        );
    }
}
