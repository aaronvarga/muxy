//! Bounded submission tickets. An evicted or unknown operation is uncertain, never a new send.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use muxy_protocol::{ChatImage, ChatSelection, ChatSubmission, OperationId, SubmissionOutcome};
use ring::digest::{Context, SHA256};

const LIMIT: usize = 256;
const ACK_TIMEOUT: Duration = Duration::from_secs(15);

struct Entry {
    status: ChatSubmission,
    fingerprint: Option<[u8; 32]>,
    started: Instant,
}

#[derive(Default)]
pub(crate) struct Submissions {
    entries: VecDeque<Entry>,
}

pub(crate) enum Start {
    New,
    Existing(ChatSubmission),
}

impl Submissions {
    pub(crate) fn prepare(&mut self, generation: u64) -> Option<OperationId> {
        if self.entries.len() >= LIMIT {
            let at = self.entries.iter().position(|entry| {
                entry.fingerprint.is_none()
                    || matches!(
                        entry.status.outcome,
                        SubmissionOutcome::Accepted | SubmissionOutcome::Rejected
                    )
            })?;
            self.entries.remove(at);
        }
        let operation = OperationId::new();
        self.entries.push_back(Entry {
            status: ChatSubmission {
                operation,
                outcome: SubmissionOutcome::Pending,
                generation,
                detail: None,
            },
            fingerprint: None,
            started: Instant::now(),
        });
        Some(operation)
    }

    pub(crate) fn start(
        &mut self,
        operation: OperationId,
        fingerprint: [u8; 32],
        generation: u64,
    ) -> Result<Start, &'static str> {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.status.operation == operation)
        else {
            return Ok(Start::Existing(unknown(operation, generation)));
        };
        if let Some(original) = entry.fingerprint {
            if original != fingerprint {
                return Err("This operation already names a different message");
            }
            return Ok(Start::Existing(entry.status.clone()));
        }
        if entry.status.generation != generation {
            return Err("The conversation changed before submission");
        }
        entry.fingerprint = Some(fingerprint);
        entry.started = Instant::now();
        Ok(Start::New)
    }

    pub(crate) fn update(
        &mut self,
        operation: OperationId,
        generation: u64,
        outcome: SubmissionOutcome,
        detail: Option<String>,
    ) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.status.operation == operation)
        {
            // A late transport failure cannot undo provider acknowledgment.
            if entry.status.outcome == SubmissionOutcome::Accepted {
                return;
            }
            entry.status.generation = generation;
            entry.status.outcome = outcome;
            entry.status.detail = detail;
        }
    }

    pub(crate) fn status(&mut self, operation: OperationId, generation: u64) -> ChatSubmission {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.status.operation == operation)
        else {
            return unknown(operation, generation);
        };
        if entry.fingerprint.is_some()
            && entry.status.outcome == SubmissionOutcome::Pending
            && entry.started.elapsed() > ACK_TIMEOUT
        {
            entry.status.outcome = SubmissionOutcome::Uncertain;
            entry.status.detail =
                Some("Provider acknowledgment is missing. The message has not been resent.".into());
        }
        entry.status.clone()
    }
}

fn unknown(operation: OperationId, generation: u64) -> ChatSubmission {
    ChatSubmission {
        operation,
        generation,
        outcome: SubmissionOutcome::Uncertain,
        detail: Some(
            "The server no longer has this operation. Check native history before sending again."
                .into(),
        ),
    }
}

pub(crate) fn fingerprint(
    selection: Option<&ChatSelection>,
    generation: u64,
    text: &str,
    images: &[ChatImage],
) -> [u8; 32] {
    let mut hash = Context::new(&SHA256);
    hash.update(&generation.to_be_bytes());
    if let Some(selection) = selection {
        hash.update(selection_bytes(selection).as_bytes());
    }
    hash.update(&(text.len() as u64).to_be_bytes());
    hash.update(text.as_bytes());
    for image in images {
        hash.update(image.mime.mime().as_bytes());
        hash.update(&(image.bytes.len() as u64).to_be_bytes());
        hash.update(&image.bytes);
    }
    let digest = hash.finish();
    let mut result = [0; 32];
    result.copy_from_slice(digest.as_ref());
    result
}

fn selection_bytes(selection: &ChatSelection) -> String {
    serde_json::to_string(selection).unwrap_or_default()
}

pub(crate) fn native_id(operation: OperationId) -> String {
    let id = operation.to_string();
    format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lost_ack_retries_and_evicted_ids_never_execute_a_second_turn() {
        let mut submissions = Submissions::default();
        let operation = submissions.prepare(1).unwrap();
        assert!(matches!(
            submissions.start(operation, [1; 32], 1),
            Ok(Start::New)
        ));
        assert!(matches!(
            submissions.start(operation, [1; 32], 1),
            Ok(Start::Existing(_))
        ));
        assert!(submissions.start(operation, [2; 32], 1).is_err());
        submissions.update(operation, 1, SubmissionOutcome::Accepted, None);
        submissions.update(operation, 1, SubmissionOutcome::Uncertain, None);
        assert_eq!(
            submissions.status(operation, 1).outcome,
            SubmissionOutcome::Accepted
        );
        for _ in 0..LIMIT {
            submissions.prepare(1).unwrap();
        }
        assert!(matches!(
            submissions.start(operation, [1; 32], 1),
            Ok(Start::Existing(ChatSubmission {
                outcome: SubmissionOutcome::Uncertain,
                ..
            }))
        ));
    }
    #[test]
    fn pending_operations_are_not_evicted_and_old_generations_are_refused() {
        let mut submissions = Submissions::default();
        let operation = submissions.prepare(1).unwrap();
        assert!(submissions.start(operation, [1; 32], 2).is_err());
        for index in 0..LIMIT {
            let operation = if index == 0 {
                operation
            } else {
                submissions.prepare(1).unwrap()
            };
            assert!(matches!(
                submissions.start(operation, [1; 32], 1),
                Ok(Start::New)
            ));
        }
        assert!(submissions.prepare(1).is_none());
    }
}
