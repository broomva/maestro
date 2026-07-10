// FS watcher (BRO-1804) — incremental index updates + node.updated projection.
export {
  isWatchedChange,
  type ReconcileResult,
  reconcileAndEmit,
  startWatcher,
  type WatcherHandle,
  type WatcherOptions,
} from "./watcher";
