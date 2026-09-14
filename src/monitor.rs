use std::io::{Error, Result};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::link::{ObservedLink, Topology};
use crate::mib::Mib;

const INITIAL_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
const EVENT_RESET: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct TableReader(Arc<RwLock<Option<Arc<Mib>>>>);

struct TablePublisher(Arc<RwLock<Option<Arc<Mib>>>>);

pub fn publication() -> TableReader {
    TableReader(Arc::new(RwLock::new(None)))
}

impl TableReader {
    pub fn snapshot(&self) -> Option<Arc<Mib>> {
        self.0
            .read()
            .expect("table publication lock poisoned")
            .clone()
    }

    fn publisher(&self) -> TablePublisher {
        TablePublisher(Arc::clone(&self.0))
    }
}

impl TablePublisher {
    fn publish(&self, mib: Mib) {
        *self.0.write().expect("table publication lock poisoned") = Some(Arc::new(mib));
    }

    fn unavailable(&self) {
        *self.0.write().expect("table publication lock poisoned") = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Attempt {
    pub continuity: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventBatch {
    pub first: Option<Duration>,
    pub last: Option<Duration>,
}

impl EventBatch {
    #[cfg(test)]
    pub fn one(at: Duration) -> Self {
        Self {
            first: Some(at),
            last: Some(at),
        }
    }
}

#[derive(Debug)]
pub struct Inventory {
    pub attempt: Attempt,
    pub links: Vec<ObservedLink>,
    pub events: EventBatch,
}

#[derive(Debug)]
pub enum AcquisitionError {
    Ordinary { error: Error, events: EventBatch },
    Lost { error: Error, events: EventBatch },
}

#[derive(Debug)]
pub enum WaitOutcome {
    Deadline,
    LinkChanged(Duration),
    #[cfg(test)]
    Shutdown,
}

pub trait Acquisition {
    fn now(&self) -> Duration;
    fn subscribe(&mut self) -> Result<()>;
    fn inventory(&mut self, attempt: Attempt) -> std::result::Result<Inventory, AcquisitionError>;
    fn wait(&mut self, timeout: Duration) -> Result<WaitOutcome>;
}

pub fn run<A: Acquisition>(mut source: A, reader: &TableReader, reconcile: Duration) {
    let publisher = reader.publisher();
    let mut state = State::new(reconcile);

    loop {
        let now = source.now();
        if !state.subscribed && state.retry_ready(now) {
            match source.subscribe() {
                Ok(()) => {
                    state.subscribed = true;
                    state.needs_inventory = true;
                }
                Err(error) => {
                    log::warn!("Cannot subscribe to link notifications: {error}");
                    state.fail(source.now());
                }
            }
        }

        let now = source.now();
        if state.subscribed && state.acquisition_ready(now) {
            state.start_acquisition();
            let attempt = Attempt {
                continuity: state.continuity,
            };
            match source.inventory(attempt) {
                Ok(inventory) => match state.accept_inventory(inventory, source.now()) {
                    Ok(topology) => publisher.publish(Mib::from_topology(&topology)),
                    Err(error) => {
                        log::warn!("Cannot publish interface inventory: {error}");
                        state.fail(source.now());
                    }
                },
                Err(AcquisitionError::Ordinary { error, events }) => {
                    state.retain_failed_events(events);
                    log::warn!("Cannot acquire interface inventory: {error}");
                    state.fail(source.now());
                }
                Err(AcquisitionError::Lost { error, events }) => {
                    state.retain_failed_events(events);
                    log::warn!("Link notification continuity was lost: {error}");
                    state.lose_continuity(source.now(), &publisher);
                }
            }
            continue;
        }

        let timeout = state.next_deadline(now).saturating_sub(now);
        match source.wait(timeout) {
            Ok(WaitOutcome::Deadline) => {}
            Ok(WaitOutcome::LinkChanged(at)) => state.record_event(at),
            #[cfg(test)]
            Ok(WaitOutcome::Shutdown) => return,
            Err(error) => {
                log::warn!("Link notification receive failed: {error}");
                state.lose_continuity(source.now(), &publisher);
            }
        }
    }
}

struct State {
    reconcile: Duration,
    continuity: u64,
    subscribed: bool,
    needs_inventory: bool,
    retry_not_before: Option<Duration>,
    failure_delay: Duration,
    event_deadline: Option<Duration>,
    event_delay: Duration,
    last_event: Option<Duration>,
    last_success: Option<Duration>,
}

impl State {
    fn new(reconcile: Duration) -> Self {
        Self {
            reconcile,
            continuity: 0,
            subscribed: false,
            needs_inventory: true,
            retry_not_before: None,
            failure_delay: INITIAL_DELAY,
            event_deadline: None,
            event_delay: INITIAL_DELAY,
            last_event: None,
            last_success: None,
        }
    }

    fn retry_ready(&self, now: Duration) -> bool {
        self.retry_not_before.is_none_or(|deadline| now >= deadline)
    }

    fn acquisition_ready(&self, now: Duration) -> bool {
        if !self.retry_ready(now) {
            return false;
        }
        self.needs_inventory
            || self.event_deadline.is_some_and(|deadline| now >= deadline)
            || self
                .last_success
                .is_some_and(|success| now >= success.saturating_add(self.reconcile))
    }

    fn start_acquisition(&mut self) {
        self.needs_inventory = false;
        if self.event_deadline.take().is_some() {
            self.event_delay = next_delay(self.event_delay);
        }
    }

    fn accept_inventory(&mut self, inventory: Inventory, now: Duration) -> Result<Topology> {
        if inventory.attempt.continuity != self.continuity {
            return Err(Error::other(
                "stale inventory from an earlier continuity generation",
            ));
        }
        self.record_batch(inventory.events)?;
        let topology = Topology::from_observed(inventory.links)?;
        self.retry_not_before = None;
        self.failure_delay = INITIAL_DELAY;
        self.last_success = Some(now);
        Ok(topology)
    }

    fn record_batch(&mut self, batch: EventBatch) -> Result<()> {
        match (batch.first, batch.last) {
            (None, None) => Ok(()),
            (Some(first), Some(last)) if first <= last && last - first < EVENT_RESET => {
                self.record_event(first);
                if first != last {
                    self.record_event(last);
                }
                Ok(())
            }
            _ => Err(Error::other("invalid inventory notification timestamps")),
        }
    }

    fn retain_failed_events(&mut self, events: EventBatch) {
        if let Err(error) = self.record_batch(events) {
            log::warn!("Cannot retain interface events from failed inventory: {error}");
        }
    }

    fn record_event(&mut self, at: Duration) {
        if self
            .last_event
            .is_none_or(|last| at.saturating_sub(last) >= EVENT_RESET)
        {
            self.event_delay = INITIAL_DELAY;
        }
        self.last_event = Some(at);
        if self.event_deadline.is_none() {
            self.event_deadline = Some(at.saturating_add(self.event_delay));
        }
    }

    fn fail(&mut self, now: Duration) {
        self.needs_inventory = true;
        self.retry_not_before = Some(now.saturating_add(self.failure_delay));
        self.failure_delay = next_delay(self.failure_delay);
    }

    fn lose_continuity(&mut self, now: Duration, publisher: &TablePublisher) {
        publisher.unavailable();
        self.continuity = self.continuity.wrapping_add(1);
        self.subscribed = false;
        self.needs_inventory = true;
        self.fail(now);
    }

    fn next_deadline(&self, now: Duration) -> Duration {
        if let Some(deadline) = self.retry_not_before {
            return deadline.max(now);
        }
        if !self.subscribed || self.needs_inventory {
            return now;
        }
        let event = self.event_deadline.unwrap_or(Duration::MAX);
        let reconcile = self
            .last_success
            .map_or(now, |success| success.saturating_add(self.reconcile));
        event.min(reconcile).max(now)
    }
}

fn next_delay(delay: Duration) -> Duration {
    (delay * 2).min(MAX_DELAY)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::link::ObservedLink;

    enum InventoryStep {
        Complete {
            links: Vec<ObservedLink>,
            elapsed: Duration,
            events: EventBatch,
            stale: bool,
        },
        Ordinary,
        Lost,
        OrdinaryWithEvents(EventBatch),
        LostWithEvents(EventBatch),
    }

    enum WaitStep {
        Change,
        Loss,
        Stop,
    }

    struct Controlled {
        now: Duration,
        subscriptions: VecDeque<bool>,
        inventories: VecDeque<InventoryStep>,
        waits: VecDeque<(Duration, WaitStep)>,
        subscription_times: Vec<Duration>,
        acquisition_times: Vec<Duration>,
        availability_while_waiting: Vec<bool>,
        reader: TableReader,
    }

    impl Controlled {
        fn new(reader: TableReader) -> Self {
            Self {
                now: Duration::ZERO,
                subscriptions: VecDeque::from([true]),
                inventories: VecDeque::new(),
                waits: VecDeque::new(),
                subscription_times: Vec::new(),
                acquisition_times: Vec::new(),
                availability_while_waiting: Vec::new(),
                reader,
            }
        }

        fn complete(index: u32) -> InventoryStep {
            InventoryStep::Complete {
                links: vec![ObservedLink::plain(index, &format!("link{index}"))],
                elapsed: Duration::ZERO,
                events: EventBatch::default(),
                stale: false,
            }
        }
    }

    impl Acquisition for &mut Controlled {
        fn now(&self) -> Duration {
            self.now
        }

        fn subscribe(&mut self) -> Result<()> {
            self.subscription_times.push(self.now);
            if self.subscriptions.pop_front().unwrap_or(true) {
                Ok(())
            } else {
                Err(Error::other("scripted subscription failure"))
            }
        }

        fn inventory(
            &mut self,
            attempt: Attempt,
        ) -> std::result::Result<Inventory, AcquisitionError> {
            self.acquisition_times.push(self.now);
            match self
                .inventories
                .pop_front()
                .unwrap_or_else(|| Controlled::complete(1))
            {
                InventoryStep::Complete {
                    links,
                    elapsed,
                    events,
                    stale,
                } => {
                    self.now += elapsed;
                    Ok(Inventory {
                        attempt: if stale {
                            Attempt {
                                continuity: attempt.continuity.saturating_sub(1),
                            }
                        } else {
                            attempt
                        },
                        links,
                        events,
                    })
                }
                InventoryStep::Ordinary => Err(AcquisitionError::Ordinary {
                    error: Error::other("scripted inventory failure"),
                    events: EventBatch::default(),
                }),
                InventoryStep::Lost => Err(AcquisitionError::Lost {
                    error: Error::other("scripted continuity loss"),
                    events: EventBatch::default(),
                }),
                InventoryStep::OrdinaryWithEvents(events) => Err(AcquisitionError::Ordinary {
                    error: Error::other("scripted inventory failure with events"),
                    events,
                }),
                InventoryStep::LostWithEvents(events) => Err(AcquisitionError::Lost {
                    error: Error::other("scripted continuity loss with events"),
                    events,
                }),
            }
        }

        fn wait(&mut self, timeout: Duration) -> Result<WaitOutcome> {
            self.availability_while_waiting
                .push(self.reader.snapshot().is_some());
            let deadline = self.now + timeout;
            let Some((at, _)) = self.waits.front() else {
                return Ok(WaitOutcome::Shutdown);
            };
            if *at > deadline {
                self.now = deadline;
                return Ok(WaitOutcome::Deadline);
            }
            let (at, step) = self.waits.pop_front().expect("front checked");
            self.now = at.max(self.now);
            match step {
                WaitStep::Change => Ok(WaitOutcome::LinkChanged(at)),
                WaitStep::Loss => Err(Error::other("scripted notification loss")),
                WaitStep::Stop => Ok(WaitOutcome::Shutdown),
            }
        }
    }

    #[test]
    fn startup_publishes_and_reads_do_not_request_acquisition() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source.inventories.push_back(Controlled::complete(2));

        run(&mut source, &reader, Duration::from_secs(3600));

        assert!(reader.snapshot().is_some());
        for _ in 0..20 {
            assert!(reader.snapshot().is_some());
        }
        assert_eq!(source.acquisition_times, [Duration::ZERO]);
    }

    #[test]
    fn published_table_remains_available_while_an_event_update_is_pending() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source.inventories.push_back(Controlled::complete(1));
        source.waits.extend([
            (Duration::from_secs(10), WaitStep::Change),
            (Duration::from_millis(10_500), WaitStep::Stop),
        ]);

        run(&mut source, &reader, Duration::from_secs(3600));

        assert_eq!(source.acquisition_times, [Duration::ZERO]);
        assert!(reader.snapshot().is_some());
        assert_eq!(source.availability_while_waiting.last(), Some(&true));
    }

    #[test]
    fn event_backoff_is_exact_and_later_events_do_not_postpone() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source
            .inventories
            .extend((0..8).map(|_| Controlled::complete(1)));
        source.waits.extend([
            (Duration::from_secs(10), WaitStep::Change),
            (Duration::from_secs(12), WaitStep::Change),
            (Duration::from_secs(15), WaitStep::Change),
            (Duration::from_secs(16), WaitStep::Change),
            (Duration::from_secs(20), WaitStep::Change),
            (Duration::from_secs(29), WaitStep::Change),
            (Duration::from_secs(46), WaitStep::Change),
            (Duration::from_secs(77), WaitStep::Change),
            (Duration::from_secs(108), WaitStep::Stop),
        ]);

        run(&mut source, &reader, Duration::from_secs(3600));

        assert_eq!(
            source.acquisition_times,
            [0, 11, 14, 19, 28, 45, 76, 107].map(Duration::from_secs)
        );
    }

    #[test]
    fn event_backoff_resets_only_after_sixty_quiet_seconds() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source
            .inventories
            .extend((0..4).map(|_| Controlled::complete(1)));
        source.waits.extend([
            (Duration::from_secs(10), WaitStep::Change),
            (Duration::from_secs(69), WaitStep::Change),
            (Duration::from_secs(70), WaitStep::Change),
            (Duration::from_secs(131), WaitStep::Change),
            (Duration::from_secs(133), WaitStep::Stop),
        ]);

        run(&mut source, &reader, Duration::from_secs(3600));

        assert_eq!(
            source.acquisition_times,
            [0, 11, 71, 132].map(Duration::from_secs)
        );
    }

    #[test]
    fn failure_backoff_is_exact_and_success_resets_it() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source
            .inventories
            .extend((0..7).map(|_| InventoryStep::Ordinary));
        source.inventories.push_back(Controlled::complete(1));
        source
            .waits
            .push_back((Duration::from_secs(92), WaitStep::Change));
        source.inventories.push_back(InventoryStep::Ordinary);
        source.inventories.push_back(Controlled::complete(1));
        source
            .waits
            .push_back((Duration::from_secs(95), WaitStep::Stop));

        run(&mut source, &reader, Duration::from_secs(3600));

        assert_eq!(
            source.acquisition_times,
            [0, 1, 3, 7, 15, 31, 61, 91, 93, 94].map(Duration::from_secs)
        );
    }

    #[test]
    fn failed_subscription_retries_without_starting_an_inventory() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source.subscriptions = VecDeque::from([false, false, true]);
        source.inventories.push_back(Controlled::complete(1));
        source
            .waits
            .push_back((Duration::from_secs(4), WaitStep::Stop));

        run(&mut source, &reader, Duration::from_secs(3600));

        assert_eq!(
            source.subscription_times,
            [0, 1, 3].map(Duration::from_secs)
        );
        assert_eq!(source.acquisition_times, [Duration::from_secs(3)]);
    }

    #[test]
    fn events_and_reconciliation_do_not_bypass_failure_not_before() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source.inventories.extend([
            Controlled::complete(1),
            InventoryStep::Ordinary,
            Controlled::complete(2),
            Controlled::complete(3),
        ]);
        source.waits.extend([
            (Duration::from_secs(10), WaitStep::Change),
            (Duration::from_millis(11_500), WaitStep::Change),
            (Duration::from_secs(25), WaitStep::Stop),
        ]);

        run(&mut source, &reader, Duration::from_secs(11));

        assert_eq!(
            source.acquisition_times,
            [0, 11, 12, 23].map(Duration::from_secs)
        );
    }

    #[test]
    fn ordinary_failure_preserves_the_table_but_loss_removes_it_until_recovery() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source.inventories.extend([
            Controlled::complete(1),
            InventoryStep::Ordinary,
            Controlled::complete(2),
            InventoryStep::Lost,
            Controlled::complete(3),
        ]);
        source.waits.extend([
            (Duration::from_secs(10), WaitStep::Change),
            (Duration::from_secs(20), WaitStep::Change),
            (Duration::from_secs(30), WaitStep::Stop),
        ]);

        run(&mut source, &reader, Duration::from_secs(3600));

        assert!(
            source
                .availability_while_waiting
                .windows(2)
                .any(|values| values == [true, false])
        );
        assert!(reader.snapshot().is_some());
    }

    #[test]
    fn notifications_during_inventory_publish_and_remain_pending() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source.inventories.extend([
            InventoryStep::Complete {
                links: vec![ObservedLink::plain(1, "first")],
                elapsed: Duration::from_millis(600),
                events: EventBatch::one(Duration::from_millis(500)),
                stale: false,
            },
            Controlled::complete(2),
        ]);
        source
            .waits
            .push_back((Duration::from_secs(2), WaitStep::Stop));

        run(&mut source, &reader, Duration::from_secs(3600));

        assert_eq!(
            source.acquisition_times,
            [Duration::ZERO, Duration::from_millis(1500)]
        );
        assert!(reader.snapshot().is_some());
    }

    #[test]
    fn failed_inventories_retain_event_backoff_across_retry_and_recovery() {
        for lost in [false, true] {
            let reader = publication();
            let mut source = Controlled::new(reader.clone());
            let failed = if lost {
                InventoryStep::LostWithEvents(EventBatch::one(Duration::from_secs(11)))
            } else {
                InventoryStep::OrdinaryWithEvents(EventBatch::one(Duration::from_secs(11)))
            };
            source.inventories.extend([
                Controlled::complete(1),
                failed,
                Controlled::complete(2),
                Controlled::complete(3),
            ]);
            source.waits.extend([
                (Duration::from_secs(10), WaitStep::Change),
                (Duration::from_secs(70), WaitStep::Change),
                (Duration::from_secs(75), WaitStep::Stop),
            ]);

            run(&mut source, &reader, Duration::from_secs(3600));

            assert_eq!(
                source.acquisition_times,
                [0, 11, 12, 74].map(Duration::from_secs),
                "lost={lost}"
            );
            assert_eq!(
                source.subscription_times,
                if lost {
                    vec![Duration::ZERO, Duration::from_secs(12)]
                } else {
                    vec![Duration::ZERO]
                }
            );
        }
    }

    #[test]
    fn stale_post_loss_completion_cannot_restore_availability() {
        let reader = publication();
        let mut source = Controlled::new(reader.clone());
        source.inventories.extend([
            Controlled::complete(1),
            InventoryStep::Complete {
                links: vec![ObservedLink::plain(2, "stale")],
                elapsed: Duration::ZERO,
                events: EventBatch::default(),
                stale: true,
            },
            Controlled::complete(3),
        ]);
        source.waits.extend([
            (Duration::from_secs(10), WaitStep::Loss),
            (Duration::from_secs(14), WaitStep::Stop),
        ]);

        run(&mut source, &reader, Duration::from_secs(3600));

        assert_eq!(
            source.acquisition_times,
            [0, 11, 13].map(Duration::from_secs)
        );
        assert!(source.availability_while_waiting.contains(&false));
        assert!(reader.snapshot().is_some());
    }
}
