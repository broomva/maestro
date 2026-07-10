// FS watcher (BRO-1804) — incremental index updates + node.updated projection.
export {
  createReconcileScheduler,
  isWatchedChange,
  type ReconcileResult,
  type ReconcileScheduler,
  reconcileAndEmit,
  type SchedulerOptions,
  startWatcher,
  type WatcherHandle,
  type WatcherOptions,
} from "./watcher";
