use std::fs;

use pipetrain::merge::Store;

struct Directory(std::path::PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn consecutive_executions_on_the_same_state_each_acquire_the_lock() {
    let directory =
        std::env::temp_dir().join(format!("pipetrain-state-reacquire-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let directory = Directory(directory);
    let path = directory.0.join("state.json");
    for round in 0..5 {
        let mut store = Store::open(Some(path.clone())).unwrap();
        store.lock().unwrap();
        store.state.queue_id = Some(format!("queue-{round}"));
        store.save().unwrap();
    }
    let mut last = Store::open(Some(path)).unwrap();
    last.lock().unwrap();
    assert_eq!(last.state.queue_id.as_deref(), Some("queue-4"));
}
