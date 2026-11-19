//! Frecency ranking.
use crate::model::Entry;

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
const MONTH: i64 = 30 * DAY;

pub fn score(e: &Entry, now: i64) -> f64 {
    let tags: i64 = e.tags.iter().map(|t| t.count).sum();
    let base = (e.visits + 2 * tags) as f64;
    let age = (now - e.last_visit).max(0);
    let mult = if age < HOUR {
        4.0
    } else if age < DAY {
        2.0
    } else if age < WEEK {
        1.0
    } else if age < MONTH {
        0.5
    } else {
        0.25
    };
    base * mult
}

pub fn sort_entries(entries: &mut [Entry], now: i64) {
    entries.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| score(b, now).total_cmp(&score(a, now)))
            .then_with(|| b.last_visit.cmp(&a.last_visit))
            .then_with(|| a.path.cmp(&b.path))
    });
}

pub fn time_ago(ts: i64, now: i64) -> String {
    if ts == 0 {
        return "never".to_string();
    }
    let d = (now - ts).max(0);
    if d < 60 {
        "now".to_string()
    } else if d < HOUR {
        format!("{}m", d / 60)
    } else if d < DAY {
        format!("{}h", d / HOUR)
    } else if d < MONTH {
        if d < WEEK {
            format!("{}d", d / DAY)
        } else {
            format!("{}w", d / WEEK)
        }
    } else if d < 365 * DAY {
        format!("{}mo", (d / MONTH).max(1))
    } else {
        format!("{}y", d / (365 * DAY))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, visits: i64, last_visit: i64, pinned: bool) -> Entry {
        Entry { path: path.into(), visits, last_visit, pinned, tags: vec![] }
    }

    #[test]
    fn pinned_first() {
        let now = 1_000_000;
        let mut v = vec![entry("/hot", 100, now, false), entry("/pin", 1, 1, true)];
        sort_entries(&mut v, now);
        assert_eq!(v[0].path, "/pin");
    }

    #[test]
    fn recent_beats_old() {
        let now = 10_000_000;
        let mut v = vec![entry("/old", 5, now - 60 * DAY, false), entry("/new", 5, now - 60, false)];
        sort_entries(&mut v, now);
        assert_eq!(v[0].path, "/new");
    }

    #[test]
    fn time_ago_cases() {
        let now = 100_000_000;
        assert_eq!(time_ago(0, now), "never");
        assert_eq!(time_ago(now - 10, now), "now");
        assert_eq!(time_ago(now - 5 * 60, now), "5m");
        assert_eq!(time_ago(now - 3 * HOUR, now), "3h");
        assert_eq!(time_ago(now - 2 * DAY, now), "2d");
        assert_eq!(time_ago(now - 4 * WEEK, now), "4w");
        assert_eq!(time_ago(now - 180 * DAY, now), "6mo");
        assert_eq!(time_ago(now - 400 * DAY, now), "1y");
    }
}
