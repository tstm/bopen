//! One lane per app: links for the same app open one at a time, in click order, each waiting
//! until the app has taken the one before. A browser that is still starting (seconds under an
//! on-access scanner) never gets a second launch racing the first, and a slow Chrome never holds
//! up a Safari link. No AppKit here; `macos` supplies what a job does.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::mpsc::{self, Sender};

pub struct Lanes<J: Send + 'static> {
    run: fn(J),
    lanes: Mutex<HashMap<String, Sender<J>>>,
}

impl<J: Send + 'static> Lanes<J> {
    pub fn new(run: fn(J)) -> Self {
        Lanes { run, lanes: Mutex::new(HashMap::new()) }
    }

    /// Queue `job` behind the jobs already queued for `key`. Returns at once.
    pub fn push(&self, key: &str, job: J) {
        let mut lanes = self.lanes.lock().unwrap_or_else(|e| e.into_inner());
        let run = self.run;
        let lane = lanes.entry(key.to_string()).or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<J>();
            std::thread::spawn(move || {
                for job in rx {
                    // A panicking job must not take the lane, and every link queued behind it, down.
                    let _ = catch_unwind(AssertUnwindSafe(|| run(job)));
                }
            });
            tx
        });
        // The worker never exits while its sender lives, so this cannot fail.
        let _ = lane.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;
    use std::time::{Duration, Instant};

    type Log = Mutex<Vec<(String, Instant, Instant)>>;
    static DONE: LazyLock<Log> = LazyLock::new(|| Mutex::new(vec![]));

    /// (name, how long it takes, panics?)
    fn job((name, ms, panics): (&'static str, u64, bool)) {
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(ms));
        if panics {
            panic!("{name}");
        }
        DONE.lock().unwrap().push((name.to_string(), start, Instant::now()));
    }

    fn wait_for(n: usize) -> Vec<(String, Instant, Instant)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while DONE.lock().unwrap().len() < n && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        DONE.lock().unwrap().clone()
    }

    #[test]
    fn same_app_runs_in_order_one_at_a_time_other_apps_do_not_wait() {
        let lanes = Lanes::new(job);
        lanes.push("chrome", ("c1", 150, false));
        lanes.push("chrome", ("boom", 0, true));
        lanes.push("chrome", ("c2", 0, false));
        lanes.push("chrome", ("c3", 0, false));
        lanes.push("safari", ("s1", 0, false));
        let done = wait_for(4);
        let names: Vec<_> = done.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(names, ["s1", "c1", "c2", "c3"], "safari first; chrome in order, past the panic");
        let at = |name: &str| done.iter().find(|(n, ..)| n == name).unwrap();
        assert!(at("c2").1 >= at("c1").2, "c2 started before c1 finished");
        assert!(at("s1").2 < at("c1").2, "safari waited for chrome");
    }
}
