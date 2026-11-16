//! Fork: each connection's chat queues, one per subscribed session, apart
//! from the control queue.
//!
//! The session owner pushes every chat event once per subscriber, already
//! numbered by revision. A queue merges only adjacent events (streamed text for
//! the same item, consecutive states), closes a batch at 256 events, at about
//! `MAX_CHAT_BATCH_BYTES`, or 50 ms after its first event, and validates it
//! before it can be sent. A subscriber that falls more than 4 MiB behind loses
//! its queue and gets one `Resync`, then nothing until it subscribes again.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use muxy_protocol::{
    ChatBatch, ChatEvent, ChatItemKind, MAX_CHAT_BATCH_BYTES, MAX_CHAT_EVENTS, MAX_CHAT_TEXT,
    PermissionKind, SessionId,
};

/// How long a batch collects events before it may be sent.
const FLUSH: Duration = Duration::from_millis(50);
/// Unsent ordinary batches past this many bytes mean the subscriber can't keep up.
pub(crate) const MAX_UNSENT: usize = 4 << 20;

#[derive(Default)]
pub(super) struct ChatQueues {
    queues: HashMap<SessionId, Queue>,
    /// Round-robin order of the subscribed sessions.
    order: VecDeque<SessionId>,
}

struct Queue {
    generation: u64,
    /// The latest revision pushed, for a `Resync`.
    revision: u64,
    snapshot: Option<ChatBatch>,
    ready: VecDeque<(ChatBatch, usize)>,
    open: Option<Open>,
    /// Estimated bytes of `ready` and `open`.
    unsent: usize,
    /// After a `Resync`, nothing more is queued until the next subscribe.
    stalled: bool,
}

struct Open {
    batch: ChatBatch,
    bytes: usize,
    due: Instant,
}

impl ChatQueues {
    /// Starts the session's queue over with its snapshot, which travels alone
    /// and outside the 4 MiB budget.
    pub(super) fn subscribe(&mut self, snapshot: ChatBatch) {
        let session = snapshot.session;
        let mut queue = Queue {
            generation: snapshot.generation,
            revision: snapshot.to_rev,
            snapshot: None,
            ready: VecDeque::new(),
            open: None,
            unsent: 0,
            stalled: false,
        };
        if snapshot.validate().is_ok() {
            queue.snapshot = Some(snapshot);
        } else {
            log::error!("chat snapshot for session {} is invalid", session.get());
            queue.resync(session);
        }
        self.queues.insert(session, queue);
        if !self.order.contains(&session) {
            self.order.push_back(session);
        }
    }

    pub(super) fn unsubscribe(&mut self, session: SessionId) {
        self.queues.remove(&session);
        self.order.retain(|queued| *queued != session);
    }

    /// Queues events `from..to` of the session's chat. Returns false when this
    /// connection no longer takes them: it unsubscribed, or it fell behind and
    /// was told to resync.
    pub(super) fn push(
        &mut self,
        session: SessionId,
        generation: u64,
        (from, to): (u64, u64),
        events: Vec<ChatEvent>,
        now: Instant,
    ) -> bool {
        let Some(queue) = self.queues.get_mut(&session) else {
            return false;
        };
        if queue.stalled || queue.generation != generation {
            return false;
        }
        if queue.revision != from {
            // A gap would be applied as missing events; let the client start over.
            log::error!(
                "chat revisions for session {} skipped {}..{from}",
                session.get(),
                queue.revision
            );
            queue.resync(session);
            return false;
        }
        queue.revision = to;
        let mut rev = from;
        for event in events {
            rev += 1;
            queue.append(session, generation, rev, event, now);
        }
        if queue.unsent > MAX_UNSENT {
            queue.resync(session);
            return false;
        }
        true
    }

    /// Whether a batch can be sent now, and else when the next one will be.
    pub(super) fn ready(&self, now: Instant) -> (bool, Option<Instant>) {
        let mut due = None::<Instant>;
        for queue in self.queues.values() {
            if queue.snapshot.is_some() || !queue.ready.is_empty() {
                return (true, None);
            }
            if let Some(open) = &queue.open {
                if open.due <= now {
                    return (true, None);
                }
                due = Some(due.map_or(open.due, |due| due.min(open.due)));
            }
        }
        (false, due)
    }

    /// The next batch, taking sessions in turn.
    pub(super) fn pop(&mut self, now: Instant) -> Option<ChatBatch> {
        for _ in 0..self.order.len() {
            let session = self.order.pop_front()?;
            self.order.push_back(session);
            if let Some(queue) = self.queues.get_mut(&session)
                && let Some(batch) = queue.pop(session, now)
            {
                return Some(batch);
            }
        }
        None
    }

    pub(super) fn clear(&mut self) {
        self.queues.clear();
        self.order.clear();
    }
}

impl Queue {
    fn append(
        &mut self,
        session: SessionId,
        generation: u64,
        rev: u64,
        event: ChatEvent,
        now: Instant,
    ) {
        let bytes = estimate(&event);
        if self.open.as_ref().is_some_and(|open| {
            open.batch.events.len() >= MAX_CHAT_EVENTS || open.bytes + bytes > MAX_CHAT_BATCH_BYTES
        }) {
            self.close(session);
        }
        let created = self.open.is_none();
        let open = self.open.get_or_insert_with(|| Open {
            batch: ChatBatch {
                session,
                generation,
                from_rev: rev - 1,
                to_rev: rev - 1,
                events: Vec::new(),
                snapshot: None,
            },
            bytes: 64,
            due: now + FLUSH,
        });
        open.batch.to_rev = rev;
        // `unsent` grows by exactly what `open.bytes` does, so `pop` balances it.
        let before = if created { 0 } else { open.bytes };
        match (open.batch.events.last_mut(), &event) {
            (
                Some(ChatEvent::Delta { item_id, text }),
                ChatEvent::Delta {
                    item_id: next,
                    text: more,
                },
            ) if item_id == next && text.len() + more.len() <= MAX_CHAT_TEXT => {
                text.push_str(more);
                open.bytes += more.len();
            }
            (Some(last @ ChatEvent::State(_)), ChatEvent::State(_)) => *last = event,
            _ => {
                open.bytes += bytes;
                open.batch.events.push(event);
            }
        }
        self.unsent += open.bytes - before;
    }

    /// Moves the open batch to `ready`, or resyncs if it isn't valid.
    fn close(&mut self, session: SessionId) {
        let Some(open) = self.open.take() else {
            return;
        };
        if open.batch.validate().is_err() {
            log::error!("chat batch for session {} is invalid", session.get());
            self.resync(session);
            return;
        }
        self.ready.push_back((open.batch, open.bytes));
    }

    fn pop(&mut self, session: SessionId, now: Instant) -> Option<ChatBatch> {
        if let Some(snapshot) = self.snapshot.take() {
            return Some(snapshot);
        }
        if self.ready.is_empty() && self.open.as_ref().is_some_and(|open| open.due <= now) {
            self.close(session);
        }
        let (batch, bytes) = self.ready.pop_front()?;
        self.unsent = self.unsent.saturating_sub(bytes);
        Some(batch)
    }

    /// Drops everything unsent and queues exactly one `Resync`.
    fn resync(&mut self, session: SessionId) {
        self.snapshot = None;
        self.open = None;
        self.ready.clear();
        self.unsent = 0;
        self.stalled = true;
        self.ready.push_back((
            ChatBatch {
                session,
                generation: self.generation,
                from_rev: self.revision,
                to_rev: self.revision,
                events: vec![ChatEvent::Resync],
                snapshot: None,
            },
            0,
        ));
    }
}

/// An over-estimate of an event's encoded size: its strings plus framing.
fn estimate(event: &ChatEvent) -> usize {
    const FIELD: usize = 16;
    let strings: usize = match event {
        ChatEvent::Upsert(item) => {
            item.id.len()
                + item.turn_id.len()
                + match &item.kind {
                    ChatItemKind::User { text }
                    | ChatItemKind::Thinking { text }
                    | ChatItemKind::Notice { text }
                    | ChatItemKind::Assistant { markdown: text, .. } => text.len(),
                    ChatItemKind::Tool {
                        name,
                        summary,
                        input_preview,
                        result,
                        ..
                    } => {
                        name.len()
                            + summary.len()
                            + input_preview.len()
                            + result.as_ref().map_or(0, String::len)
                    }
                    ChatItemKind::TurnEnd { changed, .. } => {
                        changed.iter().map(|change| change.path.len() + 8).sum()
                    }
                    ChatItemKind::Folded { .. } => 0,
                }
        }
        ChatEvent::Delta { item_id, text } => item_id.len() + text.len(),
        ChatEvent::State(state) => {
            state.model.as_ref().map_or(0, String::len)
                + state.effort.as_ref().map_or(0, String::len)
        }
        ChatEvent::PermissionAdded(prompt) => {
            prompt.request_id.len()
                + prompt.tool_use_id.as_ref().map_or(0, String::len)
                + match &prompt.kind {
                    PermissionKind::Tool {
                        tool,
                        title,
                        detail,
                        always,
                    } => {
                        tool.len()
                            + title.len()
                            + detail.len()
                            + always.as_ref().map_or(0, |always| always.label.len())
                    }
                    PermissionKind::Questions { questions } => questions
                        .iter()
                        .map(|question| {
                            question.question.len()
                                + question.header.len()
                                + question
                                    .options
                                    .iter()
                                    .map(|option| {
                                        option.label.len() + option.description.len() + 2 * FIELD
                                    })
                                    .sum::<usize>()
                                + 4 * FIELD
                        })
                        .sum(),
                    PermissionKind::Plan { markdown } => markdown.len(),
                    PermissionKind::Native {
                        title,
                        detail,
                        choices,
                    } => {
                        title.len()
                            + detail.len()
                            + choices
                                .iter()
                                .map(|choice| {
                                    choice.id.len() + choice.label.len() + choice.scope.len()
                                })
                                .sum::<usize>()
                    }
                }
        }
        ChatEvent::PermissionRemoved { request_id, .. } => request_id.len(),
        ChatEvent::Error { text }
        | ChatEvent::Exited { reason: text }
        | ChatEvent::Blocked { reason: text } => text.len(),
        ChatEvent::Resync => 0,
    };
    strings + 8 * FIELD
}

#[cfg(test)]
mod tests {
    use muxy_protocol::{ChatItem, ChatState};

    use super::*;

    fn session() -> SessionId {
        SessionId::from(std::num::NonZeroU64::MIN)
    }

    fn snapshot(rev: u64) -> ChatBatch {
        ChatBatch {
            session: session(),
            generation: 1,
            from_rev: rev,
            to_rev: rev,
            events: Vec::new(),
            snapshot: Some(Box::new(muxy_protocol::ChatSnapshot {
                subscription: 3,
                items: Vec::new(),
                state: ChatState::default(),
                capabilities: muxy_protocol::ChatCapabilities::default(),
                pending: Vec::new(),
                has_earlier: false,
            })),
        }
    }

    fn delta(id: &str, text: &str) -> ChatEvent {
        ChatEvent::Delta {
            item_id: id.into(),
            text: text.into(),
        }
    }

    fn upsert(id: &str) -> ChatEvent {
        ChatEvent::Upsert(ChatItem {
            id: id.into(),
            turn_id: "turn:0".into(),
            seq: 0,
            kind: ChatItemKind::Notice { text: "n".into() },
        })
    }

    fn drain(queues: &mut ChatQueues, now: Instant) -> Vec<ChatBatch> {
        std::iter::from_fn(|| queues.pop(now)).collect()
    }

    #[test]
    fn snapshot_first_then_contiguous_merged_batches() {
        let mut queues = ChatQueues::default();
        let now = Instant::now();
        queues.subscribe(snapshot(4));
        assert!(queues.push(
            session(),
            1,
            (4, 6),
            vec![delta("a", "x"), delta("a", "y")],
            now
        ));
        assert!(queues.push(
            session(),
            1,
            (6, 10),
            vec![
                delta("a", "z"),
                upsert("a"),
                delta("a", "w"),
                ChatEvent::State(ChatState::default())
            ],
            now
        ));
        assert!(queues.push(
            session(),
            1,
            (10, 11),
            vec![ChatEvent::State(ChatState {
                busy: true,
                ..ChatState::default()
            })],
            now
        ));
        assert!(queues.ready(now).0, "the snapshot is ready at once");
        let first = drain(&mut queues, now);
        assert_eq!(first.len(), 1, "the open batch waits for its flush");
        assert!(first[0].snapshot.is_some());
        let later = now + FLUSH;
        let batches = drain(&mut queues, later);
        assert_eq!(batches.len(), 1);
        let batch = &batches[0];
        assert_eq!((batch.from_rev, batch.to_rev), (4, 11));
        assert_eq!(
            batch.events,
            vec![
                delta("a", "xyz"),
                upsert("a"),
                delta("a", "w"),
                ChatEvent::State(ChatState {
                    busy: true,
                    ..ChatState::default()
                })
            ],
            "only adjacent deltas and states merge; the upsert is a barrier"
        );
    }

    #[test]
    fn a_subscriber_that_falls_behind_gets_one_resync_then_nothing() {
        let mut queues = ChatQueues::default();
        let now = Instant::now();
        queues.subscribe(snapshot(0));
        let text = "x".repeat(MAX_CHAT_TEXT);
        let mut rev = 0;
        let mut accepted = true;
        while accepted {
            accepted = queues.push(
                session(),
                1,
                (rev, rev + 1),
                vec![upsert(&text[..200])],
                now,
            ) && queues.push(
                session(),
                1,
                (rev + 1, rev + 2),
                vec![delta(&rev.to_string(), &text)],
                now,
            );
            rev += 2;
            assert!(rev < 1000, "never overflowed");
        }
        assert!(!queues.push(session(), 1, (rev, rev + 1), vec![upsert("late")], now));
        let batches = drain(&mut queues, now + FLUSH);
        assert_eq!(batches.len(), 1, "everything unsent was dropped");
        assert_eq!(batches[0].events, vec![ChatEvent::Resync]);

        queues.subscribe(snapshot(rev + 1));
        assert!(queues.push(session(), 1, (rev + 1, rev + 2), vec![upsert("again")], now));
        assert_eq!(
            drain(&mut queues, now + FLUSH).len(),
            2,
            "subscribing again unlocks it"
        );
    }

    #[test]
    fn merged_streams_never_leak_the_unsent_budget() {
        let mut queues = ChatQueues::default();
        let mut now = Instant::now();
        queues.subscribe(snapshot(0));
        let mut rev = 0;
        let mut sent = Vec::new();
        for round in 0..10_000 {
            let events = vec![
                delta("a", "token "),
                delta("a", "more "),
                ChatEvent::State(ChatState::default()),
                ChatEvent::State(ChatState::default()),
            ];
            assert!(queues.push(session(), 1, (rev, rev + 4), events, now));
            rev += 4;
            if round % 10 == 0 {
                now += FLUSH;
                sent.extend(drain(&mut queues, now));
            }
        }
        sent.extend(drain(&mut queues, now + FLUSH));
        assert!(sent.iter().all(|batch| batch.events != [ChatEvent::Resync]));
        assert_eq!(sent.last().map(|batch| batch.to_rev), Some(rev));
        assert_eq!(queues.queues[&session()].unsent, 0);
    }

    #[test]
    fn batches_close_at_the_event_cap() {
        let mut queues = ChatQueues::default();
        let now = Instant::now();
        queues.subscribe(snapshot(0));
        let events: Vec<_> = (0..=MAX_CHAT_EVENTS)
            .map(|index| upsert(&index.to_string()))
            .collect();
        let count = events.len() as u64;
        assert!(queues.push(session(), 1, (0, count), events, now));
        let batches = drain(&mut queues, now + FLUSH);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[1].events.len(), MAX_CHAT_EVENTS);
        assert_eq!(
            (batches[1].to_rev, batches[2].from_rev, batches[2].to_rev),
            (MAX_CHAT_EVENTS as u64, MAX_CHAT_EVENTS as u64, count)
        );
    }
}
