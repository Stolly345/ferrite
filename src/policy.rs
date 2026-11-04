//! The rules Ferrite applies on top of the engine every second: the download queue, seeding
//! goals and the alternative-speed schedule. Pure functions, so they are easy to test.

use crate::config::SeedGoal;
use crate::engine::State;

/// One torrent as the queue sees it.
#[derive(Clone, Debug)]
pub struct QueueItem {
    pub hash: String,
    /// Position in the user's № order (0 = first).
    pub order: u32,
    pub state: State,
    pub complete: bool,
    pub queued: bool,
    pub forced: bool,
    /// Asked to start a moment ago; holds its slot until the engine reports it running.
    pub starting: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct QueuePlan {
    /// Paused to free slots; they become Queued.
    pub to_queue: Vec<String>,
    /// Queued ones that get a slot.
    pub to_start: Vec<String>,
}

/// At most `limit` torrents download at a time (0 = no limit). Seeding takes no slot; a torrent
/// checking its files is never stopped; one started by hand (`forced`) runs past the limit and is
/// never paused for the queue, but holds a slot. Queued torrents start in № order.
pub fn plan_queue(items: &[QueueItem], limit: u32) -> QueuePlan {
    let mut plan = QueuePlan::default();
    let downloading = |i: &&QueueItem| {
        !i.complete && (i.starting || matches!(i.state, State::Downloading | State::GettingFileList | State::Checking))
    };
    let mut queued: Vec<&QueueItem> = items.iter().filter(|i| i.queued && !i.complete && !i.starting).collect();
    queued.sort_by_key(|i| i.order);
    if limit == 0 {
        plan.to_start = queued.iter().map(|i| i.hash.clone()).collect();
        return plan;
    }
    let limit = limit as usize;
    let mut active: Vec<&QueueItem> = items.iter().filter(downloading).collect();
    if active.len() > limit {
        // Free slots from the bottom of the order, never touching forced or checking ones.
        active.sort_by_key(|i| std::cmp::Reverse(i.order));
        let mut excess = active.len() - limit;
        for i in &active {
            if excess == 0 {
                break;
            }
            if !i.forced && i.state != State::Checking {
                plan.to_queue.push(i.hash.clone());
                excess -= 1;
            }
        }
    } else {
        let free = limit - active.len();
        plan.to_start = queued.iter().take(free).map(|i| i.hash.clone()).collect();
    }
    plan
}

/// Whether a finished torrent has met its seeding goal. `own` is the torrent's goal (None = the
/// global one). A ratio of 0 or minutes of 0 mean no limit on that measure.
pub fn goal_reached(own: Option<SeedGoal>, global: SeedGoal, uploaded: u64, size: u64, seed_secs: u64) -> bool {
    let g = own.unwrap_or(global);
    let ratio_hit = g.ratio > 0.0 && size > 0 && uploaded as f64 / size as f64 >= g.ratio;
    let time_hit = g.minutes > 0 && seed_secs >= g.minutes as u64 * 60;
    ratio_hit || time_hit
}

/// Whether the alternative limits apply at `minute` (0–1439) on `weekday` (0 = Monday).
/// A window that wraps past midnight (22:00–06:00) is allowed.
pub fn alt_active(enabled: bool, scheduled: bool, from: u32, to: u32, days: u8, weekday: u32, minute: u32) -> bool {
    if enabled {
        return true;
    }
    if !scheduled || days & (1 << weekday) == 0 {
        return false;
    }
    if from <= to { minute >= from && minute < to } else { minute >= from || minute < to }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(hash: &str, order: u32, state: State) -> QueueItem {
        QueueItem { hash: hash.into(), order, state, complete: false, queued: false, forced: false, starting: false }
    }

    #[test]
    fn queues_the_last_ones_over_the_limit() {
        let items = [
            item("a", 0, State::Downloading),
            item("b", 1, State::Downloading),
            item("c", 2, State::Downloading),
        ];
        assert_eq!(plan_queue(&items, 2).to_queue, vec!["c".to_string()]);
    }

    #[test]
    fn never_queues_forced_or_checking() {
        let mut c = item("c", 2, State::Downloading);
        c.forced = true;
        let items = [item("a", 0, State::Downloading), item("b", 1, State::Checking), c];
        // Over by two, but only "a" may be paused.
        assert_eq!(plan_queue(&items, 1).to_queue, vec!["a".to_string()]);
    }

    #[test]
    fn starts_queued_in_order_when_slots_free() {
        let mut q1 = item("q1", 5, State::Queued);
        q1.queued = true;
        let mut q0 = item("q0", 3, State::Queued);
        q0.queued = true;
        let seeding = QueueItem { complete: true, ..item("s", 0, State::Seeding) };
        let items = [item("a", 1, State::Downloading), q1, q0, seeding];
        assert_eq!(plan_queue(&items, 2).to_start, vec!["q0".to_string()]);
        assert_eq!(plan_queue(&items, 0).to_start, vec!["q0".to_string(), "q1".to_string()]);
    }

    #[test]
    fn a_torrent_being_started_holds_its_slot() {
        let mut s = item("s", 4, State::Paused);
        s.starting = true;
        let mut q = item("q", 5, State::Queued);
        q.queued = true;
        let items = [item("a", 0, State::Downloading), s, q];
        assert!(plan_queue(&items, 2).to_start.is_empty());
    }

    #[test]
    fn goals() {
        let g = SeedGoal { ratio: 2.0, minutes: 0 };
        assert!(!goal_reached(None, g, 150, 100, 99_999));
        assert!(goal_reached(None, g, 200, 100, 0));
        let own = SeedGoal { ratio: 0.0, minutes: 60 };
        assert!(goal_reached(Some(own), g, 0, 100, 3600));
        assert!(!goal_reached(Some(SeedGoal::default()), g, 500, 100, 99_999));
    }

    #[test]
    fn schedule() {
        let all = 0b111_1111;
        assert!(alt_active(false, true, 8 * 60, 23 * 60, all, 0, 12 * 60));
        assert!(!alt_active(false, true, 8 * 60, 23 * 60, all, 0, 7 * 60));
        assert!(alt_active(false, true, 22 * 60, 6 * 60, all, 3, 2 * 60));
        assert!(!alt_active(false, true, 8 * 60, 23 * 60, 0b1, 1, 12 * 60));
        assert!(alt_active(true, false, 0, 0, 0, 0, 0));
    }
}
