// The FS-row → wire projection. A live index row carries the index-internal `deletedAt`
// tombstone column; the wire shape (protocol `LiveNode` = `Omit<NodeRow, "deletedAt">`, and
// the same for sessions/gates/schedules) never exposes it. This is the single definition of
// that strip — shared by the read API (BRO-1812) and the FS-watcher's node.updated projection
// (BRO-1804) so the two can never drift.

/** Strip the index-internal `deletedAt` tombstone column to get the wire (live) shape. */
export function projectLiveNode<T extends { deletedAt: number | null }>(
  row: T,
): Omit<T, "deletedAt"> {
  const { deletedAt: _tombstone, ...rest } = row;
  return rest;
}
