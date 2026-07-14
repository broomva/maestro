// The file pane (BRO-1890 FID-4) — the workspace walked as files (`MccFilePane`, placement B: the
// pane at the layout's right edge). A DERIVED VIEW over the real node tree (`selectFileTree`): each row
// is a node, indented by its path depth; container folders (initiative/project) are non-openable, leaf
// work opens its contract document. Matte (glass stays on the composer); the pane owns its own scroll.
// The disclosure ladder holds — this is work-as-files, never the engine room (worktrees / index).

import { DotComet } from "@maestro/ui";
import { FileText, FolderOpen } from "lucide-react";
import type { FileEntry } from "@/store";

export interface FilePaneProps {
  /** The file rows (`selectFileTree`) — folders + files, path-sorted. */
  entries: FileEntry[];
  /** The open file's path (the active row), or null when a non-file tab is shown. */
  openPath: string | null;
  /** Open a file by path (adds a tab + routes to it). Folders are inert. */
  onOpen: (path: string) => void;
  /** An optional pane label (e.g. the scope folder). */
  label?: string;
  /** An optional location line + worktree receipt (the prototype's `.mcc-ftree-loc`). */
  location?: string;
  worktree?: string;
}

export function FilePane({ entries, openPath, onOpen, label, location, worktree }: FilePaneProps) {
  return (
    <div className="mcc-ftree" data-testid="file-pane" data-screen-label="File pane">
      {label ? <div className="mcc-ftree-label">{label}</div> : null}
      {location ? (
        <div className="mcc-ftree-loc">
          <span className="mcc-ftree-loc-path">{location}</span>
          {worktree ? <span className="mc-receipt">{worktree}</span> : null}
        </div>
      ) : null}
      {entries.map((e) => {
        const isFolder = e.kind === "folder";
        const active = !isFolder && e.path === openPath;
        return (
          <button
            key={e.path}
            type="button"
            className={`mcc-ftree-row${active ? " is-active" : ""}${isFolder ? " is-folder" : ""}`}
            // Indentation is the tree's nesting — inline padding-left, exactly as the prototype (the
            // structural class owns the rest; the depth is data, not a class).
            style={{ paddingLeft: 8 + e.depth * 14 }}
            disabled={isFolder}
            aria-current={active ? "page" : undefined}
            onClick={isFolder ? undefined : () => onOpen(e.path)}
          >
            {isFolder ? (
              <FolderOpen size={13} strokeWidth={2} />
            ) : (
              <FileText size={13} strokeWidth={2} />
            )}
            <span className="mcc-ftree-name">{e.name}</span>
            {e.live ? <DotComet size={11} style={{ marginLeft: "auto" }} /> : null}
          </button>
        );
      })}
    </div>
  );
}
