//! A single bounded history-query worker, independent of workflow admission.
use super::*;
use std::sync::Condvar;

pub const HISTORY_PAGE_SIZE: usize = 50;

pub struct HistoryPage {
    pub generation: u64,
    pub text: String,
    pub page: usize,
    pub total: u64,
    pub entries: Vec<Entry>,
}

pub struct HistoryPageResult {
    pub generation: u64,
    pub result: Result<HistoryPage, String>,
}

struct Request { generation: u64, text: String, page: usize }
#[derive(Default)]
struct Mailbox {
    history: Option<Arc<History>>,
    initialization_error: Option<String>,
    pending: Option<Request>,
    completed: Option<HistoryPageResult>,
    generation: u64,
    shutdown: bool,
}
type Shared = Arc<(Mutex<Mailbox>, Condvar)>;

pub struct HistorySearch { shared: Shared }
impl HistorySearch {
    pub(super) fn waiting() -> Result<Self> {
        let shared = Shared::default();
        let worker = shared.clone();
        thread::Builder::new().name("rcr-history-search".into()).spawn(move || search_loop(worker))?;
        Ok(Self { shared })
    }
    pub(super) fn initializer(&self) -> impl FnOnce(Result<Arc<History>, String>) + Send + 'static {
        let shared = self.shared.clone();
        move |history| initialize(&shared, history)
    }
    /// Searches a supplied database without constructing capture or upload services.
    pub fn new(history: Arc<History>) -> Result<Self> {
        let search = Self::waiting()?;
        initialize(&search.shared, Ok(history));
        Ok(search)
    }
    /// Replaces the one pending request. An active SQLite call finishes naturally;
    /// superseded results are discarded before publication.
    pub fn request(&self, text: &str, page: usize) -> u64 {
        let mut state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1).max(1);
        let generation = state.generation;
        state.pending = Some(Request { generation, text: text.trim().chars().take(512).collect(), page });
        state.completed = None;
        self.shared.1.notify_one();
        generation
    }
    pub fn take_result(&self) -> Option<HistoryPageResult> {
        self.shared.0.lock().unwrap_or_else(|e| e.into_inner()).completed.take()
    }
}
impl Drop for HistorySearch {
    fn drop(&mut self) {
        let mut state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        state.shutdown = true;
        state.pending = None;
        state.completed = None;
        self.shared.1.notify_one();
    }
}
fn initialize(shared: &Shared, history: Result<Arc<History>, String>) {
    let mut state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
    match history { Ok(history) => state.history = Some(history), Err(error) => state.initialization_error = Some(error) }
    shared.1.notify_one();
}
fn current(shared: &Shared, generation: u64) -> bool {
    let state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
    !state.shutdown && state.generation == generation
}
fn search_loop(shared: Shared) {
    loop {
        let (request, history) = {
            let mut state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
            while !state.shutdown && (state.pending.is_none() || (state.history.is_none() && state.initialization_error.is_none())) {
                state = shared.1.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.shutdown { return; }
            let request = state.pending.take().expect("pending request");
            let history = state.history.clone().ok_or_else(|| state.initialization_error.clone().unwrap_or_default());
            (request, history)
        };
        let generation = request.generation;
        let result = history.and_then(|history| query_page(&history, request, || current(&shared, generation)));
        let mut state = shared.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.shutdown && state.generation == generation {
            state.completed = Some(HistoryPageResult { generation, result });
        }
    }
}
fn query_page(history: &History, request: Request, current: impl Fn() -> bool) -> Result<HistoryPage, String> {
    let mut query = Query { text: (!request.text.is_empty()).then(|| request.text.clone()),
        limit: HISTORY_PAGE_SIZE, thumbnails: false, ..Default::default() };
    let total = history.count(&query).map_err(|error| error.to_string())?;
    if !current() { return Err("History search superseded".into()); }
    let last_page = total.saturating_sub(1) / HISTORY_PAGE_SIZE as u64;
    let page = request.page.min(usize::try_from(last_page).unwrap_or(usize::MAX / HISTORY_PAGE_SIZE));
    query.offset = page.saturating_mul(HISTORY_PAGE_SIZE);
    let entries = history.list(&query).map_err(|error| error.to_string())?;
    Ok(HistoryPage { generation: request.generation, text: request.text, page, total, entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn populate(history: Arc<History>) -> Arc<History> {
        for i in 0..257 {
            let mut entry = core::history::NewEntry::new(core::history::EntryKind::Image);
            entry.created_at = i;
            entry.local_path = Some(PathBuf::from(if i == 0 { "oldest-distinctive.png".into() } else { format!("capture-{i:03}.png") }));
            history.insert(&entry).unwrap();
        }
        history
    }
    fn database() -> Arc<History> { populate(Arc::new(History::open_in_memory().unwrap())) }
    fn receive(search: &HistorySearch, generation: u64) -> HistoryPage {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = search.take_result() {
                assert_eq!(result.generation, generation);
                return result.result.unwrap();
            }
            assert!(std::time::Instant::now() < deadline, "History result timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn searches_oldest_entry_and_pages_all_history_without_thumbnails() {
        let search = HistorySearch::new(database()).unwrap();
        let result = receive(&search, search.request("oldest", 0));
        assert_eq!(result.total, 1);
        assert_eq!(result.entries[0].local_path.as_deref(), Some(std::path::Path::new("oldest-distinctive.png")));
        let mut ids = std::collections::BTreeSet::new();
        for page in 0..6 {
            let result = receive(&search, search.request("", page));
            assert_eq!((result.total, result.page), (257, page));
            for entry in result.entries { assert!(entry.thumbnail.is_none()); assert!(ids.insert(entry.id)); }
        }
        assert_eq!(ids.len(), 257);
        let result = receive(&search, search.request("", usize::MAX));
        assert_eq!((result.page, result.entries.len()), (5, 7));
    }
    #[test]
    fn latest_request_replaces_pending_and_completed_results() {
        let search = HistorySearch::new(database()).unwrap();
        for i in 0..500 { search.request(&format!("missing-{i}"), i); }
        let generation = search.request("oldest", 0);
        let result = receive(&search, generation);
        assert_eq!(result.text, "oldest");
        assert_eq!(result.total, 1);
    }
    #[test]
    fn query_admission_does_not_change_a_busy_workflow_or_its_cancel_token() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(directory.path().join("config"));
        let worker = Worker::new(paths, directory.path().join("captures")).unwrap();
        worker.busy.store(true, Ordering::Release);
        let generation = worker.history_search.request("", 0);
        let page = receive(&worker.history_search, generation);
        assert_eq!(page.total, 0);
        assert!(worker.busy());
        assert!(!worker.current.lock().unwrap().is_cancelled());
    }
    #[test]
    fn disk_history_search_finds_entry_beyond_snapshot_limit() {
        let directory = tempfile::tempdir().unwrap();
        let history = populate(Arc::new(History::open(&directory.path().join("history.db")).unwrap()));
        assert_eq!(history.list(&Query { limit: 100, ..Default::default() }).unwrap().len(), 100);
        let search = HistorySearch::new(history).unwrap();
        let page = receive(&search, search.request("oldest", 0));
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].id, 1);
    }
}
