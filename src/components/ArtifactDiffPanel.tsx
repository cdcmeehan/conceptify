import type { ArtifactBlockDiff, ArtifactVersionDiff } from "../lib/api";

export function ArtifactDiffPanel({
  diff,
  selected,
  onSelect,
  onClose,
}: {
  diff: ArtifactVersionDiff;
  selected: number;
  onSelect: (index: number) => void;
  onClose: () => void;
}) {
  const counts = countChanges(diff.changes);
  const change = diff.changes[selected] ?? null;

  return (
    <section
      class="flex h-[min(17rem,40vh)] shrink-0 flex-col border-t border-line bg-paper"
      aria-label={`Changes from version ${diff.from_version} to ${diff.to_version}`}
    >
      <header class="flex items-center gap-3 border-b border-line px-3 py-2">
        <div class="min-w-0 flex-1">
          <h2 class="font-serif text-sm font-semibold text-ink">
            What changed · v{diff.from_version} → v{diff.to_version}
          </h2>
          <p class="mt-0.5 text-[11px] text-muted">{summary(counts, diff.changes.length)}</p>
        </div>
        {diff.changes.length > 1 && (
          <div class="flex items-center gap-1" aria-label="Change navigation">
            <button
              type="button"
              onClick={() => onSelect((selected - 1 + diff.changes.length) % diff.changes.length)}
              class="cfy-btn cfy-btn-secondary h-7 px-2 text-xs"
              aria-label="Previous change"
            >
              ←
            </button>
            <span class="min-w-12 text-center text-[11px] tabular-nums text-muted">
              {selected + 1} / {diff.changes.length}
            </span>
            <button
              type="button"
              onClick={() => onSelect((selected + 1) % diff.changes.length)}
              class="cfy-btn cfy-btn-secondary h-7 px-2 text-xs"
              aria-label="Next change"
            >
              →
            </button>
          </div>
        )}
        <button
          type="button"
          onClick={onClose}
          class="cfy-btn cfy-btn-ghost h-7 w-7 p-0 text-base"
          aria-label="Close changes"
        >
          ×
        </button>
      </header>

      {diff.changes.length === 0 ? (
        <div class="flex min-h-0 flex-1 items-center justify-center px-6 text-center">
          <div>
            <p class="font-serif text-sm font-semibold text-ink">No semantic text changes</p>
            <p class="mt-1 text-xs text-muted">
              These versions differ only in ignored serialization details, if at all.
            </p>
          </div>
        </div>
      ) : (
        <div class="flex min-h-0 flex-1">
          <ol class="w-52 shrink-0 overflow-y-auto border-r border-line p-1.5" aria-label="Changed blocks">
            {diff.changes.map((item, index) => (
              <li key={`${item.cfy_id ?? "document"}-${index}`}>
                <button
                  type="button"
                  onClick={() => onSelect(index)}
                  aria-current={selected === index ? "true" : undefined}
                  class={`flex w-full items-center gap-2 rounded-ctl px-2 py-1.5 text-left text-xs ${
                    selected === index ? "bg-selected text-ink" : "text-muted hover:bg-hover hover:text-ink"
                  }`}
                >
                  <span class={`h-2 w-2 shrink-0 rounded-full ${dotClass(item)}`} />
                  <span class="min-w-0 flex-1 truncate">{blockLabel(item)}</span>
                  <span class="text-[10px] uppercase tracking-wide">{kindLabel(item)}</span>
                </button>
              </li>
            ))}
          </ol>
          <div class="min-w-0 flex-1 overflow-y-auto px-4 py-3" aria-live="polite">
            {change != null && <ChangeDetail change={change} degraded={diff.degraded} />}
          </div>
        </div>
      )}
    </section>
  );
}

function ChangeDetail({ change, degraded }: { change: ArtifactBlockDiff; degraded: boolean }) {
  return (
    <div class="mx-auto max-w-3xl">
      <div class="mb-2 flex flex-wrap items-center gap-2">
        <span class={`cfy-chip ${chipClass(change)}`}>{kindLabel(change)}</span>
        <code class="text-[11px] text-muted">{change.cfy_id ?? "Document fallback"}</code>
        {change.moved && <span class="cfy-chip bg-warn-bg text-warn">Reordered</span>}
        {degraded && change.cfy_id == null && (
          <span class="text-[11px] text-warn">Id-less content · text fallback</span>
        )}
      </div>
      {change.hunks.length > 0 ? (
        <div class="overflow-hidden rounded-ctl border border-line font-mono text-[11px] leading-relaxed">
          {change.hunks
            .filter((hunk) => hunk.text !== "")
            .map((hunk, index) => (
              <div
                key={index}
                class={`grid grid-cols-[1.5rem_1fr] gap-2 px-2 py-1 ${
                  hunk.kind === "added"
                    ? "bg-ok-bg text-ok"
                    : hunk.kind === "removed"
                      ? "bg-danger-bg text-danger"
                      : "bg-paper text-muted"
                }`}
              >
                <span aria-hidden="true">{hunk.kind === "added" ? "+" : hunk.kind === "removed" ? "−" : " "}</span>
                <span class="whitespace-pre-wrap break-words">{hunk.text}</span>
              </div>
            ))}
        </div>
      ) : (
        <p class="rounded-ctl bg-well px-3 py-2 text-xs text-muted">
          {change.moved
            ? "Text is unchanged; this block moved within the artifact."
            : "No word-level text is available for this change."}
        </p>
      )}
      {change.kind === "removed" && (
        <p class="mt-2 text-[11px] text-muted">
          This block exists only in the earlier version. The red dashed gutter marks its nearest
          surviving neighbor.
        </p>
      )}
    </div>
  );
}

function countChanges(changes: ArtifactBlockDiff[]) {
  return changes.reduce(
    (counts, item) => {
      if (item.kind === "added") counts.added += 1;
      else if (item.kind === "removed") counts.removed += 1;
      else if (item.kind === "modified") counts.modified += 1;
      if (item.moved) counts.moved += 1;
      return counts;
    },
    { modified: 0, added: 0, removed: 0, moved: 0 },
  );
}

function summary(counts: ReturnType<typeof countChanges>, total: number): string {
  if (total === 0) return "No changed blocks";
  const parts = [
    counts.modified > 0 ? `${counts.modified} modified` : null,
    counts.added > 0 ? `${counts.added} added` : null,
    counts.removed > 0 ? `${counts.removed} removed` : null,
    counts.moved > 0 ? `${counts.moved} reordered` : null,
  ].filter(Boolean);
  return parts.join(" · ");
}

function blockLabel(change: ArtifactBlockDiff): string {
  if (change.cfy_id == null) return "Document fallback";
  return change.new_text ?? change.old_text ?? change.cfy_id;
}

function kindLabel(change: ArtifactBlockDiff): string {
  if (change.kind === "unchanged" && change.moved) return "Moved";
  return change.kind[0].toUpperCase() + change.kind.slice(1);
}

function dotClass(change: ArtifactBlockDiff): string {
  if (change.kind === "added") return "bg-ok";
  if (change.kind === "removed") return "bg-danger";
  if (change.moved) return "bg-warn";
  return "bg-info";
}

function chipClass(change: ArtifactBlockDiff): string {
  if (change.kind === "added") return "bg-ok-bg text-ok";
  if (change.kind === "removed") return "bg-danger-bg text-danger";
  if (change.moved) return "bg-warn-bg text-warn";
  return "bg-info-bg text-info";
}
