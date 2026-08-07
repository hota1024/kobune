//! 最終アクセス時刻の記録と、アイドル判定。
//!
//! これがあるおかげで worktree を何個作っても、実行中のコンテナは
//! 触っているものだけになる。

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// アイドル判定を回す間隔。
///
/// 短くしても意味がない。`idle_timeout` の最小が分単位のため、
/// 30 秒ごとに見れば十分で、無駄な runtime への問い合わせも避けられる。
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// ホスト名ごとの最終アクセス時刻。
///
/// プロキシがリクエストのたびに [`IdleTracker::touch`] を呼ぶため、
/// **速くなければならない**。書き込みは短く済ませ、I/O はしない。
#[derive(Default)]
pub struct IdleTracker {
    last_access: RwLock<HashMap<String, Instant>>,
    /// 起動処理中のホスト。同じホストへの同時リクエストで
    /// 二重に起動しないようにする。
    starting: Mutex<HashMap<String, ()>>,
}

impl IdleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// アクセスがあったことを記録する。
    pub fn touch(&self, host: &str) {
        let now = Instant::now();

        // 既にある鍵なら読みロックで済ませたいところだが、
        // 値の更新には書きロックが要る。保持時間は極小に抑える。
        if let Ok(mut guard) = self.last_access.write() {
            guard.insert(host.to_ascii_lowercase(), now);
        }
    }

    /// 最後にアクセスされてからの経過時間。記録が無ければ `None`。
    pub fn idle_for(&self, host: &str) -> Option<Duration> {
        let guard = self.last_access.read().ok()?;
        guard.get(&host.to_ascii_lowercase()).map(|at| at.elapsed())
    }

    /// 記録を消す。サービスを停止したときに呼ぶ。
    pub fn forget(&self, host: &str) {
        if let Ok(mut guard) = self.last_access.write() {
            guard.remove(&host.to_ascii_lowercase());
        }
    }

    /// 起動処理を始める権利を取る。
    ///
    /// 同じホストに同時にリクエストが来ても、起動するのは 1 つだけ。
    /// 取れた場合は [`StartGuard`] が返り、drop で解放される。
    pub fn begin_start(&self, host: &str) -> Option<StartGuard<'_>> {
        let key = host.to_ascii_lowercase();
        let mut guard = self.starting.lock().ok()?;

        if guard.contains_key(&key) {
            return None;
        }

        guard.insert(key.clone(), ());
        Some(StartGuard { tracker: self, key })
    }

    fn finish_start(&self, key: &str) {
        if let Ok(mut guard) = self.starting.lock() {
            guard.remove(key);
        }
    }
}

/// 起動処理中であることを示す。drop されると解除される。
pub struct StartGuard<'a> {
    tracker: &'a IdleTracker,
    key: String,
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        self.tracker.finish_start(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_reports_idle_time() {
        let tracker = IdleTracker::new();
        assert_eq!(tracker.idle_for("web.myapp.localhost"), None);

        tracker.touch("web.myapp.localhost");

        let idle = tracker.idle_for("web.myapp.localhost").expect("記録がある");
        assert!(idle < Duration::from_secs(1));
    }

    #[test]
    fn host_matching_ignores_case() {
        // Host ヘッダの大文字小文字はクライアント任せ。
        let tracker = IdleTracker::new();
        tracker.touch("WEB.MyApp.localhost");

        assert!(tracker.idle_for("web.myapp.localhost").is_some());
    }

    #[test]
    fn idle_time_grows_from_the_last_touch() {
        // アイドル判定はこの値だけを見る。触り直せば 0 に戻る必要がある。
        let tracker = IdleTracker::new();
        tracker.touch("web.myapp.localhost");
        std::thread::sleep(Duration::from_millis(20));

        let before = tracker.idle_for("web.myapp.localhost").expect("記録がある");
        assert!(before >= Duration::from_millis(20));

        tracker.touch("web.myapp.localhost");
        let after = tracker.idle_for("web.myapp.localhost").expect("記録がある");
        assert!(after < before);
    }

    #[test]
    fn forget_removes_the_record() {
        let tracker = IdleTracker::new();
        tracker.touch("web.myapp.localhost");
        tracker.forget("WEB.myapp.localhost");

        assert_eq!(tracker.idle_for("web.myapp.localhost"), None);
    }

    #[test]
    fn only_one_start_can_be_in_flight() {
        // 同じホストに同時にリクエストが来ても、起動は 1 回だけにする。
        let tracker = IdleTracker::new();

        let first = tracker.begin_start("web.myapp.localhost");
        assert!(first.is_some());

        let second = tracker.begin_start("web.myapp.localhost");
        assert!(second.is_none(), "2 つ目は取れない");

        // 別のホストは独立して起動できる。
        assert!(tracker.begin_start("api.myapp.localhost").is_some());
    }

    #[test]
    fn dropping_the_guard_allows_the_next_start() {
        let tracker = IdleTracker::new();

        {
            let _guard = tracker.begin_start("web.myapp.localhost").expect("取れる");
        }

        assert!(
            tracker.begin_start("web.myapp.localhost").is_some(),
            "失敗して解放されたら次が起動を試せる必要がある"
        );
    }
}
