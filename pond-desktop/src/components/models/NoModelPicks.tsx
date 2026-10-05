import { useCallback, useMemo, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { useModels } from "../../hooks/useModels";
import { useModelActions } from "../../hooks/useModelActions";
import { dismissPending, downloadAndUse, usePendingUse } from "../../state/downloadAndUse";
import { PickCard } from "../../sections/models/PickCard";
import { askingPick, modelLabel, raisedPick, recommendedPicks } from "../../sections/models/modelsView";
import type { ModelEntry } from "../../api/types";
import "../../styles/no-model.css";

/**
 * Shown where the conversation would be when no model is chosen. The pond picks nothing and
 * downloads nothing on its own, so this offers GIAP's suggestions with what they cost, and an
 * explicit "Download and use": it downloads, then makes the model the conversation model, only
 * because it was pressed.
 */
export function NoModelPicks() {
  const data = useModels();
  const { models, roles, memory, loading, error, transferFor } = data;
  const pending = usePendingUse();
  const [problem, setProblem] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const say = useCallback((text: string, ok = true) => {
    setProblem(ok ? null : text);
    setNote(ok ? text : null);
  }, []);
  const actions = useModelActions(data, say);

  const picks = useMemo(() => recommendedPicks(models), [models]);
  const waiting = useCallback(
    (m: ModelEntry) => pending.some((p) => p.id === m.id && p.state !== "failed"),
    [pending],
  );
  const asking = askingPick(picks, roles, (m) => waiting(m) || transferFor(m) !== null);
  const raised = raisedPick(picks, roles);
  const failures = pending.filter((p) => p.state === "failed");

  async function downloadThenUse(m: ModelEntry) {
    actions.setBusy(true);
    setProblem(null);
    try {
      const started = await downloadAndUse(m, modelLabel(m), actions.withPictures(m));
      setNote(started?.message ?? null);
      await data.reloadDownloads();
      data.watchDownloads();
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e));
    } finally {
      actions.setBusy(false);
    }
  }

  return (
    <section className="nm" aria-labelledby="nm-title">
      <h2 className="nm__title" id="nm-title">Pick a model to talk with</h2>
      <p className="nm__lead">
        This pond has no conversation model yet. Nothing is downloaded until you choose one.
      </p>

      {(problem || error) && (
        <p className="nm__problem" role="alert">
          <AlertTriangle size={14} aria-hidden="true" />
          <span>{problem ?? `Could not read the list of models: ${error}`}</span>
        </p>
      )}
      {failures.map((f) => (
        <p className="nm__problem" role="alert" key={f.id}>
          <AlertTriangle size={14} aria-hidden="true" />
          <span>
            Could not use {f.title}: {f.message}
          </span>
          <button type="button" className="mm-btn" onClick={() => dismissPending(f.id)}>
            Dismiss
          </button>
        </p>
      ))}

      {loading && <p className="nm__lead">Reading the picks…</p>}
      {!loading && picks.length === 0 && !error && (
        <p className="nm__lead">This pond has no picks to show. Open Models to add one.</p>
      )}

      <div className="mm-picks">
        {picks.map((m) => {
          const transfer = transferFor(m);
          return (
            <PickCard
              key={m.id}
              model={m}
              memory={memory}
              inUse={false}
              raised={raised?.id === m.id}
              asking={asking?.id === m.id}
              transfer={transfer}
              withPictures={actions.withPictures(m)}
              busy={actions.busy || (waiting(m) && transfer === null)}
              onWithPictures={(next) => actions.setWithPictures(m, next)}
              onUse={() => void downloadThenUse(m)}
              onDownload={() => void downloadThenUse(m)}
              onControl={(action) => transfer && void actions.control(m, transfer, action)}
              downloadLabel="Download and use"
            />
          );
        })}
      </div>

      {note && (
        <p className="nm__note" role="status">
          {note}
        </p>
      )}
      {pending.some((p) => p.state === "downloading") && (
        <p className="nm__note" role="status">
          It starts answering as soon as it arrives. You can leave this screen meanwhile.
        </p>
      )}
    </section>
  );
}
